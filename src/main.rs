mod action;
mod actor;
mod app;
mod clipboard;
mod column_mode;
mod config;
mod core;
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
mod tui;
mod watcher;

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

const HELP: &str = "tuzi - a tree-style terminal file manager

Usage: tuzi [PATH]

Arguments:
  [PATH]  Directory to open [default: current directory]

Options:
  -h, --help     Print help
  -V, --version  Print version";

enum Cli {
	Run(PathBuf),
	Help,
	Version,
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Cli, String> {
	let mut args = args.into_iter();
	let Some(first) = args.next() else {
		return Ok(Cli::Run(PathBuf::from(".")));
	};
	if first == "-h" || first == "--help" {
		return no_extra_args(args, Cli::Help);
	}
	if first == "-V" || first == "--version" {
		return no_extra_args(args, Cli::Version);
	}
	let path = if first == "--" {
		args.next().ok_or_else(|| "expected PATH after '--'".to_owned())?
	} else {
		if first.to_string_lossy().starts_with('-') {
			return Err(format!("unknown option: {}", first.to_string_lossy()));
		}
		first
	};
	if let Some(extra) = args.next() {
		return Err(format!("unexpected argument: {}", extra.to_string_lossy()));
	}
	Ok(Cli::Run(path.into()))
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
		Ok(Cli::Run(path)) => match app::App::serve(path).await {
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

#[cfg(test)]
mod cli_tests {
	use super::*;

	fn parse(args: &[&str]) -> Result<Cli, String> {
		parse_args(args.iter().map(|arg| OsString::from(*arg)))
	}

	#[test]
	fn defaults_to_the_current_directory_and_accepts_one_path() {
		assert!(matches!(parse(&[]).unwrap(), Cli::Run(path) if path.as_path() == std::path::Path::new(".")));
		assert!(matches!(parse(&["somewhere"]).unwrap(), Cli::Run(path) if path.as_path() == std::path::Path::new("somewhere")));
	}

	#[test]
	fn recognizes_help_version_and_double_dash() {
		assert!(matches!(parse(&["--help"]).unwrap(), Cli::Help));
		assert!(matches!(parse(&["-V"]).unwrap(), Cli::Version));
		assert!(matches!(parse(&["--", "-directory"]).unwrap(), Cli::Run(path) if path.as_path() == std::path::Path::new("-directory")));
	}

	#[test]
	fn rejects_unknown_options_and_extra_paths() {
		assert!(parse(&["--wat"]).is_err());
		assert!(parse(&["one", "two"]).is_err());
	}
}
