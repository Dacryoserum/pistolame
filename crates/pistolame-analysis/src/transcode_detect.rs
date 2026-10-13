//! Evidence-based assessment of a possible lossy history in decoded audio.
//!
//! A filter edge is compatible with mastering and resampling; ultrasonic energy can be
//! added after compression. Neither establishes provenance. Only a confirmed codec-grid
//! hypothesis currently produces a lossy inference. Tags and spectral measurements remain
//! visible observations, and an inconclusive result is a legitimate outcome.
//!
//! See docs/detection-research.md and the transcode-heuristic-validation skill. The weights
//! are evidence strengths, never calibrated probabilities. No method currently supports a
//! positive authenticity verdict; the reserved state must not be inferred from clean tests.

use serde::Serialize;

use crate::decode::DecodeStatus;
use crate::mdct_grid::MdctGridAnalysis;
use crate::spectral::SpectralAnalysis;
use crate::tags::EncoderTagMatch;

const NO_EVIDENCE_CONFIDENCE: f64 = 0.3;
const GRID_ONLY_CONFIDENCE: f64 = 0.85;
const GRID_CORROBORATED_CONFIDENCE: f64 = 0.9;
const STEEPNESS_TRANSCODE_THRESHOLD: f64 = 40.0;
const ABOVE_CD_CEILING_MIN_DB: f64 = -30.0;
const CD_NYQUIST_HZ: f64 = 22_050.0;
const MIN_SCANNED_KHZ: f64 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Reserved: no current measurement establishes a lossless source history.
    ProbablyAuthentic,
    ProbablyTranscoded,
    Indeterminate,
    /// The file is in a lossy format and says so. Not a verdict about deception — the
    /// question this module asks does not arise.
    ///
    /// The other three states answer "is this *lossless* file secretly lossy?". Running that
    /// question on an MP3 is a category error, and it produced a genuinely absurd answer: an
    /// ordinary MP3 came out "probably transcoded" at 80%, and an AAC file at 95% because
    /// `mdct_grid` correctly found the encoder grid that is *supposed* to be there. Nothing
    /// is hidden in either case. Every measurement is still reported; only the accusation is
    /// withdrawn.
    DeclaredLossy,
}

