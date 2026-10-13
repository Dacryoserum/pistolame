//! FFT-based spectrogram and raw spectral cutoff detection. See
//! `.claude/skills/dsp-correctness/SKILL.md` and
//! `.claude/skills/tauri-ipc-contract/SKILL.md` before touching this file.
//!
//! The spectral cutoff computed here is a **raw measurement**, not a transcode verdict.
//! Scoring lives in `transcode_detect.rs`, which reads these numbers and weighs them against
//! the other indicators. Never phrase this value in the UI as "authentic"/"transcoded".
//!
//! It is also an `Option`: `None` means the sweep found no point where the content stops,
//! which is a different statement from "the content reaches Nyquist" and must never be
//! rendered as one.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex32;
use serde::Serialize;

use crate::decode::DecodedAudio;

const FFT_SIZE: usize = 4096;
const HOP_SIZE: usize = FFT_SIZE / 2; // 50% overlap — standard STFT tradeoff.
const TARGET_TIME_BINS: usize = 600;
const TARGET_FREQUENCY_BINS: usize = 300;
/// Display floor: anything this far below the loudest frame is rendered as silence.
/// Not a measurement threshold — purely a visualization contrast choice.
const DB_FLOOR: f32 = -90.0;
const DB_CEIL: f32 = 0.0;
/// Measurement floor, deliberately far below [`DB_FLOOR`]. These must not be the same
/// number: a lossy encoder's stopband sits *below* -90 dB, so flooring measurements at the
/// display floor would bury the very cutoff this module exists to measure. Only reached by
/// exact digital silence.
const ANALYSIS_FLOOR_DB: f32 = -180.0;
/// A frame this far below the loudest frame carries no usable information about spectral
/// *shape* — it is silence, a fade tail, or a gap between movements. Such frames are
/// excluded from the steady-state envelope entirely. Including them was a real defect:
/// digital silence decodes to exact zeros, and averaging those in raises the apparent
/// noise floor above a lossy encoder's true stopband, which silently switched detection
/// off for any track with a lead-in or fade-out — i.e. most real music.
const SILENT_FRAME_THRESHOLD_DB: f32 = -70.0;
/// A frame's energy must be within this many dB of the spectrum's overall peak to count
/// toward the cutoff — i.e. "the highest frequency that still carries meaningful energy",
/// not "the highest frequency with any energy at all" (FFT leakage/dither means there's
/// always *some* energy in every bin).
///
/// **Peak-relative, so only meaningful for broadly flat material.** Real music puts its
/// spectral peak in the low mids and is 40 dB down by ~5 kHz while still carrying content
/// to Nyquist, so this measure reads ~5 kHz on a perfectly full-bandwidth track. It is kept
/// for the spectrogram's per-frame overlay, where it is explicitly a visual indication, and
/// deliberately no longer drives any verdict — see [`find_spectral_edge`].
const CUTOFF_THRESHOLD_DB: f32 = -40.0;
/// Step used when sweeping for a lowpass edge. Fine enough to land inside a codec's
/// transition band, coarse enough that the sweep stays cheap.
const EDGE_SCAN_STEP_HZ: f64 = 100.0;
/// A drop across the probe window smaller than this is not an edge, just spectral slope.
const EDGE_MIN_DROP_DB: f32 = 12.0;
/// Above a real lowpass there is nothing left: the spectrum drops and *stays* down to
/// Nyquist. Requiring the whole region above the candidate to remain at least this far
/// below the passband is what separates a codec's brick wall from an ordinary dip that the
/// spectrum climbs back out of.
const EDGE_SUSTAINED_DROP_DB: f32 = 10.0;
/// Half-width of the probe window straddling the cutoff, used to measure how abruptly
/// energy falls there — see [`measure_rolloff_steepness`].
///
/// Swept over the corpus at 300/500/1000 Hz before settling here. Narrower windows
/// separate the classes further on synthetic fixtures (300 Hz puts authentic content at
/// ≤12 dB/kHz against 135 for a LAME transcode, vs 12 against 90 at 500 Hz) but that extra
/// margin is bought by assuming the detected cutoff sits exactly on the filter edge, which
/// holds for a clean synthetic brick wall and much less well for real music. 500 Hz keeps
/// a ~5x class separation while averaging over roughly 46 FFT bins, so a slightly
/// misplaced cutoff estimate still straddles the transition.
const STEEPNESS_PROBE_HZ: f64 = 500.0;
/// Probe half-width used to locate where content *ends*, as opposed to how sharply it does
/// so. Wider than [`STEEPNESS_PROBE_HZ`] because a resampler's anti-imaging filter spreads
/// its transition across a couple of kHz — measured at ~2.7 kHz on the 44.1→96 kHz corpus
/// fixture — and at the narrow scale no single window shows enough drop to register, which
/// made an upsampled file report its full declared bandwidth.
const BANDWIDTH_PROBE_HZ: f64 = 2_500.0;
/// No lossy encoder used for music lowpasses below this — LAME's most aggressive presets
/// bottom out around 8-11 kHz. A "cutoff" measured below this is the top of the content's
/// own bandwidth, not a filter edge: a sustained chord or a test tone falls from peak to
/// noise floor within a couple of FFT bins, which any two-point steepness measurement
/// reads as an infinitely steep brick wall. Gating on plausibility here is what stops a
/// pure sine — the most unambiguously authentic signal there is — from being accused.
const MIN_PLAUSIBLE_ENCODER_CUTOFF_HZ: f64 = 8_000.0;
/// How much room the sustained-drop gate needs above a candidate edge before its verdict
/// means anything.
///
/// That gate asks whether the spectrum *stays* down from the candidate all the way to
/// Nyquist. Scanning to `nyquist - probe` let candidates land close enough to the top that
/// the region it averages over was a few hundred Hz wide — at which point "stays down"
/// stops being evidence, because a spectrum that is merely still falling clears it. A
/// genuinely lossless file with a gentle 15 kHz mastering lowpass reported a spurious edge
/// at 21.5 kHz (the very last candidate position) and dropped from "probably authentic" to
/// "indeterminate" on the strength of it. Requiring a full kHz of stopband to measure —
/// roughly 90 FFT bins at 44.1 kHz — costs nothing real: LAME's highest lowpass sits at
/// 20.5 kHz, which still clears this with room to spare.
const MIN_STOPBAND_WIDTH_HZ: f64 = 1_000.0;

