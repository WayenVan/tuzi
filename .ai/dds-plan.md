# Tuzi DDS（发布订阅事件总线）设计与实施计划

更新时间：2026-09-17

对应 `README.md` Roadmap 第 2 条：「Socket event bus — a Yazi-style
publish/subscribe mechanism for external commands, integrations, and
inter-process communication.」本计划是这一条的具体化方案，参考了
`/Users/wayenvan/tree-dev/yazi` 的 `yazi-dds` 设计，并按 Tuzi 当前架构
（无 Lua 插件层、单一 `mpsc::UnboundedSender<Event>`、Command 驱动同步
dispatch）做了裁剪。

与配置化改造、命令化改造是三条独立工作线，互不阻塞，但 DDS 会复用
`Command`/`Event`/`Dispatcher` 三个既有概念，改动前建议先确认这三者当前
状态与 `.ai/configuration-recap.md` 记录一致。

## 参考调研结论摘要

### yazi DDS 的核心形状

- `Payload{receiver, sender, body}` 信封 + `Ember`（闭合 enum，内建约
  19 种 kind）+ `EmberCustom`（string-keyed 动态兜底）两段式消息模型。
- `LOCAL`/`REMOTE` 两张 `kind -> {subscriber -> callback}` 订阅表；
  `sub_remote` 会触发一次能力（ability）重新广播。
- 本地投递也必须绕回主事件循环的 `accept_payload` actor 再回调订阅者，
  不在 publish 调用点直接执行，从而与其余状态变更共享同一条序列化路径。
- 传输层是本机 Unix Domain Socket；第一个连接失败的实例自举为 server，
  其余实例都是 client；`hi`/`hey` 握手同步各实例的订阅能力，未声明兴趣
  的 kind 不会离开发布者所在进程。
- `@` 前缀的 kind 是「静态/持久」消息，由 server 缓存并在握手时重放给
  新加入的实例，磁盘落盘保证跨重启存活。
- 新增内建 kind 是模板化流程（新增结构体 + 接入 enum + `from_str` +
  `kind()` + 可选 `pub_after!` 宏），历史上已通过宏消除过重复样板。

详见调研原始记录（未落盘，仅供本次设计参考）：yazi 侧覆盖
`yazi-dds/src/{payload,ember,pubsub,client,server,stream,state,pump}.rs`
与 `yazi-plugin/src/pubsub/*`、`yazi-actor/src/app/accept_payload.rs`。

### Tuzi 当前架构要点

- 单一 `tokio::sync::mpsc::UnboundedSender<Event>`，`App::serve` 主循环
  统一消费；所有异步生产者（`scheduler/fs.rs`、`scheduler/preview.rs`、
  `watcher/mod.rs`、`tasks.rs`、`opener.rs`）持有 `tx` clone 回灌事件。
- `Event`（`src/event.rs`）与 `Command`（`src/command.rs`）是两条独立
  的 match 路由：用户按键/`:` 命令 -> `Command` -> `App::execute()`
  同步执行；后台异步结果 -> `Event` -> `Dispatcher::dispatch_event()`。
- 渲染是无差分的立即模式重绘，没有观察者/订阅式重渲染机制，因此新增的
  发布订阅层不需要考虑渲染订阅，只需要保证「投递 -> 产生 Command ->
  `App::execute()`」这条链路即可复用现有重绘触发。
- 没有 Lua/脚本层，`grep` 未发现相关依赖；因此本地订阅者只能是 Rust
  原生注册的 handler，不是动态语言闭包。
- `src/actor/` 目前是未使用的空 `Actor` trait 存根，与本计划无直接关系，
  实施前应与用户确认是否可以复用或应先清理。
- 代码规模约 11.7k 行，`src/app/tab.rs`（2349 行）与 `src/app/app.rs`
  （1261 行）是 `Command`/`Event` 最终落地执行的地方，任何新增分发层都
  会牵涉这两个文件里对应事件的产生点。

## 设计方案

### 分层对照

