//! Shared mode (plan B5.10): the stream goes through the Windows audio engine, which converts
//! our integer PCM to its mix format. Shared mode is the default output mode.
//!
//! Pistolame keeps handing Windows lossless integer PCM at the source rate and lets
//! AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM insert "a channel matrixer and a sample rate converter
//! ... as necessary to convert between the uncompressed format supplied to
//! IAudioClient::Initialize and the audio engine mix format". With
//! AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, "a sample rate converter with better quality than
//! the default conversion ... is used. This should be used if the audio is ultimately intended
//! to be heard by humans". So the feeder stays integer-only and identical in both modes.
//!
//! Sources:
//! - `IAudioClient::Initialize` (shared event-driven mode, a single buffer, periods of 0,
//!   a failed `Initialize` leaving the client unusable):
//!   <https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-initialize>
//! - AUDCLNT_STREAMFLAGS_XXX constants:
//!   <https://learn.microsoft.com/en-us/windows/win32/coreaudio/audclnt-streamflags-xxx-constants>
//! - `IAudioClient::GetMixFormat`:
//!   <https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getmixformat>
//! - `IAudioClient::GetCurrentPadding` (how much of the shared buffer is still queued):
//!   <https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getcurrentpadding>
//! - `IAudioClient::GetDevicePeriod`:
//!   <https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getdeviceperiod>

use windows::Win32::Foundation::E_INVALIDARG;
use windows::Win32::Media::Audio::{
    AUDCLNT_E_UNSUPPORTED_FORMAT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient,
    IMMDevice,
};
use windows::Win32::System::Com::CoTaskMemFree;

use super::buffer_fill_rule::{BufferFillRule, frames_covering_period};
use super::devices::activate_audio_client;
use super::hresult_mapping::{NameWasapiCall, WasapiCallError};
use super::stream_initialization::{InitializedStream, prepare_streaming};
use super::wave_format::{
    WaveFormatRepresentation, device_sample_format_from_representation, extensible_wave_format,
    system_mix_format_from_pointer,
};
use crate::{
    DeviceAccessMode, DeviceSampleFormat, FormatProbeOutcome, FormatProbeResult, NegotiatedOutput,
    OutputDeviceInfo, OutputError, SharedModeCause, StreamFormat, SystemMixFormat,
    ValidBitsAlignment, lossless_device_format_candidates,
};

/// Stream flags for every shared-mode `Initialize` (B5.10): event-driven, converted by Windows
/// to the mix format, with the better-quality sample rate converter.
pub(crate) const SHARED_MODE_STREAM_FLAGS: u32 = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;

/// `hnsBufferDuration` and `hnsPeriodicity` in shared event-driven mode: "the caller must set
/// both hnsPeriodicity and hnsBufferDuration to 0", and `hnsPeriodicity` "can be nonzero only in
/// exclusive mode" (`Initialize` docs). The audio engine then chooses the buffer size.
const SHARED_MODE_BUFFER_DURATION_AND_PERIOD_IN_HUNDRED_NANOSECONDS: i64 = 0;

/// A shared-mode client initialized with an accepted candidate.
pub(crate) struct SharedInitialization<Initialized> {
    /// What the `initialize` step produced.
    pub(crate) initialized: Initialized,
    /// The structure `Initialize` accepted.
    pub(crate) representation: WaveFormatRepresentation,
    /// The attempts made before shared mode (exclusive ones after a fallback), then every
    /// shared attempt, the accepted one last.
    pub(crate) negotiation_log: Vec<FormatProbeResult>,
}

