//! Test support shared by this crate's tests and by the other crates' tests (feature
//! `test-support`): deterministic signals and generated FLAC fixtures. No audio file is ever
//! committed to the repository; everything here is produced at test time.

pub mod flac_fixtures;
mod high_sample_rate;
pub mod signal;

pub use flac_fixtures::*;
pub use signal::*;
