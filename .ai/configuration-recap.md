# Tuzi 配置化改造续接记录

更新时间：2026-09-17

## 总体设计

配置采用三层保护：

```text
用户配置（只写需要覆盖的字段）
        ↓ overlay
内嵌默认 TOML preset（完整默认配置，单一事实来源）
        ↓ typed deserialize / validation
Rust 类型和必要的不变量校验
```

Rust 中不再维护第二份行为默认值。没有用户配置文件、使用
`--no-config`，或者用户只配置部分字段时，都能得到完整配置。

配置目录按以下顺序确定：

1. `--config-dir DIR`
2. `TUZI_CONFIG_HOME`
3. `$XDG_CONFIG_HOME/tuzi`
4. `~/.config/tuzi`

`--no-config` 会忽略所有用户配置。

## 已完成的配置化步骤

### 1. 配置加载基础设施和行为配置

- 完整默认值位于 `preset/tuzi-default.toml`，通过 `include_str!` 内嵌。
- `src/config/mod.rs` 负责路径发现、用户 overlay、类型反序列化和校验。
- 已配置化：
  - 排序字段和正反序
  - 隐藏文件显示
  - 辅助列模式
  - 目录历史容量
  - 预览默认开关和宽度比例
  - 后台任务 worker 数量
- `Config` 通过 `Arc` 传入 App 和每个 Tab。

### 2. Keymap 配置化

- 默认 keymap 位于 `preset/keymap-default.toml`。
- 用户文件为 `keymap.toml`。
- 支持完整替换 `keymap`、优先覆盖 `prepend_keymap`、追加
  `append_keymap`。
- 支持单键、组合键、修饰键和一键多命令。
- 启动时检查未知命令、空绑定和组合键前缀冲突。
- keymap 的 `run` 格式保持兼容。

### 3. Icons 配置化

- icon 配置位于 `theme.toml` 的 `[icon]`。
- 支持目录、打开目录、特殊文件状态和 fallback 图标。
- 支持精确目录名、文件名、扩展名、glob 规则。
- 支持 prepend/append overlay 和 `enabled = false`。
- 用户规则优先于内置 devicons fallback。

### 4. Theme / UI 样式配置化

- 完整默认主题位于 `preset/theme-default.toml`。
- 所有生产 UI 的硬编码颜色已迁移为语义 style key。
- 支持 `fg`、`bg`、`bold`、`italic`、`underline`、`reverse`。
- 支持颜色名称和 `#RRGGBB`。
- 未知 style 名和未知属性会在启动时被拒绝。
- 动态语法高亮 RGB 不属于固定 UI 主题，因此仍由高亮器产生。

### 5. Opener 配置化

- opener 定义和匹配规则位于 `tuzi.toml` 的 `[opener]` 与 `[open]`。
- 规则支持 `mime`、`name`、`ext`、`glob`，从上到下首条匹配。
- `o` 执行首条匹配规则中第一个适合当前平台的 opener。
- `O` 展示所有选中文件共同可用的 opener。
- opener 支持 `for = unix/macos/linux/windows`。
- 命令由 `run` 与 `args` 分开描述，不经过 shell。
- 参数占位符：
  - `{files}`：展开为多个独立参数。
  - `{file}`：逐文件执行，要求 `per_file = true`。
  - `{dir}`：逐文件使用父目录，要求 `per_file = true`。
- `$EDITOR` 按 `VISUAL -> EDITOR -> vi/notepad` 解析。
- 进程模式：
  - `block = true`：暂停 Tuzi，让出 TTY，等待子进程。
  - `orphan = true`：后台脱离。
  - 两者都不设置：不让出 TTY，但等待进程结束。

## 配置化过程中插入完成的命令化基础

为了让 keymap 和未来命令行共用一个稳定接口，原来的 `Action` 已迁移为
`Command`：

```text
普通按键 -> Keymap/Route -> Command -> App::execute()
命令输入 -> Command parser -> Command -> App::execute()
后台结果 -> Event -> Dispatcher::dispatch_event()
```

- `src/action.rs` 已删除，核心模型位于 `src/command.rs`。
- `Route::Actions` 已改为 `Route::Commands`。
- `Dispatcher` 只处理后台 Event；用户业务统一进入 `App::execute()`。
- `:` 使用现有 edtui `InputSession` 打开 Vim 风格命令框。
- keymap 和 `:` 共用 `Command::from_str`。
- 已支持单/双引号、反斜杠转义，以及自由参数：
  - `cd ~/workspace`
  - `cd "/path with spaces"`
  - `rename "new name.txt"`
  - `create "notes/draft.md"`
- 参数由 Tuzi 解析，不进行 shell 求值。
- 命令语法已统一为 `command [target] [--behavior]`：
  - `cursor 1/top/bottom` 取代旧的 `arrow`。
  - `cd PATH` 始终把参数视为真实路径。
  - 内置位置使用 `cd @trash/@config/@selected`，避免占用普通目录名。
  - `cd`、`rename`、`create` 无参数时直接打开交互输入框。
  - `find`、`find --previous`、`filter` 直接打开相应输入框。
  - 旧的公开 `input ...` 命令已移除。
  - 空路径/名称和未知 `@target` 会被拒绝。

## 尚未完成的配置化步骤

恢复配置化主线时，建议按以下顺序继续。

### 6. 文件操作策略

第一阶段已完成：