/// Highest frequency a 44.1 kHz source can carry. Content above it did not come through a
/// CD-rate lossy encode — see `SpectralAnalysis::above_cd_ceiling_db`.
const CD_CEILING_HZ: f64 = 22_050.0;
/// Bottom of the reference band `above_cd_ceiling_db` is measured against.
const CEILING_REFERENCE_LO_HZ: f64 = 1_000.0;
/// Fraction of Nyquist at which the "is this really hi-res" band starts — the top quarter of
/// what the file declares. See `above_cd_ceiling_db` for why the top and not the whole.
const HI_BAND_FRACTION: f64 = 0.75;

/// How many distinct places in the spectrum the edge scan carries forward to the gates.
///
/// One is not enough: the strongest drop in real music is often a notch the gates correctly
/// reject, and stopping there discards the codec wall sitting a few kHz above it. Five is
/// generous for the number of unrelated spectral accidents a track shows above 8 kHz, and
/// costs two extra band averages apiece.
const MAX_EDGE_CANDIDATES: usize = 5;
/// A real encoder lowpass cuts off *broadband* content: the spectrum runs at a sustained
/// level right up to the edge and then falls off a cliff. Tonal content instead reaches
/// its highest partial and stops, with mostly noise floor underneath. Requiring the octave
/// below the cutoff to be this densely occupied (relative to the level at the edge itself,
/// not to the global peak — real music's spectral peak is down in the low mids) separates
/// "filter edge" from "the content simply ends here".
const EDGE_OCCUPANCY_MIN: f64 = 0.6;
/// How far below the level at the cutoff edge a bin may sit and still count as "occupied"
/// for [`EDGE_OCCUPANCY_MIN`].
const EDGE_OCCUPANCY_TOLERANCE_DB: f32 = 20.0;

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SpectrogramData {
    pub time_bin_count: usize,
    pub frequency_bin_count: usize,
    pub max_frequency_hz: f64,
    pub duration_seconds: f64,
    /// Row-major `[time][frequency]`, dB values quantized to u8 (DB_FLOOR..=DB_CEIL
    /// mapped to 0..=255) and base64-encoded — never a raw JSON float matrix, see
    /// tauri-ipc-contract skill. Decode client-side into a `Uint8Array`.
    pub intensity_base64: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SpectralAnalysis {
    /// Whether any spectral content rose above the numerical measurement floor. False means
    /// silence or insufficient signal, never evidence that a file has full bandwidth.
    pub has_signal: bool,
    /// Highest frequency still carrying meaningful energy, when the wide-probe sweep found
    /// a point where the content actually stops. A raw indicator only — see module docs.
    ///
    /// **`None` when nothing bounded the content**, which is a different statement from
    /// "the content reaches Nyquist" and must not be rendered as one. This used to fall back
    /// to the declared Nyquist, so a bass-only file was reported as carrying 22.05 kHz of
    /// bandwidth at 100% of its rate — a number no measurement supported — and
    /// `sample_rate.rs` took that fabricated ratio for a real one.
    pub spectral_cutoff_hz: Option<f64>,
    /// How abruptly energy drops around the cutoff, in dB per kHz (higher = steeper).
    /// A hard lossy-encoder lowpass produces a steep, narrow-band transition; a natural
    /// mix/mastering rolloff spreads the same dB drop over many kHz. Position alone
    /// (`spectral_cutoff_hz`) cannot tell these apart — real acoustic recordings can
    /// legitimately have a *low* cutoff position (see `.claude/CONTEXT.md`, ~8kHz
    /// measured on a real orchestral track) without being a transcode. A raw indicator
    /// only — see module docs.
    pub rolloff_steepness_db_per_khz: f64,
    /// Position of a sharp, sustained spectral edge. Native filters and codec lowpasses
    /// are ambiguous; neither its presence nor absence establishes provenance.
    pub encoder_edge_hz: Option<f64>,
    /// `spectral_cutoff_hz` computed independently within each of the spectrogram's time
    /// bins (same `global_peak_db` reference throughout, so values are directly
    /// comparable across the track) rather than once over the whole file. Catches a
    /// transcode that only patches in real high-frequency content for part of the track
    /// (e.g. just a loud finale) — the whole-file cutoff alone would average that out and
    /// miss it. Same length/time alignment as `spectrogram.time_bin_count`. Still a raw
    /// measurement, not a verdict.
    pub cutoff_over_time_hz: Vec<f64>,
    /// Standard deviation of `cutoff_over_time_hz` over the bins where a cutoff was
    /// measurable at all, in Hz — how much the top of the content wanders across the track.
    ///
    /// **Reported, never scored.** The obvious-sounding rule ("a codec's lowpass is a fixed
    /// filter, so a stable edge means a codec") was tested against the corpus and comes out
    /// backwards: a mastering lowpass is *also* a fixed filter, and the authentic
    /// naturally-dark fixtures measure a stability of 0 Hz while real LAME and AAC
    /// transcodes wander by 33-252 Hz. Wiring this into the verdict in either direction
    /// would make things worse, not better.
    pub cutoff_stability_hz: f64,
    /// Steady-state level of named bands relative to the loudest. Describes the spectral
    /// shape without attributing a filtering operation to a particular cause.
    pub band_levels_db: Vec<BandLevel>,
    /// How far the region above `encoder_edge_hz` sits below the region below it, in dB.
    /// `None` when no edge was found. This is the "sustained drop" that
    /// [`find_spectral_edge`]'s second gate tests, surfaced so the gate's own evidence is
    /// visible rather than implicit in a pass/fail.
    pub stopband_depth_db: Option<f64>,
    /// Power in the top quarter of the declared band (never below 22.05 kHz), relative to
    /// 1–22.05 kHz. `None` at 44.1/48 kHz or without measurable signal. Added noise can
    /// create ultrasonic content after transcoding; this observation never proves origin.
    pub above_cd_ceiling_db: Option<f64>,
    pub spectrogram: SpectrogramData,
}

/// Steady-state level of one named frequency band.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct BandLevel {
    pub low_hz: f64,
    /// `None` for the band that runs to Nyquist.
    pub high_hz: Option<f64>,
    /// Relative to the loudest band in the same file, so the table is comparable across
    /// files of different absolute levels. Always ≤ 0.
    pub level_db: f64,
}

