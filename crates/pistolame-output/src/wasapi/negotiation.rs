//! Exclusive-mode format negotiation and the policy decisions around it (plan B5.4).
//!
//! The platform call (`IAudioClient::IsFormatSupported`) is injected as a closure, so the
//! candidate loop, the WAVEFORMATEX retry rule and the fallback table are all tested without
//! hardware.
//!
//! `IsFormatSupported` in exclusive mode "returns S_OK if the audio endpoint device supports the
//! caller-specified format, or it returns AUDCLNT_E_UNSUPPORTED_FORMAT"; there is no closest
//! match, so nothing is substituted silently:
//! <https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-isformatsupported>

use windows::Win32::Foundation::{S_FALSE, S_OK};
use windows::Win32::Media::Audio::AUDCLNT_E_UNSUPPORTED_FORMAT;
use windows::core::HRESULT;

use super::hresult_mapping::{WasapiCallError, output_error_from_hresult};
use super::wave_format::{
    WaveFormatRepresentation, extensible_wave_format, plain_wave_format,
    should_retry_as_plain_wave_format,
};
use crate::{
    DeviceAccessMode, DeviceSampleFormat, FormatProbeOutcome, FormatProbeResult, OpenPolicy,
    OutputError, SharedModeCause,
};

/// What the device answered for one candidate, in both representations when the
/// WAVEFORMATEX retry applies. Public so the `probe_formats` example can show which
/// structure a driver wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExclusiveCandidateProbe {
    /// The candidate format.
    pub candidate: DeviceSampleFormat,
    /// Answer for the WAVEFORMATEXTENSIBLE form.
    pub extensible_outcome: FormatProbeOutcome,
    /// Answer for the stand-alone WAVEFORMATEX form, when the extensible form was refused and
    /// the candidate is 16-bit mono or stereo (B5.4 step 3). `None` when not tried.
    pub plain_wave_format_outcome: Option<FormatProbeOutcome>,
}

impl ExclusiveCandidateProbe {
    /// The overall answer: accepted if either representation was accepted.
    pub fn final_outcome(&self) -> FormatProbeOutcome {
        match self.plain_wave_format_outcome {
            Some(FormatProbeOutcome::Accepted) => FormatProbeOutcome::Accepted,
            Some(plain_outcome) if self.extensible_outcome == FormatProbeOutcome::Refused => {
                plain_outcome
            }
            _ => self.extensible_outcome,
        }
    }

    /// The structure to pass to `Initialize`, if the candidate was accepted.
    pub(crate) fn accepted_representation(&self) -> Option<WaveFormatRepresentation> {
        if self.extensible_outcome == FormatProbeOutcome::Accepted {
            Some(WaveFormatRepresentation::Extensible(
                extensible_wave_format(&self.candidate),
            ))
        } else if self.plain_wave_format_outcome == Some(FormatProbeOutcome::Accepted) {
            Some(WaveFormatRepresentation::Plain(plain_wave_format(
                &self.candidate,
            )))
        } else {
            None
        }
    }

    /// One log entry per attempt, in the order they were made. A candidate listed twice means
    /// the WAVEFORMATEX retry was made.
    fn append_attempts_to(&self, negotiation_log: &mut Vec<FormatProbeResult>) {
        let attempt = |outcome| FormatProbeResult {
            candidate: self.candidate,
            access_mode: DeviceAccessMode::Exclusive,
            outcome,
        };
        negotiation_log.push(attempt(self.extensible_outcome));
        if let Some(plain_outcome) = self.plain_wave_format_outcome {
            negotiation_log.push(attempt(plain_outcome));
        }
    }

    /// The first HRESULT error in this probe, if any (not counting refusals).
    fn first_platform_error(&self) -> Option<HRESULT> {
        [
            Some(self.extensible_outcome),
            self.plain_wave_format_outcome,
        ]
        .into_iter()
        .flatten()
        .find_map(|outcome| match outcome {
            FormatProbeOutcome::Error { platform_code } => Some(HRESULT(platform_code)),
            _ => None,
        })
    }
}

