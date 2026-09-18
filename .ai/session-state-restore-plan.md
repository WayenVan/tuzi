# Tuzi 会话状态复刻计划

> 状态：已实现。现有 `set-state` 保持不变；新增 `restore-state`。

## 目标

Controller 可以向一个受控 Tuzi 发送完整会话快照，使它不受当前状态影响，最终
完整复刻快照中的：

- tab 数量、顺序和活动 tab；
- 每个 tab 的根目录、光标和选择；
- 树中哪些目录展开、哪些目录折叠。

Tuzi 启动时也能通过同一种 `SessionState` 恢复会话。启动恢复与 DDS 恢复必须
共用验证和执行逻辑，不能形成两套行为。

## 状态模型

```json
{
  "version": 1,
  "active_tab": 1,
  "tabs": [
    {
      "cwd": "/project",
      "cursor": "/project/src/main.rs",
      "selection": [],
      "expanded": [
        "/project/src",
        "/project/src/app",
        "/project/tests"
      ]
    },
    {
      "cwd": "/project/tests",
      "cursor": "/project/tests/basic.rs",
      "selection": ["/project/tests/fixtures/input.txt"],
      "expanded": ["/project/tests/fixtures"]
    }
  ]
}
```

对应类型：

```text
SessionState {
    version: u32,
    active_tab: usize,
    tabs: Vec<TabState>,
}

TabState {
    cwd: PathBuf,
    cursor: Option<PathBuf>,
    selection: Vec<PathBuf>,
    expanded: Vec<PathBuf>,
}
```

路径统一使用绝对路径。cursor 保存路径而不是行号；排序和目录内容变化后，行号
不稳定。内部 Node ID 和 tab ID 不写入快照，它们只在当前进程中有效。

## 状态语义

### `set-state`

保留现有轻量语义，只修改当前活动 tab 的目录和选择，不增删 tab，也不恢复树
展开状态。

### `restore-state`

传入快照是唯一真相：传入几个 tab，最终就只保留几个；tab 顺序、活动 tab、
cwd、cursor、selection 和 expanded 均由快照决定。快照中没有出现在 `expanded`
里的目录默认折叠，因此不需要单独保存 collapsed 集合。

cursor 必须可见。恢复时自动把 cursor 的目录祖先并入 expanded，因此调用方
不必重复列出这些祖先。selection 不要求可见，不会为了 selection 自动展开目录。

## 验证与规范化

在修改 App 前完整验证并规范化输入：

- 只接受 `version = 1`，拒绝未知字段；
- 至少一个 tab，`active_tab` 不越界；
- cwd 必须是存在的目录；
- cursor 必须存在且位于对应 cwd 内；
- selection 和 expanded 必须位于对应 cwd 内；
- expanded 项必须是存在的目录；
- 去重 expanded，并加入 cursor 的目录祖先；
- 按相对 cwd 的深度从浅到深排列 expanded；
- 相同路径只加载一次。

第一版设置资源上限，避免恶意或错误快照触发无界目录加载：

- 最多 32 个 tab；
- 每个 tab 最多 256 个 expanded 路径；
- 单个恢复最多 4096 个 selection 路径；
- cwd 到目标路径的最大相对深度为 64。

文件系统可能在验证后变化。执行期间如果必要目录、cursor 或 expanded 路径消失，
本次恢复仍视为失败，不提交部分结果。

## 树恢复算法

Tuzi 的树采用惰性异步加载，不能一次性直接写入 expanded 和 cursor。每个 tab
必须按目录层级恢复：

```text
创建 cwd 根节点
→ 等待根 listing 完成
→ 展开第一层目标目录
→ 等待这些目录 listing 完成
→ 展开下一层目标目录
→ 重复直到 expanded 全部完成
→ 定位 cursor
→ 恢复 selection
```

expanded 已按深度排序；同一深度的不同分支可以并行加载。父目录尚未加载完成时，
不得尝试寻找或展开子目录。

每个 staged tab 保存恢复进度，例如待展开路径、正在等待的 listing ticket、目标
cursor 和 selection。现有带 tab ID/ticket 的异步事件继续用于拒绝过期结果。

## 原子提交

恢复期间旧 tabs 继续作为当前可见会话。新会话在后台 `staged tabs` 中构建：

```text
验证 SessionState
→ 创建 staged tabs
→ 异步恢复所有树
→ 验证最终 cursor/selection
→ 所有 tab 成功
→ 一次性替换 App.tabs 和 App.active
```

任意 staged tab 失败时，丢弃整个 staged session，保留旧 tabs、active tab 和
当前视图不变。不得先删除旧 tab，也不得把半恢复的新 tab 暴露给用户。

启动恢复没有旧会话可保留：失败时终止启动并返回明确错误。Controller 发起的
恢复失败时保留原会话并在 Tuzi 内显示警告。Controller 采用尽力投递，不等待
执行结果，因此仍只收到 `queued`，不会收到恢复成功回执。

## 启动入口

扩展统一 JSON 运行时配置的顶层结构：

```json
{
  "config": {},
  "keymap": {},
  "state": {
    "version": 1,
    "active_tab": 0,
    "tabs": [
      {
        "cwd": "/project",
        "cursor": "/project/README.md",
        "selection": [],
        "expanded": ["/project/src"]
      }
    ]
  }
}
```

通过 `--runtime-config` 或 `--runtime-config-file` 输入。多个运行时文档仍按命令行
顺序处理；`config` 和 `keymap` 沿用现有覆盖规则，state 采用最后一个出现的值。

## DDS 与 Controller

新增 `restore-state`：

```json
{
  "request_id": 8,
  "op": "restore-state",
  "peer_id": 902,
  "state": {
    "version": 1,
    "active_tab": 0,
    "tabs": []
  }
}
```

- Tuzi 注册 `restore-state` ability；
- controller 使用一个 `peer_id` 定点发送；
- controller 前置检查 peer 受控、在线并声明 `restore-state`；
- 子 Tuzi 最终检查 sender 是保存的 parent、操作受支持且内容通过完整验证；
- controller 成功提交只返回 `{ "ok": true, "status": "queued" }`；
- 现有 `set-state` 不改名、不改变协议和行为。

Token 只授权初始握手。DDS server 将 sender 绑定到完成 `Hi` 的连接，子 Tuzi 再
验证 sender 等于其 parent。Controller 的前置检查只用于尽早发现误操作，不能
替代接收方鉴权。

## 实现阶段

1. 定义 `SessionState`、`TabState`、版本与资源上限。
2. 实现纯函数式验证、路径规范化、expanded 去重和深度排序。
3. 实现单个 staged tab 的分层异步树恢复。
4. 实现多 tab staged session 与原子提交/整体回滚。
5. 将 `state` 接入 `--runtime-config` 和 `--runtime-config-file`。
6. 增加 Tuzi `restore-state` ability、controller op 与接收方鉴权。
7. 更新公开协议文档并完成回归测试。

## 测试范围

- 三个现有 tab 被完整替换为两个；
- tab 顺序和 active tab 正确；
- cwd、cursor、selection 正确；
- 多分支、多层 expanded 按层恢复；
- cursor 祖先自动加入 expanded；
- 未列入 expanded 的目录保持折叠；
- selection 不会意外展开目录；
- 路径消失、越界和资源超限时不改变旧会话；
- staged listing 的过期事件不能污染新旧会话；
- 启动恢复与 controller 恢复共用相同模型和执行器；
- 非 parent 无法执行 `restore-state`；
- 离线或未声明 ability 时 controller 立即拒绝；
- 现有 `set-state` 行为保持不变。
