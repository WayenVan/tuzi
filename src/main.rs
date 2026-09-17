mod command;
mod actor;
mod app;
mod clipboard;
mod column_mode;
mod config;
mod core;
mod dds;
mod event;
mod finder;
mod fs;
mod icon;
mod keymap;
mod notice;
mod opener;
mod preview;
mod process;
mod runner;
mod scheduler;
mod status;
mod tasks;
mod theme;
mod tui;
mod watcher;

use std::{ffi::OsString, io, path::PathBuf, process::ExitCode};

use config::{Config, LoadOptions};
use keymap::Keymap;
use theme::Theme;

const HELP: &str = "tuzi - a tree-style terminal file manager

Usage: tuzi [OPTIONS] [PATH]
       tuzi emit <KIND> [JSON]
       tuzi sub

Arguments:
  [PATH]  Directory to open [default: current directory]

Options:
      --config-dir <DIR>  Use a custom configuration directory
      --no-config         Ignore all user configuration
  -h, --help     Print help
  -V, --version  Print version

DDS commands (talk to the socket event bus, no TUI needed; see
.ai/dds-plan.md):
  emit <KIND> [JSON]  Publish a custom event; JSON defaults to null
  sub                 Print every payload published on the bus

A directory literally named 'emit' or 'sub' can still be opened with
'tuzi -- emit' / 'tuzi -- sub'.";

enum Cli {
	Run { path: PathBuf, config: LoadOptions },
	Help,
	Version,
	Emit { kind: String, data: serde_json::Value },
	Sub,
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Cli, String> {
	let mut args = args.into_iter().peekable();
	match args.peek().map(|arg| arg.to_string_lossy().into_owned()).as_deref() {
		Some("emit") => {
			args.next();
			return parse_emit_args(args);
		}
		Some("sub") => {
			args.next();
			return no_extra_args(args, Cli::Sub);
		}
		_ => {}
	}

	let mut path = None;
	let mut config = LoadOptions::default();
	while let Some(arg) = args.next() {
		match arg.to_string_lossy().as_ref() {
			"-h" | "--help" => return no_extra_args(args, Cli::Help),
			"-V" | "--version" => return no_extra_args(args, Cli::Version),
			"--no-config" => config.no_config = true,
			"--config-dir" => config.config_dir = Some(args.next().ok_or("expected DIR after '--config-dir'")?.into()),
			"--" => {
				let value = args.next().ok_or_else(|| "expected PATH after '--'".to_owned())?;
				if path.replace(value.into()).is_some() { return Err("unexpected extra PATH".into()); }
				if let Some(extra) = args.next() { return Err(format!("unexpected argument: {}", extra.to_string_lossy())); }
				break;
			},
			value if value.starts_with('-') => return Err(format!("unknown option: {value}")),
			_ if path.is_some() => return Err(format!("unexpected argument: {}", arg.to_string_lossy())),
			_ => path = Some(arg.into()),
		}
	}
	if config.no_config && config.config_dir.is_some() { return Err("--no-config and --config-dir cannot be used together".into()); }
	Ok(Cli::Run { path: path.unwrap_or_else(|| PathBuf::from(".")), config })
}

fn parse_emit_args(mut args: impl Iterator<Item = OsString>) -> Result<Cli, String> {
	let kind = args.next().ok_or("expected KIND after 'emit'")?.to_string_lossy().into_owned();
	if kind.is_empty() || dds::BUILTIN_KINDS.contains(&kind.as_str()) {
		return Err(format!("'{kind}' is not a valid custom kind (empty or a reserved built-in name)"));
	}
	let data = match args.next() {
		Some(json) => {
			let json = json.to_string_lossy().into_owned();
			serde_json::from_str(&json).map_err(|_| format!("invalid json for emit: '{json}'"))?
		}
		None => serde_json::Value::Null,
	};
	no_extra_args(args, Cli::Emit { kind, data })
}

fn no_extra_args(mut args: impl Iterator<Item = OsString>, command: Cli) -> Result<Cli, String> {
	match args.next() {
		Some(extra) => Err(format!("unexpected argument: {}", extra.to_string_lossy())),
		None => Ok(command),
	}
}

#[tokio::main]
async fn main() -> ExitCode {
	match parse_args(std::env::args_os().skip(1)) {
		Ok(Cli::Help) => {
			println!("{HELP}");
			ExitCode::SUCCESS
		}
		Ok(Cli::Version) => {
			println!("tuzi {}", env!("CARGO_PKG_VERSION"));
			ExitCode::SUCCESS
		}
		Ok(Cli::Run { path, config }) => match Config::load(&config).and_then(|behavior| Keymap::load(&config).and_then(|keymap| Theme::load(&config).map(|theme| (behavior, keymap, theme)))) {
			Err(error) => {
				eprintln!("tuzi: {error}");
				ExitCode::FAILURE
			},
			Ok((config, keymap, theme)) => match app::App::serve(path, config, keymap, theme).await {
			Ok(()) => ExitCode::SUCCESS,
			Err(error) => {
				eprintln!("tuzi: {error}");
				ExitCode::FAILURE
			}
			},
		},
		Ok(Cli::Emit { kind, data }) => match run_emit(kind, data).await {
			Ok(()) => ExitCode::SUCCESS,
			Err(error) => {
				eprintln!("tuzi: {error}");
				ExitCode::FAILURE
			}
		},
		Ok(Cli::Sub) => match run_sub().await {
			Ok(()) => ExitCode::SUCCESS,
			Err(error) => {
				eprintln!("tuzi: {error}");
				ExitCode::FAILURE
			}
		},
		Err(error) => {
			eprintln!("tuzi: {error}\nTry 'tuzi --help' for more information.");
			ExitCode::from(2)
		}
	}
}

/// `tuzi emit <kind> [json]`: a one-shot DDS publish with no TUI. Doesn't
/// need to declare any abilities of its own — it never receives anything.
async fn run_emit(kind: String, data: serde_json::Value) -> io::Result<()> {
	let (client, _inbox) = dds::Client::connect(&dds::socket_path(), Vec::new()).await?;
	client.publish(dds::Body::Custom { kind, data });
	client.flush().await;
	Ok(())
}

/// `tuzi sub`: prints every payload published on the bus, one JSON line
/// per message, until interrupted — a debug tap equivalent to `ya sub`.
async fn run_sub() -> io::Result<()> {
	let (_client, mut inbox) = dds::Client::connect(&dds::socket_path(), vec![dds::WILDCARD_ABILITY.to_string()]).await?;
	while let Some(payload) = inbox.recv().await {
		if let Ok(line) = serde_json::to_string(&payload.body) {
			println!("{line}");
		}
	}
	Ok(())
}

#[cfg(test)]
mod cli_tests {
	use super::*;

