//! Exact conversion of integer samples to 32-bit float, for devices that only accept float
//! (CoreAudio built-in outputs, plan coreaudio.md §0). Pure code with no platform calls, so it
//! is compiled and tested on every OS.
//!
//! Why it is exact: a most-significant-aligned 32-bit container holding at most 24 valid bits
//! is an integer `k * 2^(32 - valid_bits)` with `|k| <= 2^23`. Its magnitude needs at most 24
//! significant bits, which is the precision of an IEEE 754 binary32 significand, so the
//! integer-to-float cast is exact. Dividing by 2^31 only changes the exponent (no subnormal is
//! reached: the smallest nonzero magnitude is 2^8 / 2^31 = 2^-23), so the result is exact too
//! and lies in [-1.0, 1.0). This is the normalization CoreAudio float formats use.

use crate::{DeviceSampleFormat, ValidBitsAlignment};

/// Size of the integer container the conversion reads, in bytes.
pub const INTEGER_CONTAINER_BYTES: u8 = 4;
/// Size of one float32 sample, in bytes (equal to the container, so buffers keep their size).
pub const FLOAT32_SAMPLE_BYTES: usize = 4;
/// Highest number of valid bits a float32 significand holds exactly (IEEE 754 binary32: 24).
pub const MAXIMUM_EXACT_VALID_BITS: u8 = 24;
/// The divisor that maps a full-scale 32-bit container to [-1.0, 1.0): 2^31.
const FULL_SCALE_DIVISOR: f32 = 2_147_483_648.0;

/// Whether integer samples of `format` convert to float32 without any loss with
/// `convert_integer_samples_to_float32`.
pub fn is_exactly_convertible_to_float32(format: &DeviceSampleFormat) -> bool {
    format.container_bytes == INTEGER_CONTAINER_BYTES
        && format.alignment == ValidBitsAlignment::MostSignificant
        && (1..=MAXIMUM_EXACT_VALID_BITS).contains(&format.valid_bits_per_sample)
}

/// Converts little-endian, most-significant-aligned 32-bit integer samples to little-endian
/// float32 samples, one for one. Both slices must have the same length, a multiple of 4; any
/// trailing partial sample is left untouched (the callers only pass whole frames).
///
/// Real-time safe: no allocation, no lock, no panic.
pub fn convert_integer_samples_to_float32(integer_bytes: &[u8], float_bytes: &mut [u8]) {
    let (integer_samples, _) = integer_bytes.as_chunks::<FLOAT32_SAMPLE_BYTES>();
    let (float_samples, _) = float_bytes.as_chunks_mut::<FLOAT32_SAMPLE_BYTES>();
    for (integer_sample, float_sample) in integer_samples.iter().zip(float_samples) {
        let value = i32::from_le_bytes(*integer_sample) as f32 / FULL_SCALE_DIVISOR;
        *float_sample = value.to_le_bytes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChannelMask;

    fn format(container_bytes: u8, valid_bits_per_sample: u8) -> DeviceSampleFormat {
        DeviceSampleFormat {
            sample_rate_hz: 96_000,
            container_bytes,
            valid_bits_per_sample,
            alignment: ValidBitsAlignment::MostSignificant,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        }
    }

    fn convert_one(container: i32) -> f32 {
        let mut float_bytes = [0u8; 4];
        convert_integer_samples_to_float32(&container.to_le_bytes(), &mut float_bytes);
        f32::from_le_bytes(float_bytes)
    }

    #[test]
    fn only_32_bit_containers_with_at_most_24_valid_bits_are_exact() {
        assert!(is_exactly_convertible_to_float32(&format(4, 16)));
        assert!(is_exactly_convertible_to_float32(&format(4, 24)));
        assert!(!is_exactly_convertible_to_float32(&format(4, 32)));
        assert!(!is_exactly_convertible_to_float32(&format(3, 24)));
        assert!(!is_exactly_convertible_to_float32(&format(2, 16)));
        let mut least_significant = format(4, 24);
        least_significant.alignment = ValidBitsAlignment::LeastSignificant;
        assert!(!is_exactly_convertible_to_float32(&least_significant));
    }

    #[test]
    fn known_values_have_the_expected_float_bits() {
        // Hand-written expectations: full-scale negative, zero, one 24-bit step, maximum.
        assert_eq!(convert_one(i32::MIN), -1.0);
        assert_eq!(convert_one(0), 0.0);
        assert_eq!(convert_one(1 << 8), 2.0_f32.powi(-23));
        assert_eq!(convert_one(0x7FFF_FF00), 1.0 - 2.0_f32.powi(-23));
        assert_eq!(convert_one(-(1 << 16)), -(2.0_f32.powi(-15)));
    }

    /// Every one of the 2^24 possible 24-bit samples converts to a float that maps back to the
    /// same integer: the conversion is lossless, not approximately so.
    #[test]
    fn every_24_bit_sample_round_trips_exactly() {
        for sample in -(1_i32 << 23)..(1_i32 << 23) {
            let container = sample << 8;
            let float_value = convert_one(container);
            let restored = (f64::from(float_value) * 2_147_483_648.0) as i64;
            assert_eq!(restored, i64::from(container), "sample {sample}");
        }
    }

    #[test]
    fn buffers_convert_sample_by_sample() {
        let integers: Vec<u8> = [i32::MIN, 0, 1 << 16]
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        let mut floats = vec![0u8; integers.len()];
        convert_integer_samples_to_float32(&integers, &mut floats);
        let values: Vec<f32> = floats
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect();
        assert_eq!(values, vec![-1.0, 0.0, 2.0_f32.powi(-15)]);
    }
}