/// Band edges for [`SpectralAnalysis::band_levels_db`], in Hz. Roughly octave-spaced in the
/// region where a codec lowpass can sit, coarser below it where nothing interesting to this
/// tool happens. The last band is closed at Nyquist per file.
const REPORTED_BAND_EDGES_HZ: [f64; 8] = [
    0.0, 500.0, 2_000.0, 6_000.0, 10_000.0, 14_000.0, 18_000.0, 20_000.0,
];

pub fn analyze_spectrum(decoded: &DecodedAudio) -> Result<SpectralAnalysis, String> {
    if decoded.sample_rate == 0 || decoded.channels == 0 {
        return Err("cannot compute spectrum: no decoded audio".to_string());
    }

    // Shortest channel bounds the analysis: a truncated final packet can leave one channel
    // a sample longer than another, and reading past the short one would panic.
    let sample_len = decoded
        .channel_samples
        .iter()
        .map(|c| c.len())
        .min()
        .unwrap_or(0);
    if sample_len < FFT_SIZE {
        return Err("file too short for spectral analysis".to_string());
    }

    let nyquist_hz = decoded.sample_rate as f64 / 2.0;
    let raw_bin_count = FFT_SIZE / 2; // Nyquist bin excluded (real-input FFT symmetry).

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let window = hann_window(FFT_SIZE);

    let frame_count = (sample_len - FFT_SIZE) / HOP_SIZE + 1;

    // One contiguous `[frame][bin]` buffer rather than a `Vec<Vec<f32>>`. The nested form
    // meant one heap allocation per frame — 22 500 of them on an 8-minute 96 kHz file — and
    // scattered the rows, which hurts the column-wise scans below (they walk one bin across
    // every frame). Frames are independent, so they also fill in parallel; the FFT plan is
    // shared and each worker keeps its own scratch buffer.
    let mut frames_db = vec![0.0f32; frame_count * raw_bin_count];
    let channels = &decoded.channel_samples;
    let mut scratch = vec![Complex32::new(0.0, 0.0); FFT_SIZE];
    let mut fft_scratch = vec![Complex32::new(0.0, 0.0); fft.get_inplace_scratch_len()];
    let mut channel_power = vec![0.0f64; raw_bin_count];

    // Left sequential on purpose, having measured the alternative. Filling these frames in
    // parallel does cut this stage's own latency (~850ms to ~620ms), but the stage is not on
    // the critical path — `signal_analysis` is, and its true-peak meter is a single
    // sequential filter chain that cannot be split. Parallelising here just takes workers
    // away from that chain, which made total wall time *worse* (3.67s to 3.88s) while
    // raising total CPU. The whole-pipeline win came from running the four stages
    // concurrently in `analysis.rs`; subdividing further only competes with it.
    for frame_idx in 0..frame_count {
        let start = frame_idx * HOP_SIZE;

        // Average channel powers after the transform. A waveform downmix cancels L=-R,
        // hiding real audio and its codec fingerprints. Inverting any channel now leaves
        // the measurement invariant. This is an unweighted spectrum, not a BS.1770 downmix.
        channel_power.fill(0.0);
        for channel in channels {
            for (slot, (&sample, &weight)) in scratch
                .iter_mut()
                .zip(channel[start..start + FFT_SIZE].iter().zip(&window))
            {
                *slot = Complex32::new(sample * weight, 0.0);
            }
            fft.process_with_scratch(&mut scratch, &mut fft_scratch);
            for (power, bin) in channel_power.iter_mut().zip(&scratch) {
                *power += (bin.re as f64).powi(2) + (bin.im as f64).powi(2);
            }
        }
        let out = &mut frames_db[frame_idx * raw_bin_count..(frame_idx + 1) * raw_bin_count];
        for (&power, slot) in channel_power.iter().zip(out.iter_mut()) {
            let amplitude = (power / channels.len() as f64).sqrt() / (FFT_SIZE as f64 / 2.0);
            *slot = linear_to_db(amplitude as f32);
        }
    }

    let global_peak_db = frames_db.iter().copied().fold(f32::MIN, f32::max);
    let has_signal = global_peak_db > ANALYSIS_FLOOR_DB;
    let mean_db = mean_spectrum(&frames_db, raw_bin_count);

    // Two scans at two scales, because "is there a codec brick wall" and "where does the
    // content actually end" are different questions — see `find_spectral_edge`.
    //
    // Steepness comes from the narrow probe: it measures a near-vertical codec edge sharply
    // and stays blind to gentler slopes, which is what makes it a usable transcode signal.
    let encoder_edge = find_spectral_edge(&mean_db, raw_bin_count, nyquist_hz, STEEPNESS_PROBE_HZ);
    let encoder_edge_hz = encoder_edge.map(|(edge_hz, _)| edge_hz);
    let rolloff_steepness_db_per_khz = encoder_edge.map(|(_, steepness)| steepness).unwrap_or(0.0);

    // Bandwidth comes from the wide probe, which also catches a resampler's broader
    // transition. `None` when nothing bounds the content — reported as such rather than
    // rounded up to Nyquist, see `SpectralAnalysis::spectral_cutoff_hz`.
    let spectral_cutoff_hz =
        find_spectral_edge(&mean_db, raw_bin_count, nyquist_hz, BANDWIDTH_PROBE_HZ)
            .map(|(edge_hz, _)| edge_hz);

    let time_bin_count = TARGET_TIME_BINS.min(frame_count);
    let frequency_bin_count = TARGET_FREQUENCY_BINS.min(raw_bin_count);
    let intensity = downsample_and_quantize(
        &frames_db,
        raw_bin_count,
        time_bin_count,
        frequency_bin_count,
    );
    let cutoff_over_time_hz = cutoff_over_time(
        &frames_db,
        raw_bin_count,
        nyquist_hz,
        time_bin_count,
        global_peak_db,
    );

    // Measured on the same steady-state envelope the verdict is derived from, so the table
    // a reader inspects is literally the evidence, not a second pass that could disagree.
    let band_levels_db = reported_band_levels(&mean_db, raw_bin_count, nyquist_hz);
    let stopband_depth_db = encoder_edge_hz.map(|edge_hz| {
        let passband = band_level_db(
            &mean_db,
            edge_hz - STEEPNESS_PROBE_HZ,
            edge_hz,
            raw_bin_count,
            nyquist_hz,
        );
        let stopband = band_level_db(
            &mean_db,
            edge_hz + STEEPNESS_PROBE_HZ,
            nyquist_hz,
            raw_bin_count,
            nyquist_hz,
        );
        (passband - stopband) as f64
    });

    // The *top* of the declared band rather than everything above the ceiling, and the
    // difference decides whether the measurement works at all. Everything-above-the-ceiling
    // put the corpus's genuine music-like 96 kHz fixture at -17.7 dB and its upsampled
    // fixture at -22.0: 4 dB apart, which is not a separation. An upsampler's anti-imaging
    // filter leaves the top of the new band empty by construction, so reading the top
    // quarter instead moves them to -18.6 and -47.7 — 29 dB apart, and for a structural
    // reason rather than a lucky one.
    //
    // Reference band deliberately starts at 1 kHz rather than 0: the low end of music carries
    // most of its energy, and measuring the top against that would report every ordinary
    // file as empty up there.
    let above_cd_ceiling_db = (has_signal && decoded.sample_rate > 48_000).then(|| {
        let hi_lo = (nyquist_hz * HI_BAND_FRACTION).max(CD_CEILING_HZ);
        let hi = band_level_db(&mean_db, hi_lo, nyquist_hz, raw_bin_count, nyquist_hz);
        let reference = band_level_db(
            &mean_db,
            CEILING_REFERENCE_LO_HZ,
            CD_CEILING_HZ,
            raw_bin_count,
            nyquist_hz,
        );
        (hi - reference) as f64
    });

    Ok(SpectralAnalysis {
        has_signal,
        spectral_cutoff_hz,
        rolloff_steepness_db_per_khz,
        encoder_edge_hz,
        cutoff_stability_hz: cutoff_stability(&cutoff_over_time_hz),
        cutoff_over_time_hz,
        band_levels_db,
        stopband_depth_db,
        above_cd_ceiling_db,
        spectrogram: SpectrogramData {
            time_bin_count,
            frequency_bin_count,
            max_frequency_hz: nyquist_hz,
            duration_seconds: sample_len as f64 / decoded.sample_rate as f64,
            intensity_base64: base64_encode(&intensity),
        },
    })
}

