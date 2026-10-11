//! Sample formats shared by the decoder, the playback ring and the output backends.
//!
//! Everything here is plain data plus the single place that decides which device formats are
//! lossless for a given source (DRY: backends only translate the candidates).

/// Bit of `WAVEFORMATEXTENSIBLE.dwChannelMask` for front left (Microsoft "Channel mask" docs).
const SPEAKER_FRONT_LEFT: u32 = 0x1;
/// Front right speaker bit.
const SPEAKER_FRONT_RIGHT: u32 = 0x2;
/// Front center speaker bit.
const SPEAKER_FRONT_CENTER: u32 = 0x4;
/// Low-frequency effects speaker bit.
const SPEAKER_LOW_FREQUENCY: u32 = 0x8;
/// Back left speaker bit.
const SPEAKER_BACK_LEFT: u32 = 0x10;
/// Back right speaker bit.
const SPEAKER_BACK_RIGHT: u32 = 0x20;
/// Back center speaker bit.
const SPEAKER_BACK_CENTER: u32 = 0x100;
/// Side left speaker bit.
const SPEAKER_SIDE_LEFT: u32 = 0x200;
/// Side right speaker bit.
const SPEAKER_SIDE_RIGHT: u32 = 0x400;

/// Smallest container, in bytes, the device formats use (16-bit PCM).
const MINIMUM_CONTAINER_BYTES: u8 = 2;
/// Largest container, in bytes, the device formats use (32-bit PCM).
const MAXIMUM_CONTAINER_BYTES: u8 = 4;
/// Lowest source depth the candidate builder accepts.
const MINIMUM_SOURCE_BITS: u8 = 1;
/// Highest source depth the candidate builder accepts (32-bit container).
const MAXIMUM_SOURCE_BITS: u8 = 32;
/// Number of bits in one byte.
const BITS_PER_BYTE: u8 = 8;

/// Speaker-position bitmask, same bit meanings as WAVEFORMATEXTENSIBLE.dwChannelMask
/// (bit 0 = front left, bit 1 = front right, ...). Used on every platform as the neutral
/// representation; backends translate it to their own layout description.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChannelMask(pub u32);

impl ChannelMask {
    /// Default mask for a FLAC stream with `channel_count` channels and no explicit mask,
    /// following the channel order of RFC 9639 section 9.1.3. Returns `None` for 0 channels
    /// and for more than 8, which that table does not define.
    pub fn default_for_flac_channel_count(channel_count: u8) -> Option<ChannelMask> {
        let front_pair = SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT;
        let back_pair = SPEAKER_BACK_LEFT | SPEAKER_BACK_RIGHT;
        let side_pair = SPEAKER_SIDE_LEFT | SPEAKER_SIDE_RIGHT;
        let mask = match channel_count {
            1 => SPEAKER_FRONT_CENTER,
            2 => front_pair,
            3 => front_pair | SPEAKER_FRONT_CENTER,
            4 => front_pair | back_pair,
            5 => front_pair | SPEAKER_FRONT_CENTER | back_pair,
            6 => front_pair | SPEAKER_FRONT_CENTER | SPEAKER_LOW_FREQUENCY | back_pair,
            7 => {
                front_pair
                    | SPEAKER_FRONT_CENTER
                    | SPEAKER_LOW_FREQUENCY
                    | SPEAKER_BACK_CENTER
                    | side_pair
            }
            8 => front_pair | SPEAKER_FRONT_CENTER | SPEAKER_LOW_FREQUENCY | back_pair | side_pair,
            _ => return None,
        };
        Some(ChannelMask(mask))
    }

    /// Number of speaker positions set in the mask.
    pub fn channel_count(self) -> u32 {
        self.0.count_ones()
    }
}

/// Format of the integer samples produced by a decoder (the *source*), as stored in the
/// playback ring: interleaved `i32`, right-justified (the value range is that of
/// `bits_per_sample`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamFormat {
    /// Sample rate in hertz.
    pub sample_rate_hz: u32,
    /// Significant bits per sample in the source, 4..=24 in M1.
    pub bits_per_sample: u8,
    /// Number of interleaved channels.
    pub channel_count: u8,
    /// Speaker layout of those channels.
    pub channel_mask: ChannelMask,
}

