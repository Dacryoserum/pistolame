//! RMS, peak, true peak, LUFS, and clipping — see
//! `.claude/skills/dsp-correctness/SKILL.md` before touching this file.

use ebur128::{Channel, EbuR128, Mode};
use rayon::prelude::*;
use serde::Serialize;
use symphonia::core::audio::{ChannelLabel, Channels, Position};

use crate::decode::DecodedAudio;

/// Floor used instead of -inf/NaN for digital silence, since serde_json cannot serialize
/// non-finite floats.
const SILENCE_FLOOR_DB: f64 = -120.0;

/// Shortest track for which a Loudness Range figure is reported.
///
/// EBU Tech 3342 sets no minimum, but its 3-second short-term window with a 100 ms hop means
/// a 5-second clip yields overlapping views of essentially one moment, and a 10th/95th
/// percentile spread over those describes the window shape rather than the programme. Ten
/// seconds is the point at which the windows cover enough distinct material for the spread to
/// mean something; below it the honest answer is that the figure was not measured.
const MIN_LRA_DURATION_S: f64 = 10.0;

/// Bit depth assumed when the codec declares none.
///
/// MP3, AAC and friends have no fixed PCM depth to declare, so there is no LSB to reason
/// about. 16 bits is the widest tolerance of the common depths, which keeps the count
/// conservative — it can miss a sample a hair under full scale, never invent one.
const ASSUMED_BITS_WITHOUT_DECLARATION: u32 = 16;

/// How many consecutive full-scale samples make a *plateau* rather than a peak that
/// happened to land on the rail.
///
/// A single sample at full scale is an ordinary loud transient; three in a row is a waveform
/// with its top cut off, which is what "clipping" describes and what is audible. Counting
/// only samples used to report both as the same thing.
const MIN_CLIPPED_RUN: usize = 3;

/// A sample at or above this fraction of full scale sits on the top quantization step.
///
/// Derived from the declared depth rather than fixed, and the difference is not cosmetic.
/// Signed PCM is asymmetric: 16-bit runs -32768..=+32767, and decoders (symphonia included)
/// normalize by the negative bound, so positive full scale arrives as 32767/32768 = 0.99997
/// and negative full scale as exactly -1.0. Testing `abs() >= 1.0` counted every clipped
/// sample on the negative half of the waveform and none on the positive half — verified
/// against a fixture holding 1000 samples at +32767 and 1000 at -32768, which reported 1000
/// instead of 2000.
///
/// One LSB of headroom catches both bounds. Taking that LSB *at the file's own depth* is
/// what the previous fixed 16-bit constant got wrong: in 24-bit, one 16-bit LSB spans the
/// top 256 codes, so a quarter-thousand distinct sample values below full scale were all
/// counted as sitting on the rail.
fn clipping_threshold(bits_per_sample: Option<u32>) -> f32 {
    let bits = bits_per_sample
        .unwrap_or(ASSUMED_BITS_WITHOUT_DECLARATION)
        .clamp(2, 32);
    1.0 - 1.0 / 2f32.powi(bits as i32 - 1)
}