/// Standard deviation of the per-time-bin cutoff, over the bins where one was measurable.
///
/// Bins reading exactly 0 are excluded rather than averaged in as zero: `detect_cutoff_in_frames`
/// returns 0 for a window with no energy within the threshold at all (silence, a gap between
/// movements), and counting those as "the content ends at DC" would report a track with a
/// lead-in as wildly unstable purely because of its lead-in.
fn cutoff_stability(cutoff_over_time_hz: &[f64]) -> f64 {
    let measured: Vec<f64> = cutoff_over_time_hz
        .iter()
        .copied()
        .filter(|&hz| hz > 0.0)
        .collect();
    if measured.len() < 2 {
        return 0.0;
    }
    let mean = measured.iter().sum::<f64>() / measured.len() as f64;
    let variance =
        measured.iter().map(|&hz| (hz - mean).powi(2)).sum::<f64>() / measured.len() as f64;
    variance.sqrt()
}

/// Level of each [`REPORTED_BAND_EDGES_HZ`] band, normalized against the loudest band so the
/// table reads the same whatever the file's absolute level. Bands starting at or above
/// Nyquist are dropped rather than reported at the floor — a 44.1 kHz file has no 20 kHz+
/// band to describe, and printing one at -180 dB would look like a finding.
fn reported_band_levels(mean_db: &[f32], raw_bin_count: usize, nyquist_hz: f64) -> Vec<BandLevel> {
    let mut bands: Vec<BandLevel> = Vec::new();
    for (i, &low_hz) in REPORTED_BAND_EDGES_HZ.iter().enumerate() {
        if low_hz >= nyquist_hz {
            break;
        }
        let next = REPORTED_BAND_EDGES_HZ.get(i + 1).copied();
        let high_hz = next.filter(|&hz| hz < nyquist_hz);
        let upper = high_hz.unwrap_or(nyquist_hz);
        bands.push(BandLevel {
            low_hz,
            high_hz,
            level_db: band_level_db(mean_db, low_hz, upper, raw_bin_count, nyquist_hz) as f64,
        });
    }

    let loudest = bands
        .iter()
        .map(|b| b.level_db)
        .fold(f64::NEG_INFINITY, f64::max);
    if loudest.is_finite() {
        for band in &mut bands {
            band.level_db -= loudest;
        }
    }
    bands
}