/// Translates an `IsFormatSupported` answer. `S_FALSE` (a closest match exists) cannot occur
/// in exclusive mode per the docs; if it does, the exact format was not accepted, so it counts
/// as a refusal.
pub(crate) fn probe_outcome_from_hresult(hresult: HRESULT) -> FormatProbeOutcome {
    match hresult {
        S_OK => FormatProbeOutcome::Accepted,
        AUDCLNT_E_UNSUPPORTED_FORMAT | S_FALSE => FormatProbeOutcome::Refused,
        _ => FormatProbeOutcome::Error {
            platform_code: hresult.0,
        },
    }
}

/// Asks the device about one candidate: WAVEFORMATEXTENSIBLE first, then, only when refused
/// and allowed by B5.4 step 3, the stand-alone WAVEFORMATEX. Microsoft "Device Formats":
/// "exclusive-mode applications should call IsFormatSupported twice".
pub(crate) fn probe_exclusive_candidate(
    candidate: &DeviceSampleFormat,
    is_format_supported_in_exclusive_mode: &mut impl FnMut(&WaveFormatRepresentation) -> HRESULT,
) -> ExclusiveCandidateProbe {
    let extensible = WaveFormatRepresentation::Extensible(extensible_wave_format(candidate));
    let extensible_outcome =
        probe_outcome_from_hresult(is_format_supported_in_exclusive_mode(&extensible));
    let should_retry = extensible_outcome == FormatProbeOutcome::Refused
        && should_retry_as_plain_wave_format(candidate);
    let plain_wave_format_outcome = should_retry.then(|| {
        let plain = WaveFormatRepresentation::Plain(plain_wave_format(candidate));
        probe_outcome_from_hresult(is_format_supported_in_exclusive_mode(&plain))
    });
    ExclusiveCandidateProbe {
        candidate: *candidate,
        extensible_outcome,
        plain_wave_format_outcome,
    }
}

/// Result of the exclusive candidate loop.
pub(crate) enum ExclusiveNegotiation {
    /// A candidate was accepted; it is the format to initialize with.
    Accepted {
        /// The accepted structure.
        representation: WaveFormatRepresentation,
        /// Every attempt, the accepted one last.
        negotiation_log: Vec<FormatProbeResult>,
        /// Position of the accepted candidate in the slice that was negotiated.
        accepted_candidate_index: usize,
    },
    /// The loop stopped without an accepted format.
    NotOpened(ExclusiveAttemptFailure),
}

/// Why exclusive mode could not be had; the input of the fallback table.
#[derive(Debug)]
pub(crate) enum ExclusiveAttemptFailure {
    /// Every candidate got AUDCLNT_E_UNSUPPORTED_FORMAT (or a per-format error).
    EveryCandidateRefused {
        /// Every attempt.
        negotiation_log: Vec<FormatProbeResult>,
    },
    /// AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED: disabled in the Windows sound settings.
    ExclusiveModeNotAllowed {
        /// Every attempt up to that answer.
        negotiation_log: Vec<FormatProbeResult>,
    },
    /// AUDCLNT_E_DEVICE_IN_USE: "Either the device is being used in exclusive mode, or the
    /// device is being used in shared mode and the caller asked to use the device in exclusive
    /// mode" (`Initialize` docs). Only a shared-mode attempt can tell the two apart.
    DeviceInUse {
        /// Every attempt up to that answer.
        negotiation_log: Vec<FormatProbeResult>,
    },
    /// A device-level error (invalidated, service down) or any other failure.
    Failed(OutputError),
}

/// Runs the exclusive negotiation over `candidates` in order (B5.4). The first accepted
/// candidate wins. A device-level answer stops the loop; an error specific to one format is
/// logged and the next candidate is tried.
pub(crate) fn negotiate_exclusive_format(
    candidates: &[DeviceSampleFormat],
    mut is_format_supported_in_exclusive_mode: impl FnMut(&WaveFormatRepresentation) -> HRESULT,
) -> ExclusiveNegotiation {
    let mut negotiation_log = Vec::with_capacity(candidates.len() * 2);
    for (candidate_index, candidate) in candidates.iter().enumerate() {
        let probe =
            probe_exclusive_candidate(candidate, &mut is_format_supported_in_exclusive_mode);
        probe.append_attempts_to(&mut negotiation_log);
        if let Some(representation) = probe.accepted_representation() {
            return ExclusiveNegotiation::Accepted {
                representation,
                negotiation_log,
                accepted_candidate_index: candidate_index,
            };
        }
        if let Some(hresult) = probe.first_platform_error() {
            match output_error_from_hresult("IAudioClient::IsFormatSupported", hresult) {
                OutputError::ExclusiveModeNotAllowed => {
                    return ExclusiveNegotiation::NotOpened(
                        ExclusiveAttemptFailure::ExclusiveModeNotAllowed { negotiation_log },
                    );
                }
                OutputError::DeviceInUse => {
                    return ExclusiveNegotiation::NotOpened(ExclusiveAttemptFailure::DeviceInUse {
                        negotiation_log,
                    });
                }
                device_error @ (OutputError::DeviceInvalidated
                | OutputError::DeviceNotFound
                | OutputError::AudioServiceNotRunning) => {
                    return ExclusiveNegotiation::NotOpened(ExclusiveAttemptFailure::Failed(
                        device_error,
                    ));
                }
                _ => {}
            }
        }
    }
    ExclusiveNegotiation::NotOpened(ExclusiveAttemptFailure::EveryCandidateRefused {
        negotiation_log,
    })
}

