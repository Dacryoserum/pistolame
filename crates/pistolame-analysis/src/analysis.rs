//! Runs the full analysis pipeline (decode → signal → dynamic range → spectral →
//! transcode assessment) and assembles the result. Shared by the Tauri command
//! (`commands::analyze_file`) and the CLI binary (`bin/nyquist-cli.rs`) so the two never
//! drift apart — see `.claude/skills/tauri-ipc-contract/SKILL.md` before changing the
//! shape of [`AnalysisResult`].

use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::bit_depth::{self, BitDepthAnalysis};
use crate::decode::{self, DecodeStatus};
use crate::dynamic_range::{self, DynamicRangeResult};
use crate::mdct_grid::{self, MdctGridAnalysis};
use crate::metadata::{self, FileInfo};
use crate::sample_rate::{self, SampleRateAnalysis};
use crate::signal_analysis::{self, SignalAnalysis};
use crate::spectral::{self, SpectralAnalysis};
use crate::stereo::{self, StereoAnalysis};
use crate::tags::EncoderTagMatch;
use crate::transcode_detect::{self, TranscodeAssessment};

/// Version of the analysis pipeline that produced a report.
///
/// Carried in the payload so an exported JSON says which build's numbers it holds: thresholds
/// and verdict logic move between releases, and a report read six months later is otherwise
/// impossible to interpret. Tracks the crate version for released analysis reports.
pub const ANALYSIS_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct AnalysisResult {
    /// Which build of the pipeline produced these numbers — see [`ANALYSIS_VERSION`].
    pub analysis_version: &'static str,
    /// Whether the whole file reached the analysis. Anything but `complete` means every
    /// measurement here describes a fragment, and `transcode_assessment` withholds its
    /// verdict accordingly — see decode.rs.
    pub decode_status: DecodeStatus,
    pub file_info: FileInfo,
    pub signal_analysis: SignalAnalysis,
    pub dynamic_range: DynamicRangeResult,
    pub spectral_analysis: SpectralAnalysis,
    pub transcode_assessment: TranscodeAssessment,
    pub encoder_tag_matches: Vec<EncoderTagMatch>,
    /// A separate quality issue from `transcode_assessment` — a file can be padded to a
    /// wider bit depth without ever having been lossy-compressed. See bit_depth.rs.
    pub bit_depth_analysis: BitDepthAnalysis,
    /// Bandwidth occupancy only: native filtering cannot be distinguished from upsampling.
    pub sample_rate_analysis: SampleRateAnalysis,
    /// `None` for anything that is not exactly two channels. Reported information only —
    /// see stereo.rs on why the stereo image does *not* feed the transcode verdict.
    pub stereo_analysis: Option<StereoAnalysis>,
    /// AAC encoder frame-grid alignment. Unlike the stereo image, this one *does* feed the
    /// verdict — see mdct_grid.rs.
    pub mdct_grid: MdctGridAnalysis,
}

/// Wall-clock cost of each pipeline stage. Deliberately **not** part of
/// [`AnalysisResult`]: it is a profiling aid for the CLI (`nyquist-cli --timing`) and for
/// deciding whether a stage has grown slow enough to need progress reporting, not part of
/// the IPC contract the frontend depends on.
#[derive(Debug, Clone, Copy, Default)]
pub struct StageTimings {
    pub decode: Duration,
    pub signal: Duration,
    pub dynamic_range: Duration,
    pub spectral: Duration,
    pub bit_depth: Duration,
    pub stereo: Duration,
    pub mdct_grid: Duration,
    pub total: Duration,
}

/// Decodes and analyzes the complete file without retaining its samples.
pub fn analyze(path: &Path) -> Result<AnalysisResult, String> {
    analyze_full(path, &|| {}).map(|(result, _, _)| result)
}

/// [`analyze`], plus how long each stage took.
pub fn analyze_with_timings(path: &Path) -> Result<(AnalysisResult, StageTimings), String> {
    analyze_full(path, &|| {}).map(|(result, timings, _)| (result, timings))
}

/// [`analyze`], plus the decoded audio itself, so the caller can play it without decoding
/// the file a second time.
///
/// The samples are handed over rather than dropped because playback needs exactly what the
/// analysis already produced: same length, same sample rate, same clock. Decoding twice is
/// how the app ended up with two disagreeing timelines in the first place — see player.rs.
pub fn analyze_with_audio(path: &Path) -> Result<(AnalysisResult, decode::DecodedAudio), String> {
    analyze_full(path, &|| {}).map(|(result, _, decoded)| (result, decoded))
}

/// Number of completed stages emitted by [`analyze_with_progress`].
pub const PROGRESS_STAGE_COUNT: usize = 7;