| 层 | yazi 做法 | Tuzi 方案 |
|---|---|---|
| 消息模型 | `Payload{receiver,sender,body:Ember}`，闭合 enum + `Custom` 兜底 | 同构，命名为 `Payload`/`Body` |
| 本地分发 | 绕回主循环 `accept_payload` actor 再回调 Lua | 绕回主循环，`Event::DdsDeliver` -> `Dispatcher` -> handler 返回 `Vec<Command>` -> `App::execute()` |
| 订阅表 | `LOCAL`/`REMOTE` 两张表，key 为插件名 | 结构一致，key 为模块名/subscriber id |
| 跨进程传输 | 本机 Unix Domain Socket，先启动者自举为 server，能力握手过滤转发 | 同构直接复用 |

### 新模块 `src/dds/`

单 crate 项目，不像 yazi 拆独立 crate：

```text
src/dds/
  mod.rs        // 对外入口：re-export BUILTIN_KINDS/Body/Registry/Client   [P1+P3 已实现]
  body.rs       // Body 枚举：Hi/Hey/Cd/Yank/Renamed/TaskDone/Custom        [P1+P2+P3 已实现]
  registry.rs   // Registry：kind -> {subscriber -> handler}，App 的字段    [P1 已实现]
  payload.rs    // Payload{receiver,sender,body} envelope + 序列化          [P3 已实现]
  transport.rs  // Unix Socket Client/Server（首个实例自举为 server）       [P3 已实现]
```

没有 `state.rs`：`@` 静态消息持久化被明确否决（见「会话状态跨重启」
一节）。下一阶段先完善现有 `transport.rs` 的连接生命周期，再把常驻
`Client` 接入 `App::serve`；`--state` 与退出广播顺延到 P5。

`Custom(kind, data)` 兜底分支和 `serde_json` 依赖已随 P2 落地（见下面
「实施阶段」）。`Registry` 仍然没有 yazi 那样的 `LOCAL`/`REMOTE` 两张表：
P1/P2 都是单实例场景，`REMOTE`（转发给其他实例的订阅）要等 P3 有了真正
的跨实例概念才有意义。

### Body 内建 kind

只把「对外语义有意义」的事件提升为 DDS topic，例如 `open`/`cd`/`yank`/
`rename`/任务完成；`Loaded`/`PreviewLoaded`/`CompletionLoaded` 这类纯
内部 IO 分片进度继续走现有 `Event` 通道，不进入 `Body`。

跨实例传输的目标形态如下。当前除可选的 `Bye` 外均已实现：

```rust
pub enum Body {
    Hi { abilities: Vec<String> },                    // [P3 已实现]
    Hey { peers: Vec<PeerInfo> },                     // [P3 已实现]
    Bye,                                               // [待实现，可选]
    Cd { path: PathBuf },
    Hover { path: Option<PathBuf> },                  // [已实现]
    Yank { paths: Vec<PathBuf>, cut: bool },
    Renamed { from: PathBuf, to: PathBuf },
    TaskDone { kind: TaskKind, ok: bool },
    Custom { kind: String, data: serde_json::Value },
}
```

P1+P2 目前实际实现的 `src/dds/body.rs`（没有 `Hi`/`Hey`/`Bye`/`Hover`，
其余字段一致）：

```rust
pub enum Body {
    Cd { path: PathBuf },
    Yank { paths: Vec<PathBuf>, cut: bool },
    Renamed { from: PathBuf, to: PathBuf },
    TaskDone { kind: TaskKind, ok: bool },
    Custom { kind: String, data: serde_json::Value },
}
```

### 与现有 dispatch 的接线

```rust
// src/event.rs 新增
pub enum Event {
    // ...
    Pubsub(Payload),
}
```

```rust
// src/app/dispatcher.rs 的 dispatch_event 里
Event::DdsDeliver(payload) => dds::registry::deliver(app, payload),
```

`deliver` 查表拿到 handler 列表并调用，handler **不直接 mutate
state**，而是返回 `Vec<Command>` 交给 `app.execute(cmd)`。这是相对
yazi 的关键改造点：yazi 的回调直接操作 Lua 侧的 `Ctx`，Tuzi 没有脚本
层，用「handler 返回 Command 列表」替代，天然复用现有的同步执行路径，
不需要额外锁。

### 本地订阅 API（P1 已实现，见下面「实施阶段」的实际签名）

