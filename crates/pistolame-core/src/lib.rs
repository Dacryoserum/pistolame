//! Decoding, sample formats, gapless playback queue, live MD5 verification and the audio engine.
//! Contains no UI and no platform output code.

#![forbid(unsafe_code)]

pub mod decoder;
pub mod engine;
pub mod feeder;
pub mod format;
pub mod output_contract;
pub mod pcm;
pub mod playback_ring;
pub mod signal_path;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod timing;
pub mod verification;
pub mod volume;

pub use decoder::*;
pub use engine::*;
pub use feeder::*;
pub use format::*;
pub use output_contract::*;
pub use pcm::*;
pub use playback_ring::*;
pub use signal_path::*;
pub use verification::*;
pub use volume::*;