/// Codecs that are lossy by definition, so a file in one of them is not hiding anything.
///
/// An explicit list rather than "not a known lossless codec": the failure directions are not
/// symmetric. Wrongly calling a lossy codec lossless leaves the status quo — a nonsensical
/// verdict. Wrongly calling a *lossless* codec lossy would silently switch off the check on
/// a file that needs it, which is far worse. An unrecognized codec therefore keeps the full
/// assessment.
fn is_declared_lossy(codec: &str) -> bool {
    matches!(codec, "mp3" | "mp2" | "mp1" | "aac" | "vorbis" | "opus")
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct TranscodeAssessment {
    pub verdict: Verdict,
    /// Strength of the evidence behind the *stated* verdict, 0.0-1.0, or `None` where the
    /// verdict is not an inference at all ([`Verdict::DeclaredLossy`] — the container says
    /// so outright, and there is nothing to be more or less sure of).
    ///
    /// Deliberately capped well below 1.0 in every branch: this is a small set of
    /// indicators tuned on a small corpus, not a validated statistical classifier — see
    /// module docs. It is **not** a probability, and no surface should render it as one; the
    /// UI shows it as weak/moderate/strong evidence and keeps the number for the JSON
    /// report, where a reader can see what it is.
    pub confidence_score: Option<f64>,
    /// Human-readable evidence, one entry per contributing observation. Always non-empty
    /// — a verdict with no stated indicators is not acceptable in this codebase.
    pub indicators: Vec<Indicator>,
}

/// One piece of stated evidence behind a verdict.
///
/// Carries the same claim twice, on purpose. `message` is the English prose this module
/// authors: it is what `nyquist-cli` prints and what an exported JSON report preserves, so
/// a report stays readable and diffable no matter which language the UI was in when it was
/// produced. `detail` is the same observation as a code plus its raw measurements, which
/// lets the UI re-render the sentence in the user's language (see `src/lib/i18n.svelte.ts`)
/// instead of showing backend English inside a translated interface.
///
/// The prose is derived from the detail by [`IndicatorDetail::english`], never written at
/// the call site, so the two cannot drift apart.
#[derive(Debug, Serialize)]
pub struct Indicator {
    pub message: String,
    #[serde(flatten)]
    pub detail: IndicatorDetail,
}

impl Indicator {
    fn new(detail: IndicatorDetail) -> Self {
        Self {
            message: detail.english(),
            detail,
        }
    }
}

/// The closed set of observations this module can make, with the numbers each one quotes.
///
/// Serialized internally tagged on `code` and flattened into [`Indicator`], so an entry
/// reads `{"message": "...", "code": "sharp_rolloff", "steepness_db_per_khz": 92.4, ...}`.
/// Adding a variant is a frontend-visible change: `src/lib/api.ts` and the translation
/// switch in `src/lib/i18n.svelte.ts` must gain it in the same PR, and `npm run check`
/// fails until they do.
///
/// Frequencies are in kHz rather than Hz because that is the unit every message quotes;
/// keeping the conversion here means the UI formats a number instead of re-deriving one.
#[derive(Debug, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum IndicatorDetail {
    /// A container tag names an encoder that only ever produces lossy output.
    EncoderTagMatched {
        tag_key: String,
        tag_value: String,
        matched_pattern: String,
        /// Further *tags* beyond the one quoted, 0 when it was the only one. Counted by
        /// distinct `(key, value)` pair rather than by pattern hit: one tag naming an
        /// encoder twice ("LAME 3.100 (lame)") is one piece of evidence, not two.
        additional_tags: usize,
    },
    /// Metadata alone cannot establish the audio's processing history.
    TagIsOnlyEvidence,
    /// Silence or an analysis-floor spectrum supplies no spectral evidence.
    InsufficientSignal,
    IntegrityMismatch,
    /// Degenerate input: no usable sample rate, so no spectral claim can be made.
    InvalidSampleRate,
    /// A sharp edge, compatible with both native filtering and codec lowpasses.
    SharpRolloff {
        steepness_db_per_khz: f64,
        edge_khz: f64,
    },
    /// No sharp sustained edge found in the tested frequency range.
    NoEncoderLowpass {
        scanned_from_khz: f64,
        nyquist_khz: f64,
    },
    /// Clean tests cannot establish a lossless history.
    TransparentEncodeUnseen,
    /// An edge exists but is too gradual to attribute to a codec either way.
    GradualRolloff {
        cutoff_khz: f64,
        steepness_db_per_khz: f64,
    },
    /// The file's own MDCT coefficients collapse at one specific frame alignment — an AAC
    /// encoder's grid. Structural evidence, independent of the spectral envelope.
    MdctGridAligned {
        z_score: f64,
        confirmed_z_score: f64,
        window: crate::mdct_grid::MdctWindow,
        frame_offset: usize,
        zero_percent: f64,
        baseline_percent: f64,
    },
    /// No confirmed alignment under the tested AAC hypotheses; AAC is not ruled out.
    MdctGridClear,
    /// Energy above the CD ceiling, which could also have been added after transcoding.
    ContentAboveCdCeiling {
        level_db: f64,
        ceiling_khz: f64,
    },
    /// The container declares a lossy codec, so there is no disguise to see through.
    DeclaredLossyCodec {
        codec: String,
    },
    /// Part of the audio never reached the analysis, so no verdict can describe the file.
    DecodeIncomplete {
        skipped_packets: usize,
        stopped_early: bool,
        channels_unequal: bool,
    },
}