/// Number of frames held in a flat `[frame][bin]` buffer.
fn frame_count_of(frames_db: &[f32], raw_bin_count: usize) -> usize {
    frames_db.len() / raw_bin_count
}

fn hann_window(size: usize) -> Vec<f32> {
    (0..size)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (size - 1) as f32).cos())
        .collect()
}

fn linear_to_db(magnitude: f32) -> f32 {
    if magnitude <= 0.0 {
        ANALYSIS_FLOOR_DB
    } else {
        (20.0 * magnitude.log10()).max(ANALYSIS_FLOOR_DB)
    }
}

/// Highest frequency where any frame in `frames` comes within `CUTOFF_THRESHOLD_DB` of
/// `peak_db` — scans from Nyquist downward so the result is "highest surviving
/// frequency", not "first frequency that happens to be loud". `peak_db` is passed in
/// (rather than computed from `frames`) so callers can share one whole-file reference
/// across many windows — see [`cutoff_over_time`].
fn detect_cutoff_in_frames(
    frames: &[f32],
    peak_db: f32,
    raw_bin_count: usize,
    nyquist_hz: f64,
) -> f64 {
    if peak_db <= DB_FLOOR || frames.is_empty() {
        return 0.0;
    }
    let threshold = peak_db + CUTOFF_THRESHOLD_DB;

    for bin in (0..raw_bin_count).rev() {
        let bin_peak_db = frames
            .chunks_exact(raw_bin_count)
            .map(|frame| frame[bin])
            .fold(f32::MIN, f32::max);
        if bin_peak_db >= threshold {
            return bin as f64 / raw_bin_count as f64 * nyquist_hz;
        }
    }
    0.0
}

