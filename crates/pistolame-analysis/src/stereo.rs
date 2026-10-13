//! Stereo image measurements: how the two channels relate to each other, overall and per
//! frequency band.
//!
//! This is reported information, **not** a transcode signal. That distinction is the whole
//! point of this module's existence, so it is worth stating plainly: lossy encoders do
//! leave stereo fingerprints — mid/side coding, and intensity stereo which collapses the
//! high bands to a single channel plus a scale factor — and measuring them was expected to
//! give `transcode_detect` an indicator independent of the lowpass. It did not. Measured
//! across this project's corpus, the high-band side/mid ratio of a LAME V0 or AAC 256
//! transcode sits within a decibel of its lossless source. Nothing here is wired into the
//! verdict, and nothing here should be without new evidence.
//!
//! What it is genuinely good for: telling a listener what they have. A "stereo" file that
//! is really dual-mono, a fake-wide master built from an out-of-phase duplicate, or a
//! mid/side balance that will collapse on a mono playback system are all real properties of
//! a file that this tool could otherwise not report.

use serde::Serialize;

use crate::decode::DecodedAudio;

/// A side channel this far below the mid channel is inaudible as width. -60 dB is roughly
/// where a 16-bit dither floor sits relative to a normal mix level, so anything under it
/// cannot be distinguished from rounding in the container.
const SIDE_NEGLIGIBLE_DB: f64 = -60.0;