impl ExclusiveAttemptFailure {
    /// The same failure with `earlier_attempts` placed before its own log, so a negotiation
    /// resumed after a late refusal still reports one sequence.
    fn after_earlier_attempts(self, mut earlier_attempts: Vec<FormatProbeResult>) -> Self {
        match self {
            ExclusiveAttemptFailure::EveryCandidateRefused { negotiation_log } => {
                earlier_attempts.extend(negotiation_log);
                ExclusiveAttemptFailure::EveryCandidateRefused {
                    negotiation_log: earlier_attempts,
                }
            }
            ExclusiveAttemptFailure::ExclusiveModeNotAllowed { negotiation_log } => {
                earlier_attempts.extend(negotiation_log);
                ExclusiveAttemptFailure::ExclusiveModeNotAllowed {
                    negotiation_log: earlier_attempts,
                }
            }
            ExclusiveAttemptFailure::DeviceInUse { negotiation_log } => {
                earlier_attempts.extend(negotiation_log);
                ExclusiveAttemptFailure::DeviceInUse {
                    negotiation_log: earlier_attempts,
                }
            }
            failed @ ExclusiveAttemptFailure::Failed(_) => failed,
        }
    }
}

/// An exclusive stream initialized with an accepted candidate.
pub(crate) struct ExclusiveInitialization<Initialized> {
    /// What the `initialize` step produced.
    pub(crate) initialized: Initialized,
    /// The structure `Initialize` accepted.
    pub(crate) representation: WaveFormatRepresentation,
    /// Every attempt, including late refusals by `Initialize`.
    pub(crate) negotiation_log: Vec<FormatProbeResult>,
}

/// Negotiates, then initializes. `IsFormatSupported` can accept a candidate that `Initialize`
/// then refuses with AUDCLNT_E_UNSUPPORTED_FORMAT; that late refusal is logged (outcome
/// `Error` with that code, so it reads differently from an `IsFormatSupported` refusal) and
/// the negotiation resumes at the next candidate with a newly activated client, because a
/// client whose first `Initialize` failed may not accept another one (`Initialize` remarks).
/// Any other `Initialize` failure ends the attempt.
///
/// The platform calls are injected so the loop is tested without hardware.
pub(crate) fn negotiate_and_initialize_exclusive<Client, Initialized>(
    candidates: &[DeviceSampleFormat],
    mut activate_client: impl FnMut() -> Result<Client, WasapiCallError>,
    mut is_format_supported_in_exclusive_mode: impl FnMut(&Client, &WaveFormatRepresentation) -> HRESULT,
    mut initialize: impl FnMut(
        Client,
        &WaveFormatRepresentation,
    ) -> Result<Initialized, WasapiCallError>,
) -> Result<ExclusiveInitialization<Initialized>, ExclusiveAttemptFailure> {
    let failed = |error: WasapiCallError| ExclusiveAttemptFailure::Failed(error.to_output_error());
    let mut negotiation_log = Vec::new();
    let mut first_remaining_candidate = 0;
    loop {
        let client = activate_client().map_err(failed)?;
        let remaining_candidates = &candidates[first_remaining_candidate..];
        let negotiation = negotiate_exclusive_format(remaining_candidates, |representation| {
            is_format_supported_in_exclusive_mode(&client, representation)
        });
        let (representation, attempts, accepted_candidate_index) = match negotiation {
            ExclusiveNegotiation::Accepted {
                representation,
                negotiation_log: attempts,
                accepted_candidate_index,
            } => (representation, attempts, accepted_candidate_index),
            ExclusiveNegotiation::NotOpened(failure) => {
                return Err(failure.after_earlier_attempts(negotiation_log));
            }
        };
        negotiation_log.extend(attempts);
        let error = match initialize(client, &representation) {
            Ok(initialized) => {
                return Ok(ExclusiveInitialization {
                    initialized,
                    representation,
                    negotiation_log,
                });
            }
            Err(error) => error,
        };
        let late_failure = FormatProbeResult {
            candidate: remaining_candidates[accepted_candidate_index],
            access_mode: DeviceAccessMode::Exclusive,
            outcome: FormatProbeOutcome::Error {
                platform_code: error.hresult.0,
            },
        };
        match error.to_output_error() {
            OutputError::FormatNotSupported { .. } => {
                negotiation_log.push(late_failure);
                first_remaining_candidate += accepted_candidate_index + 1;
            }
            OutputError::ExclusiveModeNotAllowed => {
                return Err(ExclusiveAttemptFailure::ExclusiveModeNotAllowed { negotiation_log });
            }
            OutputError::DeviceInUse => {
                negotiation_log.push(late_failure);
                return Err(ExclusiveAttemptFailure::DeviceInUse { negotiation_log });
            }
            other => return Err(ExclusiveAttemptFailure::Failed(other)),
        }
    }
}