fn linear_to_db(value: f64) -> f64 {
    if value <= 1e-10 {
        SILENCE_FLOOR_DB
    } else {
        20.0 * value.log10()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ChannelStats {
    pub channel: usize,
    pub peak_dbfs: f64,
    pub rms_dbfs: f64,
    /// Simple peak-to-RMS crest factor — NOT the DR14 (Pleasurize Music Foundation)
    /// algorithm, which is block-based. Labelled explicitly per dsp-correctness skill.
    pub crest_factor_db: f64,
    /// Samples sitting on the top quantization step of the declared depth. A count of how
    /// often the signal touched the rail, **not** evidence of clipping on its own: a
    /// legitimately mastered track can peak at full scale without a single flattened sample.
    pub full_scale_sample_count: usize,
    /// Runs of [`MIN_CLIPPED_RUN`] or more consecutive full-scale samples — a flattened
    /// waveform top rather than a peak that touched the rail. This is the one that means
    /// clipping.
    pub clipped_run_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SignalAnalysis {
    /// False for channel layouts whose BS.1770 speaker weights are unknown. LUFS and LRA
    /// are withheld for those layouts; sampled and true peaks remain measurable.
    pub loudness_layout_supported: bool,
    pub peak_dbfs: f64,
    pub true_peak_dbtp: f64,
    pub rms_dbfs: f64,
    /// `None` for content with no gated block above the absolute/relative threshold
    /// (e.g. near-silent files) — a legitimate "not measurable", not a bug.
    pub lufs_integrated: Option<f64>,
    /// Loudness Range (EBU Tech 3342), in LU — how much the perceived loudness varies
    /// over the track. Companion metric to integrated LUFS, not a replacement.
    pub loudness_range_lu: Option<f64>,
    /// Oversampling factor `ebur128` applied for `true_peak_dbtp`: 4 below 96 kHz, 2 between
    /// 96 and 192, and **0 at 192 kHz and above**, where the library does no oversampling at
    /// all and the figure is a sampled peak wearing a true-peak label. Surfaced so the UI can
    /// say which of the two it is showing.
    pub true_peak_oversampling: u32,
    /// Samples on the top quantization step, summed over channels — see
    /// [`ChannelStats::full_scale_sample_count`].
    pub full_scale_sample_count_total: usize,
    /// Runs of consecutive full-scale samples, summed over channels. The figure that
    /// actually means clipping — see [`ChannelStats::clipped_run_count`].
    pub clipped_run_count_total: usize,
    pub per_channel: Vec<ChannelStats>,
}

pub fn analyze_signal(decoded: &DecodedAudio) -> Result<SignalAnalysis, String> {
    // Channels are independent for these reductions, and the loudness metering below is a
    // separate traversal again — so run the cheap per-channel statistics alongside the
    // expensive `ebur128` work rather than one after the other.
    let threshold = clipping_threshold(decoded.bits_per_sample);
    let (channel_results, loudness) = rayon::join(
        || {
            decoded
                .channel_samples
                .par_iter()
                .enumerate()
                .map(|(idx, samples)| {
                    let mut peak_linear: f64 = 0.0;
                    let mut sum_squares: f64 = 0.0;
                    let mut full_scale_sample_count = 0usize;
                    let mut clipped_run_count = 0usize;
                    let mut run = 0usize;

                    for &s in samples {
                        let abs = s.abs() as f64;
                        peak_linear = peak_linear.max(abs);
                        sum_squares += (s as f64) * (s as f64);
                        // The top quantization step at this file's own depth — see
                        // `clipping_threshold`, not an arbitrary epsilon.
                        if s.abs() >= threshold {
                            full_scale_sample_count += 1;
                            run += 1;
                            // Counted once, when the run first becomes long enough to be a
                            // plateau rather than a peak.
                            if run == MIN_CLIPPED_RUN {
                                clipped_run_count += 1;
                            }
                        } else {
                            run = 0;
                        }
                    }

                    let rms_linear = if samples.is_empty() {
                        0.0
                    } else {
                        (sum_squares / samples.len() as f64).sqrt()
                    };
                    let peak_dbfs = linear_to_db(peak_linear);
                    let rms_dbfs = linear_to_db(rms_linear);

                    (
                        ChannelStats {
                            channel: idx + 1,
                            peak_dbfs,
                            rms_dbfs,
                            crest_factor_db: peak_dbfs - rms_dbfs,
                            full_scale_sample_count,
                            clipped_run_count,
                        },
                        peak_linear,
                        sum_squares,
                        samples.len(),
                    )
                })
                .collect::<Vec<_>>()
        },
        || measure_loudness(decoded),
    );

    let mut per_channel = Vec::with_capacity(decoded.channels);
    let mut overall_peak_linear: f64 = 0.0;
    let mut full_scale_sample_count_total = 0usize;
    let mut clipped_run_count_total = 0usize;
    let mut total_sum_sq: f64 = 0.0;
    let mut total_samples: usize = 0;

    // Folded back in channel order, and the sum of squares is accumulated in the same
    // order as before, so the pooled RMS is bit-identical to the sequential version.
    for (stats, peak_linear, sum_squares, len) in channel_results {
        overall_peak_linear = overall_peak_linear.max(peak_linear);
        full_scale_sample_count_total += stats.full_scale_sample_count;
        clipped_run_count_total += stats.clipped_run_count;
        total_sum_sq += sum_squares;
        total_samples += len;
        per_channel.push(stats);
    }

    let overall_rms_linear = if total_samples == 0 {
        0.0
    } else {
        (total_sum_sq / total_samples as f64).sqrt()
    };

    let loudness = loudness?;

    Ok(SignalAnalysis {
        loudness_layout_supported: loudness.loudness_layout_supported,
        peak_dbfs: linear_to_db(overall_peak_linear),
        true_peak_dbtp: loudness.true_peak_dbtp,
        true_peak_oversampling: loudness.true_peak_oversampling,
        rms_dbfs: linear_to_db(overall_rms_linear),
        lufs_integrated: loudness.lufs_integrated,
        loudness_range_lu: loudness.loudness_range_lu,
        full_scale_sample_count_total,
        clipped_run_count_total,
        per_channel,
    })
}

/// LUFS, LRA (EBU R128 / EBU Tech 3342 / ITU-R BS.1770), and True Peak via `ebur128` — see
/// AGENTS.md "Décisions actées". Do not hand-roll K-weighting, gating, or oversampling here.
///
/// Runs on two meters rather than one. A single `Mode::I | Mode::LRA | Mode::TRUE_PEAK`
/// meter does both the K-weighting filter chain and the 4x polyphase oversampling in one
/// sequential traversal, and that traversal was 53% of the whole pipeline. Split, each
/// meter does strictly less work than the combined one (the loudness meter skips
/// oversampling, the true-peak meter skips K-weighting and gating) and the two run
/// concurrently.
///
/// This is a scheduling change, not a numerical one: each mode still sees every sample of
/// every channel through the same library code, so the values are unchanged. What must
/// *not* be done instead is splitting by channel — BS.1770 sums weighted channel powers
/// before gating, so per-channel meters could not be recombined into a correct integrated
/// loudness.
fn measure_loudness(decoded: &DecodedAudio) -> Result<LoudnessMeasurement, String> {
    if decoded.channels == 0 || decoded.sample_rate == 0 {
        return Ok(LoudnessMeasurement {
            loudness_layout_supported: false,
            lufs_integrated: None,
            loudness_range_lu: None,
            true_peak_dbtp: SILENCE_FLOOR_DB,
            true_peak_oversampling: 0,
        });
    }

    let channel_refs: Vec<&[f32]> = decoded
        .channel_samples
        .iter()
        .map(|c| c.as_slice())
        .collect();

    let channel_map = loudness_channel_map(&decoded.channel_layout);
    let run = |mode: Mode, mapping: Option<&[Channel]>| -> Result<EbuR128, String> {
        let mut meter = EbuR128::new(decoded.channels as u32, decoded.sample_rate, mode)
            .map_err(|e| format!("could not initialize loudness meter: {e}"))?;
        if let Some(mapping) = mapping {
            meter
                .set_channel_map(mapping)
                .map_err(|e| format!("could not configure loudness channels: {e}"))?;
        }
        meter
            .add_frames_planar_f32(&channel_refs)
            .map_err(|e| format!("loudness analysis failed: {e}"))?;
        Ok(meter)
    };

    let (loudness_meter, true_peak_meter) = rayon::join(
        || {
            channel_map
                .as_ref()
                .map(|map| run(Mode::I | Mode::LRA, Some(map)))
                .transpose()
        },
        || run(Mode::TRUE_PEAK, None),
    );
    let loudness_meter = loudness_meter?;
    let true_peak_meter = true_peak_meter?;

    let lufs_integrated = loudness_meter
        .as_ref()
        .and_then(|meter| meter.loudness_global().ok())
        .filter(|value| value.is_finite());
    // EBU Tech 3342 measures the spread between the 10th and 95th percentile of gated
    // 3-second short-term windows. A clip too short to fill more than a handful of those
    // windows has no spread to speak of, and `loudness_range()` returns a finite 0.0 for it —
    // which serialized as a real measurement of a perfectly uniform track. The standard names
    // no minimum duration, so this one is a project decision: see [`MIN_LRA_DURATION_S`].
    let duration_s = decoded
        .channel_samples
        .first()
        .map(|c| c.len() as f64 / decoded.sample_rate as f64)
        .unwrap_or(0.0);
    let loudness_range_lu = loudness_meter
        .as_ref()
        .and_then(|meter| meter.loudness_range().ok())
        .filter(|value| {
            value.is_finite() && lufs_integrated.is_some() && duration_s >= MIN_LRA_DURATION_S
        });

    // Propagated, not swallowed. `unwrap_or(0.0)` turned a library failure into a linear
    // peak of zero, which `linear_to_db` rendered as a plausible-looking -120 dBTP on a track
    // that is not remotely silent.
    let mut true_peak_linear = 0.0_f64;
    for ch in 0..decoded.channels {
        let peak = true_peak_meter
            .true_peak(ch as u32)
            .map_err(|e| format!("true peak measurement failed on channel {ch}: {e}"))?;
        true_peak_linear = true_peak_linear.max(peak);
    }

    Ok(LoudnessMeasurement {
        loudness_layout_supported: channel_map.is_some(),
        lufs_integrated,
        loudness_range_lu,
        true_peak_dbtp: linear_to_db(true_peak_linear),
        true_peak_oversampling: true_peak_oversampling(decoded.sample_rate),
    })
}

/// What `ebur128` produced, kept as named fields rather than a tuple now that there are four
/// of them and two are easy to swap by accident.
struct LoudnessMeasurement {
    loudness_layout_supported: bool,
    lufs_integrated: Option<f64>,
    loudness_range_lu: Option<f64>,
    true_peak_dbtp: f64,
    true_peak_oversampling: u32,
}

/// Maps canonical Symphonia speaker positions to BS.1770 weights through ebur128. In
/// particular, 7.1 side channels must not become `Unused`, as they do in its default map.
/// Ambisonic/discrete multichannel audio requires a rendering layout and is left unmeasured.
fn loudness_channel_map(layout: &Channels) -> Option<Vec<Channel>> {
    let positioned = |position| {
        Some(match position {
            Position::FRONT_LEFT => Channel::Left,
            Position::FRONT_RIGHT => Channel::Right,
            Position::FRONT_CENTER => Channel::Center,
            Position::LFE1 | Position::LFE2 => Channel::Unused,
            Position::REAR_LEFT => Channel::Mp135,
            Position::REAR_RIGHT => Channel::Mm135,
            Position::SIDE_LEFT => Channel::Mp090,
            Position::SIDE_RIGHT => Channel::Mm090,
            Position::REAR_CENTER => Channel::Mp180,
            Position::FRONT_LEFT_CENTER => Channel::MpSC,
            Position::FRONT_RIGHT_CENTER => Channel::MmSC,
            Position::FRONT_LEFT_WIDE => Channel::Mp060,
            Position::FRONT_RIGHT_WIDE => Channel::Mm060,
            Position::TOP_CENTER => Channel::Tp000,
            Position::TOP_FRONT_LEFT => Channel::Up030,
            Position::TOP_FRONT_CENTER => Channel::Up000,
            Position::TOP_FRONT_RIGHT => Channel::Um030,
            Position::TOP_REAR_LEFT => Channel::Up135,
            Position::TOP_REAR_CENTER => Channel::Up180,
            Position::TOP_REAR_RIGHT => Channel::Um135,
            Position::TOP_SIDE_LEFT => Channel::Up090,
            Position::TOP_SIDE_RIGHT => Channel::Um090,
            Position::BOTTOM_FRONT_CENTER => Channel::Bp000,
            Position::BOTTOM_FRONT_LEFT => Channel::Bp045,
            Position::BOTTOM_FRONT_RIGHT => Channel::Bm045,
            _ => return None,
        })
    };
    match layout {
        Channels::Positioned(positions) => positions.iter().map(positioned).collect(),
        Channels::Custom(labels) => labels
            .iter()
            .map(|label| match label {
                ChannelLabel::Positioned(position) => positioned(*position),
                _ => None,
            })
            .collect(),
        Channels::Discrete(1) => Some(vec![Channel::Center]),
        Channels::Discrete(2) => Some(vec![Channel::Left, Channel::Right]),
        _ => None,
    }
}

/// The oversampling factor `ebur128` applies at a given rate, per its own documentation:
/// 4x below 96 kHz, 2x from 96 up to 192, and none at 192 kHz and above.
///
/// Reported rather than assumed. At 192 kHz the library measures the sampled peak and the
/// result had still been presented as a true peak — a different quantity, and the one an
/// intersample overshoot hides from.
fn true_peak_oversampling(sample_rate: u32) -> u32 {
    match sample_rate {
        0..=95_999 => 4,
        96_000..=191_999 => 2,
        _ => 1,
    }
}
