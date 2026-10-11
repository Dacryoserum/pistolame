//! Integer PCM packing between the decoder's right-justified `i32` samples and the byte layouts
//! the audio device and the MD5 verification expect.
//!
//! This code sits on the bit-perfect path: it must stay integer-only (hence the lint below),
//! must never panic in release builds, and its tests compare exact bytes.
//!
//! Call [`validate_pcm_layout`] once at stream (or segment) start. The packing functions then
//! assume a valid layout; on an invalid one they write nothing rather than panic.

#![deny(clippy::float_arithmetic)]

use crate::format::{DeviceSampleFormat, ValidBitsAlignment};

/// Number of bits in a byte.
const BITS_PER_BYTE: u32 = 8;
/// Number of bits in the `i32` working word.
const WORD_BITS: u32 = i32::BITS;
/// Number of bytes in the `i32` working word.
const WORD_BYTES: usize = 4;

/// Why a (source bits, device format) pair cannot be packed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PcmLayoutError {
    /// The container must be 1 to 4 bytes.
    #[error("container size of {container_bytes} bytes is outside 1..=4")]
    ContainerBytesOutOfRange {
        /// Offending container size in bytes.
        container_bytes: u8,
    },
    /// Valid bits must fit in the container.
    #[error("{valid_bits} valid bits do not fit a container of {container_bytes} bytes")]
    ValidBitsExceedContainer {
        /// Valid bits announced to the device.
        valid_bits: u8,
        /// Container size in bytes.
        container_bytes: u8,
    },
    /// Source bits must be at least 1 and at most the valid bits.
    #[error("{source_bits} source bits are outside 1..={valid_bits} (the valid bits)")]
    SourceBitsOutOfRange {
        /// Significant bits of the decoded samples.
        source_bits: u8,
        /// Valid bits announced to the device.
        valid_bits: u8,
    },
}

/// Why container bytes could not be turned back into samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UnpackError {
    /// The layout itself is invalid (see [`validate_pcm_layout`]).
    #[error("invalid PCM layout: {0}")]
    InvalidLayout(#[from] PcmLayoutError),
    /// `bytes` is not exactly `destination.len() * container_bytes` long. Nothing was written.
    #[error("expected {expected_bytes} container bytes, got {actual_bytes}")]
    LengthMismatch {
        /// Byte count implied by the destination length and the container size.
        expected_bytes: usize,
        /// Byte count actually supplied.
        actual_bytes: usize,
    },
    /// A container breaks the layout: padding bits are not zero (most-significant alignment,
    /// or the low bits below the source precision in least-significant alignment), or the bits
    /// above the valid bits are not a pure sign extension (least-significant alignment).
    #[error("sample {sample_index} has invalid padding or sign-extension bits in its container")]
    InvalidPadding {
        /// Index of the first offending sample, counted in interleaved samples (not frames):
        /// divide by `channel_count` to get a frame index.
        sample_index: usize,
    },
}

/// Why samples could not be written in the canonical MD5 layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CanonicalMd5Error {
    /// Source bits must be in 1..=32.
    #[error("{source_bits} source bits are outside 1..=32")]
    SourceBitsOutOfRange {
        /// Offending bit depth.
        source_bits: u8,
    },
    /// `destination` is not exactly `samples.len() * ceil(source_bits / 8)` long.
    #[error("expected {expected_bytes} destination bytes, got {actual_bytes}")]
    LengthMismatch {
        /// Byte count implied by the sample count and bit depth.
        expected_bytes: usize,
        /// Byte count actually supplied.
        actual_bytes: usize,
    },
    /// A sample does not fit in `source_bits` signed bits; hashing it would hide a decoder bug.
    #[error("sample {sample_index} does not fit in the source bit depth")]
    SampleOutOfRange {
        /// Index of the first offending sample, counted in interleaved samples (not frames):
        /// divide by `channel_count` to get a frame index.
        sample_index: usize,
    },
}

