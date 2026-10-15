//! The bit-perfect assessment: its types and the pure function `assess_signal_path`, the only
//! place that decides whether the "Bit-perfect" badge may be shown.

use crate::decoder::SourceCodec;
use crate::format::{ChannelMask, DeviceSampleFormat, StreamFormat};
use crate::output_contract::{
    DeviceAccessMode, DeviceSampleDelivery, NegotiatedOutput, SharedModeCause,
};

const BITS_PER_BYTE: u32 = 8;

/// Processing the app applies before the device.
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
    /// The device is held exclusively at the source rate, but it only takes float samples: the
    /// backend converts each integer sample to float32 exactly, then the platform handles float
    /// data the app cannot verify (CoreAudio float-only devices, plan coreaudio.md §0). Badge
    /// "Exclusif", never "Bit-perfect".
    DeliveredAsFloatToPlatform,
    /// The codec decodes to floating point, rounded to 24-bit integers before the device: the
    /// samples sent are the decoder's approximation, not the file's own integers.
    FloatDecoderOutputRounded,
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
    /// Codec of the source: lossy or not, and whether its samples were rounded from float.
    pub source_codec: SourceCodec,
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

/// Decides whether the samples reach the device unchanged (B7.1).
///
/// Returns `BitPerfect` if and only if the device is held exclusively, its format matches the
/// source rate and channel layout, its precision is at least the source precision, and no
/// processing is active, and the device receives the render bytes unchanged (no float
/// delivery). Every failed condition adds its own reason, so the verdict is never
/// optimistic. In shared mode the only device-side reason is `SharedModeThroughSystemMixer`;
/// there is no device format to compare, so rules 2 to 4 are not evaluated.
///
/// Inconsistent input is handled conservatively. An exclusive output without `device_format`
/// is judged on `render_format`: in exclusive mode the bytes Pistolame writes are the bytes the
/// device receives, so it is the same comparison. A shared output without a cause is reported
/// as `UserPreference`, the default shared cause, and is never `BitPerfect`.
pub fn assess_signal_path(
    source_format: &StreamFormat,
    source_codec: &SourceCodec,
    negotiated: &NegotiatedOutput,
    processing: &ProcessingState,
) -> SignalPathAssessment {
    let mut reasons = Vec::new();
    let sent_format = negotiated.device_format.unwrap_or(negotiated.render_format);

    match negotiated.access_mode {
        DeviceAccessMode::SharedThroughSystemMixer => {
            reasons.push(NotBitPerfectReason::SharedModeThroughSystemMixer {
                cause: negotiated
                    .shared_mode_cause
                    .unwrap_or(SharedModeCause::UserPreference),
            });
        }
        DeviceAccessMode::Exclusive => {
            reasons.extend(device_format_reasons(source_format, &sent_format));
            if negotiated.sample_delivery == DeviceSampleDelivery::ExactFloat32Conversion {
                reasons.push(NotBitPerfectReason::DeliveredAsFloatToPlatform);
            }
        }
    }
    if source_codec.is_decoded_as_float {
        reasons.push(NotBitPerfectReason::FloatDecoderOutputRounded);
    }
    reasons.extend(processing_reasons(processing));

    let verdict = if reasons.is_empty() {
        BitPerfectVerdict::BitPerfect
    } else {
        BitPerfectVerdict::NotBitPerfect { reasons }
    };
    SignalPathAssessment {
        source_format: *source_format,
        source_codec: *source_codec,
        negotiated: negotiated.clone(),
        processing: *processing,
        verdict,
        is_zero_padded_into_wider_container: u32::from(sent_format.container_bytes) * BITS_PER_BYTE
            > u32::from(source_format.bits_per_sample),
    }
}

/// Reasons the exclusive device format differs from the source (rules 2 to 4 of B7.1).
fn device_format_reasons(
    source: &StreamFormat,
    device: &DeviceSampleFormat,
) -> Vec<NotBitPerfectReason> {
    let mut reasons = Vec::new();
    if device.sample_rate_hz != source.sample_rate_hz {
        reasons.push(NotBitPerfectReason::SampleRateDiffers {
            source_hz: source.sample_rate_hz,
            device_hz: device.sample_rate_hz,
        });
    }
    if device.channel_count != source.channel_count || device.channel_mask != source.channel_mask {
        reasons.push(NotBitPerfectReason::ChannelLayoutDiffers {
            source_mask: source.channel_mask,
            device_mask: device.channel_mask,
        });
    }
    if device.valid_bits_per_sample < source.bits_per_sample {
        reasons.push(NotBitPerfectReason::BitDepthReduced {
            source_bits: source.bits_per_sample,
            device_valid_bits: device.valid_bits_per_sample,
        });
    }
    reasons
}