- `[confirm].trash/delete` 分别控制回收站与永久删除确认。
- 关闭确认不会改变操作类型；普通 `remove` 永不变成永久删除。
- `[fs].paste_conflict/create_conflict/rename_conflict` 已接入业务路径。
- 当前支持 `rename` 与 `error`：自动选择 `(copy)` 名称，或拒绝冲突。
- `error` 粘贴采用整批预检，避免 cut 操作只移动一部分却丢失剪贴板状态。
- 当前不支持隐式覆盖，现有目标不会被这些策略覆盖。

后续如确有需求再增加 `ask`、`skip`、`overwrite` 和批量“全部应用”弹窗；
其中 `overwrite` 必须单独审视安全边界。

### 7. UI 行为

已完成：

- `[ui].mouse`：鼠标事件总开关。
- `[ui].popup_width`：prompt、opener、确认弹窗统一最大宽度，范围 20–200。
- `[ui].completion_max_items`：补全候选最大可见条数，范围 1–50。
- `[ui].which_key`：组合键提示开关，不影响组合键本身。
- `[notify].info_timeout/warn_timeout/error_timeout`：分级通知秒数，范围 1–3600。

### 8. 文件监听

已完成：

- `[watcher].debounce_ms`：变化停止后的合并窗口，范围 10–5000ms。
- `[watcher].max_wait_ms`：持续变化时强制刷新的上限，范围 10–10000ms。
- `[watcher].poll_interval_ms`：notify polling backend/fallback 间隔，范围 50–60000ms。
- 强制 `debounce_ms <= max_wait_ms`。
- 每个 Tab 创建 watcher 时从共享 Config 注入三个时间参数。
- 原生监听仍优先，poll interval 只配置 notify 的 polling 实现。

### 9. 预览限制

已完成：

- `[preview].max_scan_bytes`：生成一个视口最多扫描的字节数，范围 64 KiB–1 GiB；不会按文件总大小直接拒绝大文件。
- `[preview].max_line_bytes`：单行高亮上限，范围 256 B–1 MiB，且不能超过扫描上限；超出后该次预览退化为纯文本。
- `[preview].cache_bytes`：预览 LRU 缓存容量，范围 0–1 GiB；设为 0 可关闭缓存。
- `[preview].overscan_lines`：视口之外额外读取的行数，范围 0–1000。
- `[preview].syntax_highlight`：语法高亮总开关。
- 二进制检测继续固定为安全默认：前 1024 字节检测到 NUL 时拒绝文本预览，暂不引入额外策略枚举。
- 所有限制随共享 Config 注入每个 Tab 的 Preview 与后台 PreviewScheduler；无用户配置时取 embedded preset。

### 10. 任务与文件复制参数

已完成配置化主线：

- `[tasks].workers`：任务并发数，范围 1–64。
- `[tasks].copy_buffer_size`：每个运行中复制任务的缓冲区，范围 4 KiB–16 MiB。
- `[tasks].progress_interval_ms`：复制与删除任务向 UI 上报进度的最小间隔，范围 10–1000ms。
- 三项参数统一注入 `TaskManager`，复制后台任务不再使用固定的 512 KiB 缓冲区或 75ms 上报间隔。
- `.tuzi-part-*` 临时文件、原子 rename 和失败清理继续作为固定的一致性保障，不开放配置。
- 当前没有重试机制；暂不为了配置而增加重试、退避或更细的并发调度。

## 命令化后续（与配置化主线分开）

命令行当前已可用，后续尚未实施：

1. 命令历史（当前明确暂不实施）。
2. `[command]` 命名命令/命令序列。
3. 命名命令循环引用检测。

命令自动补全已经完成：复用 `g<Space>` 目录输入框的候选弹窗、上下选择、
`Ctrl-p`/`Ctrl-n` 和 Tab 确认；按整行前缀补全内置命令模板。

暂不设计变量、条件、管道、宏语言或隐式 shell。若以后需要 shell，应该提供
显式 `shell` 命令并明确 `block/orphan` 行为。

## 当前验证基线

- `cargo check`：通过，无警告。
- `cargo test`：以当前工作区最新测试结果为准。
- `git diff --check`：通过。
- 用户的未跟踪文件 `nvim.log` 不属于本次改造，不要修改或删除。

## 关键文件索引

- `preset/tuzi-default.toml`：行为和 opener 完整默认值。
- `preset/keymap-default.toml`：完整默认 keymap。
- `preset/theme-default.toml`：完整默认 UI 样式和 icon 配置。
- `src/config/mod.rs`：配置发现、overlay、校验。
- `src/keymap/config.rs`：keymap 文件加载。
- `src/command.rs`：公开命令模型与文本解析。
- `src/app/dispatcher.rs`：`App::execute(Command)` 和后台 Event 分发。
- `src/app/input.rs`：edtui InputSession 与不同 InputPurpose。
- `src/opener.rs`、`src/runner/mod.rs`、`src/process.rs`：opener 匹配、执行计划和进程模式。
- `src/theme.rs`、`src/icon.rs`：主题与图标 overlay。

## 续接建议

新 session 开始时先运行：

```text
cargo check
cargo test
git diff --check
```

确认基线后，若用户要继续配置化，从“步骤 6：文件操作策略”开始；若用户要继续
命令化，从“命令历史和补全”开始。两条工作线应保持分开，避免把命令语言选项
塞入行为配置模型。