/// Checks that `source_bits` samples can be carried by `format`:
/// `1 <= source_bits <= valid_bits <= container_bytes * 8 <= 32` and `container_bytes >= 1`.
///
/// Call it once at stream or segment start; [`pack_samples_into_container`] and
/// [`unpack_samples_from_container`] require a layout that passed.
pub fn validate_pcm_layout(
    source_bits: u8,
    format: &DeviceSampleFormat,
) -> Result<(), PcmLayoutError> {
    let container_bytes = format.container_bytes;
    let valid_bits = format.valid_bits_per_sample;
    if !(1..=WORD_BYTES as u8).contains(&container_bytes) {
        return Err(PcmLayoutError::ContainerBytesOutOfRange { container_bytes });
    }
    if u32::from(valid_bits) > u32::from(container_bytes) * BITS_PER_BYTE {
        return Err(PcmLayoutError::ValidBitsExceedContainer {
            valid_bits,
            container_bytes,
        });
    }
    if source_bits == 0 || source_bits > valid_bits {
        return Err(PcmLayoutError::SourceBitsOutOfRange {
            source_bits,
            valid_bits,
        });
    }
    Ok(())
}

/// Whether `sample` fits in `bits` signed bits. `bits` must be in 1..=32.
fn fits_in_signed_bits(sample: i32, bits: u32) -> bool {
    let shift = WORD_BITS - bits;
    sample.wrapping_shl(shift) >> shift == sample
}

/// Writes `samples` (right-justified, `source_bits` significant bits) into `destination`
/// in `format`'s container layout, little-endian. Integer only; no allocation.
///
/// - `MostSignificant`: each sample is shifted to the top of a 32-bit word and the
///   `container_bytes` most significant bytes are written, so every bit below `source_bits` is
///   zero (the WAVEFORMATEXTENSIBLE convention).
/// - `LeastSignificant`: the `valid_bits_per_sample` valid bits sit in the low bits of the
///   container, so the sample is shifted left by `valid_bits - source_bits` and sign-extended
///   over the container (ALSA S24_LE).
///
/// Preconditions: [`validate_pcm_layout`] succeeded for these arguments, every sample fits in
/// `source_bits` signed bits (debug-asserted; out-of-range values are unspecified garbage but
/// never panic in release), and `destination.len() == samples.len() * container_bytes`.
/// If the layout or the length is wrong, nothing is written.
pub fn pack_samples_into_container(
    samples: &[i32],
    source_bits: u8,
    format: &DeviceSampleFormat,
    destination: &mut [u8],
) {
    if validate_pcm_layout(source_bits, format).is_err() {
        return;
    }
    let container_bytes = usize::from(format.container_bytes);
    if destination.len() != samples.len() * container_bytes {
        return;
    }
    let left_justify_shift = WORD_BITS - u32::from(source_bits);
    let valid_bits_shift = u32::from(format.valid_bits_per_sample) - u32::from(source_bits);
    for (sample, slot) in samples
        .iter()
        .zip(destination.chunks_exact_mut(container_bytes))
    {
        debug_assert!(
            fits_in_signed_bits(*sample, u32::from(source_bits)),
            "sample {sample} does not fit in {source_bits} bits"
        );
        match format.alignment {
            ValidBitsAlignment::MostSignificant => {
                let left_justified = sample.wrapping_shl(left_justify_shift).to_le_bytes();
                slot.copy_from_slice(&left_justified[WORD_BYTES - container_bytes..]);
            }
            ValidBitsAlignment::LeastSignificant => {
                let in_valid_bits = sample.wrapping_shl(valid_bits_shift).to_le_bytes();
                slot.copy_from_slice(&in_valid_bits[..container_bytes]);
            }
        }
    }
}