/// One reason per processing stage the app applies (rule 5 of B7.1).
fn processing_reasons(processing: &ProcessingState) -> Vec<NotBitPerfectReason> {
    let mut reasons = Vec::new();
    if processing.is_software_volume_active {
        reasons.push(NotBitPerfectReason::SoftwareVolumeActive);
    }
    if processing.is_resampler_active {
        reasons.push(NotBitPerfectReason::ResamplerActive);
    }
    if processing.is_dsp_active {
        reasons.push(NotBitPerfectReason::DspActive);
    }
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::ValidBitsAlignment;
    use crate::output_contract::{OutputDeviceId, OutputDeviceInfo};

    const STEREO_MASK: ChannelMask = ChannelMask(0b11);
    const OTHER_MASK: ChannelMask = ChannelMask(0b1_0000_0011);

    fn source(rate: u32, bits: u8) -> StreamFormat {
        StreamFormat {
            sample_rate_hz: rate,
            bits_per_sample: bits,
            channel_count: 2,
            channel_mask: STEREO_MASK,
        }
    }

    fn device(rate: u32, container_bytes: u8, valid_bits: u8) -> DeviceSampleFormat {
        DeviceSampleFormat {
            sample_rate_hz: rate,
            container_bytes,
            valid_bits_per_sample: valid_bits,
            alignment: ValidBitsAlignment::MostSignificant,
            channel_count: 2,
            channel_mask: STEREO_MASK,
        }
    }

    fn output(
        mode: DeviceAccessMode,
        cause: Option<SharedModeCause>,
        device_format: Option<DeviceSampleFormat>,
        render_format: DeviceSampleFormat,
    ) -> NegotiatedOutput {
        NegotiatedOutput {
            backend_name: "test",
            device: OutputDeviceInfo {
                id: OutputDeviceId("test".to_string()),
                display_name: "Test".to_string(),
                is_system_default: false,
            },
            access_mode: mode,
            shared_mode_cause: cause,
            device_format,
            system_mix_format: None,
            render_format,
            sample_delivery: DeviceSampleDelivery::Unchanged,
            buffer_frame_count: 1024,
            device_period_in_hundred_nanoseconds: 100_000,
            negotiation_log: Vec::new(),
        }
    }

    fn exclusive(format: DeviceSampleFormat) -> NegotiatedOutput {
        output(DeviceAccessMode::Exclusive, None, Some(format), format)
    }

    fn shared(cause: SharedModeCause) -> NegotiatedOutput {
        output(
            DeviceAccessMode::SharedThroughSystemMixer,
            Some(cause),
            None,
            device(96_000, 4, 24),
        )
    }

    fn reasons_of(assessment: &SignalPathAssessment) -> Vec<NotBitPerfectReason> {
        match &assessment.verdict {
            BitPerfectVerdict::BitPerfect => Vec::new(),
            BitPerfectVerdict::NotBitPerfect { reasons } => reasons.clone(),
        }
    }

    fn assess(
        src: StreamFormat,
        negotiated: NegotiatedOutput,
        processing: ProcessingState,
    ) -> SignalPathAssessment {
        assess_signal_path(&src, &SourceCodec::FLAC, &negotiated, &processing)
    }

    #[test]
    fn exclusive_exact_match_is_bit_perfect() {
        let result = assess(
            source(96_000, 24),
            exclusive(device(96_000, 3, 24)),
            ProcessingState::default(),
        );
        assert_eq!(result.verdict, BitPerfectVerdict::BitPerfect);
        assert!(!result.is_zero_padded_into_wider_container);
    }

    #[test]
    fn exclusive_wider_valid_bits_is_bit_perfect() {
        let result = assess(
            source(44_100, 16),
            exclusive(device(44_100, 4, 32)),
            ProcessingState::default(),
        );
        assert_eq!(result.verdict, BitPerfectVerdict::BitPerfect);
    }

    #[test]
    fn sixteen_bits_in_thirty_two_sets_padding_flag_without_reason() {
        let result = assess(
            source(44_100, 16),
            exclusive(device(44_100, 4, 16)),
            ProcessingState::default(),
        );
        assert_eq!(result.verdict, BitPerfectVerdict::BitPerfect);
        assert!(result.is_zero_padded_into_wider_container);
    }

    #[test]
    fn twenty_four_in_thirty_two_sets_padding_flag() {
        let result = assess(
            source(96_000, 24),
            exclusive(device(96_000, 4, 24)),
            ProcessingState::default(),
        );
        assert!(result.is_zero_padded_into_wider_container);
    }

    #[test]
    fn sample_rate_difference_alone() {
        let result = assess(
            source(96_000, 24),
            exclusive(device(48_000, 3, 24)),
            ProcessingState::default(),
        );
        assert_eq!(
            reasons_of(&result),
            vec![NotBitPerfectReason::SampleRateDiffers {
                source_hz: 96_000,
                device_hz: 48_000
            }]
        );
    }

    #[test]
    fn bit_depth_reduced_alone() {
        let result = assess(
            source(96_000, 24),
            exclusive(device(96_000, 4, 16)),
            ProcessingState::default(),
        );
        assert_eq!(
            reasons_of(&result),
            vec![NotBitPerfectReason::BitDepthReduced {
                source_bits: 24,
                device_valid_bits: 16
            }]
        );
    }

    #[test]
    fn channel_mask_difference_alone() {
        let mut format = device(96_000, 3, 24);
        format.channel_mask = OTHER_MASK;
        let result = assess(
            source(96_000, 24),
            exclusive(format),
            ProcessingState::default(),
        );
        assert_eq!(
            reasons_of(&result),
            vec![NotBitPerfectReason::ChannelLayoutDiffers {
                source_mask: STEREO_MASK,
                device_mask: OTHER_MASK
            }]
        );
    }

    #[test]
    fn channel_count_difference_alone() {
        let mut format = device(96_000, 3, 24);
        format.channel_count = 6;
        let result = assess(
            source(96_000, 24),
            exclusive(format),
            ProcessingState::default(),
        );
        assert_eq!(
            reasons_of(&result),
            vec![NotBitPerfectReason::ChannelLayoutDiffers {
                source_mask: STEREO_MASK,
                device_mask: STEREO_MASK
            }]
        );
    }

    #[test]
    fn a_source_decoded_as_float_is_never_bit_perfect() {
        let mp3 = SourceCodec {
            display_name: "MP3",
            is_lossy: true,
            is_decoded_as_float: true,
        };
        let result = assess_signal_path(
            &source(44_100, 24),
            &mp3,
            &exclusive(device(44_100, 4, 24)),
            &ProcessingState::default(),
        );
        assert_eq!(
            reasons_of(&result),
            vec![NotBitPerfectReason::FloatDecoderOutputRounded]
        );
        assert_eq!(result.source_codec, mp3);
    }

    #[test]
    fn each_processing_stage_alone() {
        let cases = [
            (
                ProcessingState {
                    is_software_volume_active: true,
                    ..ProcessingState::default()
                },
                NotBitPerfectReason::SoftwareVolumeActive,
            ),
            (
                ProcessingState {
                    is_resampler_active: true,
                    ..ProcessingState::default()
                },
                NotBitPerfectReason::ResamplerActive,
            ),
            (
                ProcessingState {
                    is_dsp_active: true,
                    ..ProcessingState::default()
                },
                NotBitPerfectReason::DspActive,
            ),
        ];
        for (processing, expected) in cases {
            let result = assess(
                source(96_000, 24),
                exclusive(device(96_000, 3, 24)),
                processing,
            );
            assert_eq!(reasons_of(&result), vec![expected]);
        }
    }

    #[test]
    fn all_failures_combined_are_all_reported_in_order() {
        let mut format = device(48_000, 2, 16);
        format.channel_mask = OTHER_MASK;
        let processing = ProcessingState {
            is_software_volume_active: true,
            is_resampler_active: true,
            is_dsp_active: true,
        };
        let result = assess(source(96_000, 24), exclusive(format), processing);
        assert_eq!(
            reasons_of(&result),
            vec![
                NotBitPerfectReason::SampleRateDiffers {
                    source_hz: 96_000,
                    device_hz: 48_000
                },
                NotBitPerfectReason::ChannelLayoutDiffers {
                    source_mask: STEREO_MASK,
                    device_mask: OTHER_MASK
                },
                NotBitPerfectReason::BitDepthReduced {
                    source_bits: 24,
                    device_valid_bits: 16
                },
                NotBitPerfectReason::SoftwareVolumeActive,
                NotBitPerfectReason::ResamplerActive,
                NotBitPerfectReason::DspActive,
            ]
        );
    }

    #[test]
    fn shared_mode_reports_each_cause_and_no_device_format_reason() {
        for cause in [
            SharedModeCause::UserPreference,
            SharedModeCause::ExclusiveFormatRefused,
            SharedModeCause::ExclusiveModeDisabledBySystem,
            SharedModeCause::DeviceBusyInSharedMode,
        ] {
            // The render format differs from the source in rate and depth on purpose.
            let result = assess(
                source(44_100, 16),
                shared(cause),
                ProcessingState::default(),
            );
            assert_eq!(
                reasons_of(&result),
                vec![NotBitPerfectReason::SharedModeThroughSystemMixer { cause }]
            );
        }
    }

    #[test]
    fn shared_mode_with_processing_keeps_both_kinds_of_reason() {
        let processing = ProcessingState {
            is_dsp_active: true,
            ..ProcessingState::default()
        };
        let result = assess(
            source(96_000, 24),
            shared(SharedModeCause::UserPreference),
            processing,
        );
        assert_eq!(
            reasons_of(&result),
            vec![
                NotBitPerfectReason::SharedModeThroughSystemMixer {
                    cause: SharedModeCause::UserPreference
                },
                NotBitPerfectReason::DspActive,
            ]
        );
    }

    #[test]
    fn shared_mode_matching_formats_is_still_not_bit_perfect() {
        let result = assess(
            source(96_000, 24),
            shared(SharedModeCause::ExclusiveFormatRefused),
            ProcessingState::default(),
        );
        assert_ne!(result.verdict, BitPerfectVerdict::BitPerfect);
    }

    #[test]
    fn shared_mode_without_cause_is_never_bit_perfect() {
        let negotiated = output(
            DeviceAccessMode::SharedThroughSystemMixer,
            None,
            None,
            device(96_000, 4, 24),
        );
        let result = assess(source(96_000, 24), negotiated, ProcessingState::default());
        assert_eq!(
            reasons_of(&result),
            vec![NotBitPerfectReason::SharedModeThroughSystemMixer {
                cause: SharedModeCause::UserPreference
            }]
        );
    }

    #[test]
    fn exclusive_without_device_format_compares_the_render_format() {
        let matching = output(
            DeviceAccessMode::Exclusive,
            None,
            None,
            device(96_000, 3, 24),
        );
        let result = assess(source(96_000, 24), matching, ProcessingState::default());
        assert_eq!(result.verdict, BitPerfectVerdict::BitPerfect);

        let differing = output(
            DeviceAccessMode::Exclusive,
            None,
            None,
            device(48_000, 3, 24),
        );
        let result = assess(source(96_000, 24), differing, ProcessingState::default());
        assert_ne!(result.verdict, BitPerfectVerdict::BitPerfect);
    }

    #[test]
    fn assessment_echoes_its_inputs() {
        let src = source(96_000, 24);
        let negotiated = exclusive(device(96_000, 3, 24));
        let processing = ProcessingState {
            is_dsp_active: true,
            ..ProcessingState::default()
        };
        let result = assess_signal_path(&src, &SourceCodec::FLAC, &negotiated, &processing);
        assert_eq!(result.source_format, src);
        assert_eq!(result.negotiated, negotiated);
        assert_eq!(result.processing, processing);
    }

    #[test]
    fn exact_float_delivery_is_never_bit_perfect() {
        let format = device(96_000, 4, 24);
        let mut negotiated = exclusive(format);
        negotiated.sample_delivery = DeviceSampleDelivery::ExactFloat32Conversion;
        let assessment = assess_signal_path(
            &source(96_000, 24),
            &SourceCodec::FLAC,
            &negotiated,
            &ProcessingState::default(),
        );
        assert_eq!(
            reasons_of(&assessment),
            vec![NotBitPerfectReason::DeliveredAsFloatToPlatform]
        );
    }
}