/// What `open_stream` does first for a policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FirstOpenStep {
    /// Run the exclusive negotiation (B5.4).
    NegotiateExclusive,
    /// Go straight to shared mode (B5.10) for this reason.
    OpenShared(SharedModeCause),
}

/// The first step for a policy: `SharedOnly` never tries exclusive mode.
pub(crate) fn first_open_step(policy: OpenPolicy) -> FirstOpenStep {
    match policy {
        OpenPolicy::SharedOnly => FirstOpenStep::OpenShared(SharedModeCause::UserPreference),
        OpenPolicy::ExclusiveThenShared | OpenPolicy::ExclusiveOnly => {
            FirstOpenStep::NegotiateExclusive
        }
    }
}

/// What to do after exclusive mode could not be had (the B5.4 fallback table).
#[derive(Debug)]
pub(crate) enum ExclusiveFailureResolution {
    /// Open in shared mode; the log of the refused exclusive attempts is kept.
    OpenShared {
        /// Why shared mode is used.
        cause: SharedModeCause,
        /// The exclusive attempts, to which the shared attempts are appended.
        negotiation_log: Vec<FormatProbeResult>,
    },
    /// Fail with this error; no fallback.
    Fail(OutputError),
}

/// The B5.4 fallback table. Only `ExclusiveThenShared` falls back.
///
/// AUDCLNT_E_DEVICE_IN_USE also falls back, because it has two meanings: another application
/// holds the device in exclusive mode, or "the device is being used in shared mode and the
/// caller asked to use the device in exclusive mode" (`Initialize` docs). In the first case
/// the shared attempt fails with the same code ("An attempt to create a shared-mode stream
/// fails if the device is already operating in exclusive mode", `Initialize` remarks), so the
/// open still ends with `DeviceInUse`; in the second it plays through the system mixer with
/// `DeviceBusyInSharedMode`.
pub(crate) fn resolve_exclusive_failure(
    policy: OpenPolicy,
    failure: ExclusiveAttemptFailure,
) -> ExclusiveFailureResolution {
    let may_fall_back = policy == OpenPolicy::ExclusiveThenShared;
    match failure {
        ExclusiveAttemptFailure::EveryCandidateRefused { negotiation_log } if may_fall_back => {
            ExclusiveFailureResolution::OpenShared {
                cause: SharedModeCause::ExclusiveFormatRefused,
                negotiation_log,
            }
        }
        ExclusiveAttemptFailure::EveryCandidateRefused { negotiation_log } => {
            ExclusiveFailureResolution::Fail(OutputError::FormatNotSupported {
                tried: negotiation_log,
            })
        }
        ExclusiveAttemptFailure::ExclusiveModeNotAllowed { negotiation_log } if may_fall_back => {
            ExclusiveFailureResolution::OpenShared {
                cause: SharedModeCause::ExclusiveModeDisabledBySystem,
                negotiation_log,
            }
        }
        ExclusiveAttemptFailure::ExclusiveModeNotAllowed { .. } => {
            ExclusiveFailureResolution::Fail(OutputError::ExclusiveModeNotAllowed)
        }
        ExclusiveAttemptFailure::DeviceInUse { negotiation_log } if may_fall_back => {
            ExclusiveFailureResolution::OpenShared {
                cause: SharedModeCause::DeviceBusyInSharedMode,
                negotiation_log,
            }
        }
        ExclusiveAttemptFailure::DeviceInUse { .. } => {
            ExclusiveFailureResolution::Fail(OutputError::DeviceInUse)
        }
        ExclusiveAttemptFailure::Failed(error) => ExclusiveFailureResolution::Fail(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChannelMask, StreamFormat, ValidBitsAlignment, lossless_device_format_candidates};
    use windows::Win32::Foundation::E_INVALIDARG;
    use windows::Win32::Media::Audio::{
        AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_E_DEVICE_INVALIDATED,
        AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED, AUDCLNT_E_SERVICE_NOT_RUNNING,
    };

    fn candidates(bits_per_sample: u8, channel_count: u8) -> Vec<DeviceSampleFormat> {
        let source = StreamFormat {
            sample_rate_hz: 44_100,
            bits_per_sample,
            channel_count,
            channel_mask: ChannelMask::default_for_flac_channel_count(channel_count).unwrap(),
        };
        lossless_device_format_candidates(&source, ValidBitsAlignment::MostSignificant)
    }

    fn is_plain(representation: &WaveFormatRepresentation) -> bool {
        matches!(representation, WaveFormatRepresentation::Plain(_))
    }

    fn container_bits(representation: &WaveFormatRepresentation) -> u16 {
        // SAFETY: test reads the header of a live local structure.
        unsafe { representation.as_wave_format_pointer().read_unaligned() }.wBitsPerSample
    }

    fn outcomes(log: &[FormatProbeResult]) -> Vec<FormatProbeOutcome> {
        log.iter().map(|entry| entry.outcome).collect()
    }

    #[test]
    fn first_accepted_candidate_wins_and_earlier_refusals_are_logged() {
        let negotiation = negotiate_exclusive_format(&candidates(24, 2), |representation| {
            if container_bits(representation) == 32 {
                S_OK
            } else {
                AUDCLNT_E_UNSUPPORTED_FORMAT
            }
        });
        let ExclusiveNegotiation::Accepted {
            representation,
            negotiation_log,
            ..
        } = negotiation
        else {
            panic!("a 32-bit container was accepted");
        };
        assert_eq!(container_bits(&representation), 32);
        assert!(!is_plain(&representation));
        assert_eq!(
            outcomes(&negotiation_log),
            vec![FormatProbeOutcome::Refused, FormatProbeOutcome::Accepted]
        );
        assert!(
            negotiation_log
                .iter()
                .all(|entry| entry.access_mode == DeviceAccessMode::Exclusive)
        );
        assert_eq!(negotiation_log[1].candidate.valid_bits_per_sample, 24);
    }

    #[test]
    fn sixteen_bit_stereo_is_retried_as_plain_wave_format() {
        let negotiation = negotiate_exclusive_format(&candidates(16, 2), |representation| {
            if is_plain(representation) {
                S_OK
            } else {
                AUDCLNT_E_UNSUPPORTED_FORMAT
            }
        });
        let ExclusiveNegotiation::Accepted {
            representation,
            negotiation_log,
            ..
        } = negotiation
        else {
            panic!("the plain form was accepted");
        };
        assert!(is_plain(&representation));
        assert_eq!(
            outcomes(&negotiation_log),
            vec![FormatProbeOutcome::Refused, FormatProbeOutcome::Accepted]
        );
        assert_eq!(negotiation_log[0].candidate, negotiation_log[1].candidate);
    }

    #[test]
    fn no_plain_retry_for_24_bit_or_more_than_two_channels() {
        for (bits_per_sample, channel_count) in [(24, 2), (16, 6)] {
            let mut was_plain_form_tried = false;
            let negotiation =
                negotiate_exclusive_format(&candidates(bits_per_sample, channel_count), |r| {
                    was_plain_form_tried |= is_plain(r);
                    AUDCLNT_E_UNSUPPORTED_FORMAT
                });
            assert!(!was_plain_form_tried);
            let ExclusiveNegotiation::NotOpened(ExclusiveAttemptFailure::EveryCandidateRefused {
                negotiation_log,
            }) = negotiation
            else {
                panic!("everything was refused");
            };
            assert_eq!(
                negotiation_log.len(),
                candidates(bits_per_sample, channel_count).len()
            );
        }
    }

    #[test]
    fn exclusive_mode_not_allowed_stops_the_loop() {
        let mut call_count = 0;
        let negotiation = negotiate_exclusive_format(&candidates(24, 2), |_| {
            call_count += 1;
            AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED
        });
        assert_eq!(call_count, 1);
        assert!(matches!(
            negotiation,
            ExclusiveNegotiation::NotOpened(ExclusiveAttemptFailure::ExclusiveModeNotAllowed {
                negotiation_log
            }) if negotiation_log.len() == 1
        ));
    }

    #[test]
    fn device_errors_stop_the_loop_with_the_mapped_error() {
        for (hresult, is_expected) in [
            (
                AUDCLNT_E_DEVICE_INVALIDATED,
                (|error: &OutputError| matches!(error, OutputError::DeviceInvalidated))
                    as fn(&OutputError) -> bool,
            ),
            (AUDCLNT_E_SERVICE_NOT_RUNNING, |error| {
                matches!(error, OutputError::AudioServiceNotRunning)
            }),
        ] {
            let negotiation = negotiate_exclusive_format(&candidates(24, 2), |_| hresult);
            let ExclusiveNegotiation::NotOpened(ExclusiveAttemptFailure::Failed(error)) =
                negotiation
            else {
                panic!("{hresult:?} must stop the loop");
            };
            assert!(is_expected(&error), "{hresult:?} gave {error:?}");
        }
    }

    #[test]
    fn device_in_use_stops_the_loop_and_keeps_the_log() {
        let mut call_count = 0;
        let negotiation = negotiate_exclusive_format(&candidates(24, 2), |_| {
            call_count += 1;
            AUDCLNT_E_DEVICE_IN_USE
        });
        assert_eq!(call_count, 1);
        let ExclusiveNegotiation::NotOpened(ExclusiveAttemptFailure::DeviceInUse {
            negotiation_log,
        }) = negotiation
        else {
            panic!("device in use stops the loop");
        };
        assert_eq!(
            outcomes(&negotiation_log),
            vec![FormatProbeOutcome::Error {
                platform_code: AUDCLNT_E_DEVICE_IN_USE.0
            }]
        );
    }

    #[test]
    fn a_per_format_error_is_logged_and_the_next_candidate_is_tried() {
        let negotiation = negotiate_exclusive_format(&candidates(24, 2), |representation| {
            if container_bits(representation) == 24 {
                E_INVALIDARG
            } else {
                S_OK
            }
        });
        let ExclusiveNegotiation::Accepted {
            negotiation_log, ..
        } = negotiation
        else {
            panic!("the 4-byte container was accepted");
        };
        assert_eq!(
            outcomes(&negotiation_log),
            vec![
                FormatProbeOutcome::Error {
                    platform_code: E_INVALIDARG.0
                },
                FormatProbeOutcome::Accepted
            ]
        );
    }

    #[test]
    fn s_false_is_not_an_acceptance() {
        assert_eq!(
            probe_outcome_from_hresult(S_FALSE),
            FormatProbeOutcome::Refused
        );
        assert_eq!(
            probe_outcome_from_hresult(S_OK),
            FormatProbeOutcome::Accepted
        );
    }

    #[test]
    fn final_outcome_prefers_the_plain_answer_only_after_a_refusal() {
        let candidate = candidates(16, 2)[0];
        let probe = |extensible_outcome, plain_wave_format_outcome| ExclusiveCandidateProbe {
            candidate,
            extensible_outcome,
            plain_wave_format_outcome,
        };
        let error = FormatProbeOutcome::Error { platform_code: -1 };
        assert_eq!(
            probe(
                FormatProbeOutcome::Refused,
                Some(FormatProbeOutcome::Accepted)
            )
            .final_outcome(),
            FormatProbeOutcome::Accepted
        );
        assert_eq!(
            probe(FormatProbeOutcome::Refused, Some(error)).final_outcome(),
            error
        );
        assert_eq!(
            probe(FormatProbeOutcome::Accepted, None).final_outcome(),
            FormatProbeOutcome::Accepted
        );
        assert_eq!(probe(error, None).final_outcome(), error);
    }

    fn sample_log() -> Vec<FormatProbeResult> {
        vec![FormatProbeResult {
            candidate: candidates(24, 2)[0],
            access_mode: DeviceAccessMode::Exclusive,
            outcome: FormatProbeOutcome::Refused,
        }]
    }

    #[test]
    fn shared_only_policy_goes_straight_to_shared_mode() {
        assert_eq!(
            first_open_step(OpenPolicy::SharedOnly),
            FirstOpenStep::OpenShared(SharedModeCause::UserPreference)
        );
        assert_eq!(
            first_open_step(OpenPolicy::ExclusiveThenShared),
            FirstOpenStep::NegotiateExclusive
        );
        assert_eq!(
            first_open_step(OpenPolicy::ExclusiveOnly),
            FirstOpenStep::NegotiateExclusive
        );
    }

    #[test]
    fn refused_formats_fall_back_only_under_exclusive_then_shared() {
        let resolution = resolve_exclusive_failure(
            OpenPolicy::ExclusiveThenShared,
            ExclusiveAttemptFailure::EveryCandidateRefused {
                negotiation_log: sample_log(),
            },
        );
        assert!(matches!(
            resolution,
            ExclusiveFailureResolution::OpenShared {
                cause: SharedModeCause::ExclusiveFormatRefused,
                negotiation_log
            } if negotiation_log == sample_log()
        ));
        let resolution = resolve_exclusive_failure(
            OpenPolicy::ExclusiveOnly,
            ExclusiveAttemptFailure::EveryCandidateRefused {
                negotiation_log: sample_log(),
            },
        );
        assert!(matches!(
            resolution,
            ExclusiveFailureResolution::Fail(OutputError::FormatNotSupported { tried })
                if tried == sample_log()
        ));
    }

    #[test]
    fn exclusive_disabled_falls_back_only_under_exclusive_then_shared() {
        let resolution = resolve_exclusive_failure(
            OpenPolicy::ExclusiveThenShared,
            ExclusiveAttemptFailure::ExclusiveModeNotAllowed {
                negotiation_log: sample_log(),
            },
        );
        assert!(matches!(
            resolution,
            ExclusiveFailureResolution::OpenShared {
                cause: SharedModeCause::ExclusiveModeDisabledBySystem,
                ..
            }
        ));
        let resolution = resolve_exclusive_failure(
            OpenPolicy::ExclusiveOnly,
            ExclusiveAttemptFailure::ExclusiveModeNotAllowed {
                negotiation_log: sample_log(),
            },
        );
        assert!(matches!(
            resolution,
            ExclusiveFailureResolution::Fail(OutputError::ExclusiveModeNotAllowed)
        ));
    }

    #[test]
    fn device_in_use_falls_back_only_under_exclusive_then_shared() {
        let resolution = resolve_exclusive_failure(
            OpenPolicy::ExclusiveThenShared,
            ExclusiveAttemptFailure::DeviceInUse {
                negotiation_log: sample_log(),
            },
        );
        assert!(matches!(
            resolution,
            ExclusiveFailureResolution::OpenShared {
                cause: SharedModeCause::DeviceBusyInSharedMode,
                negotiation_log
            } if negotiation_log == sample_log()
        ));
        let resolution = resolve_exclusive_failure(
            OpenPolicy::ExclusiveOnly,
            ExclusiveAttemptFailure::DeviceInUse {
                negotiation_log: sample_log(),
            },
        );
        assert!(matches!(
            resolution,
            ExclusiveFailureResolution::Fail(OutputError::DeviceInUse)
        ));
    }

    #[test]
    fn other_device_errors_never_fall_back() {
        for policy in [OpenPolicy::ExclusiveThenShared, OpenPolicy::ExclusiveOnly] {
            assert!(matches!(
                resolve_exclusive_failure(
                    policy,
                    ExclusiveAttemptFailure::Failed(OutputError::DeviceInvalidated)
                ),
                ExclusiveFailureResolution::Fail(OutputError::DeviceInvalidated)
            ));
            assert!(matches!(
                resolve_exclusive_failure(
                    policy,
                    ExclusiveAttemptFailure::Failed(OutputError::AudioServiceNotRunning)
                ),
                ExclusiveFailureResolution::Fail(OutputError::AudioServiceNotRunning)
            ));
        }
    }

    /// A fake client: its number tells which activation produced it.
    struct FakeClient {
        activation_number: usize,
    }

    fn call_error(hresult: HRESULT) -> WasapiCallError {
        WasapiCallError {
            operation: "IAudioClient::Initialize",
            hresult,
        }
    }

    #[test]
    fn late_initialize_refusal_moves_to_the_next_candidate_with_a_new_client() {
        let mut activation_count = 0;
        let mut initialized_container_bits = Vec::new();
        let result = negotiate_and_initialize_exclusive(
            &candidates(24, 2),
            || {
                activation_count += 1;
                Ok(FakeClient {
                    activation_number: activation_count,
                })
            },
            |_client, _representation| S_OK,
            |client, representation| {
                initialized_container_bits.push(container_bits(representation));
                if container_bits(representation) == 24 {
                    Err(call_error(AUDCLNT_E_UNSUPPORTED_FORMAT))
                } else {
                    Ok(client.activation_number)
                }
            },
        );
        let Ok(initialization) = result else {
            panic!("the second candidate initializes");
        };
        assert_eq!(activation_count, 2);
        assert_eq!(
            initialization.initialized, 2,
            "a new client after the refusal"
        );
        assert_eq!(initialized_container_bits, vec![24, 32]);
        assert_eq!(container_bits(&initialization.representation), 32);
        assert_eq!(
            outcomes(&initialization.negotiation_log),
            vec![
                FormatProbeOutcome::Accepted,
                FormatProbeOutcome::Error {
                    platform_code: AUDCLNT_E_UNSUPPORTED_FORMAT.0
                },
                FormatProbeOutcome::Accepted,
            ]
        );
        assert_eq!(
            initialization.negotiation_log[0].candidate.container_bytes,
            3
        );
        assert_eq!(
            initialization.negotiation_log[1].candidate.container_bytes,
            3
        );
        assert_eq!(
            initialization.negotiation_log[2].candidate.container_bytes,
            4
        );
    }

    #[test]
    fn late_refusal_of_every_candidate_reports_the_whole_log() {
        let result = negotiate_and_initialize_exclusive(
            &candidates(24, 2),
            || {
                Ok(FakeClient {
                    activation_number: 0,
                })
            },
            |_client, _representation| S_OK,
            |_client, _representation| -> Result<(), WasapiCallError> {
                Err(call_error(AUDCLNT_E_UNSUPPORTED_FORMAT))
            },
        );
        let Err(ExclusiveAttemptFailure::EveryCandidateRefused { negotiation_log }) = result else {
            panic!("every candidate was refused late");
        };
        // Three candidates, each accepted by IsFormatSupported then refused by Initialize.
        assert_eq!(negotiation_log.len(), 6);
    }

    #[test]
    fn other_initialize_failures_end_the_attempt() {
        let in_use = negotiate_and_initialize_exclusive(
            &candidates(24, 2),
            || {
                Ok(FakeClient {
                    activation_number: 0,
                })
            },
            |_client, _representation| S_OK,
            |_client, _representation| -> Result<(), WasapiCallError> {
                Err(call_error(AUDCLNT_E_DEVICE_IN_USE))
            },
        );
        assert!(matches!(
            in_use,
            Err(ExclusiveAttemptFailure::DeviceInUse { negotiation_log })
                if outcomes(&negotiation_log) == vec![
                    FormatProbeOutcome::Accepted,
                    FormatProbeOutcome::Error { platform_code: AUDCLNT_E_DEVICE_IN_USE.0 },
                ]
        ));
        let not_allowed = negotiate_and_initialize_exclusive(
            &candidates(24, 2),
            || {
                Ok(FakeClient {
                    activation_number: 0,
                })
            },
            |_client, _representation| S_OK,
            |_client, _representation| -> Result<(), WasapiCallError> {
                Err(call_error(AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED))
            },
        );
        assert!(matches!(
            not_allowed,
            Err(ExclusiveAttemptFailure::ExclusiveModeNotAllowed { negotiation_log })
                if negotiation_log.len() == 1
        ));
    }
}