impl IndicatorDetail {
    /// English report wording, shared by the CLI, exported JSON and English UI.
    fn english(&self) -> String {
        match self {
            Self::EncoderTagMatched { tag_key, tag_value, matched_pattern, additional_tags } =>
                format!("Encoder tag {tag_key}={tag_value:?} matches {matched_pattern:?} ({additional_tags} additional distinct tags). Metadata can be stale or edited."),
            Self::TagIsOnlyEvidence =>
                "Encoder metadata alone cannot establish a lossy history.".into(),
            Self::InsufficientSignal =>
                "No usable spectral signal was measured. Silence cannot establish bandwidth or provenance.".into(),
            Self::IntegrityMismatch =>
                "The embedded audio checksum does not match the decoded samples. The provenance verdict is withheld.".into(),
            Self::InvalidSampleRate =>
                "Invalid sample rate; spectral provenance cannot be assessed.".into(),
            Self::SharpRolloff { steepness_db_per_khz, edge_khz } =>
                format!("A sharp spectral edge (~{steepness_db_per_khz:.0} dB/kHz) was measured near {edge_khz:.1} kHz. A lossy codec, a mastering filter and a resampler can all produce it; this observation alone does not identify the cause."),
            Self::NoEncoderLowpass { scanned_from_khz, nyquist_khz } =>
                format!("No qualifying sharp edge was detected in the tested range ({scanned_from_khz:.0}–{nyquist_khz:.1} kHz). This does not rule out a filter or a lossy source."),
            Self::TransparentEncodeUnseen =>
                "The available measurements cannot establish provenance. Transparent encodes, resampling, added noise and other processing can hide compression traces.".into(),
            Self::GradualRolloff { cutoff_khz, steepness_db_per_khz } =>
                format!("A gradual edge was measured near {cutoff_khz:.1} kHz (~{steepness_db_per_khz:.0} dB/kHz). Its cause is not established."),
            Self::MdctGridAligned { z_score, confirmed_z_score, window, frame_offset, zero_percent, baseline_percent } =>
                format!("A 1024-coefficient MDCT grid ({window:?} window, offset {frame_offset}) was found and confirmed on separate audio frames: robust scores {z_score:.1} and {confirmed_z_score:.1}. Confirmation measured {zero_percent:.1}% near-zero coefficients against {baseline_percent:.1}% at control offsets. This is evidence consistent with AAC long-block quantization; the scores are not probabilities."),
            Self::MdctGridClear =>
                "No confirmed AAC long-block grid was detected with the tested sine/KBD windows. This does not exclude AAC, short-block encoding, processed audio or MP3.".into(),
            Self::ContentAboveCdCeiling { level_db, ceiling_khz } =>
                format!("The tested upper band above {ceiling_khz:.2} kHz measures {level_db:.0} dB relative to the reference band. Noise or processing after a lossy encode can create this content, so it does not establish authenticity."),
            Self::DeclaredLossyCodec { codec } =>
                format!("This file declares the lossy codec {}. The question of a lossy source hidden in a lossless file does not apply.", codec.to_uppercase()),
            Self::DecodeIncomplete { skipped_packets, stopped_early, channels_unequal } =>
                format!("The decode is incomplete: {skipped_packets} skipped packet(s), early stop: {stopped_early}, unequal channel lengths: {channels_unequal}. Measurements describe only the decoded portion; the provenance verdict is withheld."),
        }
    }
}

