# Changelog

All notable changes to Tuzi are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Tuzi follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.3] - 2026-09-21

### Added

- `R` (`:refresh`) reads every open directory of the active tab again, keeping the cursor, selection and expansion. It is the way out when something on screen is stale.
- A session home shared by every tab: `g=` goes to it, `--home DIR` sets it at startup, and it is an optional top-level `home` in session snapshots (`get-state`, `restore-state`, `tuzi-exit`, startup state). At startup `--home` wins over the snapshot's `home`, which wins over `PATH`.
- A `set-home` controller operation that changes a running Tuzi's home without moving any tab. The controller protocol stays at version 2.
- `emit --parent KIND [JSON]` sends a custom event straight to the controlling parent instead of broadcasting it. It needs no controller ability and never falls back to a broadcast.
- `tabs.index_active` and `tabs.index_inactive` theme styles for the number in front of a tab name.

### Changed

- The tab bar no longer gives every tab an equal share of the width. Tabs that fit keep their full names; when space is short only the long names are truncated; and when fewer than 8 columns per tab would remain, the bar scrolls around the active tab and marks the hidden side with `‹` / `›`. A click always lands on the tab that was drawn there.
- The number in front of a tab name is drawn quieter than the name. Existing `theme.toml` files need no change.
- Info notices disappear after 2 seconds by default instead of 3 (`notify.info_timeout`).
- File watching is driven by the tree: Tuzi declares which directories are open and the watcher keeps its watches matching, so a directory is watched exactly while it is expanded.
- If the native watcher cannot be used (typically the per-user limit on inotify instances or watches), Tuzi falls back to polling every `watcher.poll_interval_ms` and says so, instead of failing to open the tab. A root directory that cannot be watched no longer stops a tab from opening.
- `emit` rejects a kind that starts with `-`, so a mistyped flag cannot become an event kind.

### Fixed

- A directory that was deleted and recreated (`rm -rf build && mkdir build`, a git branch switch, `mv dir dir.old && mkdir dir`) stopped updating for good. The watcher now notices the lost watch itself, also when the directory comes back later, and reads the directory again.
- Deleting or renaming an expanded directory left it in its parent's listing, shown as an error, for good. The parent now hears about it.
- Expanding a collapsed directory showed its stale cached listing. It is now read again, with the old rows on screen until the new ones arrive.
- When the OS dropped file events or the watcher backend failed, open directories could stay stale unnoticed. Tuzi now reads them again and says so.
- A watch that cannot be registered because of a limit on watching itself is reported instead of silently leaving that directory stale.

## [0.4.2] - 2026-09-19

### Added

- An entry-details popup on `K` that shows the complete selected name, path, metadata, and symbolic-link target.
- An optional filename peek on `I` that shows the truncated remainder of the selected name below its row.
- Adaptive preview layouts that stack below the tree in narrow terminals and remain side by side at wider widths, with configurable layout and split threshold.
- `zm` to collapse all directory subtrees at the current sibling level; global subtree collapse now uses `zM`.

### Changed

- Directory history navigation with `<C-o>` and `<C-i>` now restores each location's cursor and expanded tree state.
- Returning to a parent or ancestor directory focuses the child that was just exited.

### Fixed

- Long symbolic-link targets no longer displace filenames when the tree pane is narrow.
- Entering a symbolic-link directory preserves its logical path, so parent and history navigation return to the link instead of its canonical target.

## [0.4.1] - 2026-09-19

### Added

- Controller operations `get-tabs`, `switch-tab`, `get-state`, and `reveal` for listing and switching tabs, capturing a restorable session snapshot, and revealing a path in the active tab.
- A `tuzi-exit` controller event carrying the final restorable session snapshot when a controlled Tuzi exits gracefully.

### Changed

- The controller protocol is now version 2: `set-state` is replaced by `update-tab`, whose request field is `update` instead of `state`.
- Implicit state events from a controlled Tuzi are sent directly to its controlling parent; only kinds listed in `dds.broadcast` are broadcast publicly.
- `hover` is no longer a default controller ability; pass it explicitly with `--abilities` to receive cursor movement events.

## [0.4.0] - 2026-09-18

### Added

- User-overridable configuration, keymaps, themes, icons, openers, previews, and file-operation policies.
- A local DDS event bus with publish, subscribe, direct messaging, peer discovery, and reconnect support.
- The `tu` companion CLI for DDS inspection, scripted events, managed Tuzi launches, and JSON Lines editor control.
- Runtime configuration overlays and atomic restoration of complete multi-tab sessions, including roots, cursors, selections, and expanded directories.
- Authenticated launch handshakes for Neovim and other controlling hosts.

### Changed

- Built-in actions now share a public command model used by keymaps, prompts, and external integrations.
- The README now highlights current features, integration protocols, and the focused Neovim and tree-workflow roadmap.

### Fixed

- Batch deletion no longer collapses the tree when an entry disappears during an in-progress directory scan.

## [0.3.0] - 2026-09-17

### Added

- Symlink targets are shown directly in the tree, with dangling links highlighted as broken.
- Commands to collapse the current subtree or all subtrees and to center the current row.
- A `Space` command prefix for fuzzy jumping and relative or absolute symlink paste.

### Changed

- Tree refresh and sorting preserve cursor position and symlink metadata more reliably.
- Status, permission, cursor-row, and focus styling have improved contrast and consistency.

### Fixed

- Cursor-row highlighting no longer clashes with explicit colors used by links, loading states, errors, and search matches.

## [0.2.0] - 2026-09-17

### Added

- Mouse support for selecting tree rows, scrolling the tree and preview, switching tabs, toggling directories with right-click, and resizing the preview pane.
- Relative and absolute symbolic-link paste commands.
- Zoxide integration for interactive directory jumping and visit tracking.
- Terminal focus detection with a lower-contrast cursor-row background when Tuzi is unfocused.
- A Yazi-style clipboard counter in the top-right corner, with distinct copy and cut colors.
- Support for expanding symlinked directories and tracking filesystem changes through them.

### Changed

- Pasting into a directory now requires that directory to be expanded, matching file creation behavior; collapsed directories target their parent.
- Preview pane width can be adjusted with the mouse and is retained while Tuzi is running.
- Mouse input is isolated from modal dialogs and prompts so clicks do not leak into the tree underneath.

### Fixed

- Filesystem events passing through symlinked directories are reported using the visible link path.

## [0.1.0] - 2026-09-17

- Initial release.

[0.4.3]: https://github.com/WayenVan/tuzi/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/WayenVan/tuzi/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/WayenVan/tuzi/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/WayenVan/tuzi/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/WayenVan/tuzi/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/WayenVan/tuzi/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/WayenVan/tuzi/releases/tag/v0.1.0