/// Band edges used for the per-band breakdown, in Hz. Three bands rather than a fine sweep:
/// this is a description for a human, and "bass is mono, highs are wide" is the shape of
/// answer that is actually useful.
const BAND_EDGES_HZ: [(f64, f64); 3] = [(20.0, 250.0), (250.0, 4_000.0), (4_000.0, f64::MAX)];
const BAND_NAMES: [&str; 3] = ["low", "mid", "high"];

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct BandStereo {
    /// `low` / `mid` / `high`.
    pub name: &'static str,
    pub low_hz: f64,
    /// `None` for the top band, which runs to Nyquist.
    pub high_hz: Option<f64>,
    /// Side energy relative to mid, in dB. 0 means side and mid carry equal energy (a very
    /// wide image); large negative values mean the band is effectively mono.
    pub side_to_mid_db: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct StereoAnalysis {
    /// Pearson correlation between the two channels over the whole file, in -1.0..=1.0.
    /// 1.0 is identical channels, 0.0 unrelated content, negative means the channels are
    /// substantially out of phase with each other.
    pub correlation: f64,
    /// Side (L-R)/2 energy relative to mid (L+R)/2, in dB, whole-file.
    pub side_to_mid_db: f64,
    /// True when the side channel is *exactly* digital silence: the two channels are
    /// bit-identical, so the file is mono content in a stereo container. Distinct from
    /// "very narrow" — this is an exact test, not a threshold.
    pub dual_mono: bool,
    /// True when the side channel exists but sits below [`SIDE_NEGLIGIBLE_DB`].
    pub effectively_mono: bool,
    /// Set when correlation is negative: summing this file to mono will cancel content
    /// rather than just narrow it. Usually a fake-widened master.
    pub mono_compatibility_risk: bool,
    pub per_band: Vec<BandStereo>,
}

/// Measures the stereo image. Returns `None` for anything that is not exactly two channels.
pub fn analyze_stereo(decoded: &DecodedAudio) -> Option<StereoAnalysis> {
    if decoded.channels != 2 || decoded.channel_samples.len() != 2 {
        return None;
    }
    let left = &decoded.channel_samples[0];
    let right = &decoded.channel_samples[1];
    let len = left.len().min(right.len());
    if len == 0 {
        return None;
    }
    let (left, right) = (&left[..len], &right[..len]);

    let correlation = pearson(left, right);

    // Exact test, deliberately: `dual_mono` claims the channels are bit-identical, so it
    // must not be true for a file that merely has a very quiet side channel.
    let dual_mono = left.iter().zip(right).all(|(l, r)| l == r);

    let mut mid_energy = 0.0f64;
    let mut side_energy = 0.0f64;
    for (&l, &r) in left.iter().zip(right) {
        let mid = (l as f64 + r as f64) * 0.5;
        let side = (l as f64 - r as f64) * 0.5;
        mid_energy += mid * mid;
        side_energy += side * side;
    }
    // Two values, because the displayed one is clamped and the test must not be. Comparing
    // the clamped figure against the clamp made `effectively_mono` unreachable: the ratio can
    // never read below the floor it is pinned to, so `side_to_mid_db < SIDE_NEGLIGIBLE_DB`
    // was false for every file ever analysed.
    let raw_side_to_mid_db = raw_energy_ratio_db(side_energy, mid_energy);
    let side_to_mid_db = raw_side_to_mid_db.max(SIDE_NEGLIGIBLE_DB);

    Some(StereoAnalysis {
        correlation,
        side_to_mid_db,
        dual_mono,
        effectively_mono: !dual_mono && raw_side_to_mid_db <= SIDE_NEGLIGIBLE_DB,
        mono_compatibility_risk: correlation < 0.0,
        per_band: per_band_stereo(left, right, decoded.sample_rate),
    })
}

fn pearson(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len() as f64;
    let (mut sa, mut sb) = (0.0f64, 0.0f64);
    for (&x, &y) in a.iter().zip(b) {
        sa += x as f64;
        sb += y as f64;
    }
    let (ma, mb) = (sa / n, sb / n);

    let (mut cov, mut va, mut vb) = (0.0f64, 0.0f64, 0.0f64);
    for (&x, &y) in a.iter().zip(b) {
        let (dx, dy) = (x as f64 - ma, y as f64 - mb);
        cov += dx * dy;
        va += dx * dx;
        vb += dy * dy;
    }
    // A digitally silent (or DC-only) channel has no variance and no correlation to report.
    // Reported as 1.0 rather than 0.0 when *both* are flat: two constant channels really are
    // perfectly matched, and calling that "unrelated" would be misleading.
    if va <= 0.0 && vb <= 0.0 {
        return 1.0;
    }
    if va <= 0.0 || vb <= 0.0 {
        return 0.0;
    }
    (cov / (va.sqrt() * vb.sqrt())).clamp(-1.0, 1.0)
}

/// The side/mid ratio as measured, with no display floor applied.
///
/// Callers that show the number clamp it themselves; callers that *test* it must not, or the
/// test compares a value against the bound it was just pinned to.
fn raw_energy_ratio_db(numerator: f64, denominator: f64) -> f64 {
    if denominator <= 0.0 {
        // No mid at all: either silence, or a purely out-of-phase file. The first is not
        // mono-like and the second is the opposite of it, so neither may read as negligible.
        return if numerator > 0.0 {
            0.0
        } else {
            SIDE_NEGLIGIBLE_DB
        };
    }
    if numerator <= 0.0 {
        // No side energy whatsoever: as mono as a non-bit-identical pair can be.
        return f64::NEG_INFINITY;
    }
    10.0 * (numerator / denominator).log10()
}

/// [`raw_energy_ratio_db`] clamped to the display floor.
fn energy_ratio_db(numerator: f64, denominator: f64) -> f64 {
    raw_energy_ratio_db(numerator, denominator).max(SIDE_NEGLIGIBLE_DB)
}

/// Per-band side/mid, via one FFT pass over mid and side rather than a filter bank: the
/// bands here are coarse and only their relative energy matters, so bin summation is both
/// simpler and exact enough. Windowed and overlapped like `spectral.rs` so a band edge
/// doesn't pick up transient splatter.
fn per_band_stereo(left: &[f32], right: &[f32], sample_rate: u32) -> Vec<BandStereo> {
    const FFT_SIZE: usize = 4096;
    const HOP: usize = FFT_SIZE / 2;

    let nyquist = sample_rate as f64 / 2.0;
    if left.len() < FFT_SIZE || sample_rate == 0 {
        return Vec::new();
    }

    use rustfft::FftPlanner;
    use rustfft::num_complex::Complex32;
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let window: Vec<f32> = (0..FFT_SIZE)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
        .collect();

    let bin_count = FFT_SIZE / 2;
    let mut mid_power = vec![0.0f64; bin_count];
    let mut side_power = vec![0.0f64; bin_count];
    let mut mid_buf = vec![Complex32::new(0.0, 0.0); FFT_SIZE];
    let mut side_buf = vec![Complex32::new(0.0, 0.0); FFT_SIZE];

    let frames = (left.len() - FFT_SIZE) / HOP + 1;
    for f in 0..frames {
        let start = f * HOP;
        for i in 0..FFT_SIZE {
            let (l, r) = (left[start + i], right[start + i]);
            mid_buf[i] = Complex32::new((l + r) * 0.5 * window[i], 0.0);
            side_buf[i] = Complex32::new((l - r) * 0.5 * window[i], 0.0);
        }
        fft.process(&mut mid_buf);
        fft.process(&mut side_buf);
        for bin in 0..bin_count {
            mid_power[bin] += (mid_buf[bin].norm() as f64).powi(2);
            side_power[bin] += (side_buf[bin].norm() as f64).powi(2);
        }
    }

    BAND_EDGES_HZ
        .iter()
        .zip(BAND_NAMES)
        .map(|(&(lo_hz, hi_hz), name)| {
            let to_bin = |hz: f64| ((hz / nyquist * bin_count as f64) as usize).min(bin_count);
            let lo = to_bin(lo_hz);
            let hi = to_bin(hi_hz).max(lo + 1).min(bin_count);
            let mid: f64 = mid_power[lo..hi].iter().sum();
            let side: f64 = side_power[lo..hi].iter().sum();
            BandStereo {
                name,
                low_hz: lo_hz,
                high_hz: if hi_hz.is_finite() { Some(hi_hz) } else { None },
                side_to_mid_db: energy_ratio_db(side, mid),
            }
        })
        .collect()
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

    /// `effectively_mono` was unreachable for every file ever analysed: the test compared the
    /// side/mid ratio against the very floor the display path had just clamped it to, so
    /// `side_to_mid_db < SIDE_NEGLIGIBLE_DB` could never hold.
    #[test]
    fn a_barely_wide_file_reads_as_effectively_mono() {
        // Identical but for one sample one LSB apart, so `dual_mono` (an exact claim) is
        // false while the side channel is far below the negligible floor.
        let left: Vec<f32> = (0..8192).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let mut right = left.clone();
        right[4096] += 1.0 / 32768.0;

        let stereo = analyze_stereo(&decoded_for_test(44_100, Some(16), vec![left, right]))
            .expect("two channels should be analyzable");

        assert!(
            !stereo.dual_mono,
            "one differing sample means not bit-identical"
        );
        assert!(
            stereo.effectively_mono,
            "a side channel this far down must read as effectively mono; side/mid was {} dB",
            stereo.side_to_mid_db
        );
        assert!(
            stereo.side_to_mid_db >= SIDE_NEGLIGIBLE_DB,
            "the *displayed* value stays clamped to the floor"
        );
    }

    /// The other direction: a genuinely wide file must not be swept up by the fix above.
    #[test]
    fn a_wide_file_is_not_effectively_mono() {
        let left: Vec<f32> = (0..8192).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let right: Vec<f32> = (0..8192).map(|i| (i as f32 * 0.013).sin() * 0.5).collect();

        let stereo = analyze_stereo(&decoded_for_test(44_100, Some(16), vec![left, right]))
            .expect("two channels should be analyzable");
        assert!(
            !stereo.effectively_mono,
            "side/mid was {} dB",
            stereo.side_to_mid_db
        );
    }
}
