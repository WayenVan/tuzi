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
  mod.rs        // 对外入口：re-export Body/Registry                         [P1 已实现]
  body.rs       // Body 枚举：Cd/Yank/Renamed/TaskDone/Custom               [P1+P2 已实现]
  registry.rs   // Registry：kind -> {subscriber -> handler}，App 的字段     [P1 已实现]
  payload.rs    // Payload{receiver,sender,body} envelope + 序列化           [P3 待实现]
  transport.rs  // Unix Socket client/server（首个实例自举为 server）        [P3 待实现]
  state.rs      // 可选：@ 前缀 static topic 的磁盘持久化                    [P4 待实现]
```

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
这是刻意搁置的集成工作，见下面「尚待确认事项」。

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
- 第一期不做 `@` 静态消息持久化；等真的出现「新开 tab/实例需要立刻拿到
  当前状态」的需求再加（跟「Event/State 双轨模型」一节的 State 路径是
  同一件事，那边已经把接口形状定下来了）。

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

### Event/State 双轨模型（状态恢复的通用抽象）

「新开 tab 要恢复上次的路径和选中文件」这类需求，不该靠订阅方重放历史
事件来重建状态，而是需要在消息模型里正交切出第二种语义。这不是给
`Body` 加一个 `TabState` 变体那么简单，而是 Registry 从一开始就要区分
两种发布方式：

| | Event（事实流） | State（状态视图） |
|---|---|---|
| 语义 | "发生了一次什么" | "现在是什么" |
| 保留 | 不保留，过时即丢 | 按 key 保留**最新一份** |
| 投递 | 推模型：广播给当前在线订阅者 | 拉模型：消费方随时主动 `get_state` 查询，没有"订阅时补发"的推送机制 |
| 类比 | Kafka 的 stream / 日志 | Kafka 的 compacted topic / 数据库表 |
| tuzi 现有例子 | `cd`/`yank`/`renamed`/`task-done`（P1 已有） | tab 的 `(path, selection)`、当前排序策略、当前主题（P4 待做） |
| 对应 yazi 概念 | 普通 kind | `@` 前缀 kind + server 端缓存 + 握手重放 |

接口形状：

```rust
// Event：一次性广播，不保留（P1 已实现的 Registry::deliver 路径）
registry.publish(Body::Cd { .. });