```rust
pub type Handler = Box<dyn Fn(&Body) -> Vec<Command> + Send + Sync>;

impl Registry {
    pub fn sub(&mut self, subscriber: &str, kind: &str, f: Handler) -> bool;
    pub fn unsub(&mut self, subscriber: &str, kind: &str);
    pub fn deliver(&self, body: &Body) -> Vec<Command>;
}
```

同一 subscriber 对同一 kind 只能注册一次（对齐 yazi 防重复订阅行为）。

这里没有 yazi 式的 `sub_remote`/统一 `Pubsub` 门面：`Registry` 仍只负责
进程内 `Body -> Command`，`dds::Client` 只负责 socket 生命周期和传输。
P4 已把两者在 App 边界接通：建立 Client 前完成内建 Registry 注册，再用
`Registry::abilities()` 的 kind 快照发送 `Hi`；socket inbox 回灌
`Event::DdsDeliver` 后仍由 Registry 决定如何处理。当前生产订阅只有
`set-state`，所以 TUI 声明 `["set-state"]`；通配符 `"*"` 只给
`tuzi sub` 这种调试流量探针使用。

### 跨实例传输（P3 已实现，见下面「实施阶段」的细节）

- Socket 路径：`$XDG_RUNTIME_DIR/tuzi/dds.sock`，缺省回退系统临时目录下
  带有效 UID 的 `tuzi-<uid>/dds.sock`，避免不同本机用户共享路径
  （`src/dds/payload.rs::socket_path`，手写 env 查找，风格对齐
  `config::default_config_dir`，没有引入 XDG crate）。
  Server 校验 runtime 目录归当前用户所有并收紧为 `0700`，socket 为
  `0600`。
- 协议：换行分隔的单行 JSON object，用 `Payload`/`Body` 的默认 serde
  enum 表示（`{"Cd":{"path":"..."}}` 这种外部打标签形式），比 yazi 的
  逗号分隔混合格式更简单，两端都是 Rust，不需要跨语言兼容，仍保持纯
  文本可 `nc`/`cat` 调试。
- 握手：`Hi`（携带本实例声明的 kind 集合）-> server 记录 -> 广播 `Hey`
  （全量 peer 表）。
- 转发过滤：`receiver==0` 只广播给声明了对应 ability 的 peer（或声明了
  通配符 `"*"`，见下）；否则定点转发。
- 不做 `@` 静态消息持久化/服务端缓存——跨重启的状态恢复已经明确划给
  外部程序负责（见「会话状态跨重启」一节），tuzi 自己不缓存任何保留值。

### CLI 对外接口（已实现）

- `tu dds pub <kind> [json]`：一次性广播，`json` 缺省 `null`，`kind` 为空
  或撞上 `dds::BUILTIN_KINDS` 会被拒绝。不需要声明 ability（只发不收）。
- `tu dds pub-to <peer-id> <kind> [json]`：绕过 ability 过滤，定点发送给
  一个在线 peer。
- `tu dds sub [kind...]`：声明给定 ability；未给 kind 时声明通配符
  `dds::WILDCARD_ABILITY`（`"*"`），持续打印收到的消息；`--json` 输出
  一行一个完整 JSON `Payload`。这是 `ya sub` 的等价
  物，没有做 yazi 那样的 `--local-events`/`--remote-events` 过滤参数
  （用不上：`tu dds sub` 本身就是唯一目的是看流量的调试用途，不像 yazi
  那样要跟正常运行的 TUI 共享同一个二进制的参数体系）。
- `tu dds peers` 等待一次 `Hey` 并打印 peer id 与 abilities；`sub`/`peers`
  都支持 `--json`。每个 clap 子命令的 `--help` 内置可直接复制的示例。
- 旧的 `tuzi emit` / `tuzi sub` 暂时保留为兼容入口；新的 DDS 命令不再与
  `[PATH]` 位置参数争用。旧入口仍存在
  "第一个参数字面量等于 `emit`/`sub`"，跟已有的 `[PATH]` 位置参数解析
  共用同一个 `parse_args`。真有目录字面量叫 `emit`/`sub` 时用
  `tuzi -- emit`/`tuzi -- sub` 转义（复用已有的 `--` 语法，不是新加的）。