/// Inverse: reads container bytes back to right-justified samples of `source_bits`.
///
/// Fails, before writing anything, if the layout is invalid or `bytes` is not exactly
/// `destination.len() * container_bytes` long. Otherwise fails at the first container that
/// breaks the layout (see [`UnpackError::InvalidPadding`]); samples before it are already
/// written to `destination`.
///
/// Precondition: [`validate_pcm_layout`] succeeded for these arguments (checked again here, so
/// a violation is an error, not a panic).
pub fn unpack_samples_from_container(
    bytes: &[u8],
    source_bits: u8,
    format: &DeviceSampleFormat,
    destination: &mut [i32],
) -> Result<(), UnpackError> {
    validate_pcm_layout(source_bits, format)?;
    let container_bytes = usize::from(format.container_bytes);
    let expected_bytes = destination.len() * container_bytes;
    if bytes.len() != expected_bytes {
        return Err(UnpackError::LengthMismatch {
            expected_bytes,
            actual_bytes: bytes.len(),
        });
    }
    let left_justify_shift = WORD_BITS - u32::from(source_bits);
    let valid_bits = u32::from(format.valid_bits_per_sample);
    let valid_bits_shift = valid_bits - u32::from(source_bits);
    let container_shift = WORD_BITS - u32::from(format.container_bytes) * BITS_PER_BYTE;
    // Bits that must be zero: below the source precision, in the left-justified word (MS) or
    // below the source precision within the valid bits (LS).
    let low_padding_mask = |shift: u32| 1u32.wrapping_shl(shift).wrapping_sub(1);
    for (sample_index, (slot, chunk)) in destination
        .iter_mut()
        .zip(bytes.chunks_exact(container_bytes))
        .enumerate()
    {
        let mut word_bytes = [0u8; WORD_BYTES];
        *slot = match format.alignment {
            ValidBitsAlignment::MostSignificant => {
                word_bytes[WORD_BYTES - container_bytes..].copy_from_slice(chunk);
                let left_justified = i32::from_le_bytes(word_bytes);
                if left_justified as u32 & low_padding_mask(left_justify_shift) != 0 {
                    return Err(UnpackError::InvalidPadding { sample_index });
                }
                left_justified >> left_justify_shift
            }
            ValidBitsAlignment::LeastSignificant => {
                word_bytes[..container_bytes].copy_from_slice(chunk);
                let in_valid_bits =
                    i32::from_le_bytes(word_bytes).wrapping_shl(container_shift) >> container_shift;
                let has_clean_low_bits =
                    in_valid_bits as u32 & low_padding_mask(valid_bits_shift) == 0;
                if !has_clean_low_bits || !fits_in_signed_bits(in_valid_bits, valid_bits) {
                    return Err(UnpackError::InvalidPadding { sample_index });
                }
                in_valid_bits >> valid_bits_shift
            }
        };
    }
    Ok(())
}

