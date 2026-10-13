//! Shared-mode volume applied to integer samples (WASAPI shared mode writes integer PCM to the
//! Windows mixer). Only ever used in shared mode, where the system mixer converts the stream
//! anyway; exclusive streams never pass through here (bit-perfect).
//!
//! Real-time safe: no allocation, no lock, no panic.

use crate::{DeviceSampleFormat, ValidBitsAlignment};

/// Bits in one byte.
const BITS_PER_BYTE: u32 = 8;

/// Scales every little-endian, most-significant-aligned sample of `bytes` by `gain`
/// (`0.0..=1.0`), rounding to the nearest step of the format's valid bits. Bits below the valid
/// bits stay zero, as the format requires. Formats this function does not handle (least
/// significant alignment) are left unchanged.
pub fn apply_gain_to_integer_samples(bytes: &mut [u8], format: &DeviceSampleFormat, gain: f32) {
    if format.alignment != ValidBitsAlignment::MostSignificant {
        return;
    }
    let container_bytes = usize::from(format.container_bytes);
    if !(2..=4).contains(&container_bytes) {
        return;
    }
    let container_bits = container_bytes as u32 * BITS_PER_BYTE;
    let valid_bits = u32::from(format.valid_bits_per_sample).clamp(1, container_bits);
    // Samples are read as left-justified 32-bit values; `step` is one valid-bit unit.
    let step = 1i64 << (32 - valid_bits);
    let gain = f64::from(gain.clamp(0.0, 1.0));
    for sample in bytes.chunks_exact_mut(container_bytes) {
        let mut word = [0u8; 4];
        word[4 - container_bytes..].copy_from_slice(sample);
        let value = i64::from(i32::from_le_bytes(word));
        let scaled = ((value as f64 * gain) / step as f64).round() as i64 * step;
        let clamped = scaled.clamp(i64::from(i32::MIN), i64::from(i32::MAX) - (step - 1));
        let out = (clamped as i32).to_le_bytes();
        sample.copy_from_slice(&out[4 - container_bytes..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChannelMask;

    fn format(container_bytes: u8, valid_bits: u8) -> DeviceSampleFormat {
        DeviceSampleFormat {
            sample_rate_hz: 48_000,
            container_bytes,
            valid_bits_per_sample: valid_bits,
            alignment: ValidBitsAlignment::MostSignificant,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        }
    }

    #[test]
    fn sixteen_bit_samples_are_halved_and_rounded() {
        let mut bytes: Vec<u8> = [1000i16, -1000, 3, i16::MIN]
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        apply_gain_to_integer_samples(&mut bytes, &format(2, 16), 0.5);
        let values: Vec<i16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair))
            .collect();
        assert_eq!(values, vec![500, -500, 2, -16384]);
    }

    #[test]
    fn twenty_four_bits_in_thirty_two_keep_the_low_byte_at_zero() {
        let sample = 0x1234_5600_i32;
        let mut bytes = sample.to_le_bytes().to_vec();
        apply_gain_to_integer_samples(&mut bytes, &format(4, 24), 0.3);
        let out = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(out & 0xFF, 0);
        let expected = ((f64::from(sample) * 0.3) / 256.0).round() as i32 * 256;
        assert_eq!(out, expected);
    }

    #[test]
    fn twenty_four_bit_packed_samples_scale() {
        // 0x400000 (half of full scale positive) in a 3-byte little-endian container.
        let mut bytes = vec![0x00, 0x00, 0x40];
        apply_gain_to_integer_samples(&mut bytes, &format(3, 24), 0.5);
        assert_eq!(bytes, vec![0x00, 0x00, 0x20]);
    }

    #[test]
    fn zero_gain_is_silence_and_unity_is_unchanged() {
        let original: Vec<u8> = [12345i16, -321]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let mut silent = original.clone();
        apply_gain_to_integer_samples(&mut silent, &format(2, 16), 0.0);
        assert!(silent.iter().all(|byte| *byte == 0));
        let mut unchanged = original.clone();
        apply_gain_to_integer_samples(&mut unchanged, &format(2, 16), 1.0);
        assert_eq!(unchanged, original);
    }
}
