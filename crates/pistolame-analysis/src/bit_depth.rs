//! Detects a "fake hi-res" file: content whose container declares a bit depth (e.g.
//! 24-bit) that its actual samples never use, because the real information behind it
//! never exceeded a narrower depth (e.g. 16-bit) — the file was zero-padded/upsampled,
//! not genuinely re-mastered. A different problem from the lossy-transcode detection in
//! `transcode_detect.rs`: this file may never have touched a lossy codec at all.
//!
//! ## Method: exact quantization-grid alignment, not a noise-floor/SNR estimate
//!
//! A tempting approach is comparing the measured noise floor to the theoretical
//! quantization SNR for N-bit PCM (~6.02*N dB) — deliberately not used here: it needs a
//! genuinely quiet passage to measure against (unreliable on brickwalled masters, exactly
//! the kind of file this feature also needs to work on) and depends on a formula this
//! project can't cite from an ITU/EBU standard the way LUFS can.
//!
//! Instead: if a file's real information never exceeded N bits, every decoded sample,
//! quantized back to the *container's declared* bit depth, is an exact multiple of the
//! N-bit quantization step — not approximately, exactly (up to float round-trip error).
//! This is checked directly rather than estimated, and reports nothing (`None`) rather
//! than guessing when the answer isn't clear-cut.
//!
//! ## Known false-negative (by design, not a bug)
//!
//! A file that was properly dithered before being padded to a wider container will *not*
//! align exactly to the narrower grid — dither intentionally adds sub-LSB noise, which is
//! exactly what defeats this check. That's an acceptable, honest gap: it means this
//! detector only ever fires on genuine, exact evidence (a lazy padding job with no
//! dithering — the common real-world case for careless fake-hi-res files), never on a
//! guess. See `.claude/skills/transcode-heuristic-validation/SKILL.md`.

use serde::Serialize;

use crate::decode::DecodedAudio;

/// Alignment must clear this fraction of samples to count — not 100%, to tolerate a
/// handful of float round-trip edge cases without demanding literal bit-exactness.
const ALIGNMENT_THRESHOLD: f64 = 0.999;
/// Below this, "effective bit depth" stops being a meaningful concept to report (8-bit
/// audio is not a realistic candidate to distinguish from padding artifacts).
const MIN_CANDIDATE_BITS: u32 = 8;
const MAX_DECLARED_BITS: u32 = 32;
/// Samples reach this module as `f32`, whose mantissa carries 24 significant bits. Beyond
/// that the decode itself has already discarded the low-order bits, so every sample would
/// appear to sit on a coarser grid than declared and a 32-bit file would be confidently
/// mislabelled as padded. Files declaring more than this are reported as unverifiable
/// (`None`) rather than analyzed against evidence that no longer exists.
const MAX_VERIFIABLE_BITS: u32 = 24;
/// Fraction of a file that must be non-silent before its grid alignment means anything.
///
/// Below this there is too little signal to distinguish "padded from a narrower depth" from
/// "mostly quiet", and the answer is withheld rather than guessed — see
/// [`BitDepthAnalysis::active_sample_ratio`].
const MIN_ACTIVE_SAMPLE_RATIO: f64 = 0.01;

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct BitDepthAnalysis {
    /// From the container (e.g. FLAC STREAMINFO) — `None` for codecs where this isn't a
    /// meaningful concept (MP3, AAC: no fixed PCM bit depth to declare).
    pub declared_bit_depth: Option<u32>,
    /// Smallest bit depth whose quantization grid explains ~all samples. `None` if there
    /// was nothing to check (no declared depth) or no narrower depth fit — which includes
    /// the common, unremarkable case of a file that's exactly as deep as it claims.
    pub effective_bit_depth: Option<u32>,
    /// Fraction of samples that were non-zero and therefore carried usable evidence.
    ///
    /// Digital silence sits on every quantization grid there is, so a track with a long
    /// silent stretch used to reach the alignment threshold on the strength of its silence
    /// alone: a genuine 24-bit recording that is 99.9% quiet was reported as 16-bit padding.
    /// The measurement now runs on the active samples only, and this says how much of the
    /// file that was.
    pub active_sample_ratio: f64,
}

