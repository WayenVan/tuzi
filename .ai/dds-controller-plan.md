# `tu dds controller` 最小设计

> 状态：第一版及受控 peer 下线检测已实现。

## 边界

Controller 是一个与 Neovim 同生命周期的常驻 DDS client，只负责它与受控
Tuzi 之间的链路。第一版不管理 Neovim window/session，不负责 spawn，不管理
子进程生命周期，也不实现动态 ability。

## 建立受控关系

1. Controller 连接 DDS，在 stdout 输出 `controller-ready` 与自身 peer ID。
2. 调用方通过 stdin 注册一次性 launch token。
3. 调用方用 controller peer ID 与 token 启动 Tuzi。
4. Controller 只接受已注册 token 对应的定点 `Attach`，记录
   `token -> Tuzi peer ID`。
5. 未注册、重复或已经消费的 token 对应的 Attach 被忽略。

## JSON Lines

stdin 请求：

```json
{"request_id":1,"op":"register","token":"launch-token"}
{"request_id":2,"op":"set-state","peer_id":902,"state":{"path":"/project"}}
```

stdout 事件/响应：

```json
{"event":"controller-ready","protocol_version":1,"peer_id":701}
{"request_id":1,"ok":true}
{"event":"tuzi-ready","token":"launch-token","peer_id":902}
{"event":"message","peer_id":902,"kind":"hover","body":{}}
{"request_id":2,"ok":true}
```

stdout 只写 JSON Lines 并逐行 flush。语法错误写结构化 error 响应，进程级
错误写 stderr。stdin EOF 时 controller 正常退出。

首条 `controller-ready` 必须包含整数 `protocol_version`。当前版本为 `1`；
调用方确认支持该版本后才可发送请求。

## 消息过滤

Controller 在 Join 中声明配置的 abilities，因此 server 可能把非受控 Tuzi
的同 kind 广播也转发给它。Controller 必须再检查 `Payload.sender` 是否存在
于 controlled 映射，只有受控 peer 的消息才能写到 stdout。Attach 是定点
消息，不依赖 ability。

默认 abilities：`hover,cd,yank,renamed,task-done`。第一版通过启动参数
`--abilities` 一次性配置。

## 定点发送

token 只用于启动握手。Attach 后调用方使用 `peer_id` 定点寻址；`set-state`
通过 `publish_to` 发送受控状态，`publish` 仅作为自定义 kind 的逃生口。
Controller 在发送前验证该 peer 属于 controlled 集合。

## 下线

Controller 从 `Sync` 维护在线 peer 集合。受控 peer 首次缺席时只标记 missing；
若 500ms 内重新出现，视为 server 换主并取消标记。超过 grace period 仍缺席
时，删除 `token -> peer ID` 与 missing 记录，并输出一次：

```json
{"event":"tuzi-left","token":"launch-token","peer_id":902}
```

删除后 token 可以再次 register；正常的新启动仍推荐生成新 token。

## 后续而非当前范围

- 动态修改 abilities 并重新发送 Join。
- 请求 ACK、超时与可靠重试。
- Controller 代替 Neovim spawn 或终止 Tuzi。

## Neovim 插件参考：异步请求与回调

Controller 是一个长期 job，stdout 由 Neovim 事件循环异步读取。Lua 不应写
阻塞的 `while` 等待响应，而应维护 `request_id -> pending request` 映射：

```lua
local pending = {
  -- [request_id] = {
  --   callback = function(response) end,
  --   timer = uv_timer,
  -- }
}
```

发送请求的正确顺序：

1. 分配当前 pending 集合中尚未使用的 `request_id`。
2. **先**把 callback 与 timeout timer 写入 pending。
3. 再向 Controller stdin 写一行 JSON。
4. 立即返回，不阻塞 Neovim 主循环。

必须先注册 pending 再写 stdin，防止 Controller 的快速响应先于本地记录。

```lua
local function request(op, params, callback)
  local request_id = allocate_request_id()
  local timer = start_timeout(request_id)

  pending[request_id] = {
    callback = callback,
    timer = timer,
  }

  params.request_id = request_id
  params.op = op
  send_json_line(params)
  return request_id
end
```

stdout dispatcher 收到完整 JSON Line 后分两路处理：

- 有 `request_id`：它是某个请求的 response；查 pending、先删除记录并停止
  timer，再调用 callback。
- 有 `event`：它是 Controller 主动事件，例如 `controller-ready`、
  `tuzi-ready`、`message`、`tuzi-left`，交给独立 event dispatcher。

```lua
local function handle_message(message)
  if message.request_id ~= nil then
    local item = pending[message.request_id]
    if item == nil then
      return -- 未知、已取消或已经超时的响应
    end

    pending[message.request_id] = nil
    item.timer:stop()
    item.timer:close()
    safe_call(item.callback, message)
    return
  end

  if message.event ~= nil then
    handle_event(message)
  end
end
```

回调前删除 pending，避免 callback 重入时重复消费同一请求。callback 应通过
`pcall`/`xpcall` 或 `vim.schedule_wrap` 隔离，单个插件回调报错不能破坏
stdout reader。

每个 pending request 必须有超时：超时回调先删除 pending，再报告 timeout。
Controller job 退出或 stdin 写失败时，应一次性取出并以 connection-closed
错误结束所有 pending callback。

`request_id` 只需保证“当前所有未完成请求中唯一”。推荐在 Lua 精确整数范围
`1..2^53-1` 内递增，回绕时跳过仍存在于 pending 中的编号；响应完成后编号
可以复用。pending 是用于乱序匹配的 map，不是 FIFO 队列。

由于 `stdout_buffered = false` 可能把一条 JSON Line 拆成多个 chunk，也可能
一次提供多行，Lua reader 还必须保存字符串 buffer，只把遇到换行符的完整行
交给 `vim.json.decode`，末尾残片留到下一次回调继续拼接。