- `Command` 新增 `Command::Emit(kind, Option<json>)` 变体，使 keymap 的
  `run = "emit my-kind {...}"` 可以直接发布事件，在脚本引擎出现之前先
  提供一定可编程性。

### Event/State 双轨模型（概念记录，未被采用于跨重启恢复）

讨论过程中曾提出一个通用抽象：消息除了「一次性事实」（Event，不保留，
过时即丢）之外，还可以有「保留最新一份」的语义（State，按 key 覆盖式
保留，随时可查询，类比 Kafka 的 compacted topic）。这个区分本身是对的、
仍然是有效的词汇，但**最终决定不用它来做"tuzi 重启后记得上次状态"这
件事**——原因见下面新方案，这里只留概念记录，避免以后又绕回来重新发明。

### 会话状态跨重启：交给外部程序，tuzi 只管"退出广播 + 启动注入"

明确决定：tuzi **不自己维护任何跨重启的状态缓存**（不管内存还是磁盘）。
"记住上次打开的目录/选中了哪些文件"这件事，由外部包装程序（shell 脚本
或更上层的 session 管理器）自己订阅、自己存、自己在下次启动时喂回来。
tuzi 的职责收窄成两件对称的事，都是**一次性广播/一次性接收**，不是
"保留最新值供查询"：

1. **启动时注入**：新增 CLI 参数 `--state <JSON>`，外部程序调用
   `tuzi --state '{"selection":["a.txt","b.txt"]}' /path/to/dir` 时把它
   自己存好的状态喂进来。`path` 沿用已有的位置参数，不重复放进
   `--state` 里；`--state` 目前只携带 `selection`（选中的文件列表），
   应用方式是启动第一个 tab 后把这些路径塞进
   `tab.selection`（存在于当前列表里的才生效，找不到的静默忽略，不算
   错误——目标目录的内容随时可能变化）。
   ```rust
   enum Cli {
       Run { path: PathBuf, config: LoadOptions, state: Option<RestoreState> },
       ...
   }
   struct RestoreState { selection: Vec<PathBuf> }
   ```
2. **退出时广播**：`App::serve` 主循环 `app.quit` 即将返回前，通过 P4b
   已接入的常驻 DDS Client 发布 `Body::Custom { kind: "exit-state", data:
   {"path": ..., "selection": [...]} }`，等待已排队消息完成写入后再退出。
   给收尾过程设置短超时（例如 500ms），避免 socket 故障卡住退出。早期
   方案曾考虑退出时临时连接；该方案已经废弃，因为 TUI 在 P4b 会成为
   常驻 Peer，并且运行期间也需要收发 `set-state` 等消息。

这样设计的关键含义，需要用户理解并在写外部包装脚本时对应处理：

- **这是纯 Event，没有保留值**：`exit-state` 广播时，外部程序必须已经
  有一个类似 `tuzi sub` 的监听者正连着 socket，否则这条消息广播出去
  没人接住就彻底丢了，不会有任何地方能"事后查询"到它。典型用法是
  包装脚本形如：
  ```sh
  tuzi sub | grep '"kind":"exit-state"' > /tmp/tuzi-last-state.json &
  SUB_PID=$!
  tuzi --state "$(cat ~/.cache/tuzi-last-state.json 2>/dev/null)" "$dir"
  kill "$SUB_PID"
  ```
- 之所以选这个方案而不是内存/磁盘 State 缓存，是因为「新开 tab 恢复
  同一次运行里的状态」（内存 State 就够用）和「关掉 tuzi 再打开还记得
  上次」（这次讨论的场景）其实是两个不同的需求，而后者被明确划给外部
  程序负责，tuzi 没有必要为了自己不需要的持久化能力增加复杂度。

## 实施阶段

