# Changelog

All notable changes to Tuzi are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Tuzi follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.3.0]: https://github.com/WayenVan/tuzi/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/WayenVan/tuzi/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/WayenVan/tuzi/releases/tag/v0.1.0
