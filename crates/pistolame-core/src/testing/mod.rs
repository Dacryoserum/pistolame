//! Test support shared by this crate's tests and by the other crates' tests (feature
//! `test-support`): deterministic signals, generated FLAC fixtures and a fake output backend.
//! No audio file is ever committed to the repository; everything here is produced at test time.

pub mod fake_output;
pub mod flac_fixtures;
mod high_sample_rate;
pub mod signal;

pub use fake_output::*;
pub use flac_fixtures::*;
pub use signal::*;