1. **P1 内部骨架**（已完成）：`src/dds/{body,registry}.rs` + `Event::DdsDeliver`
   接线，纯进程内、无 socket。已把 `cd`（`Tab::cd_inner`）、`yank`
   （`App::yank_selected`）、`rename`（`Tab::confirm_rename`）、任务完成
   （`App::on_task_event`）四个内部事件迁到这条路径。`Registry` 是
   `App` 的一个字段（不是全局静态），`sub`/`unsub` 按 kind+subscriber
   name 去重，`deliver` 返回 `Vec<Command>` 交给 `App::execute()` 执行，
   与用户按键走同一条同步路径。P1 阶段暂未引入 `Payload{receiver,
   sender,...}` 信封结构——单实例场景下这些字段没有意义，留到 P3 引入
   跨实例传输时再加。已用
   `app::tests::a_pubsub_subscriber_can_drive_app_state_through_a_yank`
   验证端到端链路：订阅 -> 发布 -> `Event::DdsDeliver` -> `Dispatcher` ->
   `App::execute()`。
   - 涉及测试基础设施的连带修改：`app.rs`/`tab.rs` 测试模块里的 `pump`/
     `apply` 辅助函数原先假设事件通道里只有测试期望的那一个事件，新增
     的 `Event::DdsDeliver` 会先于目标事件被消费掉，导致既有测试断言错位。
     两处 `pump` 都已改为「跳过 Pubsub 事件继续等待」，`tab.rs` 的
     `apply` 对 `Event::DdsDeliver(_)` 显式忽略（无订阅者时是合法的空操作）。
2. **P2 CLI 可编程**（已完成）：新增 `Command::Emit { kind: String, data:
   serde_json::Value }`，让 keymap/`:` 命令能发布任意 custom kind，不
   依赖 socket。语法定为 `emit <kind> [json]`：
   - `emit my-kind` — `data` 缺省为 `Value::Null`。
   - `emit my-kind '{"a":1}'` — JSON 参数必须整体用单引号包住，直接
     复用现有 tokenizer 的引号处理（双引号在 tokenizer 里会被当成
     token 内的二次引号消耗掉，裸写 `{"a":1}` 不加外层单引号会被
     tokenizer 吃掉内部的双引号，解析出 `{a:1}` 这种非法 JSON）。
   - `kind` 为空或撞上内建 kind（`cd`/`yank`/`renamed`/`task-done`，见
     `dds::BUILTIN_KINDS`）会被拒绝，防止 `emit` 冒充内建事件。
   - `Body` 补上了 P1 特意留空的 `Custom { kind, data }` 兜底分支；
     `App::execute` 里 `Command::Emit { kind, data } =>
     self.publish(Body::Custom { kind, data })`。
   - `Command` 的 derive 从 `Eq, PartialEq` 降成只有 `PartialEq`
     （`serde_json::Value` 含 `f64`，不能 `Eq`），连带 `keymap::Route`
     也去掉了 `Eq`。
   - 端到端测试：`app::tests::the_emit_command_publishes_a_custom_kind_with_its_json_payload`
     跑通 `":emit ..."` 解析 -> `execute` -> `Event::DdsDeliver` ->
     `Dispatcher` -> 订阅者收到 `data` 且产生的 `Command` 被执行。
3. **P3 跨实例 socket**（已完成）：新增 `src/dds/{payload,transport}.rs`
   + `Cargo.toml` 加 `serde_json`/tokio `net` feature + `TaskKind` 补
   `Serialize`/`Deserialize` + `Body`/`PeerInfo`/`Payload` 全部可序列化。
   - `Client::connect(socket_path, abilities)`：先 `UnixStream::connect`，
     失败则 `connect_or_bootstrap` 尝试自举 server 再连自己；返回
     `(Client, mpsc::UnboundedReceiver<Payload>)`。`Client::publish`/
     `publish_to`/`flush`（后者给 `tuzi emit` 这种一次性调用用，关闭
     发送端并等 supervisor 把已入队的消息真正落到 socket 上再返回）。
   - `Server`（`transport.rs` 内部私有，外部拿不到句柄）：`try_bind` +
     `serve`（accept 循环，每个连接一对读写 task + 一份 `PeerTable`）。
     `Hi` -> 记录 ability -> 广播 `Hey`；`receiver==0` 按 ability 过滤
     广播（含通配符 `dds::WILDCARD_ABILITY = "*"`，`tuzi sub` 用它收
     全部消息）；`receiver!=0` 定点转发；连接断开时移出 peer 表并立即向
     剩余 Peer 广播新版 `Hey`，避免各 Client 持有过期的 Peer 列表。
   - `tuzi emit <kind> [json]` / `tuzi sub`：`src/main.rs` 新增 `Cli`
     变体，`parse_args` 在进入原有的位置参数解析前先看第一个参数是不是
     字面量 `emit`/`sub`。
   - **自举竞态**：多个进程同时冷启动（谁都连不上，都要抢着 `bind`）时，
     `bind` 失败的一方要连去赢家那里而不是直接报错，且不能无条件
     `remove_file` 抢位置——那样会把刚绑定成功、活得好好的赢家 socket
     从文件系统里删掉，产生孤儿 server。`connect_or_bootstrap` 用带
     随机抖动的指数退避重试（避免所有竞争者在同一个 tick 上一起判定
     "死了"然后一起冲上去删）+ 连续 3 次判定"既绑不上也连不上"才清理
     stale 文件。压测过：15 个进程同时冷启动能把失败率控制在个位数百
     分比（不追求归零——那需要真正的 flock 文件锁，对"一个长驻 TUI +
     偶尔几次 emit/sub"这个实际使用模式不值得），5 个进程同时冷启动在
     75 次试验里 0 失败，这才是这套机制真正要扛住的场景。
   - **P3 没做的事**：交互式 `App` 还没接入 `Client`——正在跑的 TUI 收不
     到别的实例或 `tuzi emit` 发来的消息（P4 补上）；`Body` 没有 `Bye`
     变体，靠连接断开（EOF）让 server 清理 peer 表，没做 yazi 那样的
     优雅下线握手；没有 `@` 静态消息持久化（已明确不做，见「会话状态
     跨重启」一节）。
