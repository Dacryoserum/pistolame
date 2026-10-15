//! The integer sample layouts the ALSA backend uses, and the order it tries them in.
//!
//! Platform-neutral on purpose: the mapping decides what reaches the DAC, so it is tested on
//! every platform, not only on Linux. The ALSA backend (`alsa_backend/`) turns each layout into a
//! `snd_pcm_format_t`.
//!
//! Source: ALSA PCM formats, <https://www.alsa-project.org/alsa-doc/alsa-lib/group___p_c_m.html>
//! (`SND_PCM_FORMAT_S16_LE`, `S24_3LE`: 24 bits in 3 bytes, `S24_LE`: 24 bits in the low bits of
//! 4 bytes, `S32_LE`).

// Only the Linux backend uses this module outside tests.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use crate::{
    DeviceSampleFormat, StreamFormat, ValidBitsAlignment, lossless_device_format_candidates,
};

/// Valid bits of ALSA's `S24_LE` (24 bits in a 4-byte container, low bits).
const S24_IN_32_VALID_BITS: u8 = 24;
/// Container size of `S24_LE`, in bytes.
const S24_IN_32_CONTAINER_BYTES: u8 = 4;

/// An ALSA little-endian signed integer layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AlsaSampleLayout {
    /// `S16_LE`: 16 bits in 2 bytes.
    Signed16,
    /// `S24_3LE`: 24 bits packed in 3 bytes.
    Signed24Packed,
    /// `S24_LE`: 24 bits in the low bits of 4 bytes, sign-extended.
    Signed24InLow32,
    /// `S32_LE`: 32 bits in 4 bytes.
    Signed32,
}

impl AlsaSampleLayout {
    /// The ALSA layout that carries `format`'s bytes unchanged, if any. Most-significant
    /// alignment fills the whole container (the bits below the source are zero), so only the
    /// container size matters; least-significant alignment is ALSA's `S24_LE` only.
    pub(crate) fn of(format: &DeviceSampleFormat) -> Option<AlsaSampleLayout> {
        match (format.alignment, format.container_bytes) {
            (ValidBitsAlignment::MostSignificant, 2) => Some(AlsaSampleLayout::Signed16),
            (ValidBitsAlignment::MostSignificant, 3) => Some(AlsaSampleLayout::Signed24Packed),
            (ValidBitsAlignment::MostSignificant, 4) => Some(AlsaSampleLayout::Signed32),
            (ValidBitsAlignment::LeastSignificant, S24_IN_32_CONTAINER_BYTES)
                if format.valid_bits_per_sample == S24_IN_32_VALID_BITS =>
            {
                Some(AlsaSampleLayout::Signed24InLow32)
            }
            _ => None,
        }
    }

    /// ALSA's name for the layout, as `aplay --dump-hw-params` prints it (tests read better
    /// with it).
    #[cfg(test)]
    pub(crate) fn alsa_name(self) -> &'static str {
        match self {
            AlsaSampleLayout::Signed16 => "S16_LE",
            AlsaSampleLayout::Signed24Packed => "S24_3LE",
            AlsaSampleLayout::Signed24InLow32 => "S24_LE",
            AlsaSampleLayout::Signed32 => "S32_LE",
        }
    }
}

/// Device formats to try for `source`, most preferred first, each with its ALSA layout. They
/// all carry the source samples without loss. The smallest container comes first (fewer bytes
/// to move); two candidates the device sees as the same ALSA format are tried once. `S24_LE`,
/// which some USB DACs only offer for 24-bit, comes last and only when the stream may carry it
/// (`may_use_low_aligned_24`: the shared-mode volume only scales high-aligned samples).
pub(crate) fn alsa_candidates(
    source: &StreamFormat,
    may_use_low_aligned_24: bool,
) -> Vec<(DeviceSampleFormat, AlsaSampleLayout)> {
    let mut candidates: Vec<(DeviceSampleFormat, AlsaSampleLayout)> = Vec::new();
    for format in lossless_device_format_candidates(source, ValidBitsAlignment::MostSignificant) {
        if let Some(layout) = AlsaSampleLayout::of(&format)
            && !candidates.iter().any(|(_, known)| *known == layout)
        {
            candidates.push((format, layout));
        }
    }
    if may_use_low_aligned_24 && source.bits_per_sample <= S24_IN_32_VALID_BITS {
        let format = DeviceSampleFormat {
            sample_rate_hz: source.sample_rate_hz,
            container_bytes: S24_IN_32_CONTAINER_BYTES,
            valid_bits_per_sample: S24_IN_32_VALID_BITS,
            alignment: ValidBitsAlignment::LeastSignificant,
            channel_count: source.channel_count,
            channel_mask: source.channel_mask,
        };
        candidates.push((format, AlsaSampleLayout::Signed24InLow32));
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChannelMask;

    fn stereo(bits_per_sample: u8) -> StreamFormat {
        StreamFormat {
            sample_rate_hz: 96_000,
            bits_per_sample,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        }
    }

    fn layouts(candidates: &[(DeviceSampleFormat, AlsaSampleLayout)]) -> Vec<&'static str> {
        candidates
            .iter()
            .map(|(_, layout)| layout.alsa_name())
            .collect()
    }

    #[test]
    fn sixteen_bit_tries_each_alsa_format_once_smallest_first() {
        assert_eq!(
            layouts(&alsa_candidates(&stereo(16), true)),
            ["S16_LE", "S24_3LE", "S32_LE", "S24_LE"]
        );
    }

    #[test]
    fn twenty_four_bit_never_offers_sixteen() {
        assert_eq!(
            layouts(&alsa_candidates(&stereo(24), true)),
            ["S24_3LE", "S32_LE", "S24_LE"]
        );
    }

    #[test]
    fn shared_mode_leaves_out_the_low_aligned_layout() {
        assert_eq!(
            layouts(&alsa_candidates(&stereo(24), false)),
            ["S24_3LE", "S32_LE"]
        );
    }

    #[test]
    fn the_first_candidate_of_each_layout_keeps_the_source_bits() {
        for (format, _) in alsa_candidates(&stereo(16), true) {
            let expected_valid_bits = if format.alignment == ValidBitsAlignment::LeastSignificant {
                24
            } else {
                16
            };
            assert_eq!(format.valid_bits_per_sample, expected_valid_bits);
            assert_eq!(format.sample_rate_hz, 96_000);
            assert_eq!(format.channel_count, 2);
        }
    }

    #[test]
    fn layouts_follow_the_container() {
        let format = |container_bytes, valid_bits, alignment| DeviceSampleFormat {
            sample_rate_hz: 44_100,
            container_bytes,
            valid_bits_per_sample: valid_bits,
            alignment,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        };
        use ValidBitsAlignment::{LeastSignificant, MostSignificant};
        assert_eq!(
            AlsaSampleLayout::of(&format(4, 24, MostSignificant)),
            Some(AlsaSampleLayout::Signed32)
        );
        assert_eq!(
            AlsaSampleLayout::of(&format(4, 24, LeastSignificant)),
            Some(AlsaSampleLayout::Signed24InLow32)
        );
        // 32 valid bits in the low bits is S32_LE written another way: not offered as S24_LE.
        assert_eq!(AlsaSampleLayout::of(&format(4, 32, LeastSignificant)), None);
        assert_eq!(AlsaSampleLayout::of(&format(3, 24, LeastSignificant)), None);
    }
}
