//! Types of the bit-perfect assessment. The pure function `assess_signal_path` belongs to a
//! later task.

use crate::format::{ChannelMask, StreamFormat};
use crate::output_contract::{NegotiatedOutput, SharedModeCause};

/// Processing the app applies before the device. M1: always the default (all `false`, i.e. `ProcessingState::default()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ProcessingState {
    /// A software volume is applied.
    pub is_software_volume_active: bool,
    /// A resampler is applied.
    pub is_resampler_active: bool,
    /// Any DSP is applied.
    pub is_dsp_active: bool,
}

/// Why the path is not proven bit-perfect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotBitPerfectReason {
    /// Badge "Mode partagé", never "Traité": Windows may convert, resample or mix; the path is
    /// unproven rather than proven altered (B13.1 Q5). `cause` comes from
    /// `NegotiatedOutput::shared_mode_cause`.
    SharedModeThroughSystemMixer {
        /// Why shared mode is in use.
        cause: SharedModeCause,
    },
    /// The device rate differs from the source rate.
    SampleRateDiffers {
        /// Source sample rate in hertz.
        source_hz: u32,
        /// Device sample rate in hertz.
        device_hz: u32,
    },
    /// The device precision is lower than the source precision.
    BitDepthReduced {
        /// Source bits per sample.
        source_bits: u8,
        /// Valid bits per sample on the device.
        device_valid_bits: u8,
    },
    /// The device channel layout differs from the source layout.
    ChannelLayoutDiffers {
        /// Source speaker mask.
        source_mask: ChannelMask,
        /// Device speaker mask.
        device_mask: ChannelMask,
    },
    /// A software volume is applied.
    SoftwareVolumeActive,
    /// A resampler is applied.
    ResamplerActive,
    /// A DSP is applied.
    DspActive,
}

/// Outcome of the assessment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BitPerfectVerdict {
    /// The samples reach the device unchanged.
    BitPerfect,
    /// At least one reason prevents proving bit-perfect output.
    NotBitPerfect {
        /// All reasons found, never empty.
        reasons: Vec<NotBitPerfectReason>,
    },
}

/// Everything the badge and the signal-path popover show, computed from real state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignalPathAssessment {
    /// Format of the decoded source.
    pub source_format: StreamFormat,
    /// What the backend actually negotiated.
    pub negotiated: NegotiatedOutput,
    /// Processing applied before the device.
    pub processing: ProcessingState,
    /// Bit-perfect or not, with every reason; the only output of `assess_signal_path`.
    pub verdict: BitPerfectVerdict,
    /// True when the source is padded into a wider container (e.g. 16 bits in 32);
    /// lossless, shown as information, not as a reason.
    pub is_zero_padded_into_wider_container: bool,
}
