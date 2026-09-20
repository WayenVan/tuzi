<p align="center">
  <img src="assets/tuzi-icon.png" width="180" alt="Tuzi icon">
</p>

<h1 align="center">Tuzi</h1>

<p align="center">
  A tree-style <a href="https://github.com/sxyazi/yazi">Yazi</a> for the terminal.
</p>

<p align="center">
  <a href="https://github.com/WayenVan/tuzi/releases/latest"><img src="https://img.shields.io/github/v/release/WayenVan/tuzi?style=flat-square&label=release" alt="Latest release"></a>
  <a href="https://github.com/WayenVan/tuzi/actions/workflows/release.yml"><img src="https://img.shields.io/github/actions/workflow/status/WayenVan/tuzi/release.yml?style=flat-square&label=build" alt="Release build"></a>
  <a href="https://github.com/WayenVan/tuzi/releases"><img src="https://img.shields.io/github/downloads/WayenVan/tuzi/total?style=flat-square&label=downloads" alt="Total downloads"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/WayenVan/tuzi?style=flat-square" alt="MIT license"></a>
</p>

<p align="center">
  <img src="assets/demo.gif" width="960" alt="Tuzi demo">
</p>

## Why Tuzi?

I love Yazi, but some of my work calls for a more convenient tree-shaped view, especially inside Neovim. After spending hours looking for something that fit, I finally decided to sell my soul to Codex and Claude and build exactly what I wanted.

## Features

- **Filesystem navigation and tooling** — Browse lazy directory trees, jump
  with fzf or zoxide, search and filter files, manage visual selections, and
  copy, move, rename, trash, or link files without leaving the TUI.
- **Built for editor integration** — Launch and control Tuzi from Neovim or
  other hosts through its authenticated DDS and JSON Lines controller.
- **Atomic session restoration** — Restore tabs, roots, cursors, selections,
  and expanded directories from startup configuration or a running controller.
- **Configurable workflows** — Customize keymaps, themes, icons, openers,
  file-operation policies, previews, and process-local runtime settings.

## Install