4. **P4a DDS 连接生命周期闭环**（已完成）：先让传输层在承载 Server 的
   进程退出后能够自行恢复，再接业务层。当前 P3 只会在首次连接时自举
   Server；连接建立后若读到 EOF 或写入失败，读写 task 会直接结束，仍在
   运行的 Peer 不会重连。这会让后续所有 App 集成都建立在不稳定的生命周期
   上。P4a 只修改 `src/dds/transport.rs` 及其测试，不接触 `App`、
   `Registry`、`Command`：
   - 把 `Client` 重构为常驻连接管理器；它保存自己的 `id`、abilities 和
     出站队列，并在内部拥有单一 supervisor task。
   - 首次连接和断线重连共用同一条 `connect_or_bootstrap` 路径；读 EOF 或
     写失败后立即进行第一次重连/选主。只有连续失败才按
     `20ms -> 50ms -> 100ms -> 250ms -> 500ms` 退避并封顶，避免长故障
     期间忙循环。
   - 每次成功连接都重新发送 `Hi`，确保新 Server 恢复该 Peer 的 abilities；
     已收到的 `Hey` 继续通过 inbox 交给上层。
   - 写失败时只重试当前尚未确认写入的消息一次；DDS 仍是 best-effort，
     不引入 ACK、磁盘队列或 exactly-once 语义。
   - 保持 `Registry` 与传输层完全独立：Registry 只做进程内
     `Body -> Vec<Command>`，不参与连接、选主和重连。
   - 已新增确定性测试：启动 Server-owner 与两个 Peer，终止 owner 后验证
     幸存 Peer 能重新选出 Server、重发 `Hi`，并继续相互收发。

