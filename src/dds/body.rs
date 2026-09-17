use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::tasks::TaskKind;

use super::PeerId;

/// A message published on Tuzi's internal event bus, and — for the
/// non-handshake variants — the wire format for the DDS Unix socket
/// (`.ai/dds-plan.md` P3). Only events with external/cross-tab meaning
/// belong here — pure IO progress chunks (`Loaded`, `PreviewLoaded`, ...)
/// stay on the plain `Event` channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Body {
	/// Handshake sent by a client right after connecting: the kinds it
	/// wants forwarded from other peers.
	Hi { abilities: Vec<String> },
	/// Broadcast by the server whenever the peer table changes.
	Hey { peers: Vec<PeerInfo> },
	Cd { path: PathBuf },
	Yank { paths: Vec<PathBuf>, cut: bool },
	Renamed { from: PathBuf, to: PathBuf },
	TaskDone { kind: TaskKind, ok: bool },
	/// A user-defined event published via `Command::Emit` (the `emit`
	/// command) or the `tuzi emit` CLI — the dynamic escape hatch for
	/// kinds that aren't one of the built-in variants above.
	Custom { kind: String, data: serde_json::Value },
}

/// Built-in kind names, reserved so `emit` can't be used to spoof one of
/// them.
pub const BUILTIN_KINDS: &[&str] = &["hi", "hey", "cd", "yank", "renamed", "task-done"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeerInfo {
	pub id:        PeerId,
	pub abilities: Vec<String>,
}

impl Body {
	pub fn kind(&self) -> &str {
		match self {
			Body::Hi { .. } => "hi",
			Body::Hey { .. } => "hey",
			Body::Cd { .. } => "cd",
			Body::Yank { .. } => "yank",
			Body::Renamed { .. } => "renamed",
			Body::TaskDone { .. } => "task-done",
			Body::Custom { kind, .. } => kind,
		}
	}
}