Download the macOS or Linux archive for your platform from [GitHub Releases](https://github.com/WayenVan/tuzi/releases), extract it, and place `tuzi` and its companion command `tu` somewhere on your `PATH`.

To build from source, install a recent Rust toolchain, clone the repository, and run:

```sh
cargo install --path .
```

For file icons, use a terminal font that includes Nerd Font symbols. `fzf` is optional and only required for the jump command.

## Usage

```sh
tuzi [--home DIR] [PATH]
```

`PATH` must be a directory and defaults to the current directory.
Press `gh` (or run `cd @home`) to return to the session home: `--home DIR`
takes priority, followed by `PATH`, then the startup current directory.
Relative home paths are resolved at startup and shared across tabs.
For example, `tuzi --home /project /tmp` opens `/tmp`; `gh` returns to `/project`.

```sh
tuzi --help
tuzi --version
```

The companion CLI can inspect and exchange messages on Tuzi's local event
bus. Every subcommand has examples in its long help:

```sh
tu dds pub greeting '{"text":"hello"}'
tu dds pub-to PEER_ID greeting '{"text":"hello"}'
tu dds sub hover yank
tu dds peers
tu dds spawn -- /project
tu dds pub --help
```

`sub` subscribes to every kind when no kinds are supplied. Add `--json` to
`sub` or `peers` for script-friendly output.

An editor or plugin that launches Tuzi can identify that exact DDS peer with
a point-to-point startup handshake. Connect the controller to DDS first, then
launch Tuzi with a fresh token and the controller's peer ID:

```sh
TUZI_DDS_PARENT=701 TUZI_DDS_TOKEN=random-launch-token tuzi /project
```

Tuzi replies directly with an `attach` payload. Its `sender` field is the new
Tuzi peer ID. The two variables must be provided together; managed startup
fails instead of silently disabling DDS. `--dds-parent` and `--dds-token` are
also available for manual debugging, but environment variables avoid exposing
the token in ordinary command arguments.

For manual testing, `tu dds spawn` performs that controller workflow itself:

```sh
tu dds spawn -- /project
tu dds spawn --json -- --config-dir /tmp/tuzi-test /project
```

It remains as the foreground wrapper while Tuzi runs, then prints the launched
Tuzi's peer ID after the TUI restores the terminal. Keeping the wrapper alive
is required for correct shell job control. If the handshake times out, it
terminates the unassociated child instead of leaving an unmanaged process
behind. Use `tu dds peers` from another terminal to inspect the live ID.

`tu dds controller` is the long-running JSON Lines bridge intended for editor
plugins. See the concise [controller protocol guide](docs/controller-protocol.md)
and [DDS message reference](docs/dds-protocol.md) for integration details. It
only accepts a Tuzi after its launch token has been
registered:

```text
← {"event":"controller-ready","protocol_version":2,"peer_id":701}
→ {"request_id":1,"op":"register","token":"launch-token"}
← {"request_id":1,"ok":true}
```

The editor then starts Tuzi with `TUZI_DDS_PARENT=701` and the same
`TUZI_DDS_TOKEN`. After the Attach handshake, the controller reports and tracks
that peer:

```text
← {"event":"tuzi-ready","token":"launch-token","peer_id":902}
← {"event":"message","peer_id":902,"kind":"hover","body":{...}}
→ {"request_id":2,"op":"update-tab","peer_id":902,"update":{"path":"/project","selection":["README.md"]}}
← {"request_id":2,"ok":true,"status":"queued"}
```

Use `restore-state` when the host needs to replace the complete tab session,
including cursor positions and expanded directories. The replacement is built
off-screen and becomes visible only after every tab has restored successfully.
Use `get-state` to retrieve the current complete session as a matching
snapshot for later restoration. Its response arrives after Tuzi has built and
validated the snapshot; it is not a `queued` acknowledgement.
Use `get-tabs` for the current tab order, runtime IDs, roots, and active ID;
`switch-tab` selects one of those IDs.

Only messages whose sender is a successfully controlled Tuzi are emitted.
The token authorizes the initial handshake; runtime commands address one
controlled `peer_id` at a time. Use `list`, `cancel-register`, `detach`, and
`ping` to inspect or manage controller state.
The default abilities are `cd,yank,renamed,task-done`; pass a complete
`--abilities` list including `hover` when cursor movement events are needed.
A controlled Tuzi sends matching state events directly to its online parent
by default. Events explicitly listed in `dds.broadcast` are public broadcasts
instead. Stdout contains JSON Lines only, while stdin EOF shuts the controller
down.

When a controlled peer disappears from `Sync`, the controller waits 500ms for
server failover/reconnection. If it remains absent, the mapping is removed and
the controller emits:

```json
{"event":"tuzi-left","token":"launch-token","peer_id":902}
```

## Configuration

Tuzi starts from the complete configuration embedded from
`preset/tuzi-default.toml`, then overlays `~/.config/tuzi/tuzi.toml` on it.
On systems using
`XDG_CONFIG_HOME`, it reads `$XDG_CONFIG_HOME/tuzi/tuzi.toml` instead.
Only values you want to change need to be specified:

```toml
[mgr]
sort_by = "name"          # name, modified, size, extension
sort_reverse = false
show_hidden = false
column_mode = "none"      # none, size, permissions, modified
history_size = 60

[preview]
show = false
ratio = 40                # 10–90
layout = "auto"           # auto, horizontal (side-by-side), vertical (stacked)
split_threshold = 100     # auto stacks when the body is narrower than this
max_scan_bytes = 5242880  # 64 KiB–1 GiB per viewport scan
max_line_bytes = 16384    # 256 B–1 MiB; longer lines disable highlighting
cache_bytes = 16777216    # 0 disables cache; maximum 1 GiB
overscan_lines = 20       # 0–1000 lines beyond the visible viewport
syntax_highlight = true

[tasks]
workers = 2               # 1–64
copy_buffer_size = 524288 # 4 KiB–16 MiB per running copy task
progress_interval_ms = 75 # 10–1000 ms between UI progress updates

[confirm]
trash = true              # Confirm before moving to trash
delete = true             # Confirm before permanent deletion

[fs]
paste_conflict = "rename" # rename or error
create_conflict = "error" # rename or error
rename_conflict = "error" # rename or error

[ui]
mouse = true
popup_width = 50           # 20–200 columns
completion_max_items = 8   # 1–50 rows
which_key = true
filename_peek = false      # Show truncated filename continuations near the cursor

[notify]
info_timeout = 3           # seconds, 1–3600
warn_timeout = 5
error_timeout = 8

[watcher]
debounce_ms = 80           # 10–5000
max_wait_ms = 500          # 10–10000; must be >= debounce_ms
poll_interval_ms = 1000    # 50–60000

[dds]
enabled = true
open = "auto"             # auto, local, parent
# Implicit events go to a subscribed controlling parent, if present.
# List kinds here only to broadcast them publicly to other DDS peers:
# "cd", "hover", "yank", "renamed", "task-done".
broadcast = []
```

Use `tuzi --config-dir DIR` to select another configuration directory, or
`tuzi --no-config` to run with the built-in defaults. `TUZI_CONFIG_HOME` can
also set the configuration directory globally.

`--runtime-config` and `--runtime-config-file` apply JSON configuration and
optional session state to one Tuzi process without editing its files. They may
be repeated and are applied
in command-line order. Configuration and keymap values retain their overlay
behavior; the last document containing `state` supplies the complete startup
session:

```sh
tuzi --runtime-config '{
  "config": { "dds": { "open": "parent", "broadcast": ["hover"] } },
  "keymap": { "mgr": { "prepend_keymap": [
    { "on": "o", "run": "open", "desc": "Open through configured route" }
  ] } }
}' /project

tuzi --runtime-config-file /tmp/tuzi-session.json /project
```

For example, `/tmp/tuzi-session.json` may contain:

```json
{
  "state": {
    "version": 1,
    "active_tab": 0,
    "tabs": [{
      "cwd": "/project",
      "cursor": "/project/README.md",
      "selection": [],
      "expanded": ["/project/src"]
    }]
  }
}
```

Session paths must be absolute. Startup waits for the lazy directory listings
to finish before entering the TUI; an invalid snapshot or a restore failure
terminates startup with an error.

`dds.open = "auto"` sends ordinary opens to an available controlling parent
and otherwise uses the local opener. `local` always uses the local opener;
`parent` requires a controlled launch and never silently falls back. Interactive
open remains local.

The embedded TOML files are the single source of truth for defaults. An
invalid embedded preset is treated as a Tuzi bug; Rust does not maintain a
second copy of the behavior defaults.

Disabling a confirmation never changes the operation itself: `remove` still
uses the system trash, while permanent deletion still requires the explicit
`remove --permanently` command. Conflict policy `rename` chooses a free
`(copy)` name; `error` refuses an existing target without overwriting it.
`popup_width` applies consistently to prompts, opener dialogs, and
 confirmation dialogs. `filename_peek` controls the initial state of the
cursor-following truncated-name continuation (`I` toggles it at runtime).
Disabling `which_key` only hides chord hints; the
keymap sequences themselves continue to work.
Watcher changes are grouped for `debounce_ms`; `max_wait_ms` forces a refresh
during nonstop filesystem churn. `poll_interval_ms` configures notify's
polling backend/fallback and does not replace native watching where available.

### Keymap

`keymap.toml` uses Yazi-style key descriptions. `prepend_keymap` overrides a
default binding with the same key, while `append_keymap` adds bindings that do
not already exist:

```toml
[mgr]
prepend_keymap = [
  { on = "<C-p>", run = "preview toggle", desc = "Toggle preview" },
  { on = ["g", "g"], run = ["cursor top", "preview toggle"], desc = "Top and preview" },
]

append_keymap = [
  { on = "<F2>", run = "hidden toggle", desc = "Toggle hidden files" },
]
```

Printable keys are written directly. Special keys and modifiers use forms
such as `<Esc>`, `<Enter>`, `<Space>`, `<C-p>`, `<A-j>`, `<S-Down>`, and
`<F2>`. Setting `keymap = [...]` replaces the complete manager keymap. The
full command vocabulary and default bindings are available in
[`preset/keymap-default.toml`](preset/keymap-default.toml).

### Command line

Press `:` to open the command prompt. It reuses the same edtui Vim editor as
the other input dialogs: `Esc` leaves Insert mode, Normal-mode Vim motions
edit the line, and `Enter` submits it. Command candidates update as you type;
use Up/Down or `Ctrl-p`/`Ctrl-n` to select one and Tab to complete it, matching
the interactive directory prompt. Commands use exactly the same language
as keymap `run` entries, for example:

```text
:open --interactive
:sort modified --reverse
:preview toggle
:tab create
:cd ~/workspace
:cd "/path with spaces"
:rename "new name.txt"
:create "notes/draft one.md"
:cd @trash
:cd @config
:cd @selected
:cursor top
```

Invalid commands stay in the prompt and report an error. Keymaps and the
command prompt both parse into `Command` and execute through the same
`App::execute()` entry point. Single quotes, double quotes, and backslash
escapes are supported. Arguments are parsed by Tuzi and are not evaluated by
a shell.

Command syntax follows `command [target] [--behavior]`. A `cd` argument is
always a real path unless it uses the explicit built-in target namespace:
`@trash`, `@config`, or `@selected`. Thus `cd trash` enters a directory named
`trash`, while `cd @trash` opens the system trash; use `cd ./@trash` for a
literal directory named `@trash`. Running `cd`, `rename`, or `create` without
an argument opens its interactive input dialog. Empty targets and unknown
`@targets` are rejected.

### Openers

Openers and matching rules live in `tuzi.toml`. Plain `o` runs the first
opener in the first matching rule; `O` lists every matching opener:

```toml
[opener]
edit = [
  { run = "$EDITOR", args = ["{files}"], desc = "$EDITOR", for = "unix", block = true },
]
browser = [
  { run = "firefox", args = ["{file}"], desc = "Firefox", for = "unix", orphan = true, per_file = true },
]

[open]
rules = [
  { ext = "md", use = ["edit", "browser"] },
  { mime = "text/*", use = ["edit", "browser"] },
  { mime = "*", use = ["browser"] },
]
```

Rules are checked from top to bottom and may match `mime`, `name`, `ext`, or
`glob`. Arguments are passed directly to the executable without a shell:
`{files}` expands to separate arguments, while `{file}` and `{dir}` require
`per_file = true`. `$EDITOR` resolves `VISUAL`, then `EDITOR`, then the
platform fallback. `block = true` suspends Tuzi and gives the child the TTY;
`orphan = true` detaches it. With neither flag, Tuzi waits without surrendering
the TTY. Complete defaults are in
[`preset/tuzi-default.toml`](preset/tuzi-default.toml).

### Icons

Icons are configured in `theme.toml`. User rules are checked before the
built-in `devicons` fallback:

```toml
[icon]
directory      = { text = "", fg = "#03a9f4" }
directory_open = { text = "", fg = "#03a9f4" }

prepend_dirs = [
  { name = ".git", text = "", fg = "#f54d27" },
]
prepend_files = [
  { name = "Dockerfile", text = "󰡨", fg = "#458ee6" },
]
prepend_exts = [
  { name = "rs", text = "", fg = "#f74c00" },
]
prepend_globs = [
  { url = "*/tests/*.rs", text = "󰙨", fg = "light-green" },
]
```

The matching order is glob, exact directory/file name, extension, special
file state, `devicons`, then `fallback`. Set `enabled = false` under `[icon]`
to remove icons and their spacing. Colors accept names or `#RRGGBB` values.

All interface styles use semantic keys from the embedded
[`preset/theme-default.toml`](preset/theme-default.toml). A user theme only
needs to patch the properties it wants to change:

```toml
[style]
"mgr.cursor_unfocused" = { bg = "#24273a" }
"mgr.find_match"       = { fg = "#eed49f", bold = true, underline = true }
"tabs.active"          = { fg = "black", bg = "#8aadf4", bold = true }
"status.normal"        = { fg = "black", bg = "#8aadf4" }
"popup.border"         = { fg = "magenta" }
"notify.error"         = { fg = "#ed8796", bold = true }
```

Style properties are `fg`, `bg`, `bold`, `italic`, `underline`, and
`reverse`. Unknown style names and properties are rejected instead of being
silently ignored.

## Keybindings

| Key | Action |
| --- | --- |
| `j` / `k` | Move down / up |
| `l` / `h` | Expand / collapse |
| `Enter` | Toggle directory |
| `zc` | Collapse current subtree |
| `zm` | Collapse sibling subtrees at the current level |
| `zM` | Collapse all subtrees |
| `zz` | Center current row |
| `;` | Toggle selection |
| `v` | Visual selection |
| `o` / `O` | Open / open with |
| `a` | Create file or directory |
| `r` | Rename |
| `K` | Show details for the current entry |
| `I` | Toggle truncated filename peek |
| `y` / `x` | Copy / cut selected files |
| `p` | Paste |
| `Space Space` | Jump with `fzf` |
| `Space -` / `Space _` | Paste as a relative / absolute symlink |
| `d` / `D` | Move to trash / delete permanently |
| `/` / `?` | Find next / previous |
| `f` | Filter |
| `.` | Toggle hidden files |
| `gh` | Go to session home (`--home DIR`, otherwise startup PATH or current directory) |
| `g~` / `gc` | Go to home / config directory |
| `gd` / `gD` | Go to Downloads / Desktop |
| `Ctrl-o` / `Ctrl-i` | Back / forward in directory history |
| `w` | Show task manager |
| `Ctrl-p` | Toggle preview |
| `tt` / `W` | Create / close tab |
| `[` / `]` | Previous / next tab |
| `q` | Quit |

Prefix keys such as `Space`, `g`, `c`, `m`, and `,` show their available commands inside Tuzi.

## Roadmap

- **Neovim integration** — an official plugin with editor synchronization,
  flexible layouts, and workspace session restoration.
- **Deeper tree workflows** — Git and diagnostics, bookmarks and workspace
  roots, ignore rules, batch rename, and contextual actions.

## Acknowledgements

Tuzi would not exist without [Yazi](https://github.com/sxyazi/yazi). Most of its interaction design, keybindings, asynchronous architecture, task system, and implementation approach were learned from or adapted from Yazi. Sincere thanks to Yazi and all of its contributors for their outstanding work.

## License

Tuzi is distributed under the [MIT License](LICENSE). Yazi's original copyright and MIT notice are preserved in [Third-Party Notices](THIRD_PARTY_NOTICES.md).