/// `detect_cutoff_in_frames`, computed independently within each of `time_bin_count`
/// windows over `frames_db` — same window boundaries as `downsample_and_quantize`, so the
/// result lines up with the spectrogram's time axis. See `SpectralAnalysis::cutoff_over_time_hz`.
fn cutoff_over_time(
    frames_db: &[f32],
    raw_bin_count: usize,
    nyquist_hz: f64,
    time_bin_count: usize,
    global_peak_db: f32,
) -> Vec<f64> {
    let frame_count = frame_count_of(frames_db, raw_bin_count);
    (0..time_bin_count)
        .map(|t| {
            let frame_start = t * frame_count / time_bin_count;
            let frame_end = ((t + 1) * frame_count / time_bin_count)
                .max(frame_start + 1)
                .min(frame_count);
            detect_cutoff_in_frames(
                &frames_db[frame_start * raw_bin_count..frame_end * raw_bin_count],
                global_peak_db,
                raw_bin_count,
                nyquist_hz,
            )
        })
        .collect()
}

/// Steady-state spectral envelope, averaged in the **power** domain over the frames that
/// actually carry signal.
///
/// Two things here are deliberate and were both bugs in the first implementation:
///
/// - **Power-domain, not dB-domain, averaging.** Averaging dB values computes a geometric
///   mean of power, which is dominated by the quietest frames rather than representing the
///   track's actual energy distribution.
/// - **Near-silent frames are skipped.** Digital silence decodes to exact zeros, i.e. the
///   measurement floor, in *every* bin. Averaging those in lifts the whole envelope's
///   noise floor toward that floor and flattens the contrast the cutoff measurement
///   depends on. Concretely: prepending a few seconds of silence to a known LAME 128
///   transcode used to drop its measured steepness from 191 dB/kHz to 0, flipping the
///   verdict from "probably transcoded" to "indeterminate" — a silent false negative on
///   almost every real track, since nearly all of them start or end quiet.
///
/// Mean over time rather than max: max-over-time is right for the visualization (it
/// preserves transients) but overstates how much energy *survives* at a given frequency,
/// which is what a sustained rolloff shape is about.
fn mean_spectrum(frames_db: &[f32], raw_bin_count: usize) -> Vec<f32> {
    let frame_level = |frame: &[f32]| frame.iter().copied().fold(f32::MIN, f32::max);
    let loudest = frames_db
        .chunks_exact(raw_bin_count)
        .map(frame_level)
        .fold(f32::MIN, f32::max);
    let silence_cutoff = loudest + SILENT_FRAME_THRESHOLD_DB;

    let mut power = vec![0.0f64; raw_bin_count];
    let mut counted = 0usize;
    for frame in frames_db
        .chunks_exact(raw_bin_count)
        .filter(|f| frame_level(f) > silence_cutoff)
    {
        for (p, &db) in power.iter_mut().zip(frame.iter()) {
            *p += 10f64.powf(db as f64 / 10.0);
        }
        counted += 1;
    }

    // Every frame was silence (a digitally silent file): nothing to characterize.
    if counted == 0 {
        return vec![ANALYSIS_FLOOR_DB; raw_bin_count];
    }

    power
        .iter()
        .map(|&p| {
            let mean = p / counted as f64;
            if mean <= 0.0 {
                ANALYSIS_FLOOR_DB
            } else {
                (10.0 * mean.log10()).max(ANALYSIS_FLOOR_DB as f64) as f32
            }
        })
        .collect()
}

