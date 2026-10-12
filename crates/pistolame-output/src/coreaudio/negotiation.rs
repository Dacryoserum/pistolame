//! Which render format a CoreAudio stream uses, decided from the device's capabilities. Pure
//! logic: the tests need no hardware (plan coreaudio.md §0).
//!
//! Both modes render the same way: 32-bit containers, valid bits = source bits,
//! most-significant aligned, converted exactly to float32 by the backend. Exclusive mode also
//! needs the device to run at the source rate with the source channel count, so the platform
//! does no resampling and no channel mapping.

use crate::float_conversion::is_exactly_convertible_to_float32;
use crate::{
    DeviceAccessMode, DeviceSampleFormat, FormatProbeOutcome, FormatProbeResult, StreamFormat,
    ValidBitsAlignment, lossless_device_format_candidates,
};

/// What the device offers for exclusive playback.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExclusiveDeviceCapabilities {
    /// Ranges from `kAudioDevicePropertyAvailableNominalSampleRates`, as (minimum, maximum) Hz.
    pub(crate) nominal_sample_rate_ranges_hz: Vec<(f64, f64)>,
    /// Output channels of the device.
    pub(crate) output_channel_count: u32,
}

impl ExclusiveDeviceCapabilities {
    fn accepts(&self, candidate: &DeviceSampleFormat) -> bool {
        let sample_rate_hz = f64::from(candidate.sample_rate_hz);
        is_exactly_convertible_to_float32(candidate)
            && u32::from(candidate.channel_count) == self.output_channel_count
            && self
                .nominal_sample_rate_ranges_hz
                .iter()
                .any(|(minimum, maximum)| (*minimum..=*maximum).contains(&sample_rate_hz))
    }
}

/// The negotiated render format and the answer for every candidate tried.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NegotiationResult {
    /// The first accepted candidate, if any.
    pub(crate) accepted: Option<DeviceSampleFormat>,
    /// Every candidate tried and its answer, in order.
    pub(crate) log: Vec<FormatProbeResult>,
}

/// Tries the lossless candidates, most preferred first, against the device's exclusive
/// capabilities.
pub(crate) fn negotiate_exclusive_format(
    source_format: &StreamFormat,
    capabilities: &ExclusiveDeviceCapabilities,
) -> NegotiationResult {
    let mut log = Vec::new();
    for candidate in candidates(source_format) {
        let is_accepted = capabilities.accepts(&candidate);
        log.push(FormatProbeResult {
            candidate,
            access_mode: DeviceAccessMode::Exclusive,
            outcome: if is_accepted {
                FormatProbeOutcome::Accepted
            } else {
                FormatProbeOutcome::Refused
            },
        });
        if is_accepted {
            return NegotiationResult {
                accepted: Some(candidate),
                log,
            };
        }
    }
    NegotiationResult {
        accepted: None,
        log,
    }
}

/// The render format for shared mode: the first candidate the exact float conversion
/// supports. The system mixer converts the rate and the channels.
pub(crate) fn shared_render_format(source_format: &StreamFormat) -> Option<DeviceSampleFormat> {
    candidates(source_format).find(is_exactly_convertible_to_float32)
}

fn candidates(source_format: &StreamFormat) -> impl Iterator<Item = DeviceSampleFormat> {
    lossless_device_format_candidates(source_format, ValidBitsAlignment::MostSignificant)
        .into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChannelMask;

    fn stereo_source(sample_rate_hz: u32, bits_per_sample: u8) -> StreamFormat {
        StreamFormat {
            sample_rate_hz,
            bits_per_sample,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        }
    }

    /// The MacBook Air speakers and headphone jack measured on 2026-10-12.
    fn macbook_air_output() -> ExclusiveDeviceCapabilities {
        ExclusiveDeviceCapabilities {
            nominal_sample_rate_ranges_hz: vec![
                (44_100.0, 44_100.0),
                (48_000.0, 48_000.0),
                (88_200.0, 88_200.0),
                (96_000.0, 96_000.0),
            ],
            output_channel_count: 2,
        }
    }

    #[test]
    fn a_24_bit_96_khz_source_gets_a_32_bit_container_at_96_khz() {
        let result = negotiate_exclusive_format(&stereo_source(96_000, 24), &macbook_air_output());
        let accepted = result.accepted.expect("96 kHz is available");
        assert_eq!(
            (
                accepted.sample_rate_hz,
                accepted.container_bytes,
                accepted.valid_bits_per_sample
            ),
            (96_000, 4, 24)
        );
        // 24-in-3 bytes is refused first: only 32-bit containers convert exactly.
        assert_eq!(result.log.len(), 2);
        assert_eq!(result.log[0].outcome, FormatProbeOutcome::Refused);
        assert_eq!(result.log[1].outcome, FormatProbeOutcome::Accepted);
    }

    #[test]
    fn a_16_bit_source_keeps_16_valid_bits_in_32() {
        let result = negotiate_exclusive_format(&stereo_source(44_100, 16), &macbook_air_output());
        let accepted = result.accepted.expect("44.1 kHz is available");
        assert_eq!(
            (accepted.container_bytes, accepted.valid_bits_per_sample),
            (4, 16)
        );
    }

    #[test]
    fn a_rate_the_device_lacks_is_refused_with_every_candidate_logged() {
        let result = negotiate_exclusive_format(&stereo_source(192_000, 24), &macbook_air_output());
        assert_eq!(result.accepted, None);
        assert_eq!(result.log.len(), 3);
        assert!(
            result
                .log
                .iter()
                .all(|probe| probe.outcome == FormatProbeOutcome::Refused)
        );
    }

    #[test]
    fn a_channel_count_the_device_lacks_is_refused() {
        let mono = StreamFormat {
            channel_count: 1,
            channel_mask: ChannelMask(0x4),
            ..stereo_source(48_000, 24)
        };
        assert_eq!(
            negotiate_exclusive_format(&mono, &macbook_air_output()).accepted,
            None
        );
    }

    #[test]
    fn a_continuous_rate_range_accepts_rates_inside_it() {
        let capabilities = ExclusiveDeviceCapabilities {
            nominal_sample_rate_ranges_hz: vec![(8_000.0, 192_000.0)],
            output_channel_count: 2,
        };
        assert!(
            negotiate_exclusive_format(&stereo_source(88_200, 24), &capabilities)
                .accepted
                .is_some()
        );
    }

    #[test]
    fn shared_mode_renders_32_bit_containers_and_refuses_32_bit_sources() {
        let format = shared_render_format(&stereo_source(48_000, 24)).expect("24 bits fit");
        assert_eq!(
            (format.container_bytes, format.valid_bits_per_sample),
            (4, 24)
        );
        assert_eq!(shared_render_format(&stereo_source(48_000, 32)), None);
    }
}