/// Where the valid bits sit inside a container wider than them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValidBitsAlignment {
    /// Valid bits in the most significant bits, low bits zero. This is the WAVEFORMATEXTENSIBLE
    /// convention ("left-justified", Microsoft "Extensible Wave-Format Descriptors"), the
    /// CoreAudio kAudioFormatFlagIsAlignedHigh convention, and ALSA's S24_3LE/S32_LE.
    MostSignificant,
    /// Valid bits in the least significant bits, sign-extended. ALSA S24_LE (24 in 32).
    LeastSignificant,
}

/// Exact integer PCM layout the device receives. Floats are deliberately not representable:
/// a float device format can never be part of the bit-perfect path in M1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DeviceSampleFormat {
    /// Sample rate in hertz.
    pub sample_rate_hz: u32,
    /// Container size in bytes per sample: 2, 3 or 4.
    pub container_bytes: u8,
    /// Bits of precision the device is told about (wValidBitsPerSample on Windows).
    pub valid_bits_per_sample: u8,
    /// Position of the valid bits inside the container.
    pub alignment: ValidBitsAlignment,
    /// Number of interleaved channels.
    pub channel_count: u8,
    /// Speaker layout of those channels.
    pub channel_mask: ChannelMask,
}

impl DeviceSampleFormat {
    /// Size of one frame (one sample for every channel) in the device buffer, in bytes.
    pub fn bytes_per_frame(&self) -> usize {
        usize::from(self.container_bytes) * usize::from(self.channel_count)
    }
}

