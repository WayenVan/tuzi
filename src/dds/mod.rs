//! Internal publish/subscribe skeleton — phase 1 of the DDS design in
//! `.ai/dds-plan.md`. Everything here is in-process only; cross-instance
//! transport over a Unix socket is a later phase.

mod body;
mod payload;
mod registry;
mod transport;

pub use body::{BUILTIN_KINDS, Body};
pub use payload::socket_path;
pub use registry::Registry;
pub use transport::{Client, WILDCARD_ABILITY};

// Visible to sibling submodules (`super::X`) but not part of `dds`'s
// public surface — nothing outside this module needs the wire-format
// envelope or peer bookkeeping types directly.
use body::PeerInfo;
use payload::{PeerId, Payload, new_peer_id};
