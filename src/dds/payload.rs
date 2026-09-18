use std::{
	env,
	path::PathBuf,
	time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use super::Body;

/// Identifies a DDS peer (one running `tuzi` process, or one `tuzi emit`/
/// `tuzi sub` CLI invocation). `0` is reserved for "broadcast" as a
/// `receiver` and "the server itself" as a `sender`.
pub type PeerId = u64;

/// The envelope every message travels the DDS Unix socket in, one per
/// line as JSON (`.ai/dds-plan.md` P3). `receiver == 0` means broadcast to
/// every peer that declared interest in the body's kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Payload {
	pub receiver: PeerId,
	pub sender:   PeerId,
	pub body:     Body,
}

impl Payload {
	pub fn broadcast(sender: PeerId, body: Body) -> Self {
		Self { receiver: 0, sender, body }
	}
}

/// A best-effort unique id for this process's DDS connection. Collisions are
/// astronomically unlikely (process id mixed with a nanosecond timestamp),
/// and the server rejects a duplicate rather than replacing the first route.
pub fn new_peer_id() -> PeerId {
	let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
	((std::process::id() as u64) << 32) ^ nanos
}

/// Where the DDS Unix socket lives: `$XDG_RUNTIME_DIR/tuzi/dds.sock`,
/// falling back to the system temp directory when unset (mirrors the
/// manual env-var lookups `config::default_config_dir` already uses,
/// rather than pulling in an XDG crate for one path).
pub fn socket_path() -> PathBuf {
	let base = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(env::temp_dir);
	let directory = if env::var_os("XDG_RUNTIME_DIR").is_some() {
		"tuzi".to_string()
	} else {
		// SAFETY: `geteuid` has no preconditions and only reads process state.
		format!("tuzi-{}", unsafe { libc::geteuid() })
	};
	base.join(directory).join("dds.sock")
}