/// Tries each candidate with `Initialize` in shared mode, in order, until one is accepted
/// (B5.10 step 2). `IsFormatSupported` is not used: it takes no stream flags, so it cannot
/// answer for a stream that AUTOCONVERTPCM will convert.
///
/// Each attempt gets a newly activated client, because a client whose `Initialize` failed may
/// not accept another call (`Initialize` remarks). A candidate-specific answer moves on to the
/// next candidate (see `is_candidate_refusal`); any other failure (device in use, invalidated,
/// service down) ends the attempt with its mapped error. When every candidate is refused, the
/// error carries the whole log, `earlier_attempts` first (B5.10 step 3).
///
/// The platform calls are injected so the loop is tested without hardware.
pub(crate) fn negotiate_and_initialize_shared<Client, Initialized>(
    candidates: &[DeviceSampleFormat],
    earlier_attempts: Vec<FormatProbeResult>,
    mut activate_client: impl FnMut() -> Result<Client, WasapiCallError>,
    mut initialize_shared: impl FnMut(
        Client,
        &WaveFormatRepresentation,
    ) -> Result<Initialized, WasapiCallError>,
) -> Result<SharedInitialization<Initialized>, OutputError> {
    let mut negotiation_log = earlier_attempts;
    negotiation_log.reserve(candidates.len());
    for candidate in candidates {
        let shared_attempt = |outcome| FormatProbeResult {
            candidate: *candidate,
            access_mode: DeviceAccessMode::SharedThroughSystemMixer,
            outcome,
        };
        let representation =
            WaveFormatRepresentation::Extensible(extensible_wave_format(candidate));
        let client = activate_client()?;
        match initialize_shared(client, &representation) {
            Ok(initialized) => {
                negotiation_log.push(shared_attempt(FormatProbeOutcome::Accepted));
                return Ok(SharedInitialization {
                    initialized,
                    representation,
                    negotiation_log,
                });
            }
            Err(error) => match candidate_refusal_outcome(error) {
                Some(outcome) => negotiation_log.push(shared_attempt(outcome)),
                None => return Err(error.to_output_error()),
            },
        }
    }
    Err(OutputError::FormatNotSupported {
        tried: negotiation_log,
    })
}

/// The log entry for an `Initialize` failure that concerns only this candidate, or `None` when
/// the failure concerns the device and must end the open.
///
/// - AUDCLNT_E_UNSUPPORTED_FORMAT: "The audio engine (shared mode) or audio endpoint device
///   (exclusive mode) does not support the specified format" (`Initialize` docs): `Refused`.
/// - E_INVALIDARG: "Parameter pFormat ... points to an invalid format description"
///   (`Initialize` docs). Our structures are built by one tested function, so this means the
///   driver or engine dislikes this particular description; it is logged with its code, so it
///   reads differently from a plain refusal, and the next candidate is tried.
fn candidate_refusal_outcome(error: WasapiCallError) -> Option<FormatProbeOutcome> {
    match error.hresult {
        AUDCLNT_E_UNSUPPORTED_FORMAT => Some(FormatProbeOutcome::Refused),
        E_INVALIDARG => Some(FormatProbeOutcome::Error {
            platform_code: E_INVALIDARG.0,
        }),
        _ => None,
    }
}

/// What the platform granted in shared mode. The device format is not ours to choose, so it is
/// `None`; the mix format is reported instead, and the render format is the lossless candidate
/// Windows accepted for conversion (B3.4).
pub(crate) fn shared_negotiated_output(
    device: OutputDeviceInfo,
    shared_mode_cause: SharedModeCause,
    system_mix_format: SystemMixFormat,
    render_format: DeviceSampleFormat,
    buffer_frame_count: u32,
    device_period_in_hundred_nanoseconds: i64,
    negotiation_log: Vec<FormatProbeResult>,
) -> NegotiatedOutput {
    NegotiatedOutput {
        device,
        access_mode: DeviceAccessMode::SharedThroughSystemMixer,
        shared_mode_cause: Some(shared_mode_cause),
        device_format: None,
        system_mix_format: Some(system_mix_format),
        render_format,
        buffer_frame_count,
        device_period_in_hundred_nanoseconds,
        negotiation_log,
    }
}

