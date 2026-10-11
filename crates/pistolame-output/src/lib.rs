//! Audio output backends implementing the `AudioOutput` contract defined in `pistolame-core`.

pub use pistolame_core::format::{
    ChannelMask, DeviceSampleFormat, StreamFormat, ValidBitsAlignment,
    lossless_device_format_candidates,
};
pub use pistolame_core::output_contract::*;
