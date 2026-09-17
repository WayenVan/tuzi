//! Internal publish/subscribe skeleton — phase 1 of the DDS design in
//! `.ai/dds-plan.md`. Everything here is in-process only; cross-instance
//! transport over a Unix socket is a later phase.

mod body;
mod registry;

pub use body::Body;
pub use registry::Registry;