/// Opens `device` in shared mode (B5.10 steps 1 to 5). Runs on the render thread with COM
/// initialized. `earlier_attempts` holds the refused exclusive attempts after a fallback, empty
/// otherwise.
pub(crate) fn initialize_shared_stream(
    device: &IMMDevice,
    device_info: OutputDeviceInfo,
    source_format: &StreamFormat,
    shared_mode_cause: SharedModeCause,
    earlier_attempts: Vec<FormatProbeResult>,
) -> Result<InitializedStream, OutputError> {
    let system_mix_format = read_system_mix_format(&activate_audio_client(device)?)?;
    let candidates =
        lossless_device_format_candidates(source_format, ValidBitsAlignment::MostSignificant);
    let initialization = negotiate_and_initialize_shared(
        &candidates,
        earlier_attempts,
        || activate_audio_client(device),
        |audio_client, representation| {
            call_initialize_shared(&audio_client, representation)?;
            Ok(audio_client)
        },
    )?;
    let render_format = device_sample_format_from_representation(&initialization.representation)
        .ok_or(OutputError::Platform {
            operation: "read back the accepted shared-mode format",
            platform_code: AUDCLNT_E_UNSUPPORTED_FORMAT.0,
        })?;
    let audio_client = initialization.initialized;
    let device_period_in_hundred_nanoseconds = default_device_period(&audio_client)?;
    let streaming = prepare_streaming(&audio_client)?;
    Ok(InitializedStream {
        negotiated: shared_negotiated_output(
            device_info,
            shared_mode_cause,
            system_mix_format,
            render_format,
            streaming.buffer_frame_count,
            device_period_in_hundred_nanoseconds,
            initialization.negotiation_log,
        ),
        audio_client,
        render_client: streaming.render_client,
        buffer_event: streaming.buffer_event,
        fill_rule: BufferFillRule::SharedFreeSpace {
            engine_period_frames: frames_covering_period(
                device_period_in_hundred_nanoseconds,
                render_format.sample_rate_hz,
            ),
        },
    })
}

/// `Initialize` in shared event-driven mode with conversion to the mix format.
fn call_initialize_shared(
    audio_client: &IAudioClient,
    representation: &WaveFormatRepresentation,
) -> Result<(), WasapiCallError> {
    // SAFETY: valid, not yet initialized client; the format pointer refers to
    // `representation`, alive for the whole call; no session GUID (default session).
    unsafe {
        audio_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            SHARED_MODE_STREAM_FLAGS,
            SHARED_MODE_BUFFER_DURATION_AND_PERIOD_IN_HUNDRED_NANOSECONDS,
            SHARED_MODE_BUFFER_DURATION_AND_PERIOD_IN_HUNDRED_NANOSECONDS,
            representation.as_wave_format_pointer(),
            None,
        )
    }
    .for_operation("IAudioClient::Initialize (shared)")
}

/// The audio engine's default period, "the default interval between periodic processing
/// passes by the audio engine" (`GetDevicePeriod` docs): the pace of a shared-mode stream.
fn default_device_period(audio_client: &IAudioClient) -> Result<i64, WasapiCallError> {
    let mut default_period_in_hundred_nanoseconds: i64 = 0;
    // SAFETY: valid client; the out pointer refers to a live local; the minimum period is not
    // requested.
    unsafe { audio_client.GetDevicePeriod(Some(&mut default_period_in_hundred_nanoseconds), None) }
        .for_operation("IAudioClient::GetDevicePeriod")?;
    Ok(default_period_in_hundred_nanoseconds)
}