5. **P4b App 接入 DDS 主循环 + `Command::SetState`**（已完成）：P4a
   通过后再让 TUI
   成为可靠的常驻 Peer。真实需求是广播 App 当前 state，并允许其它客户端
   发"设置 state"的消息来改这个 App 的状态。这个需求复用现有 `Command`
   机制，不引入 Event/State 双轨模型："收到消息 -> 翻译成 Command ->
   `App::execute()`"跟按键触发走同一条路。具体：
   - [已完成] `App` 新增字段持有一个常驻 `dds::Client`：`App::serve` 构造
     `App` 之后 `Client::connect(socket_path, abilities)` 一次，贯穿
     整个运行期（不再是 `tuzi emit`/`tuzi sub` 那种一次性连接）。
   - [已完成] ability 策略：`Registry::abilities()` 返回去重、排序后的已
     注册 kind，App 用该启动时快照发送 `Hi`。当前值为 `["set-state"]`；
     `tuzi sub` 仍用 `"*"` 接收全部流量。运行期动态 sub/unsub 尚无生产
     用例；未来若加入，需要在注册表变化后重新发送 `Hi`。
   - [已完成] 后台任务把 `Client` 收到的 `Payload` 转成 `Event::DdsDeliver(body)`
     灌回 `tx`，直接复用 P1 已有的 `Dispatcher::dispatch_event ->
     Registry.deliver -> Command -> App::execute()`，不需要新写分发
     逻辑。
   - [已完成] 本地生产者通过 `Event::DdsPublish` 与本地分发
     `Event::DdsDeliver` 明确区分，防止入站消息形成广播回路。显式 `emit`
     在 DDS 启用时直接外发；隐式 `cd`/`hover`/`yank`/`renamed`/`task-done` 只有
     列入 `dds.broadcast` allowlist 才外发，默认列表为空。
   - [已完成] 新增 `Command::SetState { path: Option<PathBuf>, selection:
     Vec<PathBuf> }`（字段跟下面 P5 的 `--state`/`RestoreState` 保持
     一致——出现第二个"批量应用一份状态快照"的场景时，就值得让两者共用
     同一个 apply 函数）。
   - [已完成] 注册一个内建订阅者（`registry.sub("core", "set-state", handler)`），
     把 `Body::Custom { kind: "set-state", data }` 翻译成
     `Command::SetState`。
   - `selection` 中的相对路径以应用 state 后的 tab root 为基准；绝对路径
     必须位于该 root 下。不存在或越出 root 的路径静默忽略。这样即使新
     root 的异步 listing 尚未返回，也能基于文件系统安全恢复 selection。
   - "广播 state"的具体触发点（每次 cd/yank 都广播，还是只在显式请求
     时才广播）留到实现时定，见「尚待确认事项」。
6. **P5 会话状态跨重启**：按上面「会话状态跨重启：交给外部程序」一节
   实施——`Cli::Run` 加 `state: Option<RestoreState>` 字段和
   `--state <JSON>` 解析；`RestoreState { selection: Vec<PathBuf> }`
   应用到初始 tab 的 `selection`（跟 P4b 的 `Command::SetState` 共用同一
   个 apply 函数）；`App::serve` 的 `quit` 分支退出前广播
   `Body::Custom { kind: "exit-state", data: {...} }`——P4b 落地后 `App`
   已经持有常驻 `Client`，这里直接复用它 `publish` 一次 + `flush`，
   不用再像最初设想的那样单独开一次一次性连接。**不需要**
   `publish_state`/`get_state`、不需要磁盘持久化、不需要
   `Command::RestoreTab`（并入 P4 的 `Command::SetState`）。

每阶段应可独立验证、独立提交，不必一次性大改完才能用。

## 已确认的实施顺序

P4a 与 P4b 已完成并通过测试：TUI 是声明 Registry 精确 ability 的常驻
Peer（当前为 `set-state`），socket inbox 会回灌 `Event::DdsDeliver`，显式
emit 会对外广播，隐式内建事件受 `dds.broadcast` allowlist 控制；
`set-state` 会经 Registry 转为 `Command::SetState`。
下一步进入 P5，实现 `--state` 启动注入和 `exit-state` 退出广播。

## 尚待确认事项

- `src/actor/` 空存根是否要在本计划里复用或先删除，需与用户确认。
- `--state` 目前只设计了 `selection` 一个字段；要不要扩展到
  `sort_policy`/`column_mode`/展开的子树等，等外部程序真的需要时再加，
  不必一次性照搬全部 Tab 字段。
- P4b 首次 DDS 连接失败时，TUI 是否降级为纯本地模式并显示 warning；建议
  降级，不让辅助总线阻止文件管理器启动。P4a 内部的暂时断线则自动重连。
- 退出广播的超时时长（草案 500ms）、以及最终发送失败时是否给用户一个
  可见警告还是静默退出，留到 P5 实现时定。

## 续接建议

新 session 开始时先运行：

```text
cargo check
cargo test
git diff --check
```

确认基线后，检查本文件「实施阶段」的当前进度。当前 P1-P4b 已完成，下一步
是 **P5 会话状态跨重启**：让 `--state` 与 `Command::SetState` 共用状态应用
路径，然后实现带短超时的 `exit-state` 退出广播。
