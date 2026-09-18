# Tuzi DDS 启动握手设计与实施计划

> 状态：P1、P2、P4 已完成；P3 文档已完成，人工并发验证待做。

## 目标

让 Neovim 插件、编辑器扩展或其他控制器能够启动一个 Tuzi，并可靠地知道
“这个 Tuzi”对应的 DDS peer ID，而不是读取 `tu dds peers` 后根据进程顺序、
目录或 abilities 猜测。

典型场景：一个 Neovim 同时启动多个 Tuzi，机器上还存在其他独立 Tuzi。
控制器必须能把每次 spawn 与唯一的 DDS peer 对应起来，随后使用
`publish_to(peer_id, ...)` 做点对点通信。

## 已有条件与约束

- DDS peer ID 由 `Client::connect` 创建，Tuzi App 当前无法在连接前预知它，
  但连接成功后可以通过 `Client::id()` 读取。
- `Client` 的 supervisor 在 socket 断开、server 换主后仍沿用同一个 ID；
  因此同一 Tuzi 进程内的传输重连**不会改变 peer ID**。只有进程退出并重新
  启动才会产生新 ID。
- `receiver != 0` 的消息由 server 直接投递给指定 peer，不经过 ability
  过滤，适合握手回复。
- DDS socket 仅允许当前系统用户访问。启动 token 用于关联请求与响应，
  不是跨用户认证或安全边界。
- 普通 Tuzi 启动仍允许 DDS 不可用时降级为本地模式；被控制器启动时，DDS
  握手是启动契约，不能静默降级后让控制器永久等待。
- server 拒绝重复 peer ID 的新连接，不允许后连接者覆盖已有路由。这不是
  token 认证的一部分，但可以阻止碰撞或外部指定 ID 导致的路由劫持。

## 协议

新增内建消息：

```rust
Body::Ready {
    token: String,
}
```

它的 kind 为 `"ready"`，并加入 `BUILTIN_KINDS`，防止 Custom 消息冒充。

不在 Body 内重复携带 Tuzi peer ID。完整 DDS envelope 已经包含权威来源：

```json
{
  "receiver": 701,
  "sender": 902,
  "body": {
    "Ready": {
      "token": "random-launch-token"
    }
  }
}
```

- `receiver`：启动者/控制器的 peer ID。
- `sender`：新 Tuzi 的 peer ID，控制器应保存这个值。
- `token`：把 Ready 与某一次 spawn 关联起来；同一控制器并发启动多个 Tuzi
  时不能只凭消息到达顺序匹配。

Ready 必须使用 `publish_to(parent, ...)` 定点发送，不加入 `[dds].broadcast`
白名单，也不经过 App 的隐式广播路径。

### 为什么不使用固定 peer ID

peer ID 是传输层连接身份，不是用户可配置的永久实例名。允许调用者指定它
会引入冲突、冒充和陈旧 ID 的处理问题。控制器只需在每次启动握手后保存
`Payload.sender`。

### 为什么 token 仍然必要

只有 parent peer ID 不能区分该 parent 同时 spawn 的多个 Tuzi。随机 token
提供请求—响应关联；推荐至少 128 bit 随机值，并且每次 spawn 重新生成。

## 启动接口

第一版提供显式参数：

```text
tuzi --dds-parent <PEER_ID> --dds-token <TOKEN> [PATH]
```

两项必须同时出现：

- 只给其中一项时参数解析失败。
- `PEER_ID` 必须是非零 `u64`；`0` 表示广播，不能作为 parent。
- token 必须非空，并设置合理上限（建议 256 bytes），防止无界协议数据。
- 参数可以和 `--config-dir`、`--no-config` 组合。

自动化调用推荐使用环境变量，避免 token 出现在进程参数列表：

```text
TUZI_DDS_PARENT=<PEER_ID>
TUZI_DDS_TOKEN=<TOKEN>
```

优先级：命令行成对参数高于环境变量；禁止把一半来自命令行、一半来自环境
变量。环境变量也必须成对并经过相同校验。命令行形式主要用于人工调试，
插件应优先使用环境变量。

内部引入一个独立值对象，避免继续膨胀 `App::serve` 参数：

```rust
pub struct DdsLaunch {
    pub parent: PeerId,
    pub token: String,
}

pub struct ServeOptions {
    pub path: PathBuf,
    pub dds_launch: Option<DdsLaunch>,
}
```

如果现在不值得全面引入 `ServeOptions`，也可先给 `App::serve` 增加一个
`Option<DdsLaunch>`；但 DdsLaunch 必须是有校验的整体，不能在 App 内继续
传两个独立 Option。

## 时序

```text
Controller                         Tuzi                         DDS server
    |                               |                               |
    | connect(); obtain parent ID   |                               |
    |<---------- Hey ---------------|-------------------------------|
    |                               |                               |
    | spawn Tuzi(parent, token) ---->|                               |
    |                               | connect + Hi ---------------->|
    |                               | publish_to(parent, Ready) ---->|
    |<----- Payload(sender=Tuzi ID, Ready{token}) ------------------|
    | verify token; store sender     |                               |
    | publish_to(Tuzi ID, ...) ----->|                               |
```

控制器必须先完成自己的 DDS 连接并获得 parent ID，再 spawn Tuzi。因为 DDS
目前是 best-effort 且不缓存消息，如果 parent 尚未在线，Ready 会丢失。

## Tuzi 侧行为