/// The shared-mode mix format of the device behind `audio_client` (B5.10 step 1). "The caller
/// is responsible for freeing the structure by calling CoTaskMemFree" (`GetMixFormat` docs).
pub(crate) fn read_system_mix_format(
    audio_client: &IAudioClient,
) -> Result<SystemMixFormat, WasapiCallError> {
    // SAFETY: valid client.
    let mix_format =
        unsafe { audio_client.GetMixFormat() }.for_operation("IAudioClient::GetMixFormat")?;
    // SAFETY: GetMixFormat succeeded, so the pointer refers to a complete structure whose
    // `cbSize` is correct.
    let system_mix_format = unsafe { system_mix_format_from_pointer(mix_format) };
    // SAFETY: frees the structure allocated by GetMixFormat, exactly once; it is not used
    // afterwards.
    unsafe { CoTaskMemFree(Some(mix_format as *const _)) };
    Ok(system_mix_format)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChannelMask, OutputDeviceId};
    use windows::Win32::Media::Audio::{
        AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_E_SERVICE_NOT_RUNNING,
        WAVEFORMATEX,
    };

    fn stereo_source(bits_per_sample: u8, sample_rate_hz: u32) -> StreamFormat {
        StreamFormat {
            sample_rate_hz,
            bits_per_sample,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        }
    }

    fn candidates(bits_per_sample: u8) -> Vec<DeviceSampleFormat> {
        lossless_device_format_candidates(
            &stereo_source(bits_per_sample, 96_000),
            ValidBitsAlignment::MostSignificant,
        )
    }

    fn container_bits(representation: &WaveFormatRepresentation) -> u16 {
        // SAFETY: test reads the header of a live local structure.
        unsafe { representation.as_wave_format_pointer().read_unaligned() }.wBitsPerSample
    }

    fn call_error(hresult: windows::core::HRESULT) -> WasapiCallError {
        WasapiCallError {
            operation: "IAudioClient::Initialize (shared)",
            hresult,
        }
    }

    fn exclusive_refusal(candidate: DeviceSampleFormat) -> FormatProbeResult {
        FormatProbeResult {
            candidate,
            access_mode: DeviceAccessMode::Exclusive,
            outcome: FormatProbeOutcome::Refused,
        }
    }

    /// A fake client: its number tells which activation produced it.
    struct FakeClient {
        activation_number: usize,
    }

    #[test]
    fn stream_flags_are_event_driven_with_better_quality_conversion() {
        assert_eq!(
            SHARED_MODE_STREAM_FLAGS,
            0x8000_0000 | 0x0800_0000 | 0x0004_0000
        );
        assert_eq!(
            SHARED_MODE_BUFFER_DURATION_AND_PERIOD_IN_HUNDRED_NANOSECONDS,
            0
        );
    }

    #[test]
    fn first_accepted_candidate_wins_with_a_new_client_per_attempt() {
        let mut activation_count = 0;
        let mut tried_container_bits = Vec::new();
        let initialization = negotiate_and_initialize_shared(
            &candidates(24),
            Vec::new(),
            || {
                activation_count += 1;
                Ok(FakeClient {
                    activation_number: activation_count,
                })
            },
            |client, representation| {
                tried_container_bits.push(container_bits(representation));
                if container_bits(representation) == 32 {
                    Ok(client.activation_number)
                } else {
                    Err(call_error(AUDCLNT_E_UNSUPPORTED_FORMAT))
                }
            },
        )
        .unwrap_or_else(|error| panic!("the 4-byte candidate is accepted: {error}"));
        assert_eq!(tried_container_bits, vec![24, 32]);
        assert_eq!(activation_count, 2);
        assert_eq!(
            initialization.initialized, 2,
            "a new client after a refusal"
        );
        assert_eq!(container_bits(&initialization.representation), 32);
        let outcomes: Vec<_> = initialization
            .negotiation_log
            .iter()
            .map(|entry| (entry.access_mode, entry.outcome))
            .collect();
        assert_eq!(
            outcomes,
            vec![
                (
                    DeviceAccessMode::SharedThroughSystemMixer,
                    FormatProbeOutcome::Refused
                ),
                (
                    DeviceAccessMode::SharedThroughSystemMixer,
                    FormatProbeOutcome::Accepted
                ),
            ]
        );
        assert_eq!(
            initialization.negotiation_log[1].candidate,
            candidates(24)[1]
        );
    }

    #[test]
    fn the_first_candidate_is_the_exact_source_format() {
        let initialization = negotiate_and_initialize_shared(
            &candidates(16),
            Vec::new(),
            || Ok(()),
            |(), _representation| Ok(()),
        )
        .unwrap_or_else(|error| panic!("accepted: {error}"));
        let render_format =
            device_sample_format_from_representation(&initialization.representation)
                .expect("an integer PCM format");
        assert_eq!(render_format.container_bytes, 2);
        assert_eq!(render_format.valid_bits_per_sample, 16);
        assert_eq!(render_format.sample_rate_hz, 96_000);
        assert_eq!(render_format.channel_mask, ChannelMask(0x3));
        assert_eq!(initialization.negotiation_log.len(), 1);
    }

    #[test]
    fn a_fallback_log_reads_as_one_sequence() {
        let exclusive_attempts: Vec<_> =
            candidates(24).into_iter().map(exclusive_refusal).collect();
        let initialization = negotiate_and_initialize_shared(
            &candidates(24),
            exclusive_attempts.clone(),
            || Ok(()),
            |(), _representation| Ok(()),
        )
        .unwrap_or_else(|error| panic!("accepted: {error}"));
        assert_eq!(
            initialization.negotiation_log[..exclusive_attempts.len()],
            exclusive_attempts[..]
        );
        let shared_attempt = &initialization.negotiation_log[exclusive_attempts.len()];
        assert_eq!(
            shared_attempt.access_mode,
            DeviceAccessMode::SharedThroughSystemMixer
        );
        assert_eq!(shared_attempt.outcome, FormatProbeOutcome::Accepted);
    }

    #[test]
    fn every_candidate_refused_reports_both_modes_attempts() {
        let exclusive_attempts = vec![exclusive_refusal(candidates(24)[0])];
        let result = negotiate_and_initialize_shared(
            &candidates(24),
            exclusive_attempts,
            || Ok(()),
            |(), _representation| -> Result<(), WasapiCallError> {
                Err(call_error(AUDCLNT_E_UNSUPPORTED_FORMAT))
            },
        );
        let Err(OutputError::FormatNotSupported { tried }) = result else {
            panic!("every candidate was refused");
        };
        assert_eq!(tried.len(), 1 + candidates(24).len());
        assert_eq!(tried[0].access_mode, DeviceAccessMode::Exclusive);
        assert!(tried[1..].iter().all(|entry| {
            entry.access_mode == DeviceAccessMode::SharedThroughSystemMixer
                && entry.outcome == FormatProbeOutcome::Refused
        }));
    }

    #[test]
    fn invalid_argument_is_logged_and_the_next_candidate_is_tried() {
        let exclusive_attempts = vec![exclusive_refusal(candidates(24)[0])];
        let initialization = negotiate_and_initialize_shared(
            &candidates(24),
            exclusive_attempts.clone(),
            || Ok(()),
            |(), representation| {
                if container_bits(representation) == 24 {
                    Err(call_error(E_INVALIDARG))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_or_else(|error| panic!("the 4-byte candidate is accepted: {error}"));
        let log = &initialization.negotiation_log;
        assert_eq!(log[0], exclusive_attempts[0], "exclusive attempts kept");
        assert_eq!(
            log[1].outcome,
            FormatProbeOutcome::Error {
                platform_code: E_INVALIDARG.0
            }
        );
        assert_eq!(
            log[1].access_mode,
            DeviceAccessMode::SharedThroughSystemMixer
        );
        assert_eq!(log[2].outcome, FormatProbeOutcome::Accepted);
        assert_eq!(log.len(), 3);

        let every_candidate_invalid = negotiate_and_initialize_shared(
            &candidates(24),
            exclusive_attempts,
            || Ok(()),
            |(), _representation| -> Result<(), WasapiCallError> { Err(call_error(E_INVALIDARG)) },
        );
        let Err(OutputError::FormatNotSupported { tried }) = every_candidate_invalid else {
            panic!("no candidate was accepted");
        };
        assert_eq!(tried.len(), 1 + candidates(24).len());
        assert_eq!(tried[0].access_mode, DeviceAccessMode::Exclusive);
    }

    #[test]
    fn device_level_failures_end_the_attempt_without_trying_other_candidates() {
        for (hresult, is_expected) in [
            (
                AUDCLNT_E_DEVICE_IN_USE,
                (|error: &OutputError| matches!(error, OutputError::DeviceInUse))
                    as fn(&OutputError) -> bool,
            ),
            (AUDCLNT_E_DEVICE_INVALIDATED, |error| {
                matches!(error, OutputError::DeviceInvalidated)
            }),
            (AUDCLNT_E_SERVICE_NOT_RUNNING, |error| {
                matches!(error, OutputError::AudioServiceNotRunning)
            }),
        ] {
            let mut initialize_count = 0;
            let result = negotiate_and_initialize_shared(
                &candidates(24),
                Vec::new(),
                || Ok(()),
                |(), _representation| -> Result<(), WasapiCallError> {
                    initialize_count += 1;
                    Err(call_error(hresult))
                },
            );
            let Err(error) = result else {
                panic!("{hresult:?} must fail the open");
            };
            assert!(is_expected(&error), "{hresult:?} gave {error:?}");
            assert_eq!(initialize_count, 1);
        }
    }

    #[test]
    fn a_failed_activation_ends_the_attempt() {
        let result = negotiate_and_initialize_shared(
            &candidates(24),
            Vec::new(),
            || -> Result<(), WasapiCallError> {
                Err(WasapiCallError {
                    operation: "IMMDevice::Activate",
                    hresult: AUDCLNT_E_DEVICE_INVALIDATED,
                })
            },
            |(), _representation| Ok(()),
        );
        assert!(matches!(result, Err(OutputError::DeviceInvalidated)));
    }

    #[test]
    fn negotiated_output_reports_shared_mode_with_its_cause() {
        let render_format = candidates(24)[0];
        let mix_format = SystemMixFormat {
            sample_rate_hz: 176_400,
            bits_per_sample: 32,
            is_float: true,
            channel_count: 2,
        };
        for cause in [
            SharedModeCause::UserPreference,
            SharedModeCause::ExclusiveFormatRefused,
            SharedModeCause::ExclusiveModeDisabledBySystem,
        ] {
            let negotiated = shared_negotiated_output(
                OutputDeviceInfo {
                    id: OutputDeviceId("device".to_owned()),
                    display_name: "K11".to_owned(),
                    is_system_default: true,
                },
                cause,
                mix_format,
                render_format,
                1_764,
                100_000,
                Vec::new(),
            );
            assert_eq!(
                negotiated.access_mode,
                DeviceAccessMode::SharedThroughSystemMixer
            );
            assert_eq!(negotiated.shared_mode_cause, Some(cause));
            assert_eq!(negotiated.device_format, None);
            assert_eq!(negotiated.system_mix_format, Some(mix_format));
            assert_eq!(negotiated.render_format, render_format);
            assert_eq!(negotiated.buffer_frame_count, 1_764);
            assert_eq!(negotiated.device_period_in_hundred_nanoseconds, 100_000);
        }
    }

    #[test]
    fn mix_format_of_the_k11_reads_as_float() {
        // The K11's shared-mode mix format as reported by `probe_formats` (plan B13.3).
        let mut extensible = extensible_wave_format(&DeviceSampleFormat {
            sample_rate_hz: 176_400,
            container_bytes: 4,
            valid_bits_per_sample: 32,
            alignment: ValidBitsAlignment::MostSignificant,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        });
        extensible.SubFormat = windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
        // SAFETY: the pointer refers to a complete local WAVEFORMATEXTENSIBLE whose `cbSize`
        // is 22.
        let mix_format = unsafe {
            system_mix_format_from_pointer(std::ptr::from_ref(&extensible).cast::<WAVEFORMATEX>())
        };
        assert_eq!(
            mix_format,
            SystemMixFormat {
                sample_rate_hz: 176_400,
                bits_per_sample: 32,
                is_float: true,
                channel_count: 2,
            }
        );
    }
}
