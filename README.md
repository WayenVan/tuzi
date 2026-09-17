<p align="center">
  <img src="assets/tuzi-icon.png" width="180" alt="tuzi icon">
</p>

<h1 align="center">tuzi</h1>

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
  <img src="assets/demo.gif" width="960" alt="tuzi demo">
</p>

## Why tuzi?

I love Yazi, but some of my work calls for a more convenient tree-shaped view, especially inside Neovim. After spending hours looking for something that fit, I finally decided to sell my soul to Codex and Claude and build exactly what I wanted.

## Install

Download the macOS or Linux archive for your platform from [GitHub Releases](https://github.com/WayenVan/tuzi/releases), extract it, and place `tuzi` somewhere on your `PATH`.

To build from source, install a recent Rust toolchain, clone the repository, and run:

```sh
cargo install --path .
```

For file icons, use a terminal font that includes Nerd Font symbols. `fzf` is optional and only required for the jump command.

## Usage

```sh
tuzi [PATH]
```

`PATH` must be a directory and defaults to the current directory.

```sh
tuzi --help
tuzi --version
```

## Keybindings

| Key | Action |
| --- | --- |
| `j` / `k` | Move down / up |
| `l` / `h` | Expand / collapse |
| `Enter` | Toggle directory |
| `;` | Toggle selection |
| `v` | Visual selection |
| `o` / `O` | Open / open with |
| `a` | Create file or directory |
| `r` | Rename |
| `y` / `x` | Copy / cut selected files |
| `p` | Paste |
| `d` / `D` | Move to trash / delete permanently |
| `/` / `?` | Find next / previous |
| `f` | Filter |
| `.` | Toggle hidden files |
| `z` | Jump with `fzf` |
| `g~` / `gc` | Go to home / config directory |
| `gd` / `gD` | Go to Downloads / Desktop |
| `Ctrl-o` / `Ctrl-i` | Back / forward in directory history |
| `w` | Show task manager |
| `Ctrl-p` | Toggle preview |
| `tt` / `W` | Create / close tab |
| `[` / `]` | Previous / next tab |
| `q` | Quit |

Prefix keys such as `g`, `c`, `m`, and `,` show their available commands inside tuzi.

## Roadmap

1. **User configuration** — configurable keybindings, themes, and behavior without rebuilding tuzi.
2. **Socket event bus** — a Yazi-style publish/subscribe mechanism for external commands, integrations, and inter-process communication.

## Acknowledgements

tuzi would not exist without [Yazi](https://github.com/sxyazi/yazi). Most of its interaction design, keybindings, asynchronous architecture, task system, and implementation approach were learned from or adapted from Yazi. Sincere thanks to Yazi and all of its contributors for their outstanding work.

## License

tuzi is distributed under the [MIT License](LICENSE). Yazi's original copyright and MIT notice are preserved in [Third-Party Notices](THIRD_PARTY_NOTICES.md).
