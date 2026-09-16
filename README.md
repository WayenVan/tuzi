# tuzi

A tree-style TUI file manager, inspired by [yazi](https://github.com/sxyazi/yazi).

## Layout

Modules mirror the crate boundaries yazi eventually grew into; each one
splits out into its own crate once it needs independent compilation or reuse.

- `event` — the `Event` enum and dispatch channel
- `app` — event loop, dispatcher, keymap router (the composition root)
- `core` — domain state: the node tree, selection, filter/find
- `actor` — `Cmd -> Opt -> Actor::act` command handlers
- `fs` — filesystem engine: cached metadata, read_dir backends, sorting
- `scheduler` — background task queue (copy/move/delete/size)
- `runner` — "open with" resolution
- `watcher` — filesystem change notifications
- `tui` — terminal backend and widgets, including the tree view
- `config` — keymap/theme/config loading

Extraction order: `fs` first, then the tree `core`, then `event`/`app`,
then `scheduler`/`runner`, then `watcher`. Scripting/plugins are deliberately
out of scope until the core tree experience works.