/// Writes samples in the FLAC STREAMINFO MD5 canonical layout (RFC 9639 section 8.2):
/// interleaved, signed, little-endian, each sample sign-extended to ceil(bits/8) bytes.
///
/// RFC 9639 section 8.2 states that the MD5 message "consists of all the samples of all
/// channels interleaved, represented in signed, little-endian form" and that, when the bit depth
/// is not a whole number of bytes, "the value of each sample is sign-extended to the next whole
/// number of bytes". Samples are right-justified, so the low `ceil(source_bits / 8)` bytes of
/// the `i32` are exactly that representation.
///
/// Runs on the decode thread, so it reports problems instead of hashing wrong data: it fails if
/// `source_bits` is not in 1..=32, if `destination.len() != samples.len() * ceil(source_bits / 8)`
/// (nothing is written), or at the first sample that does not fit in `source_bits` signed bits
/// (earlier samples are written; the destination must then be discarded). Never panics.
pub fn write_canonical_md5_bytes(
    samples: &[i32],
    source_bits: u8,
    destination: &mut [u8],
) -> Result<(), CanonicalMd5Error> {
    if !(1..=WORD_BITS as u8).contains(&source_bits) {
        return Err(CanonicalMd5Error::SourceBitsOutOfRange { source_bits });
    }
    let bytes_per_sample = usize::from(source_bits).div_ceil(BITS_PER_BYTE as usize);
    let expected_bytes = samples.len() * bytes_per_sample;
    if destination.len() != expected_bytes {
        return Err(CanonicalMd5Error::LengthMismatch {
            expected_bytes,
            actual_bytes: destination.len(),
        });
    }
    for (sample_index, (sample, slot)) in samples
        .iter()
        .zip(destination.chunks_exact_mut(bytes_per_sample))
        .enumerate()
    {
        if !fits_in_signed_bits(*sample, u32::from(source_bits)) {
            return Err(CanonicalMd5Error::SampleOutOfRange { sample_index });
        }
        slot.copy_from_slice(&sample.to_le_bytes()[..bytes_per_sample]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{ChannelMask, StreamFormat, lossless_device_format_candidates};

    /// Mono layout (front center); the mask is irrelevant to packing.
    const FRONT_CENTER_MASK: u32 = 0x4;

    fn stream_format(bits_per_sample: u8) -> StreamFormat {
        StreamFormat {
            sample_rate_hz: 44_100,
            bits_per_sample,
            channel_count: 1,
            channel_mask: ChannelMask(FRONT_CENTER_MASK),
        }
    }

    fn device_format(
        container_bytes: u8,
        valid_bits_per_sample: u8,
        alignment: ValidBitsAlignment,
    ) -> DeviceSampleFormat {
        DeviceSampleFormat {
            sample_rate_hz: 44_100,
            container_bytes,
            valid_bits_per_sample,
            alignment,
            channel_count: 1,
            channel_mask: ChannelMask(FRONT_CENTER_MASK),
        }
    }

    fn full_container_format(
        container_bytes: u8,
        alignment: ValidBitsAlignment,
    ) -> DeviceSampleFormat {
        let valid_bits = u32::from(container_bytes) * BITS_PER_BYTE;
        device_format(container_bytes, valid_bits as u8, alignment)
    }

    fn pack(samples: &[i32], bits: u8, format: &DeviceSampleFormat) -> Vec<u8> {
        let mut out = vec![0u8; samples.len() * usize::from(format.container_bytes)];
        pack_samples_into_container(samples, bits, format, &mut out);
        out
    }

    fn concat(rows: &[&[u8]]) -> Vec<u8> {
        rows.concat()
    }

    /// Hand-written expected bytes for `[min, -1, 0, 1, max]`, most-significant alignment,
    /// keyed by (source bits, container bytes).
    fn expected_most_significant(bits: u8, container_bytes: u8) -> Vec<u8> {
        match (bits, container_bytes) {
            (4, 2) => concat(&[
                &[0x00, 0x80],
                &[0x00, 0xF0],
                &[0x00, 0x00],
                &[0x00, 0x10],
                &[0x00, 0x70],
            ]),
            (4, 3) => concat(&[
                &[0x00, 0x00, 0x80],
                &[0x00, 0x00, 0xF0],
                &[0x00, 0x00, 0x00],
                &[0x00, 0x00, 0x10],
                &[0x00, 0x00, 0x70],
            ]),
            (4, 4) => concat(&[
                &[0x00, 0x00, 0x00, 0x80],
                &[0x00, 0x00, 0x00, 0xF0],
                &[0x00, 0x00, 0x00, 0x00],
                &[0x00, 0x00, 0x00, 0x10],
                &[0x00, 0x00, 0x00, 0x70],
            ]),
            (8, 2) => concat(&[
                &[0x00, 0x80],
                &[0x00, 0xFF],
                &[0x00, 0x00],
                &[0x00, 0x01],
                &[0x00, 0x7F],
            ]),
            (8, 3) => concat(&[
                &[0x00, 0x00, 0x80],
                &[0x00, 0x00, 0xFF],
                &[0x00, 0x00, 0x00],
                &[0x00, 0x00, 0x01],
                &[0x00, 0x00, 0x7F],
            ]),
            (8, 4) => concat(&[
                &[0x00, 0x00, 0x00, 0x80],
                &[0x00, 0x00, 0x00, 0xFF],
                &[0x00, 0x00, 0x00, 0x00],
                &[0x00, 0x00, 0x00, 0x01],
                &[0x00, 0x00, 0x00, 0x7F],
            ]),
            (12, 2) => concat(&[
                &[0x00, 0x80],
                &[0xF0, 0xFF],
                &[0x00, 0x00],
                &[0x10, 0x00],
                &[0xF0, 0x7F],
            ]),
            (12, 3) => concat(&[
                &[0x00, 0x00, 0x80],
                &[0x00, 0xF0, 0xFF],
                &[0x00, 0x00, 0x00],
                &[0x00, 0x10, 0x00],
                &[0x00, 0xF0, 0x7F],
            ]),
            (12, 4) => concat(&[
                &[0x00, 0x00, 0x00, 0x80],
                &[0x00, 0x00, 0xF0, 0xFF],
                &[0x00, 0x00, 0x00, 0x00],
                &[0x00, 0x00, 0x10, 0x00],
                &[0x00, 0x00, 0xF0, 0x7F],
            ]),
            (16, 2) => concat(&[
                &[0x00, 0x80],
                &[0xFF, 0xFF],
                &[0x00, 0x00],
                &[0x01, 0x00],
                &[0xFF, 0x7F],
            ]),
            (16, 3) => concat(&[
                &[0x00, 0x00, 0x80],
                &[0x00, 0xFF, 0xFF],
                &[0x00, 0x00, 0x00],
                &[0x00, 0x01, 0x00],
                &[0x00, 0xFF, 0x7F],
            ]),
            (16, 4) => concat(&[
                &[0x00, 0x00, 0x00, 0x80],
                &[0x00, 0x00, 0xFF, 0xFF],
                &[0x00, 0x00, 0x00, 0x00],
                &[0x00, 0x00, 0x01, 0x00],
                &[0x00, 0x00, 0xFF, 0x7F],
            ]),
            (20, 3) => concat(&[
                &[0x00, 0x00, 0x80],
                &[0xF0, 0xFF, 0xFF],
                &[0x00, 0x00, 0x00],
                &[0x10, 0x00, 0x00],
                &[0xF0, 0xFF, 0x7F],
            ]),
            (20, 4) => concat(&[
                &[0x00, 0x00, 0x00, 0x80],
                &[0x00, 0xF0, 0xFF, 0xFF],
                &[0x00, 0x00, 0x00, 0x00],
                &[0x00, 0x10, 0x00, 0x00],
                &[0x00, 0xF0, 0xFF, 0x7F],
            ]),
            (24, 3) => concat(&[
                &[0x00, 0x00, 0x80],
                &[0xFF, 0xFF, 0xFF],
                &[0x00, 0x00, 0x00],
                &[0x01, 0x00, 0x00],
                &[0xFF, 0xFF, 0x7F],
            ]),
            (24, 4) => concat(&[
                &[0x00, 0x00, 0x00, 0x80],
                &[0x00, 0xFF, 0xFF, 0xFF],
                &[0x00, 0x00, 0x00, 0x00],
                &[0x00, 0x01, 0x00, 0x00],
                &[0x00, 0xFF, 0xFF, 0x7F],
            ]),
            other => panic!("no expectation for {other:?}"),
        }
    }

    fn extreme_samples(bits: u8) -> [i32; 5] {
        [-(1 << (bits - 1)), -1, 0, 1, (1 << (bits - 1)) - 1]
    }

    #[test]
    fn most_significant_packing_matches_literal_bytes_for_every_candidate() {
        for bits in [4u8, 8, 12, 16, 20, 24] {
            let candidates = lossless_device_format_candidates(
                &stream_format(bits),
                ValidBitsAlignment::MostSignificant,
            );
            assert!(!candidates.is_empty());
            for candidate in candidates {
                let packed = pack(&extreme_samples(bits), bits, &candidate);
                assert_eq!(
                    packed,
                    expected_most_significant(bits, candidate.container_bytes),
                    "bits {bits}, candidate {candidate:?}"
                );
            }
        }
    }

    #[test]
    fn documented_examples() {
        let in_four = full_container_format(4, ValidBitsAlignment::MostSignificant);
        assert_eq!(pack(&[-1], 24, &in_four), [0x00, 0xFF, 0xFF, 0xFF]);
        let in_three = full_container_format(3, ValidBitsAlignment::MostSignificant);
        assert_eq!(pack(&[0x1234], 16, &in_three), [0x00, 0x34, 0x12]);
        let in_two = full_container_format(2, ValidBitsAlignment::MostSignificant);
        assert_eq!(pack(&[-1], 12, &in_two), [0xF0, 0xFF]);
    }

    #[test]
    fn least_significant_packing_places_valid_bits_in_the_low_bits() {
        let cases: [(u8, u8, u8, Vec<u8>); 4] = [
            (
                24,
                4,
                24,
                concat(&[
                    &[0x00, 0x00, 0x80, 0xFF],
                    &[0xFF, 0xFF, 0xFF, 0xFF],
                    &[0x00; 4],
                    &[0x01, 0x00, 0x00, 0x00],
                    &[0xFF, 0xFF, 0x7F, 0x00],
                ]),
            ),
            (
                24,
                4,
                32,
                concat(&[
                    &[0x00, 0x00, 0x00, 0x80],
                    &[0x00, 0xFF, 0xFF, 0xFF],
                    &[0x00; 4],
                    &[0x00, 0x01, 0x00, 0x00],
                    &[0x00, 0xFF, 0xFF, 0x7F],
                ]),
            ),
            (
                16,
                4,
                24,
                concat(&[
                    &[0x00, 0x00, 0x80, 0xFF],
                    &[0x00, 0xFF, 0xFF, 0xFF],
                    &[0x00; 4],
                    &[0x00, 0x01, 0x00, 0x00],
                    &[0x00, 0xFF, 0x7F, 0x00],
                ]),
            ),
            (
                16,
                3,
                16,
                concat(&[
                    &[0x00, 0x80, 0xFF],
                    &[0xFF, 0xFF, 0xFF],
                    &[0x00; 3],
                    &[0x01, 0x00, 0x00],
                    &[0xFF, 0x7F, 0x00],
                ]),
            ),
        ];
        for (source_bits, container_bytes, valid_bits, expected) in cases {
            let format = device_format(
                container_bytes,
                valid_bits,
                ValidBitsAlignment::LeastSignificant,
            );
            assert_eq!(
                pack(&extreme_samples(source_bits), source_bits, &format),
                expected,
                "{source_bits} in ({container_bytes}, valid {valid_bits})"
            );
        }
    }

    #[test]
    fn round_trip_is_identity_for_both_alignments() {
        for alignment in [
            ValidBitsAlignment::MostSignificant,
            ValidBitsAlignment::LeastSignificant,
        ] {
            for bits in [4u8, 8, 12, 16, 20, 24] {
                let mut samples = extreme_samples(bits).to_vec();
                samples.extend([3, -3, 5 << (bits - 4)]);
                for candidate in lossless_device_format_candidates(&stream_format(bits), alignment)
                {
                    let packed = pack(&samples, bits, &candidate);
                    let mut restored = vec![0i32; samples.len()];
                    let result =
                        unpack_samples_from_container(&packed, bits, &candidate, &mut restored);
                    assert_eq!(result, Ok(()), "bits {bits}, {candidate:?}");
                    assert_eq!(restored, samples, "bits {bits}, {candidate:?}");
                }
            }
        }
    }

    #[test]
    fn unpack_detects_non_zero_padding_in_most_significant_alignment() {
        let format = full_container_format(3, ValidBitsAlignment::MostSignificant);
        // 16-bit samples in 3 bytes: the lowest container byte must be zero.
        let bytes = [0x00, 0x34, 0x12, 0x01, 0x34, 0x12];
        let mut out = [0i32; 2];
        let result = unpack_samples_from_container(&bytes, 16, &format, &mut out);
        assert_eq!(result, Err(UnpackError::InvalidPadding { sample_index: 1 }));
        assert_eq!(out[0], 0x1234);
    }

    #[test]
    fn unpack_detects_padding_bit_sharing_a_byte_with_valid_bits() {
        let format = full_container_format(3, ValidBitsAlignment::MostSignificant);
        // 20 bits in 3 bytes: the low nibble of byte 0 is padding, the high nibble is data.
        let mut out = [0i32; 1];
        assert_eq!(
            unpack_samples_from_container(&[0x08, 0x00, 0x00], 20, &format, &mut out),
            Err(UnpackError::InvalidPadding { sample_index: 0 })
        );
        assert_eq!(
            unpack_samples_from_container(&[0x10, 0x00, 0x00], 20, &format, &mut out),
            Ok(())
        );
        assert_eq!(out[0], 1);
    }

    #[test]
    fn unpack_detects_bad_low_bits_and_bad_sign_extension_in_least_significant_alignment() {
        let mut out = [0i32; 1];
        // 24 in (4, valid 32): the low 8 bits must be zero.
        let valid_32 = device_format(4, 32, ValidBitsAlignment::LeastSignificant);
        assert_eq!(
            unpack_samples_from_container(&[0x01, 0x00, 0x00, 0x00], 24, &valid_32, &mut out),
            Err(UnpackError::InvalidPadding { sample_index: 0 })
        );
        // 24 in (4, valid 24): 0x00800000 does not fit in 24 signed bits.
        let valid_24 = device_format(4, 24, ValidBitsAlignment::LeastSignificant);
        assert_eq!(
            unpack_samples_from_container(&[0x00, 0x00, 0x80, 0x00], 24, &valid_24, &mut out),
            Err(UnpackError::InvalidPadding { sample_index: 0 })
        );
    }

    #[test]
    fn unpack_reports_length_mismatch_without_touching_destination() {
        let format = full_container_format(3, ValidBitsAlignment::MostSignificant);
        let mut out = [7i32; 2];
        let result = unpack_samples_from_container(&[0u8; 5], 24, &format, &mut out);
        assert_eq!(
            result,
            Err(UnpackError::LengthMismatch {
                expected_bytes: 6,
                actual_bytes: 5
            })
        );
        assert_eq!(out, [7, 7]);
    }

    #[test]
    fn validate_pcm_layout_accepts_every_candidate_and_rejects_bad_layouts() {
        for bits in [4u8, 8, 12, 16, 20, 24, 32] {
            for alignment in [
                ValidBitsAlignment::MostSignificant,
                ValidBitsAlignment::LeastSignificant,
            ] {
                for candidate in lossless_device_format_candidates(&stream_format(bits), alignment)
                {
                    assert_eq!(validate_pcm_layout(bits, &candidate), Ok(()));
                }
            }
        }
        let ms = ValidBitsAlignment::MostSignificant;
        assert_eq!(
            validate_pcm_layout(16, &device_format(0, 16, ms)),
            Err(PcmLayoutError::ContainerBytesOutOfRange { container_bytes: 0 })
        );
        assert_eq!(
            validate_pcm_layout(16, &device_format(5, 16, ms)),
            Err(PcmLayoutError::ContainerBytesOutOfRange { container_bytes: 5 })
        );
        assert_eq!(
            validate_pcm_layout(16, &device_format(2, 24, ms)),
            Err(PcmLayoutError::ValidBitsExceedContainer {
                valid_bits: 24,
                container_bytes: 2
            })
        );
        assert_eq!(
            validate_pcm_layout(0, &device_format(2, 16, ms)),
            Err(PcmLayoutError::SourceBitsOutOfRange {
                source_bits: 0,
                valid_bits: 16
            })
        );
        assert_eq!(
            validate_pcm_layout(24, &device_format(3, 16, ms)),
            Err(PcmLayoutError::SourceBitsOutOfRange {
                source_bits: 24,
                valid_bits: 16
            })
        );
    }

    #[test]
    fn invalid_layout_writes_nothing_and_never_panics() {
        let bad = device_format(0, 16, ValidBitsAlignment::MostSignificant);
        let mut bytes = [0xAAu8; 4];
        pack_samples_into_container(&[1, 2], 16, &bad, &mut bytes);
        assert_eq!(bytes, [0xAA; 4]);
        let mut out = [9i32; 2];
        assert!(matches!(
            unpack_samples_from_container(&bytes, 16, &bad, &mut out),
            Err(UnpackError::InvalidLayout(_))
        ));
        assert_eq!(out, [9, 9]);
    }

    #[test]
    fn canonical_md5_bytes_are_sign_extended_little_endian() {
        let mut sixteen = [0u8; 4];
        assert_eq!(
            write_canonical_md5_bytes(&[-1, 1], 16, &mut sixteen),
            Ok(())
        );
        assert_eq!(sixteen, [0xFF, 0xFF, 0x01, 0x00]);

        let mut twelve = [0u8; 2];
        assert_eq!(write_canonical_md5_bytes(&[-1], 12, &mut twelve), Ok(()));
        assert_eq!(twelve, [0xFF, 0xFF]);

        let mut twenty = [0u8; 3];
        assert_eq!(write_canonical_md5_bytes(&[-1], 20, &mut twenty), Ok(()));
        assert_eq!(twenty, [0xFF, 0xFF, 0xFF]);

        let mut twenty_four = [0u8; 6];
        assert_eq!(
            write_canonical_md5_bytes(&[-8_388_608, 0x123456], 24, &mut twenty_four),
            Ok(())
        );
        assert_eq!(twenty_four, [0x00, 0x00, 0x80, 0x56, 0x34, 0x12]);

        let mut four_bits = [0u8; 2];
        assert_eq!(
            write_canonical_md5_bytes(&[-8, 7], 4, &mut four_bits),
            Ok(())
        );
        assert_eq!(four_bits, [0xF8, 0x07]);
    }

    #[test]
    fn canonical_md5_writer_reports_out_of_range_samples_and_bad_arguments() {
        let mut two = [0u8; 4];
        assert_eq!(
            write_canonical_md5_bytes(&[1, 0x8000], 16, &mut two),
            Err(CanonicalMd5Error::SampleOutOfRange { sample_index: 1 })
        );
        assert_eq!(
            write_canonical_md5_bytes(&[-0x8001, 0], 16, &mut two),
            Err(CanonicalMd5Error::SampleOutOfRange { sample_index: 0 })
        );
        assert_eq!(
            write_canonical_md5_bytes(&[1, 2], 0, &mut two),
            Err(CanonicalMd5Error::SourceBitsOutOfRange { source_bits: 0 })
        );
        assert_eq!(
            write_canonical_md5_bytes(&[1, 2], 33, &mut two),
            Err(CanonicalMd5Error::SourceBitsOutOfRange { source_bits: 33 })
        );
        assert_eq!(
            write_canonical_md5_bytes(&[1, 2, 3], 16, &mut two),
            Err(CanonicalMd5Error::LengthMismatch {
                expected_bytes: 6,
                actual_bytes: 4
            })
        );
    }
}