// State：按 key 覆盖式保留，任何时候可查询当前值（P4 待实现）
registry.publish_state("tab-state:0", TabState { path, selection });
let current: Option<TabState> = registry.get_state("tab-state:0");
```

`publish_state` 内部就是 `HashMap<Key, LatestValue>` 的 upsert；
`get_state` 就是查表。有没有订阅者不影响这份保留值是否存在——这是它和
Event 的本质区别，也是「新开 tab 能不能立刻拿到上次状态」的关键：不用
等下一次事件恰好发生，直接查当前值。

**`publish_state`/`get_state` 不经过 `Event::Pubsub`/主循环，是对
`Registry` 的直接同步调用。** 这一点和 Event 路径不同，要分清楚为什么：
Event 路径（`publish` -> `Event::Pubsub` -> `tx.send` -> 下一轮
`dispatch_event` -> `deliver` -> `Vec<Command>` -> `app.execute()`）必须
绕回主循环，是因为它最终要执行 `Command`，需要和其它所有触碰 `App`
状态的操作（按键、后台事件）保持同一个串行顺序。而 State 只是写一份
`HashMap` 缓存，不产生 `Command`、不触碰 `App` 的其它字段，没有需要
排队的理由，直接在调用点同步执行即可：

```rust
// 发布者决定调哪个方法——不是先包成同一种消息，dispatch 时再按 kind 分叉
app.pubsub.publish(Body::Cd { .. });                          // Event：仍经 tx.send(Event::Pubsub(..))
app.pubsub.publish_state("tab-state:0", TabState { .. });     // State：直接同步写，不经过 channel
```

（这是本设计和最初讨论时的一处分歧：曾经设想过「一条 `Event::Pubsub`
消息里带 kind，`dispatch_event` 内部再判断是 Event 还是 State」，类比
yazi 的 `@` 前缀。放弃这个方案，因为 State 写入没有 Command 那样的执行
顺序约束，硬塞进 channel 只是多绕一圈，没有必要。）

`Command` 在两条路径里的角色不变，只是产生方式不同：

- Event 路径：订阅者收到 Event -> 产生若干小粒度 Command（跟用户按键
  等价），沿用 P1 已有的 `Registry::deliver -> Vec<Command> ->
  App::execute()`。
- State 路径：消费方（tab 初始化逻辑）主动 `get_state` 拿到一份完整
  快照 -> 产生**一个**"应用快照"的 Command，一次性灌回去，例如：

  ```rust
  Command::RestoreTab { path: PathBuf, selection: Vec<PathBuf> }
  ```

  `App::execute` 直接对目标 tab 做批量赋值（cd 到 path，再重建
  selection），而不是拆成一串 `Cd`/`ToggleSelect` 重放。

跨进程持久化（真正扛得住重启）就是把 `get_state`/`publish_state` 的
存储后端从纯内存 `HashMap` 换成「内存 + 落盘」，退出时把保留值写入磁盘，
下次启动前先加载回来——语义完全一致，只是保留值的来源变了。这正是
`state.rs`（P4）要做的事，现在只是把它的设计明确下来，不再是「等需求
场景再定」的空白。

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
     到别的实例或 `tuzi emit` 发来的消息（见「尚待确认事项」）；`Body`
     没有 `Bye` 变体，靠连接断开（EOF）让 server 清理 peer 表，没做
     yazi 那样的优雅下线握手；没有 `@` 静态消息持久化（P4 的事）。
4. **P4（可选，按需）**：按上面「Event/State 双轨模型」给 `Registry` 加
   `publish_state`/`get_state`（内存 `HashMap<Key, LatestValue>`），以及
   `state.rs` 的磁盘持久化（退出时落盘、启动时加载）。第一个消费场景是
   tab 状态恢复（路径 + 选中文件），落地为一个新的 `Command::RestoreTab`
   变体。未来若要嵌入脚本引擎（Lua/Rhai），只需在 `registry` 里加一种
   新的 handler 变体，接线不用动。

每阶段应可独立验证、独立提交，不必一次性大改完才能用。

## 尚待确认事项

- **交互式 `App` 还没加入 DDS socket**：`Registry`（进程内订阅）和
  `dds::Client`（P3 的 socket 客户端）目前互不相通。要让正在运行的
  TUI 真正参与跨实例总线，需要：(a) `App::serve` 启动时 `Client::connect`
  一次，(b) 后台读到的 `Payload` 转成 `Event::Pubsub(body)` 灌回
  `tx`（复用现有本地投递路径，不用新写分发逻辑），(c) 决定 `App` 的
  `abilities` 怎么来——目前 `Registry` 没有区分"只本地"和"也接受远程"
  的订阅（yazi 的 `sub`/`sub_remote` 区分），如果照搬现状，`abilities`
  只能是"当前 Registry 里已注册的所有 kind"或者干脆留空（等于什么都不
  接收远程）。没有做这一步是因为目前没有真实的订阅者会用到它——跟 P1
  的 `sub`/`unsub` 一样，等真的有一个要跨实例响应的场景（比如「emit
  'refresh' 时让所有开着的 tuzi 都刷新当前目录」）时再接，不要为了接
  而接。
- `src/actor/` 空存根是否要在本计划里复用或先删除，需与用户确认。
- State 落盘（P4 `state.rs`）的具体文件格式、路径、key 命名规则
  （如 `"tab-state:{id}"` 还是更结构化的 key 类型）尚未设计，落地 P4
  时再定；`publish_state`/`get_state` 的内存版接口形状已经在
  「Event/State 双轨模型」一节定下来了。
- `Command::RestoreTab`（或等价变体）的具体字段和「哪些状态值得恢复」
  （path/selection 之外要不要包含 sort_policy、column_mode、展开的子树）
  待 P4 实现时按需扩展，不必一次性照搬全部 Tab 字段。

## 续接建议

新 session 开始时先运行：

```text
cargo check
cargo test
git diff --check
```

确认基线后，从「P1 内部骨架」开始实施；若 P1 已完成，检查本文件的
「实施阶段」章节确认当前进度并继续下一阶段。
