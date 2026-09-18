# Tuzi 当前进程配置覆盖层

> 状态：配置覆盖层与 parent open 已实现。

## 目标

外部插件启动 Tuzi 时，可以覆盖任意受支持配置，覆盖只在该 Tuzi 进程内
有效，不修改用户的 `tuzi.toml`，也不为每个配置项增加专用 CLI 参数。

## 接口

```sh
tuzi --runtime-config '{"config":{"dds":{"open":"parent","broadcast":["hover"]}}}' /project
```

`--runtime-config JSON` 与 `--runtime-config-file FILE` 可重复。外部插件应使用 argv 数组启动，不拼接 shell
字符串。配置优先级从低到高：

```text
内置 preset < 用户配置文件 < 启动时 runtime config（按出现顺序）
```

所有覆盖复用正式配置的类型解析与最终校验；未知 key、类型错误或非法组合
直接导致启动失败。运行时覆盖统一使用 JSON，顶层分为 `config` 和 `keymap`。

DDS 的 parent peer ID 与 launch token 是握手身份，不属于配置，继续通过
`TUZI_DDS_PARENT` / `TUZI_DDS_TOKEN`（或对应调试参数）传递。

## Parent open

`dds.open` 计划支持 `auto`、`local`、`parent`：

- `auto`：存在可用 parent 时定点发送，否则使用本地 opener。
- `local`：始终使用本地 opener。
- `parent`：必须发送给 parent，绝不静默回退到本地 opener。

如果最终配置为 `parent`，但启动时完全没有 DdsLaunch（parent/token），配置
校验失败并终止启动。如果启动时声明了 parent，但它随后下线，Tuzi 保持
运行；执行 open 时显示 `controller unavailable`，不发送到不存在的 peer，
也不在本地意外打开。parent 重新出现在 Sync 后恢复可用。