	fn parse(args: &[&str]) -> Result<Cli, String> {
		parse_args(args.iter().map(|arg| OsString::from(*arg)))
	}

	#[test]
	fn defaults_to_the_current_directory_and_accepts_one_path() {
		assert!(matches!(parse(&[]).unwrap(), Cli::Run { path, .. } if path.as_path() == std::path::Path::new(".")));
		assert!(matches!(parse(&["somewhere"]).unwrap(), Cli::Run { path, .. } if path.as_path() == std::path::Path::new("somewhere")));
	}

	#[test]
	fn recognizes_help_version_and_double_dash() {
		assert!(matches!(parse(&["--help"]).unwrap(), Cli::Help));
		assert!(matches!(parse(&["-V"]).unwrap(), Cli::Version));
		assert!(matches!(parse(&["--", "-directory"]).unwrap(), Cli::Run { path, .. } if path.as_path() == std::path::Path::new("-directory")));
	}

	#[test]
	fn rejects_unknown_options_and_extra_paths() {
		assert!(parse(&["--wat"]).is_err());
		assert!(parse(&["one", "two"]).is_err());
	}

	#[test]
	fn accepts_config_options_before_or_after_the_path() {
		assert!(matches!(parse(&["--no-config", "somewhere"]).unwrap(), Cli::Run { config: LoadOptions { no_config: true, .. }, .. }));
		assert!(matches!(parse(&["somewhere", "--config-dir", "settings"]).unwrap(), Cli::Run { config: LoadOptions { config_dir: Some(path), .. }, .. } if path == PathBuf::from("settings")));
		assert!(parse(&["--no-config", "--config-dir", "settings"]).is_err());
	}
}
