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
| 本地分发 | 绕回主循环 `accept_payload` actor 再回调 Lua | 绕回主循环，`Event::Pubsub` -> `Dispatcher` -> handler 返回 `Vec<Command>` -> `App::execute()` |
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
一节），P4 不需要新增 dds 子模块，只改 `main.rs`（`--state` 解析）和
`App::serve`（退出广播）。

`Custom(kind, data)` 兜底分支和 `serde_json` 依赖已随 P2 落地（见下面
「实施阶段」）。`Registry` 仍然没有 yazi 那样的 `LOCAL`/`REMOTE` 两张表：
P1/P2 都是单实例场景，`REMOTE`（转发给其他实例的订阅）要等 P3 有了真正
的跨实例概念才有意义。

### Body 内建 kind

只把「对外语义有意义」的事件提升为 DDS topic，例如 `cd`/`yank`/
`rename`/任务完成；`Loaded`/`PreviewLoaded`/`CompletionLoaded` 这类纯
内部 IO 分片进度继续走现有 `Event` 通道，不进入 `Body`。

P3 引入跨实例传输后的目标形态（`Hi`/`Hey`/`Bye`/`Hover` 是握手和跨实例
才需要的 kind，现在还不存在）：

```rust
pub enum Body {
    Hi { abilities: Vec<String>, version: String },   // [P3 待实现]
    Hey { peers: Vec<PeerInfo> },                     // [P3 待实现]
    Bye,                                               // [P3 待实现]
    Cd { path: PathBuf },
    Hover { path: Option<PathBuf> },                  // [P3 待实现]
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
Event::Pubsub(payload) => dds::registry::deliver(app, payload),
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

这里没有 yazi 式的 `sub_remote`/统一 `Pubsub` 门面——`Registry`（本地订阅
表）和 `dds::Client`（P3 的 socket 客户端）目前是两个独立的东西，没有
互相打通：`Registry` 只服务进程内订阅（`Event::Pubsub` -> `deliver` ->
`Command`），`Client` 只服务 `tuzi emit`/`tuzi sub` 这两个不跑 TUI 的
独立 CLI 调用。交互式的 `App` 目前不持有 `Client`，也就是说**正在运行
的 TUI 还没有接入 socket，收不到别的实例/`tuzi emit` 发来的消息**——
这是 P4 要补上的集成工作（见下面「实施阶段」），ability 策略已定为
方案 A：`App` 用通配符 `dds::WILDCARD_ABILITY` 声明，照单全收交给本地
`Registry.deliver` 按 kind 过滤。

### 跨实例传输（P3 已实现，见下面「实施阶段」的细节）

- Socket 路径：`$XDG_RUNTIME_DIR/tuzi/dds.sock`，缺省回退系统临时目录
  （`src/dds/payload.rs::socket_path`，手写 env 查找，风格对齐
  `config::default_config_dir`，没有引入 XDG crate）。
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

### CLI 对外接口（P3 已实现）

- `tuzi emit <kind> [json]`：一次性发布，`json` 缺省 `null`，`kind` 为空
  或撞上 `dds::BUILTIN_KINDS` 会被拒绝。不需要声明 ability（只发不收）。
- `tuzi sub`：声明通配符 ability `dds::WILDCARD_ABILITY`（`"*"`），打印
  收到的每条消息（一行一个 JSON `Body`），直到被中断——`ya sub` 的等价
  物，没有做 yazi 那样的 `--local-events`/`--remote-events` 过滤参数
  （用不上：`tuzi sub` 本身就是唯一目的是看流量的调试用途，不像 yazi
  那样要跟正常运行的 TUI 共享同一个二进制的参数体系）。
- 两个子命令都是 `src/main.rs` 里 `Cli` 枚举新增的两个变体，识别方式是
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
2. **退出时广播**：`App::serve` 主循环 `app.quit` 即将返回前，做一次
   跟 `tuzi emit` 内部逻辑完全一样的一次性发布——连接 DDS socket、
   `client.publish(Body::Custom { kind: "exit-state", data: {"path":
   ..., "selection": [...]} })`、`client.flush().await`、再真正退出。
   **不需要**让 App 常驻加入 socket、不需要解决"App 该声明哪些
   ability"这个之前搁置的难题——退出广播只是最后连一下、发一条、走人，
   跟正在运行时是否已经是 DDS peer无关。给这次连接+发布+flush 包一个
   超时（例如 500ms），避免 socket 有问题时卡住退出流程。

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

1. **P1 内部骨架**（已完成）：`src/dds/{body,registry}.rs` + `Event::Pubsub`
   接线，纯进程内、无 socket。已把 `cd`（`Tab::cd_inner`）、`yank`
   （`App::yank_selected`）、`rename`（`Tab::confirm_rename`）、任务完成
   （`App::on_task_event`）四个内部事件迁到这条路径。`Registry` 是
   `App` 的一个字段（不是全局静态），`sub`/`unsub` 按 kind+subscriber
   name 去重，`deliver` 返回 `Vec<Command>` 交给 `App::execute()` 执行，
   与用户按键走同一条同步路径。P1 阶段暂未引入 `Payload{receiver,
   sender,...}` 信封结构——单实例场景下这些字段没有意义，留到 P3 引入
   跨实例传输时再加。已用
   `app::tests::a_pubsub_subscriber_can_drive_app_state_through_a_yank`
   验证端到端链路：订阅 -> 发布 -> `Event::Pubsub` -> `Dispatcher` ->
   `App::execute()`。
   - 涉及测试基础设施的连带修改：`app.rs`/`tab.rs` 测试模块里的 `pump`/
     `apply` 辅助函数原先假设事件通道里只有测试期望的那一个事件，新增
     的 `Event::Pubsub` 会先于目标事件被消费掉，导致既有测试断言错位。
     两处 `pump` 都已改为「跳过 Pubsub 事件继续等待」，`tab.rs` 的
     `apply` 对 `Event::Pubsub(_)` 显式忽略（无订阅者时是合法的空操作）。
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
     跑通 `":emit ..."` 解析 -> `execute` -> `Event::Pubsub` ->
     `Dispatcher` -> 订阅者收到 `data` 且产生的 `Command` 被执行。
3. **P3 跨实例 socket**（已完成）：新增 `src/dds/{payload,transport}.rs`
   + `Cargo.toml` 加 `serde_json`/tokio `net` feature + `TaskKind` 补
   `Serialize`/`Deserialize` + `Body`/`PeerInfo`/`Payload` 全部可序列化。
   - `Client::connect(socket_path, abilities)`：先 `UnixStream::connect`，
     失败则 `connect_or_bootstrap` 尝试自举 server 再连自己；返回
     `(Client, mpsc::UnboundedReceiver<Payload>)`。`Client::publish`/
     `publish_to`/`flush`（后者给 `tuzi emit` 这种一次性调用用，关闭
     发送端并等后台写任务把已入队的消息真正落到 socket 上再返回）。
   - `Server`（`transport.rs` 内部私有，外部拿不到句柄）：`try_bind` +
     `serve`（accept 循环，每个连接一对读写 task + 一份 `PeerTable`）。
     `Hi` -> 记录 ability -> 广播 `Hey`；`receiver==0` 按 ability 过滤
     广播（含通配符 `dds::WILDCARD_ABILITY = "*"`，`tuzi sub` 用它收
     全部消息）；`receiver!=0` 定点转发；连接断开时移出 peer 表。
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
4. **P4 App 接入 DDS 主循环 + `Command::SetState`**：不再是预先搭脚手架
   ——现在有明确的真实需求驱动：广播 App 当前 state，并允许其它客户端
   发"设置 state"的消息来改这个 App 的状态。结论：**这个需求靠现有的
   `Command` 机制就能优雅实现，不需要引入 Event/State 双轨模型**——
   "收到消息 -> 翻译成 Command -> `App::execute()`"跟按键触发完全是
   同一条路，"广播 state"也就是普通的一次性 `publish`，都不需要
   "保留最新值可查询"这个语义。具体：
   - `App` 新增字段持有一个常驻 `dds::Client`：`App::serve` 构造
     `App` 之后 `Client::connect(socket_path, abilities)` 一次，贯穿
     整个运行期（不再是 `tuzi emit`/`tuzi sub` 那种一次性连接）。
   - ability 策略：先用通配符 `dds::WILDCARD_ABILITY`（`"*"`），照单全收
     交给本地 `Registry.deliver` 过滤——没人订阅的 kind 就是空操作，
     简单；等真出现性能/噪音问题，再收紧成"只声明 `Registry` 里已注册
     的 kind 集合"。
   - 后台任务把 `Client` 收到的 `Payload` 转成 `Event::Pubsub(body)`
     灌回 `tx`，直接复用 P1 已有的 `Dispatcher::dispatch_event ->
     Registry.deliver -> Command -> App::execute()`，不需要新写分发
     逻辑。
   - `App::publish` 同时对外广播（不再只 `tx.send` 本地）——本地事件
     （`cd`/`yank`/`emit` 等）从此也对外可见，这是"广播 state"这个
     需求成立的前提。
   - 新增 `Command::SetState { path: Option<PathBuf>, selection:
     Vec<PathBuf> }`（字段跟下面 P5 的 `--state`/`RestoreState` 保持
     一致——出现第二个"批量应用一份状态快照"的场景时，就值得让两者共用
     同一个 apply 函数）。
   - 注册一个内建订阅者（`registry.sub("core", "set-state", handler)`），
     把 `Body::Custom { kind: "set-state", data }` 翻译成
     `Command::SetState`。
   - "广播 state"的具体触发点（每次 cd/yank 都广播，还是只在显式请求
     时才广播）留到实现时定，见「尚待确认事项」。
5. **P5 会话状态跨重启**：按上面「会话状态跨重启：交给外部程序」一节
   实施——`Cli::Run` 加 `state: Option<RestoreState>` 字段和
   `--state <JSON>` 解析；`RestoreState { selection: Vec<PathBuf> }`
   应用到初始 tab 的 `selection`（跟 P4 的 `Command::SetState` 共用同一
   个 apply 函数）；`App::serve` 的 `quit` 分支退出前广播
   `Body::Custom { kind: "exit-state", data: {...} }`——P4 落地后 `App`
   已经持有常驻 `Client`，这里直接复用它 `publish` 一次 + `flush`，
   不用再像最初设想的那样单独开一次一次性连接。**不需要**
   `publish_state`/`get_state`、不需要磁盘持久化、不需要
   `Command::RestoreTab`（并入 P4 的 `Command::SetState`）。

每阶段应可独立验证、独立提交，不必一次性大改完才能用。

## 尚待确认事项

- P4 的具体广播触发点：`App::publish` 已经统一对外广播之后，是不是
  所有内部事件（`cd`/`yank`/`renamed`/`task-done`）都无条件对外广播，
  还是只有显式的 `Command::Emit`/`SetState` 相关的才广播？全量广播最
  简单，但意味着"这个人在哪个目录、选中了什么"默认就是对外可见的——
  要不要留一个配置项关掉，等实现时定。
- `src/actor/` 空存根是否要在本计划里复用或先删除，需与用户确认。
- `--state` 目前只设计了 `selection` 一个字段；要不要扩展到
  `sort_policy`/`column_mode`/展开的子树等，等外部程序真的需要时再加，
  不必一次性照搬全部 Tab 字段。
- 退出广播的超时时长（草案 500ms）、以及 socket 连接失败时是否要给用户
  一个可见的警告还是静默放弃退出流程，留到 P4 实现时定。

## 续接建议

新 session 开始时先运行：

```text
cargo check
cargo test
git diff --check
```

确认基线后，从「P1 内部骨架」开始实施；若 P1 已完成，检查本文件的
「实施阶段」章节确认当前进度并继续下一阶段。