pub fn analyze_bit_depth(decoded: &DecodedAudio) -> BitDepthAnalysis {
    let declared_bit_depth = decoded.bits_per_sample;

    let unverifiable = |active_sample_ratio: f64| BitDepthAnalysis {
        declared_bit_depth,
        effective_bit_depth: None,
        active_sample_ratio,
    };

    let Some(declared_bits) =
        declared_bit_depth.filter(|&b| b > MIN_CANDIDATE_BITS && b <= MAX_DECLARED_BITS)
    else {
        return unverifiable(0.0);
    };

    // Beyond f32's 24-bit mantissa there is nothing left to measure — see
    // MAX_VERIFIABLE_BITS. Say so instead of reporting a confident wrong answer.
    if declared_bits > MAX_VERIFIABLE_BITS {
        return unverifiable(0.0);
    }

    let scale = 2f64.powi((declared_bits - 1) as i32);

    // A sample sits on the grid for candidate depth `c` exactly when its integer value is
    // divisible by `2^(declared - c)`, i.e. when it has at least `declared - c` trailing
    // zero bits. So one pass tallying trailing-zero counts answers the question for every
    // candidate at once, instead of materializing all samples and rescanning per candidate.
    //
    // That materialization was the single largest allocation in the whole pipeline: a
    // `Vec<i64>` over every sample of every channel, ~737 MB on an 8-minute 96 kHz stereo
    // file, walked up to 16 times. This version allocates nothing and reads each sample once.
    let mut trailing_zero_histogram = [0u64; 64];
    let mut active_samples: u64 = 0;
    let mut total_samples: u64 = 0;
    for channel in &decoded.channel_samples {
        for &sample in channel {
            total_samples += 1;
            let value = (sample as f64 * scale).round() as i64;
            // Silence is aligned to every grid at once and therefore says nothing about
            // which one the file was made on. Excluded from the tally rather than counted
            // as agreement: with a 99.9% threshold, a genuine 24-bit master holding a long
            // silent passage used to clear it on the strength of the silence.
            if value == 0 {
                continue;
            }
            let tz = value.trailing_zeros().min(63) as usize;
            trailing_zero_histogram[tz] += 1;
            active_samples += 1;
        }
    }

    let active_sample_ratio = if total_samples == 0 {
        0.0
    } else {
        active_samples as f64 / total_samples as f64
    };

    // Too little signal to tell padding from quiet — say so rather than guess.
    if active_samples == 0 || active_sample_ratio < MIN_ACTIVE_SAMPLE_RATIO {
        return unverifiable(active_sample_ratio);
    }

    // Coarsest grid first: alignment to a coarse grid implies alignment to every finer
    // one, so the first candidate that fits is the smallest depth explaining the data.
    for candidate in MIN_CANDIDATE_BITS..declared_bits {
        let required_trailing_zeros = (declared_bits - candidate) as usize;
        let aligned: u64 = trailing_zero_histogram[required_trailing_zeros..]
            .iter()
            .sum();
        if aligned as f64 / active_samples as f64 >= ALIGNMENT_THRESHOLD {
            return BitDepthAnalysis {
                declared_bit_depth,
                effective_bit_depth: Some(candidate),
                active_sample_ratio,
            };
        }
    }

    BitDepthAnalysis {
        declared_bit_depth,
        effective_bit_depth: Some(declared_bits),
        active_sample_ratio,
    }
}
#[cfg(test)]
fn decoded_for_test(sample_rate: u32, bits: Option<u32>, channels: Vec<Vec<f32>>) -> DecodedAudio {
    DecodedAudio {
        sample_rate,
        channels: channels.len(),
        channel_layout: symphonia::core::audio::Channels::Discrete(channels.len() as u16),
        codec_short_name: "flac".into(),
        container_short_name: "flac".into(),
        bits_per_sample: bits,
        channel_samples: channels,
        integrity_verified: None,
        encoder_tag_matches: Vec::new(),
        decode_status: crate::decode::DecodeStatus {
            complete: true,
            skipped_packets: 0,
            stopped_early: false,
            channels_unequal: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Digital silence sits on every quantization grid at once, so counting it as agreement
    /// let a long quiet passage decide the answer: a genuine 24-bit master that is almost
    /// entirely silent was reported as 16-bit padding.
    #[test]
    fn a_mostly_silent_24_bit_file_is_not_called_padded() {
        let step = 1.0 / 8_388_608.0; // one 24-bit LSB
        let mut samples = vec![0.0f32; 200_000];
        // 0.5% of the file carries signal, none of it on a 16-bit grid.
        for (i, slot) in samples.iter_mut().take(1_000).enumerate() {
            *slot = step * (i as f32 * 7.0 + 3.0);
        }

        let analysis = analyze_bit_depth(&decoded_for_test(96_000, Some(24), vec![samples]));
        assert_eq!(
            analysis.effective_bit_depth, None,
            "too little active signal to judge; the answer must be withheld, not guessed"
        );
        assert!(analysis.active_sample_ratio < MIN_ACTIVE_SAMPLE_RATIO);
    }

    /// With enough active signal, the same non-aligned content is correctly read as genuine 24-bit.
    #[test]
    fn genuine_24_bit_content_is_not_reduced() {
        let step = 1.0 / 8_388_608.0;
        let samples: Vec<f32> = (0..100_000)
            .map(|i| step * ((i % 4096) as f32 * 7.0 + 3.0))
            .collect();

        let analysis = analyze_bit_depth(&decoded_for_test(96_000, Some(24), vec![samples]));
        assert_eq!(analysis.effective_bit_depth, Some(24));
        assert!(analysis.active_sample_ratio > 0.99);
    }

    /// And padding is still caught: every sample an exact multiple of the 16-bit step.
    #[test]
    fn zero_padded_16_bit_content_is_still_detected() {
        let step = 1.0 / 32_768.0;
        let samples: Vec<f32> = (0..100_000)
            .map(|i| step * ((i % 4096) as f32 - 2048.0))
            .collect();

        let analysis = analyze_bit_depth(&decoded_for_test(96_000, Some(24), vec![samples]));
        assert_eq!(analysis.effective_bit_depth, Some(16));
    }
}
