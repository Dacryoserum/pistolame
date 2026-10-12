//! Audio output backends implementing the `AudioOutput` contract defined in `pistolame-core`.
//!
//! The backend for the current platform is chosen once, in `platform_default_output`. This is
//! the only place outside the backend modules where the target platform is tested.

pub use pistolame_core::format::{
    ChannelMask, DeviceSampleFormat, StreamFormat, ValidBitsAlignment,
    lossless_device_format_candidates,
};
pub use pistolame_core::output_contract::*;

pub mod float_conversion;
mod stream_state;
pub mod unsupported;

#[cfg(target_os = "macos")]
pub mod coreaudio;
#[cfg(windows)]
pub mod wasapi;

/// The output backend for the platform this binary runs on: WASAPI on Windows, CoreAudio on
/// macOS, a backend with no devices on Linux until the ALSA backend exists (plan B5.9).
pub fn platform_default_output() -> Box<dyn AudioOutput> {
    #[cfg(windows)]
    {
        Box::new(wasapi::WasapiOutput::new())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(coreaudio::CoreAudioOutput::new())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Box::new(unsupported::UnsupportedPlatformOutput)
    }
}
