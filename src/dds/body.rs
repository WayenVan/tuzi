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
}

impl Body {
	pub fn kind(&self) -> &'static str {
		match self {
			Body::Cd { .. } => "cd",
			Body::Yank { .. } => "yank",
			Body::Renamed { .. } => "renamed",
			Body::TaskDone { .. } => "task-done",
		}
	}
}
