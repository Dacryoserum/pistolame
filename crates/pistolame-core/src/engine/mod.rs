//! Engine-facing types: the messages between the UI and the engine thread, and the lock-free
//! counters the real-time thread writes for the UI.

pub mod counters;
pub mod messages;

pub use counters::*;
pub use messages::*;
