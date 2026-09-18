//! Tuzi's local publish/subscribe bus: an in-process registry plus the
//! Unix-socket client used by Tuzi and companion tools such as `tu dds`.

mod body;
mod launch;
mod payload;
mod registry;
mod transport;

pub use body::{BUILTIN_KINDS, Body, PeerInfo, TabInfo};
pub use launch::{DdsLaunch, MAX_LAUNCH_TOKEN_BYTES};
pub use payload::{Payload, PeerId, socket_path};
pub use registry::Registry;
pub use transport::{Client, WILDCARD_ABILITY};

// Construction helpers stay private; the wire envelope and peer types are
// public because companion binaries need to inspect received messages.
use payload::new_peer_id;