/// Mean power level (in dB) of the steady-state envelope across a frequency band.
fn band_level_db(
    mean_db: &[f32],
    lo_hz: f64,
    hi_hz: f64,
    raw_bin_count: usize,
    nyquist_hz: f64,
) -> f32 {
    let to_bin = |hz: f64| {
        ((hz / nyquist_hz * raw_bin_count as f64).round() as isize).clamp(0, raw_bin_count as isize)
            as usize
    };
    let lo = to_bin(lo_hz);
    let hi = to_bin(hi_hz).max(lo + 1).min(raw_bin_count);
    if lo >= hi {
        return ANALYSIS_FLOOR_DB;
    }

    let sum: f64 = mean_db[lo..hi]
        .iter()
        .map(|&db| 10f64.powf(db as f64 / 10.0))
        .sum();
    let mean = sum / (hi - lo) as f64;
    if mean <= 0.0 {
        ANALYSIS_FLOOR_DB
    } else {
        (10.0 * mean.log10()).max(ANALYSIS_FLOOR_DB as f64) as f32
    }
}

/// Fraction of bins in the octave below `cutoff_hz` sitting within
/// [`EDGE_OCCUPANCY_TOLERANCE_DB`] of the level at the cutoff edge itself.
///
/// This is the test that tells a filter edge apart from "the content simply ran out".
/// A lossy encoder's lowpass truncates broadband material, so the band leading up to it is
/// densely filled; a chord or a test tone reaches its top partial with nothing but noise
/// floor underneath. Referenced to the edge level rather than the spectrum's global peak
/// on purpose — real music peaks down in the low mids, so a peak-relative test would
/// report near-zero occupancy for every track and disable detection wholesale.
fn edge_occupancy(mean_db: &[f32], cutoff_hz: f64, raw_bin_count: usize, nyquist_hz: f64) -> f64 {
    let edge_level = band_level_db(
        mean_db,
        cutoff_hz * 0.9,
        cutoff_hz,
        raw_bin_count,
        nyquist_hz,
    );
    let threshold = edge_level - EDGE_OCCUPANCY_TOLERANCE_DB;

    let to_bin = |hz: f64| {
        ((hz / nyquist_hz * raw_bin_count as f64).round() as isize).clamp(0, raw_bin_count as isize)
            as usize
    };
    let lo = to_bin(cutoff_hz * 0.5);
    let hi = to_bin(cutoff_hz).max(lo + 1).min(raw_bin_count);
    if lo >= hi {
        return 0.0;
    }

    let occupied = mean_db[lo..hi]
        .iter()
        .filter(|&&db| db >= threshold)
        .count();
    occupied as f64 / (hi - lo) as f64
}

