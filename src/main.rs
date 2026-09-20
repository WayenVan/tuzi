use std::{ffi::OsString, io, path::PathBuf, process::ExitCode};

use tuzi::{
	app,
	config::{Config, LoadOptions, RuntimeConfigSource, load_runtime_state},
	dds,
	keymap::Keymap,
	theme::Theme,
};

const HELP: &str = "tuzi - a tree-style terminal file manager

Usage: tuzi [OPTIONS] [PATH]
       tuzi emit <KIND> [JSON]
       tuzi sub

Arguments:
  [PATH]  Directory to open [default: current directory]

Options:
      --home <DIR>        Set the session home directory (g=)
      --config-dir <DIR>  Use a custom configuration directory
      --no-config         Ignore all user configuration
      --runtime-config <JSON>       Apply process-local config/keymap/state JSON (repeatable)
      --runtime-config-file <FILE>  Apply process-local config/keymap/state JSON file (repeatable)
      --dds-parent <ID>   DDS controller peer for a managed launch
      --dds-token <TOKEN> Correlation token paired with --dds-parent
  -h, --help     Print help
  -V, --version  Print version

DDS commands (talk to the socket event bus, no TUI needed; see
.ai/dds-plan.md):
  emit <KIND> [JSON]  Publish a custom event; JSON defaults to null
  sub                 Print every payload published on the bus

A directory literally named 'emit' or 'sub' can still be opened with
'tuzi -- emit' / 'tuzi -- sub'.";

enum Cli {
	Run { path: PathBuf, home: Option<PathBuf>, config: LoadOptions, dds_launch: Option<dds::DdsLaunch> },
	Help,
	Version,
	Emit { kind: String, data: serde_json::Value },
	Sub,
}

#[cfg(test)]
fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Cli, String> {
	parse_args_with_env(args, None, None)
}

