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
  mod.rs        // 对外入口：Pubsub::publish / subscribe
  payload.rs    // Payload envelope + 序列化（换行分隔 JSON，比 yazi 的
                // csv+json 混合格式更简单，仍可 nc/cat 调试）
  body.rs       // Body 枚举：内建 kind + Custom(kind: String, data: serde_json::Value)
  registry.rs   // LOCAL / REMOTE 订阅表 + ability 广播
  transport.rs  // Unix Socket client/server（首个实例自举为 server）
  state.rs      // 可选：@ 前缀 static topic 的磁盘持久化，第一期不做
```

### Body 内建 kind

只把「对外语义有意义」的事件提升为 DDS topic，例如 `cd`/`yank`/
`rename`/任务完成；`Loaded`/`PreviewLoaded`/`CompletionLoaded` 这类纯
内部 IO 分片进度继续走现有 `Event` 通道，不进入 `Body`。

```rust
pub enum Body {
    Hi { abilities: Vec<String>, version: String },
    Hey { peers: Vec<PeerInfo> },
    Bye,
    Cd { path: PathBuf },
    Hover { path: Option<PathBuf> },
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

### 本地订阅 API

```rust
pub type Handler = Box<dyn Fn(&Body) -> Vec<Command> + Send + Sync>;

impl Pubsub {
    pub fn sub(subscriber: &str, kind: &str, f: Handler) -> bool;
    pub fn sub_remote(subscriber: &str, kind: &str, f: Handler) -> bool;
    pub fn unsub(subscriber: &str, kind: &str);
    pub fn publish(body: Body);                    // receiver = 0
    pub fn publish_to(receiver: PeerId, body: Body);
}
```

同一 subscriber 对同一 kind 只能注册一次（对齐 yazi 防重复订阅行为），
`sub_remote` 触发一次能力重新广播（重发 `Hi`）。

### 跨实例传输

- Socket 路径：`$XDG_RUNTIME_DIR/tuzi/dds.sock`，缺省回退
  `Xdg::state_dir()`。
- 协议：换行分隔的单行 JSON object（比 yazi 的逗号分隔混合格式更不易
  踩转义坑，仍保持纯文本可 `nc`/`cat` 调试）。
- 握手：`Hi`（携带本实例 `sub_remote` 过的 kind 集合）-> server 记录
  -> 广播 `Hey`（全量 peer 表）。
- 转发过滤：`receiver==0` 只广播给声明了对应 ability 的 peer；否则定点
  转发。没有任何 peer 声明兴趣时，`publish` 直接跳过 socket 写入（对齐
  yazi 的 `any_remote_own` 优化，避免每次 `cd` 都写 socket）。
- 第一期不做 `@` 静态消息持久化；等真的出现「新开 tab/实例需要立刻拿到
  当前状态」的需求再加。

### CLI 对外接口

- 新增 `tuzi emit <kind> [json]` / `tuzi sub [--local-events|--remote-events]`
  子命令，对齐 `ya emit`/`ya sub`，复用 `dds::transport` 客户端逻辑，
  不需要跑完整 TUI 即可收发消息。
- `Command` 新增 `Command::Emit(kind, Option<json>)` 变体，使 keymap 的
  `run = "emit my-kind {...}"` 可以直接发布事件，在脚本引擎出现之前先
  提供一定可编程性。

## 实施阶段

1. **P1 内部骨架**：`src/dds/{payload,body,registry}.rs` + `Event::Pubsub`
   接线，纯进程内、无 socket。先把 `cd`/`yank`/`rename`/任务完成几个最
   有价值的内部事件迁到这条路径，验证「handler 返回 Command」这套接线
   跑得通。
2. **P2 CLI 可编程**：新增 `Command::Emit`，让 keymap/`:` 命令能发布任意
   custom kind，不依赖 socket。
3. **P3 跨实例 socket**：新增 `transport.rs`（client 自举为 server）+
   `tuzi emit`/`tuzi sub` 子命令，打通多实例/外部脚本集成。
4. **P4（可选，按需）**：`@` 静态消息持久化 + 状态重放；未来若要嵌入
   脚本引擎（Lua/Rhai），只需在 `registry` 里加一种新的 handler 变体，
   接线不用动。

每阶段应可独立验证、独立提交，不必一次性大改完才能用。

## 尚待确认事项

- `src/actor/` 空存根是否要在本计划里复用或先删除，需与用户确认。
- 静态 topic（P4）落盘路径与格式尚未设计，等出现具体需求场景再定。
- `Command::Emit` 的 JSON 参数解析方式（内联 JSON vs. 简化 key=value）
  待 P2 阶段结合 `Command::from_str` 现有语法一起设计。

## 续接建议

新 session 开始时先运行：

```text
cargo check
cargo test
git diff --check
```

确认基线后，从「P1 内部骨架」开始实施；若 P1 已完成，检查本文件的
「实施阶段」章节确认当前进度并继续下一阶段。
