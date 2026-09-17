use std::path::PathBuf;

use crate::tasks::TaskKind;

/// A message published on Tuzi's internal event bus. Only events with
/// external/cross-tab meaning belong here — pure IO progress chunks
/// (`Loaded`, `PreviewLoaded`, ...) stay on the plain `Event` channel.
#[derive(Clone, Debug, PartialEq)]
pub enum Body {
	Cd { path: PathBuf },
	Yank { paths: Vec<PathBuf>, cut: bool },
	Renamed { from: PathBuf, to: PathBuf },
	TaskDone { kind: TaskKind, ok: bool },
	/// A user-defined event published via `Command::Emit` (the `emit`
	/// command) — the dynamic escape hatch for kinds that aren't one of
	/// the built-in variants above.
	Custom { kind: String, data: serde_json::Value },
}

/// Built-in kind names, reserved so `emit` can't be used to spoof one of
/// them.
pub const BUILTIN_KINDS: &[&str] = &["cd", "yank", "renamed", "task-done"];

impl Body {
	pub fn kind(&self) -> &str {
		match self {
			Body::Cd { .. } => "cd",
			Body::Yank { .. } => "yank",
			Body::Renamed { .. } => "renamed",
			Body::TaskDone { .. } => "task-done",
			Body::Custom { kind, .. } => kind,
		}
	}
}