/// Retains playback samples and reports each completed stage. The callback can run on
/// concurrent Rayon workers and must not block or assume a fixed stage completion order.
pub fn analyze_with_progress(
    path: &Path,
    progress: &(dyn Fn() + Sync),
) -> Result<(AnalysisResult, decode::DecodedAudio), String> {
    analyze_full(path, progress).map(|(result, _, decoded)| (result, decoded))
}

/// The one body every entry point above goes through, so the timed, untimed and
/// with-audio paths can never diverge.
fn analyze_full(
    path: &Path,
    progress: &(dyn Fn() + Sync),
) -> Result<(AnalysisResult, StageTimings, decode::DecodedAudio), String> {
    let mut timings = StageTimings::default();
    let started = Instant::now();

    let stage = Instant::now();
    let decoded = decode::decode_file(path)?;
    timings.decode = stage.elapsed();
    progress();

    let file_info = metadata::build_file_info(path, &decoded)?;

    // Everything past the decode reads `decoded` and nothing else, so these four stages are
    // independent of one another and only ran in sequence out of habit. Wall time for the
    // group is now the slowest member rather than their sum.
    //
    // Each is still timed individually, so `--timing` reports the cost of the work rather
    // than the elapsed span, and a stage that grows expensive is still visible even when it
    // no longer sits on the critical path.
    let ((signal_analysis, dynamic_range), (spectral_analysis, bit_depth_analysis)) = rayon::join(
        || {
            rayon::join(
                || {
                    let stage = Instant::now();
                    let out = signal_analysis::analyze_signal(&decoded);
                    progress();
                    (out, stage.elapsed())
                },
                || {
                    let stage = Instant::now();
                    let out = dynamic_range::compute_dr14(&decoded);
                    progress();
                    (out, stage.elapsed())
                },
            )
        },
        || {
            rayon::join(
                || {
                    let stage = Instant::now();
                    let out = spectral::analyze_spectrum(&decoded);
                    progress();
                    (out, stage.elapsed())
                },
                || {
                    rayon::join(
                        || {
                            let stage = Instant::now();
                            let out = bit_depth::analyze_bit_depth(&decoded);
                            progress();
                            (out, stage.elapsed())
                        },
                        || {
                            rayon::join(
                                || {
                                    let stage = Instant::now();
                                    let out = stereo::analyze_stereo(&decoded);
                                    progress();
                                    (out, stage.elapsed())
                                },
                                || {
                                    let stage = Instant::now();
                                    let out = mdct_grid::analyze_mdct_grid(&decoded);
                                    progress();
                                    (out, stage.elapsed())
                                },
                            )
                        },
                    )
                },
            )
        },
    );

    let (signal_analysis, signal_elapsed) = signal_analysis;
    let (dynamic_range, dr_elapsed) = dynamic_range;
    let (spectral_analysis, spectral_elapsed) = spectral_analysis;
    let (
        (bit_depth_analysis, bit_depth_elapsed),
        ((stereo_analysis, stereo_elapsed), (mdct_grid, mdct_elapsed)),
    ) = bit_depth_analysis;

    timings.signal = signal_elapsed;
    timings.dynamic_range = dr_elapsed;
    timings.spectral = spectral_elapsed;
    timings.bit_depth = bit_depth_elapsed;
    timings.stereo = stereo_elapsed;
    timings.mdct_grid = mdct_elapsed;

    let signal_analysis = signal_analysis?;
    let spectral_analysis = spectral_analysis?;
    let mut transcode_assessment = transcode_detect::assess_transcode_risk(
        &spectral_analysis,
        file_info.nyquist_hz as f64,
        &decoded.encoder_tag_matches,
        &mdct_grid,
        &decoded.codec_short_name,
        &decoded.decode_status,
    );
    if decoded.integrity_verified == Some(false) {
        transcode_assessment.withhold_on_integrity_failure();
    }
    let sample_rate_analysis = sample_rate::analyze_sample_rate(
        file_info.sample_rate_hz,
        spectral_analysis.spectral_cutoff_hz,
    );
    let encoder_tag_matches = decoded.encoder_tag_matches.clone();
    timings.total = started.elapsed();

    Ok((
        AnalysisResult {
            analysis_version: ANALYSIS_VERSION,
            decode_status: decoded.decode_status,
            file_info,
            signal_analysis,
            dynamic_range,
            spectral_analysis,
            transcode_assessment,
            encoder_tag_matches,
            bit_depth_analysis,
            sample_rate_analysis,
            stereo_analysis,
            mdct_grid,
        },
        timings,
        decoded,
    ))
}