/// Where a lossy encoder's lowpass sits, found by sweeping the whole plausible range rather
/// than trusting one position.
///
/// The earlier design measured steepness only at `spectral_cutoff_hz`, the highest bin
/// within 40 dB of the spectrum's *peak*. That reference is only sound for flat material.
/// Real music peaks in the low mids, so it is 40 dB down by around 5 kHz while still
/// carrying content all the way up — which meant the measurement was taken at ~5 kHz, far
/// from any encoder edge, and the "spectral content reaches Nyquist" test for authenticity
/// (cutoff/Nyquist ≥ 0.92) was unreachable for anything that sounded like music. Every real
/// FLAC came out `Indeterminate` at 30%, and genuine hi-res was additionally misreported as
/// upsampled because the same number fed `sample_rate.rs`. The synthetic corpus never
/// showed it: white noise is flat, so for those fixtures the peak-relative point really is
/// at Nyquist.
///
/// Returns `(edge_hz, steepness_db_per_khz)`, or `None` when no edge survives the gates —
/// which is the positive evidence that no encoder lowpass is present.
/// `probe_hz` sets the scale the edge is looked for at, and the two scales answer different
/// questions. A codec's brick wall is near-vertical, so the narrow probe measures its
/// steepness sharply. A resampler's anti-imaging filter is comparatively wide — the
/// 44.1→96 kHz corpus fixture spreads its transition over roughly 2.7 kHz — so at the narrow
/// scale no single window ever shows a large enough drop and the edge is missed entirely.
/// Scanning again with a wide probe finds where content actually ends, which is what the
/// sample-rate check needs; using the narrow result for it reported that fixture as
/// full-bandwidth 96 kHz.
fn find_spectral_edge(
    mean_db: &[f32],
    raw_bin_count: usize,
    nyquist_hz: f64,
    probe_hz: f64,
) -> Option<(f64, f64)> {
    // Stop far enough below Nyquist that the sustained-drop gate below still has a real
    // band to average over — see MIN_STOPBAND_WIDTH_HZ.
    let scan_end = nyquist_hz - probe_hz - MIN_STOPBAND_WIDTH_HZ;
    if MIN_PLAUSIBLE_ENCODER_CUTOFF_HZ >= scan_end {
        return None;
    }

    // Every drop across the probe window that is large enough to be worth testing, strongest
    // first. Keeping several rather than only the strongest is what stops one deep musical
    // notch from masking the real edge: the notch wins the scan on depth, fails the gates
    // below, and used to take the whole file down with it — the codec wall a few kHz further
    // up was never examined. Real music has more than one spectral accident in it.
    let mut candidates: Vec<(f64, f32)> = Vec::new();
    let mut candidate = MIN_PLAUSIBLE_ENCODER_CUTOFF_HZ;
    while candidate <= scan_end {
        let below = band_level_db(
            mean_db,
            candidate - probe_hz,
            candidate,
            raw_bin_count,
            nyquist_hz,
        );
        let above = band_level_db(
            mean_db,
            candidate,
            candidate + probe_hz,
            raw_bin_count,
            nyquist_hz,
        );
        let drop = below - above;
        if drop >= EDGE_MIN_DROP_DB {
            candidates.push((candidate, drop));
        }
        candidate += EDGE_SCAN_STEP_HZ;
    }
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1));

    // Adjacent scan steps on one edge all clear the threshold, so the raw list is mostly the
    // same wall described several times over. Thinning it keeps the shortlist covering
    // genuinely distinct places in the spectrum rather than one place at sub-step resolution.
    let mut shortlist: Vec<(f64, f32)> = Vec::new();
    for (hz, drop) in candidates {
        if shortlist
            .iter()
            .all(|(kept, _)| (kept - hz).abs() >= probe_hz)
        {
            shortlist.push((hz, drop));
        }
        if shortlist.len() == MAX_EDGE_CANDIDATES {
            break;
        }
    }

    // Gates, in descending order of drop: the first candidate that passes both is the edge.
    for (edge_hz, drop) in shortlist {
        // Gate 1: the band leading up to the edge must be broadband, not the last partial of
        // tonal content — see `edge_occupancy`.
        if edge_occupancy(mean_db, edge_hz, raw_bin_count, nyquist_hz) < EDGE_OCCUPANCY_MIN {
            continue;
        }

        // Gate 2: everything above must stay down. A codec lowpass leaves an empty stopband
        // all the way to Nyquist; a dip the spectrum recovers from is program material, not a
        // filter.
        let passband = band_level_db(
            mean_db,
            edge_hz - probe_hz,
            edge_hz,
            raw_bin_count,
            nyquist_hz,
        );
        let stopband = band_level_db(
            mean_db,
            edge_hz + probe_hz,
            nyquist_hz,
            raw_bin_count,
            nyquist_hz,
        );
        if passband - stopband < EDGE_SUSTAINED_DROP_DB {
            continue;
        }

        return Some((edge_hz, drop as f64 / (probe_hz / 1000.0)));
    }

    None
}

/// Max-pools over time (preserves transient peaks for the visualization) and mean-pools
/// over frequency (avoids single-bin noise causing visual banding), then quantizes to u8
/// against a fixed dB range for a consistent look across files.
fn downsample_and_quantize(
    frames_db: &[f32],
    raw_bin_count: usize,
    time_bin_count: usize,
    frequency_bin_count: usize,
) -> Vec<u8> {
    let frame_count = frame_count_of(frames_db, raw_bin_count);
    let mut out = vec![0u8; time_bin_count * frequency_bin_count];

    for t in 0..time_bin_count {
        let frame_start = t * frame_count / time_bin_count;
        let frame_end = ((t + 1) * frame_count / time_bin_count)
            .max(frame_start + 1)
            .min(frame_count);
        let window = &frames_db[frame_start * raw_bin_count..frame_end * raw_bin_count];

        for f in 0..frequency_bin_count {
            let bin_start = f * raw_bin_count / frequency_bin_count;
            let bin_end = ((f + 1) * raw_bin_count / frequency_bin_count)
                .max(bin_start + 1)
                .min(raw_bin_count);

            let mut max_over_time = DB_FLOOR;
            for frame in window.chunks_exact(raw_bin_count) {
                let mean_over_freq: f32 =
                    frame[bin_start..bin_end].iter().sum::<f32>() / (bin_end - bin_start) as f32;
                max_over_time = max_over_time.max(mean_over_freq);
            }

            let normalized = ((max_over_time - DB_FLOOR) / (DB_CEIL - DB_FLOOR)).clamp(0.0, 1.0);
            out[t * frequency_bin_count + f] = (normalized * 255.0).round() as u8;
        }
    }

    out
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}