fn parse_args_with_env(
	args: impl IntoIterator<Item = OsString>,
	env_parent: Option<OsString>,
	env_token: Option<OsString>,
) -> Result<Cli, String> {
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
	let mut home = None;
	let mut config = LoadOptions::default();
	let mut cli_parent = None;
	let mut cli_token = None;
	while let Some(arg) = args.next() {
		match arg.to_string_lossy().as_ref() {
			"-h" | "--help" => return no_extra_args(args, Cli::Help),
			"-V" | "--version" => return no_extra_args(args, Cli::Version),
			"--home" => home = Some(PathBuf::from(args.next().ok_or("expected DIR after '--home'")?)),
			"--no-config" => config.no_config = true,
			"--config-dir" => config.config_dir = Some(args.next().ok_or("expected DIR after '--config-dir'")?.into()),
			"--runtime-config" => config.runtime_config.push(RuntimeConfigSource::Inline(args.next().ok_or("expected JSON after '--runtime-config'")?.into_string().map_err(|_| "--runtime-config value must be valid UTF-8")?)),
			"--runtime-config-file" => config.runtime_config.push(RuntimeConfigSource::File(args.next().ok_or("expected FILE after '--runtime-config-file'")?.into())),
			"--dds-parent" => {
				let value = args.next().ok_or("expected ID after '--dds-parent'")?;
				let value = value.to_str().ok_or("DDS parent peer ID must be valid UTF-8")?;
				cli_parent = Some(value.parse::<u64>().map_err(|_| "DDS parent peer ID must be a non-zero integer")?);
			}
			"--dds-token" => {
				let value = args.next().ok_or("expected TOKEN after '--dds-token'")?;
				cli_token = Some(value.into_string().map_err(|_| "DDS launch token must be valid UTF-8")?);
			}
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
	let dds_launch = resolve_dds_launch(cli_parent, cli_token, env_parent, env_token)?;
	let path = path.unwrap_or_else(|| PathBuf::from("."));
	Ok(Cli::Run { home, path, config, dds_launch })
}

fn resolve_dds_launch(
	cli_parent: Option<u64>,
	cli_token: Option<String>,
	env_parent: Option<OsString>,
	env_token: Option<OsString>,
) -> Result<Option<dds::DdsLaunch>, String> {
	match (cli_parent, cli_token) {
		(Some(parent), Some(token)) => return dds::DdsLaunch::new(parent, token).map(Some),
		(Some(_), None) | (None, Some(_)) => return Err("--dds-parent and --dds-token must be provided together".into()),
		(None, None) => {}
	}
	match (env_parent, env_token) {
		(None, None) => Ok(None),
		(Some(parent), Some(token)) => {
			let parent = parent.to_str().ok_or("TUZI_DDS_PARENT must be valid UTF-8")?.parse::<u64>().map_err(|_| "TUZI_DDS_PARENT must be a non-zero integer")?;
			let token = token.into_string().map_err(|_| "TUZI_DDS_TOKEN must be valid UTF-8")?;
			dds::DdsLaunch::new(parent, token).map(Some)
		}
		_ => Err("TUZI_DDS_PARENT and TUZI_DDS_TOKEN must be provided together".into()),
	}
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
	match parse_args_with_env(
		std::env::args_os().skip(1),
		std::env::var_os("TUZI_DDS_PARENT"),
		std::env::var_os("TUZI_DDS_TOKEN"),
	) {
		Ok(Cli::Help) => {
			println!("{HELP}");
			ExitCode::SUCCESS
		}
		Ok(Cli::Version) => {
			println!("tuzi {}", env!("CARGO_PKG_VERSION"));
			ExitCode::SUCCESS
		}
		Ok(Cli::Run { path, home, config, dds_launch }) => match Config::load(&config).and_then(|behavior| Keymap::load(&config).and_then(|keymap| Theme::load(&config).and_then(|theme| load_runtime_state(&config).map(|state| (behavior, keymap, theme, state))))) {
			Err(error) => {
				eprintln!("tuzi: {error}");
				ExitCode::FAILURE
			},
			Ok((config, _, _, _)) if config.dds.open == tuzi::config::DdsOpen::Parent && dds_launch.is_none() => {
				eprintln!("tuzi: dds.open=parent requires a controlled DDS launch");
				ExitCode::FAILURE
			}
			Ok((config, keymap, theme, state)) => match app::App::serve(path, home, config, keymap, theme, state, dds_launch).await {
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
	fn session_home_priority() {
		for (args, expected_path, expected_home) in [
			(vec![], ".", None),
			(vec!["project"], "project", None),
			(vec!["--home", "base"], ".", Some("base")),
			(vec!["--home", "base", "project"], "project", Some("base")),
			(vec!["project", "--home", "base"], "project", Some("base")),
		] {
			let Cli::Run { path, home, .. } = parse(&args).unwrap() else { panic!("expected Run") };
			assert_eq!(path, PathBuf::from(expected_path));
			// `None` means "no explicit --home"; the App then falls back to a
			// snapshot home and finally to PATH.
			assert_eq!(home, expected_home.map(PathBuf::from));
		}
		assert!(parse(&["--home"]).is_err());
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

	#[test]
	fn controlled_launch_arguments_are_paired_and_validated() {
		assert!(matches!(
			parse(&["--dds-parent", "42", "--dds-token", "launch-1"]).unwrap(),
			Cli::Run { dds_launch: Some(dds::DdsLaunch { parent: 42, token }), .. } if token == "launch-1"
		));
		assert!(parse(&["--dds-parent", "42"]).is_err());
		assert!(parse(&["--dds-token", "launch-1"]).is_err());
		assert!(parse(&["--dds-parent", "0", "--dds-token", "launch-1"]).is_err());
	}

	#[test]
	fn controlled_launch_can_come_from_a_complete_environment_pair() {
		let cli = parse_args_with_env(
			Vec::<OsString>::new(),
			Some(OsString::from("73")),
			Some(OsString::from("from-env")),
		).unwrap();
		assert!(matches!(
			cli,
			Cli::Run { dds_launch: Some(dds::DdsLaunch { parent: 73, token }), .. } if token == "from-env"
		));
		assert!(parse_args_with_env(
			Vec::<OsString>::new(),
			Some(OsString::from("73")),
			None,
		).is_err());
	}

	#[test]
	fn runtime_config_collects_inline_and_file_sources_in_order() {
		assert!(matches!(
			parse(&["--runtime-config", "{\"config\":{}}", "--runtime-config-file", "session.json", "."]).unwrap(),
			Cli::Run { config: LoadOptions { runtime_config, .. }, .. }
				if matches!(runtime_config.as_slice(), [RuntimeConfigSource::Inline(_), RuntimeConfigSource::File(path)] if path == std::path::Path::new("session.json"))
		));
	}
}
