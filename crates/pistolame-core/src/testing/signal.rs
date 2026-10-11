//! Deterministic test signals: a seeded pseudo-random sequence and a sine.
//!
//! The generator is a hand-written xorshift64 so tests need no `rand` dependency and every run
//! produces the same samples.

use std::f64::consts::TAU;

/// Frame index where the forced extreme values start in the pseudo-random signal.
pub const EXTREME_VALUES_START_FRAME: usize = 1;

/// Frequency of the sine signal, in hertz (a prime tone, so it never aligns with block
/// boundaries).
const SINE_FREQUENCY_HZ: f64 = 997.0;
/// Sine peak as a fraction of full scale, so LPC residuals stay well inside the sample range.
const SINE_AMPLITUDE_FRACTION: f64 = 0.5;
/// First (left) shift of the xorshift64 triple (13, 7, 17), from Marsaglia, "Xorshift RNGs"
/// (2003).
const XORSHIFT_FIRST_LEFT_SHIFT: u32 = 13;
/// Second (right) shift of the xorshift64 triple.
const XORSHIFT_RIGHT_SHIFT: u32 = 7;
/// Third (left) shift of the xorshift64 triple.
const XORSHIFT_SECOND_LEFT_SHIFT: u32 = 17;
/// Bits in the generator state.
const XORSHIFT_STATE_BITS: u32 = 64;
/// State used when a caller passes seed 0, which would make xorshift64 stay at 0 forever.
const FALLBACK_NONZERO_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Minimal xorshift64 generator.
struct Xorshift64 {
    state: u64,
}

impl Xorshift64 {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                FALLBACK_NONZERO_SEED
            } else {
                seed
            },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut state = self.state;
        state ^= state << XORSHIFT_FIRST_LEFT_SHIFT;
        state ^= state >> XORSHIFT_RIGHT_SHIFT;
        state ^= state << XORSHIFT_SECOND_LEFT_SHIFT;
        self.state = state;
        state
    }

    /// Next uniformly distributed signed sample that fits in `bits_per_sample` bits.
    fn next_sample(&mut self, bits_per_sample: u8) -> i32 {
        let shift = XORSHIFT_STATE_BITS - u32::from(bits_per_sample);
        // Arithmetic shift of the signed reinterpretation keeps the top bits, sign included.
        ((self.next_u64() as i64) >> shift) as i32
    }
}

/// Smallest signed value representable in `bits_per_sample` bits (`-2^(b-1)`).
pub fn minimum_sample_value(bits_per_sample: u8) -> i32 {
    (-(1_i64 << (bits_per_sample - 1))) as i32
}

/// Largest signed value representable in `bits_per_sample` bits (`2^(b-1)-1`).
pub fn maximum_sample_value(bits_per_sample: u8) -> i32 {
    ((1_i64 << (bits_per_sample - 1)) - 1) as i32
}

/// Interleaved pseudo-random PCM of `frame_count` frames. Starting at
/// [`EXTREME_VALUES_START_FRAME`], four consecutive frames carry (on every channel) the minimum
/// value, the maximum value, `0` and `-1`, to catch sign-extension and shift bugs.
/// Random data mostly makes the encoder choose verbatim subframes.
pub fn pseudo_random_interleaved_pcm(
    seed: u64,
    frame_count: usize,
    channel_count: u8,
    bits_per_sample: u8,
) -> Vec<i32> {
    let channels = usize::from(channel_count);
    let mut generator = Xorshift64::new(seed);
    let mut samples: Vec<i32> = (0..frame_count * channels)
        .map(|_| generator.next_sample(bits_per_sample))
        .collect();
    let extreme_values = [
        minimum_sample_value(bits_per_sample),
        maximum_sample_value(bits_per_sample),
        0,
        -1,
    ];
    for (offset, value) in extreme_values.into_iter().enumerate() {
        let frame_index = EXTREME_VALUES_START_FRAME + offset;
        if frame_index < frame_count {
            let first_sample = frame_index * channels;
            samples[first_sample..first_sample + channels].fill(value);
        }
    }
    samples
}

/// Interleaved sine PCM, identical on every channel. Smooth data makes the encoder use LPC
/// subframes, which random data does not exercise.
pub fn sine_interleaved_pcm(
    frame_count: usize,
    channel_count: u8,
    bits_per_sample: u8,
    sample_rate_hz: u32,
) -> Vec<i32> {
    let amplitude = f64::from(maximum_sample_value(bits_per_sample)) * SINE_AMPLITUDE_FRACTION;
    let radians_per_frame = TAU * SINE_FREQUENCY_HZ / f64::from(sample_rate_hz);
    (0..frame_count)
        .flat_map(|frame_index| {
            let value = (amplitude * (radians_per_frame * frame_index as f64).sin()).round() as i32;
            std::iter::repeat_n(value, usize::from(channel_count))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_signal_is_deterministic_and_in_range() {
        let first = pseudo_random_interleaved_pcm(7, 1000, 2, 20);
        assert_eq!(first, pseudo_random_interleaved_pcm(7, 1000, 2, 20));
        assert_ne!(first, pseudo_random_interleaved_pcm(8, 1000, 2, 20));
        assert!(first.iter().all(|&sample| {
            (minimum_sample_value(20)..=maximum_sample_value(20)).contains(&sample)
        }));
    }

    #[test]
    fn random_signal_contains_the_forced_extremes_on_every_channel() {
        let samples = pseudo_random_interleaved_pcm(1, 16, 2, 24);
        let start = EXTREME_VALUES_START_FRAME * 2;
        assert_eq!(
            &samples[start..start + 8],
            &[-8_388_608, -8_388_608, 8_388_607, 8_388_607, 0, 0, -1, -1]
        );
    }

    #[test]
    fn sample_limits_match_twos_complement_ranges() {
        assert_eq!(minimum_sample_value(16), -32_768);
        assert_eq!(maximum_sample_value(16), 32_767);
        assert_eq!(minimum_sample_value(24), -8_388_608);
        assert_eq!(maximum_sample_value(24), 8_388_607);
    }

    #[test]
    fn sine_is_smooth_and_peaks_below_full_scale() {
        let samples = sine_interleaved_pcm(2000, 2, 16, 44_100);
        assert_eq!(samples.len(), 4000);
        assert_eq!(samples[0], 0);
        assert!(samples.iter().all(|&sample| sample.abs() <= 16_384));
        assert!(samples.iter().any(|&sample| sample > 16_000));
        assert!(samples.chunks(2).all(|frame| frame[0] == frame[1]));
    }
}