1. main 解析并验证可选的 `DdsLaunch`。
2. App 创建 Registry，并按现有流程连接 DDS。
3. 若未传 DdsLaunch：保持现有行为；连接失败时显示 warning 并本地运行。
4. 若传了 DdsLaunch：
   - `[dds].enabled = false` 是配置冲突，启动失败并给出明确错误；
   - DDS 连接失败时启动失败，不进入 TUI；
   - 连接成功后立即调用
     `client.publish_to(parent, Body::Ready { token })`；
   - Ready 入队后再开始终端 UI。无需 `flush`，App 会长期持有 Client；
     supervisor 总是先写 Hi，再写 outbox，因此 server 会先注册该 peer。
5. socket 断线重连时不重发 Ready：当前 Client ID 在 supervisor 生命周期内
   不变，控制器保存的地址仍有效。未来若改为 server 分配 ID，则必须同时
   增加 connection-generation 通知和 Ready 重发。

## 控制器侧约定

控制器实现应遵循以下顺序：

1. 以一个长期 DDS Client 连接，读取 `client.id()`。
2. 为本次 spawn 生成不可预测 token。
3. 通过环境变量传 parent ID 与 token 并启动 Tuzi。
4. 持续读取 inbox，只接受同时满足下列条件的响应：
   - `payload.receiver == controller.id()`；
   - body 为 `Ready`；
   - token 与该次 spawn 完全相同。
5. 将 `payload.sender` 与插件自己的窗口/buffer/session 对象绑定。
6. 设置超时（建议 3～5 秒）；子进程提前退出时立即报告 stderr/退出状态，
   不要只等待超时。

同一个 parent 可以管理多个 Tuzi，每个 launch token 对应一个 pending spawn。
peer 从 Hey 表消失时，控制器应清除对应绑定。

## `tu dds` 的辅助能力

本功能不要求 `tu dds` 代替真正的控制器，但应便于人工验证：

- `tu dds peers` 已能显示 parent peer ID。
- `tu dds sub` 不适合接收定点 Ready 后立即退出，因为订阅命令自身才是
  parent，必须先知道并保持它的 ID。
- 后续可增加专用测试命令：

```text
tu dds spawn [--timeout 5s] -- <tuzi arguments...>
```

它连接 DDS、生成 token、注入环境变量、spawn Tuzi，收到 Ready 后打印
Tuzi peer ID。该命令属于便利增强，不阻塞核心握手实现。

## 错误与安全边界

- parent 在 Ready 发出前退出：server 会静默丢弃定点消息；Tuzi 本身仍可
  运行。第一版不把 Ready 当成“控制器必须确认”的租约。
- 恶意同用户进程理论上可连接同一 socket 并伪造协议。现有 0600 socket
  权限只隔离其他系统用户；本计划不宣称同用户进程间身份认证。
- token 不写日志、不进入 Notice；错误信息只能说明 token 无效，不能回显
  完整 token。
- 如果未来需要“控制器退出则 Tuzi 自动退出”，应另行设计 parent lease/
  heartbeat，不能把一次性 Ready 握手误当生命周期绑定。

## 实施步骤

### P1：协议与参数（已完成）

1. 在 `dds::Body` 增加 `Ready { token }`，更新 kind 和保留名测试。
2. 新增 `DdsLaunch` 的构造/校验逻辑。
3. 扩展 `tuzi` 参数与环境变量解析；补齐 help。
4. 单元测试覆盖成对约束、非零 parent、空/超长 token、CLI 优先级。

### P2：App 启动握手（已完成）

1. 将 `Option<DdsLaunch>` 传入 App 启动路径。
2. 在 DDS connect 成功后定点发布 Ready。
3. 控制器模式下将 disabled/connect failure 改为硬错误；普通启动保持降级。
4. 端到端测试连接 controller、启动 App 初始化路径并验证：
   - Ready 只到 parent；
   - `Payload.sender == app.dds_client.id()`；
   - token 原样匹配；
   - 其他 wildcard observer 不收到定点 Ready。

### P3：文档与人工验证（部分完成）

1. README 增加编辑器集成时序和环境变量说明。
2. 更新 `.ai/dds-plan.md` 的内建 kind 与 CLI/集成状态。
3. 用两个 controller 并发启动多个 Tuzi，确认 token 不串线。
4. 杀掉 server owner 触发换主，确认控制器保存的 Tuzi peer ID仍可定点投递。

### P4：`tu dds spawn`（已完成）

已实现测试/脚本便利命令：它使用系统安全随机源生成 128-bit token，注入
环境变量并并发等待 Ready 或子进程退出。支持 `--timeout` 和 `--json`；超时
时终止未完成关联的子进程。握手成功后 wrapper 必须继续等待 Tuzi 退出，
不能立刻返回：否则 shell 会收回前台终端，仍运行的 TUI 将失去正确的 job
control。peer ID 在 TUI 恢复终端后打印；运行中可从另一个终端用 `peers`
查看。当前只允许默认 DDS socket，因为 Tuzi 尚未提供自定义 socket 的启动
参数。

## 完成标准

- 控制器不枚举或猜测 peers，即可确定自己启动的 Tuzi peer ID。
- 并发启动多个 Tuzi 时，响应能按 token 精确关联。
- 握手只定点发送给 parent，不扩大 Tuzi 的默认广播面。
- 普通启动的 DDS 降级语义不变；受控启动不会静默丢失握手契约。
- server failover 后，同一进程的已知 peer ID 继续可用。