/// Evaluates measured evidence without mistaking bandwidth, a filter or metadata for
/// provenance. A codec-grid detection must include an independent confirmation.
pub fn assess_transcode_risk(
    spectral: &SpectralAnalysis,
    nyquist_hz: f64,
    encoder_tag_matches: &[EncoderTagMatch],
    mdct_grid: &MdctGridAnalysis,
    codec: &str,
    decode_status: &DecodeStatus,
) -> TranscodeAssessment {
    if is_declared_lossy(codec) {
        return TranscodeAssessment {
            verdict: Verdict::DeclaredLossy,
            confidence_score: None,
            indicators: vec![Indicator::new(IndicatorDetail::DeclaredLossyCodec {
                codec: codec.into(),
            })],
        };
    }

    let mut assessment = TranscodeAssessment {
        verdict: Verdict::Indeterminate,
        confidence_score: Some(NO_EVIDENCE_CONFIDENCE),
        indicators: Vec::new(),
    };
    let sharp = spectral.rolloff_steepness_db_per_khz >= STEEPNESS_TRANSCODE_THRESHOLD;
    if !nyquist_hz.is_finite() || nyquist_hz <= 0.0 {
        assessment
            .indicators
            .push(Indicator::new(IndicatorDetail::InvalidSampleRate));
    } else if !spectral.has_signal {
        assessment
            .indicators
            .push(Indicator::new(IndicatorDetail::InsufficientSignal));
    } else {
        let detail = match spectral.encoder_edge_hz {
            Some(hz) if sharp => IndicatorDetail::SharpRolloff {
                steepness_db_per_khz: spectral.rolloff_steepness_db_per_khz,
                edge_khz: hz / 1000.0,
            },
            Some(hz) => IndicatorDetail::GradualRolloff {
                cutoff_khz: hz / 1000.0,
                steepness_db_per_khz: spectral.rolloff_steepness_db_per_khz,
            },
            None => IndicatorDetail::NoEncoderLowpass {
                scanned_from_khz: MIN_SCANNED_KHZ.min(nyquist_hz / 1000.0),
                nyquist_khz: nyquist_hz / 1000.0,
            },
        };
        assessment.indicators.push(Indicator::new(detail));
        if let Some(level_db) = spectral
            .above_cd_ceiling_db
            .filter(|db| *db >= ABOVE_CD_CEILING_MIN_DB)
        {
            assessment
                .indicators
                .push(Indicator::new(IndicatorDetail::ContentAboveCdCeiling {
                    level_db,
                    ceiling_khz: CD_NYQUIST_HZ / 1000.0,
                }));
        }
    }

    if let Some(window) = mdct_grid
        .window
        .filter(|_| mdct_grid.analyzed && mdct_grid.grid_detected)
    {
        assessment.verdict = Verdict::ProbablyTranscoded;
        assessment.confidence_score = Some(if sharp {
            GRID_CORROBORATED_CONFIDENCE
        } else {
            GRID_ONLY_CONFIDENCE
        });
        assessment
            .indicators
            .push(Indicator::new(IndicatorDetail::MdctGridAligned {
                z_score: mdct_grid.z_score,
                confirmed_z_score: mdct_grid.confirmed_z_score,
                window,
                frame_offset: mdct_grid.frame_offset,
                zero_percent: mdct_grid.zero_fraction_at_offset * 100.0,
                baseline_percent: mdct_grid.zero_fraction_baseline * 100.0,
            }));
    } else {
        if mdct_grid.analyzed {
            assessment
                .indicators
                .push(Indicator::new(IndicatorDetail::MdctGridClear));
        }
        assessment
            .indicators
            .push(Indicator::new(IndicatorDetail::TransparentEncodeUnseen));
    }

    if let Some(first) = encoder_tag_matches.first() {
        let distinct_tags = encoder_tag_matches
            .iter()
            .map(|tag| (tag.tag_key.as_str(), tag.tag_value.as_str()))
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        assessment
            .indicators
            .push(Indicator::new(IndicatorDetail::EncoderTagMatched {
                tag_key: first.tag_key.clone(),
                tag_value: first.tag_value.clone(),
                matched_pattern: first.matched_pattern.clone(),
                additional_tags: distinct_tags.saturating_sub(1),
            }));
        if assessment.verdict == Verdict::Indeterminate {
            assessment
                .indicators
                .push(Indicator::new(IndicatorDetail::TagIsOnlyEvidence));
        }
    }

    if !decode_status.complete {
        assessment.verdict = Verdict::Indeterminate;
        assessment.confidence_score = Some(NO_EVIDENCE_CONFIDENCE);
        assessment.indicators.insert(
            0,
            Indicator::new(IndicatorDetail::DecodeIncomplete {
                skipped_packets: decode_status.skipped_packets,
                stopped_early: decode_status.stopped_early,
                channels_unequal: decode_status.channels_unequal,
            }),
        );
    }
    assessment
}