/// Device formats to try, most preferred first, for a source format. All candidates carry
/// the source sample rate and channel layout unchanged and can hold the source bits without
/// loss. Order, per container from the smallest one that fits up to 4 bytes: valid bits =
/// source bits, then valid bits = container size (some drivers only accept that, and it is
/// still lossless because the extra low bits are zero). With most-significant alignment the
/// bytes in the buffer are identical for both valid-bits values, so the order only decides
/// which container wins, and the smaller container moves fewer bytes.
/// Example for 24-bit: 24-in-3 bytes (valid 24), 24-in-4 bytes (valid 24), 4 bytes (valid 32).
/// Example for 16-bit: 2 bytes (16), 3 bytes (valid 16), 3 bytes (valid 24),
/// 4 bytes (valid 16), 4 bytes (valid 32).
///
/// Precondition: `bits_per_sample` is in 1..=32 (the decoder rejects anything else). Outside
/// that range the list is empty rather than containing an invalid candidate.
pub fn lossless_device_format_candidates(
    source_format: &StreamFormat,
    alignment: ValidBitsAlignment,
) -> Vec<DeviceSampleFormat> {
    let source_bits = source_format.bits_per_sample;
    if !(MINIMUM_SOURCE_BITS..=MAXIMUM_SOURCE_BITS).contains(&source_bits) {
        return Vec::new();
    }
    let smallest_fitting_container_bytes = source_bits
        .div_ceil(BITS_PER_BYTE)
        .clamp(MINIMUM_CONTAINER_BYTES, MAXIMUM_CONTAINER_BYTES);

    let build_candidate = |container_bytes: u8, valid_bits_per_sample: u8| DeviceSampleFormat {
        sample_rate_hz: source_format.sample_rate_hz,
        container_bytes,
        valid_bits_per_sample,
        alignment,
        channel_count: source_format.channel_count,
        channel_mask: source_format.channel_mask,
    };

    let mut candidates = Vec::new();
    for container_bytes in smallest_fitting_container_bytes..=MAXIMUM_CONTAINER_BYTES {
        candidates.push(build_candidate(container_bytes, source_bits));
        let container_bits = container_bytes * BITS_PER_BYTE;
        if container_bits != source_bits {
            candidates.push(build_candidate(container_bytes, container_bits));
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_source(bits_per_sample: u8) -> StreamFormat {
        StreamFormat {
            sample_rate_hz: 96_000,
            bits_per_sample,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        }
    }

    fn container_and_valid_bits(candidates: &[DeviceSampleFormat]) -> Vec<(u8, u8)> {
        candidates
            .iter()
            .map(|candidate| (candidate.container_bytes, candidate.valid_bits_per_sample))
            .collect()
    }

    #[test]
    fn candidates_for_16_bit_are_ordered_as_specified() {
        let candidates = lossless_device_format_candidates(
            &stereo_source(16),
            ValidBitsAlignment::MostSignificant,
        );
        assert_eq!(
            container_and_valid_bits(&candidates),
            vec![(2, 16), (3, 16), (3, 24), (4, 16), (4, 32)]
        );
    }

    #[test]
    fn candidates_for_every_supported_depth_are_consistent() {
        for bits_per_sample in 4..=24u8 {
            let candidates = lossless_device_format_candidates(
                &stereo_source(bits_per_sample),
                ValidBitsAlignment::MostSignificant,
            );
            let pairs = container_and_valid_bits(&candidates);
            for (index, pair) in pairs.iter().enumerate() {
                assert!(!pairs[..index].contains(pair), "duplicate {pair:?}");
                assert!(pair.1 >= bits_per_sample);
                assert!(pair.1 <= pair.0 * BITS_PER_BYTE);
                assert!(
                    pairs.contains(&(pair.0, pair.0 * BITS_PER_BYTE)),
                    "{bits_per_sample} bits: container {} lacks its full-size entry",
                    pair.0
                );
            }
        }
    }

    #[test]
    fn candidates_are_empty_outside_one_to_thirty_two_bits() {
        for bits_per_sample in [0, 33, 64] {
            assert!(
                lossless_device_format_candidates(
                    &stereo_source(bits_per_sample),
                    ValidBitsAlignment::MostSignificant
                )
                .is_empty()
            );
        }
    }

    #[test]
    fn candidates_for_20_bit_are_ordered_as_specified() {
        let candidates = lossless_device_format_candidates(
            &stereo_source(20),
            ValidBitsAlignment::MostSignificant,
        );
        assert_eq!(
            container_and_valid_bits(&candidates),
            vec![(3, 20), (3, 24), (4, 20), (4, 32)]
        );
    }

    #[test]
    fn candidates_for_24_bit_are_ordered_as_specified() {
        let candidates = lossless_device_format_candidates(
            &stereo_source(24),
            ValidBitsAlignment::MostSignificant,
        );
        assert_eq!(
            container_and_valid_bits(&candidates),
            vec![(3, 24), (4, 24), (4, 32)]
        );
    }

    #[test]
    fn every_candidate_keeps_source_rate_mask_channels_and_alignment() {
        for bits_per_sample in [16, 20, 24] {
            let source = stereo_source(bits_per_sample);
            let candidates =
                lossless_device_format_candidates(&source, ValidBitsAlignment::LeastSignificant);
            assert!(!candidates.is_empty());
            for candidate in candidates {
                assert_eq!(candidate.sample_rate_hz, source.sample_rate_hz);
                assert_eq!(candidate.channel_mask, source.channel_mask);
                assert_eq!(candidate.channel_count, source.channel_count);
                assert_eq!(candidate.alignment, ValidBitsAlignment::LeastSignificant);
                assert!(candidate.valid_bits_per_sample >= bits_per_sample);
                assert!(
                    candidate.container_bytes * BITS_PER_BYTE >= candidate.valid_bits_per_sample
                );
            }
        }
    }

    #[test]
    fn bytes_per_frame_is_container_times_channels() {
        let format = DeviceSampleFormat {
            sample_rate_hz: 44_100,
            container_bytes: 3,
            valid_bits_per_sample: 24,
            alignment: ValidBitsAlignment::MostSignificant,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        };
        assert_eq!(format.bytes_per_frame(), 6);
        let surround = DeviceSampleFormat {
            container_bytes: 4,
            channel_count: 8,
            ..format
        };
        assert_eq!(surround.bytes_per_frame(), 32);
    }

    #[test]
    fn default_flac_channel_masks_match_rfc_9639_section_9_1_3() {
        // Expected values written out by hand from the RFC table and the WAVEFORMATEXTENSIBLE
        // bit assignments (FL 0x1, FR 0x2, FC 0x4, LFE 0x8, BL 0x10, BR 0x20, BC 0x100,
        // SL 0x200, SR 0x400).
        let expected_masks: [(u8, u32); 8] = [
            (1, 0x004),
            (2, 0x003),
            (3, 0x007),
            (4, 0x033),
            (5, 0x037),
            (6, 0x03F),
            (7, 0x70F),
            (8, 0x63F),
        ];
        for (channel_count, expected_mask) in expected_masks {
            let mask = ChannelMask::default_for_flac_channel_count(channel_count);
            assert_eq!(
                mask,
                Some(ChannelMask(expected_mask)),
                "{channel_count} channels"
            );
            assert_eq!(
                mask.map(ChannelMask::channel_count),
                Some(u32::from(channel_count))
            );
        }
    }

    #[test]
    fn default_flac_channel_mask_is_undefined_outside_one_to_eight() {
        assert_eq!(ChannelMask::default_for_flac_channel_count(0), None);
        assert_eq!(ChannelMask::default_for_flac_channel_count(9), None);
    }
}