impl TranscodeAssessment {
    /// Withdraws the provenance inference when the file's embedded checksum failed.
    pub fn withhold_on_integrity_failure(&mut self) {
        if self.verdict != Verdict::DeclaredLossy {
            self.verdict = Verdict::Indeterminate;
            self.confidence_score = Some(NO_EVIDENCE_CONFIDENCE);
            self.indicators
                .insert(0, Indicator::new(IndicatorDetail::IntegrityMismatch));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spectral::{BandLevel, SpectrogramData};

    #[test]
    fn a_checksum_failure_withdraws_even_a_structural_verdict() {
        let mut assessment = TranscodeAssessment {
            verdict: Verdict::ProbablyTranscoded,
            confidence_score: Some(GRID_ONLY_CONFIDENCE),
            indicators: Vec::new(),
        };
        assessment.withhold_on_integrity_failure();
        assert_eq!(assessment.verdict, Verdict::Indeterminate);
        assert_eq!(assessment.confidence_score, Some(NO_EVIDENCE_CONFIDENCE));
        assert!(matches!(
            assessment.indicators[0].detail,
            IndicatorDetail::IntegrityMismatch
        ));
    }

    fn spectral(
        encoder_edge_hz: Option<f64>,
        steepness: f64,
        cutoff: Option<f64>,
    ) -> SpectralAnalysis {
        SpectralAnalysis {
            has_signal: true,
            spectral_cutoff_hz: cutoff,
            rolloff_steepness_db_per_khz: steepness,
            encoder_edge_hz,
            cutoff_over_time_hz: Vec::new(),
            cutoff_stability_hz: 0.0,
            band_levels_db: Vec::<BandLevel>::new(),
            stopband_depth_db: None,
            above_cd_ceiling_db: None,
            spectrogram: SpectrogramData {
                time_bin_count: 0,
                frequency_bin_count: 0,
                max_frequency_hz: 22_050.0,
                duration_seconds: 0.0,
                intensity_base64: String::new(),
            },
        }
    }

    fn clear_grid() -> MdctGridAnalysis {
        MdctGridAnalysis {
            window: Some(crate::mdct_grid::MdctWindow::Sine),
            confirmed_z_score: 1.0,
            analyzed: true,
            grid_detected: false,
            z_score: 1.0,
            frame_offset: 0,
            zero_fraction_at_offset: 0.1,
            zero_fraction_baseline: 0.1,
            sweep_profile_base64: String::new(),
        }
    }

    fn complete() -> DecodeStatus {
        DecodeStatus {
            complete: true,
            skipped_packets: 0,
            stopped_early: false,
            channels_unequal: false,
        }
    }

    /// The failure this module's most important fix addresses: a clean spectral sweep is an
    /// absence of evidence, and absence used to be returned as `ProbablyAuthentic` at 0.65 —
    /// the tool vouching for the two real LAME V0 transcodes in its own corpus.
    #[test]
    fn a_clean_sweep_alone_no_longer_vouches_for_a_file() {
        let assessment = assess_transcode_risk(
            &spectral(None, 0.0, None),
            22_050.0,
            &[],
            &clear_grid(),
            "flac",
            &complete(),
        );
        assert_eq!(assessment.verdict, Verdict::Indeterminate);
        assert!(
            assessment
                .indicators
                .iter()
                .any(|i| matches!(i.detail, IndicatorDetail::NoEncoderLowpass { .. }))
        );
    }

    /// Positive evidence still reaches a verdict: content in the top of a hi-res band cannot
    /// have come through a CD-rate lossy encode.
    #[test]
    fn ultrasonic_content_does_not_establish_authenticity() {
        let mut sp = spectral(None, 0.0, None);
        sp.above_cd_ceiling_db = Some(-5.0);
        let assessment =
            assess_transcode_risk(&sp, 48_000.0, &[], &clear_grid(), "flac", &complete());
        assert_eq!(assessment.verdict, Verdict::Indeterminate);
        assert!(
            assessment
                .indicators
                .iter()
                .any(|i| matches!(i.detail, IndicatorDetail::ContentAboveCdCeiling { .. }))
        );
    }

    /// An upsampled file has content above the ceiling too, but only ringing. It must not
    /// clear the bar the measurement above sets.
    #[test]
    fn upsampled_ringing_does_not_support_an_authentic_verdict() {
        let mut sp = spectral(None, 0.0, Some(25_000.0));
        sp.above_cd_ceiling_db = Some(-47.7); // the corpus's upsampled fixture
        let assessment =
            assess_transcode_risk(&sp, 48_000.0, &[], &clear_grid(), "flac", &complete());
        assert_eq!(assessment.verdict, Verdict::Indeterminate);
    }

    /// A tag naming a lossy encoder on a file the sweep found no lowpass in is a conflict,
    /// not a confession. Letting the tag carry the verdict would turn a leftover encoder
    /// string into an accusation — the reason `NoLowpass` is tracked separately from the
    /// `Indeterminate` it now produces.
    #[test]
    fn encoder_metadata_alone_never_accuses_a_file() {
        let tags = [EncoderTagMatch {
            tag_key: "ENCODER".into(),
            tag_value: "LAME 3.100".into(),
            matched_pattern: "lame".into(),
        }];
        let assessment = assess_transcode_risk(
            &spectral(None, 0.0, None),
            22_050.0,
            &tags,
            &clear_grid(),
            "flac",
            &complete(),
        );
        assert_eq!(assessment.verdict, Verdict::Indeterminate);
        assert_eq!(assessment.confidence_score, Some(NO_EVIDENCE_CONFIDENCE));
        assert!(
            assessment
                .indicators
                .iter()
                .any(|i| matches!(i.detail, IndicatorDetail::TagIsOnlyEvidence))
        );
    }

    /// One tag matching two patterns is one piece of evidence. It used to be reported as
    /// "plus 1 more matching tag" on a file carrying exactly one.
    #[test]
    fn repeated_patterns_in_one_tag_count_once() {
        let tags = [
            EncoderTagMatch {
                tag_key: "ENCODER".into(),
                tag_value: "LAME 3.100 (lame)".into(),
                matched_pattern: "lame".into(),
            },
            EncoderTagMatch {
                tag_key: "ENCODER".into(),
                tag_value: "LAME 3.100 (lame)".into(),
                matched_pattern: "lame3".into(),
            },
        ];
        let assessment = assess_transcode_risk(
            &spectral(Some(16_000.0), 90.0, Some(16_000.0)),
            22_050.0,
            &tags,
            &clear_grid(),
            "flac",
            &complete(),
        );
        let additional = assessment.indicators.iter().find_map(|i| match i.detail {
            IndicatorDetail::EncoderTagMatched {
                additional_tags, ..
            } => Some(additional_tags),
            _ => None,
        });
        assert_eq!(
            additional,
            Some(0),
            "two patterns in one tag is still one tag"
        );
    }

    /// A verdict describes a file. When part of the file never decoded, there is no file to
    /// describe — the measurements stand, the claim does not.
    #[test]
    fn an_incomplete_decode_withholds_the_verdict() {
        let damaged = DecodeStatus {
            complete: false,
            skipped_packets: 12,
            stopped_early: false,
            channels_unequal: false,
        };
        let assessment = assess_transcode_risk(
            // A spectrum that would otherwise read as an obvious transcode.
            &spectral(Some(16_000.0), 95.0, Some(16_000.0)),
            22_050.0,
            &[],
            &clear_grid(),
            "flac",
            &damaged,
        );
        assert_eq!(assessment.verdict, Verdict::Indeterminate);
        assert!(matches!(
            assessment.indicators.first().map(|i| &i.detail),
            Some(IndicatorDetail::DecodeIncomplete {
                skipped_packets: 12,
                ..
            })
        ));
        // The measurement itself is still reported — only the accusation is withdrawn.
        assert!(
            assessment
                .indicators
                .iter()
                .any(|i| matches!(i.detail, IndicatorDetail::SharpRolloff { .. }))
        );
    }

    /// Channels of different lengths mean different sections of the report describe
    /// different amounts of audio. That is a damaged file, and it withholds the verdict for
    /// the same reason a skipped packet does.
    #[test]
    fn unequal_channel_lengths_withhold_the_verdict() {
        let ragged = DecodeStatus {
            complete: false,
            skipped_packets: 0,
            stopped_early: false,
            channels_unequal: true,
        };
        let assessment = assess_transcode_risk(
            &spectral(Some(16_000.0), 95.0, Some(16_000.0)),
            22_050.0,
            &[],
            &clear_grid(),
            "flac",
            &ragged,
        );
        assert_eq!(assessment.verdict, Verdict::Indeterminate);
        assert!(
            assessment
                .indicators
                .iter()
                .any(|i| matches!(i.detail, IndicatorDetail::DecodeIncomplete { .. }))
        );
    }

    /// A lossy container is not accused of hiding anything, and its verdict carries no
    /// confidence figure — there is nothing to be more or less sure of.
    #[test]
    fn a_declared_lossy_file_carries_no_confidence_score() {
        let assessment = assess_transcode_risk(
            &spectral(Some(16_000.0), 95.0, Some(16_000.0)),
            22_050.0,
            &[],
            &clear_grid(),
            "mp3",
            &complete(),
        );
        assert_eq!(assessment.verdict, Verdict::DeclaredLossy);
        assert_eq!(assessment.confidence_score, None);
    }
}
