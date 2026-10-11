//! Opening a stream on the render thread: policy handling, exclusive negotiation,
//! `Initialize` with the alignment retry, event handle and render client (plan B5.4, B5.5).
//!
//! `IAudioClient::Initialize`, including the exclusive event-driven rules and the
//! AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED procedure quoted below:
//! <https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-initialize>

use std::time::Duration;

use pistolame_core::timing::PREFERRED_EXCLUSIVE_PERIOD;
use windows::Win32::Media::Audio::{
    AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED, AUDCLNT_E_UNSUPPORTED_FORMAT, AUDCLNT_SHAREMODE_EXCLUSIVE,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, IAudioClient, IAudioRenderClient, IMMDevice,
};

use super::buffer_fill_rule::BufferFillRule;
use super::devices::{DeviceEnumerator, activate_audio_client};
use super::hresult_mapping::{NameWasapiCall, WasapiCallError};
use super::negotiation::{
    ExclusiveAttemptFailure, ExclusiveFailureResolution, FirstOpenStep, first_open_step,
    negotiate_and_initialize_exclusive, resolve_exclusive_failure, shared_mode_not_implemented_yet,
};
use super::wave_format::{WaveFormatRepresentation, device_sample_format_from_representation};
use super::win32_event::AutoResetEvent;
use crate::{
    DeviceAccessMode, NegotiatedOutput, OpenStreamRequest, OutputDeviceInfo, OutputError,
    ValidBitsAlignment, lossless_device_format_candidates,
};

/// WASAPI's time unit: one REFERENCE_TIME tick is 100 ns.
const HUNDRED_NANOSECONDS_PER_SECOND: u64 = 10_000_000;
/// Nanoseconds in one REFERENCE_TIME tick.
const NANOSECONDS_PER_HUNDRED_NANOSECONDS: u128 = 100;

/// A duration in REFERENCE_TIME units (100 ns), saturating.
pub(crate) fn duration_in_hundred_nanoseconds(duration: Duration) -> i64 {
    i64::try_from(duration.as_nanos() / NANOSECONDS_PER_HUNDRED_NANOSECONDS).unwrap_or(i64::MAX)
}

/// The exclusive period to request: the preferred 20 ms, or the device minimum if larger
/// (B5.5: robustness over latency).
pub(crate) fn requested_exclusive_period_in_hundred_nanoseconds(
    minimum_device_period_in_hundred_nanoseconds: i64,
) -> i64 {
    duration_in_hundred_nanoseconds(PREFERRED_EXCLUSIVE_PERIOD)
        .max(minimum_device_period_in_hundred_nanoseconds)
}

/// The period matching an aligned buffer size, rounded to the nearest tick. Integer form of
/// Microsoft's `(REFERENCE_TIME)((10000.0 * 1000 / nSamplesPerSec * nFrames) + 0.5)` from the
/// AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED procedure in the `Initialize` remarks.
pub(crate) fn aligned_period_in_hundred_nanoseconds(
    aligned_buffer_frame_count: u32,
    sample_rate_hz: u32,
) -> i64 {
    let sample_rate_hz = u64::from(sample_rate_hz.max(1));
    let rounded = (HUNDRED_NANOSECONDS_PER_SECOND * u64::from(aligned_buffer_frame_count)
        + sample_rate_hz / 2)
        / sample_rate_hz;
    i64::try_from(rounded).unwrap_or(i64::MAX)
}

/// Everything the render loop needs once the device is initialized.
///
/// Field order matters: Rust drops fields in declaration order, so the client (and its render
/// service) is released before `buffer_event` is closed, and the event registered with
/// `SetEventHandle` outlives every object that could signal it.
pub(crate) struct InitializedStream {
    /// The initialized client; only the render thread touches it.
    pub(crate) audio_client: IAudioClient,
    /// The render service obtained from `audio_client`.
    pub(crate) render_client: IAudioRenderClient,
    /// Event WASAPI signals when a buffer is ready to be filled.
    pub(crate) buffer_event: AutoResetEvent,
    /// What the platform granted.
    pub(crate) negotiated: NegotiatedOutput,
    /// How many frames to write per wake-up.
    pub(crate) fill_rule: BufferFillRule,
}

/// Opens the requested device according to the policy. Runs on the render thread with COM
/// initialized.
pub(crate) fn initialize_stream(
    request: &OpenStreamRequest,
) -> Result<InitializedStream, OutputError> {
    let enumerator = DeviceEnumerator::new()?;
    let (device, device_info) = enumerator.find_active_render_device(&request.device_id)?;
    if let FirstOpenStep::OpenShared(cause) = first_open_step(request.policy) {
        return Err(shared_mode_not_implemented_yet(cause, Vec::new()));
    }
    match initialize_exclusive_stream(&device, device_info, request) {
        Ok(stream) => Ok(stream),
        Err(failure) => match resolve_exclusive_failure(request.policy, failure) {
            ExclusiveFailureResolution::OpenShared {
                cause,
                negotiation_log,
            } => Err(shared_mode_not_implemented_yet(cause, negotiation_log)),
            ExclusiveFailureResolution::Fail(error) => Err(error),
        },
    }
}

fn initialize_exclusive_stream(
    device: &IMMDevice,
    device_info: OutputDeviceInfo,
    request: &OpenStreamRequest,
) -> Result<InitializedStream, ExclusiveAttemptFailure> {
    let candidates = lossless_device_format_candidates(
        &request.source_format,
        ValidBitsAlignment::MostSignificant,
    );
    let initialization = negotiate_and_initialize_exclusive(
        &candidates,
        || activate_audio_client(device),
        |client: &IAudioClient, representation| {
            // SAFETY: the format pointer refers to `representation`, alive for the whole call;
            // no closest-match pointer is requested, as exclusive mode has none.
            unsafe {
                client.IsFormatSupported(
                    AUDCLNT_SHAREMODE_EXCLUSIVE,
                    representation.as_wave_format_pointer(),
                    None,
                )
            }
        },
        |client, representation| initialize_exclusive_client(device, client, representation),
    )?;
    let failed = |error: WasapiCallError| ExclusiveAttemptFailure::Failed(error.to_output_error());
    let device_format = device_sample_format_from_representation(&initialization.representation)
        .ok_or(ExclusiveAttemptFailure::Failed(OutputError::Platform {
            operation: "read back the accepted format",
            platform_code: AUDCLNT_E_UNSUPPORTED_FORMAT.0,
        }))?;
    let initialized = initialization.initialized;
    let streaming = prepare_streaming(&initialized.audio_client).map_err(failed)?;
    Ok(InitializedStream {
        negotiated: NegotiatedOutput {
            device: device_info,
            access_mode: DeviceAccessMode::Exclusive,
            shared_mode_cause: None,
            device_format: Some(device_format),
            system_mix_format: None,
            render_format: device_format,
            buffer_frame_count: streaming.buffer_frame_count,
            device_period_in_hundred_nanoseconds: initialized.period_in_hundred_nanoseconds,
            negotiation_log: initialization.negotiation_log,
        },
        audio_client: initialized.audio_client,
        render_client: streaming.render_client,
        buffer_event: streaming.buffer_event,
        fill_rule: BufferFillRule::ExclusiveWholeBuffer,
    })
}

struct InitializedClient {
    audio_client: IAudioClient,
    period_in_hundred_nanoseconds: i64,
}

/// `Initialize` in exclusive event-driven mode, with the documented alignment retry (once).
fn initialize_exclusive_client(
    device: &IMMDevice,
    audio_client: IAudioClient,
    representation: &WaveFormatRepresentation,
) -> Result<InitializedClient, WasapiCallError> {
    let mut minimum_period_in_hundred_nanoseconds: i64 = 0;
    // SAFETY: valid client; the out pointer refers to a live local; the default period is not
    // requested.
    unsafe { audio_client.GetDevicePeriod(None, Some(&mut minimum_period_in_hundred_nanoseconds)) }
        .for_operation("IAudioClient::GetDevicePeriod")?;
    let period =
        requested_exclusive_period_in_hundred_nanoseconds(minimum_period_in_hundred_nanoseconds);
    match call_initialize_exclusive(&audio_client, period, representation) {
        Ok(()) => Ok(InitializedClient {
            audio_client,
            period_in_hundred_nanoseconds: period,
        }),
        Err(error) if error.hresult == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED => {
            // Step 1: "Call IAudioClient::GetBufferSize and receive the next-highest-aligned
            // buffer size (in frames)." Step 2: release the client.
            // SAFETY: valid client; GetBufferSize is documented to work after this failure.
            let aligned_frames = unsafe { audio_client.GetBufferSize() }
                .for_operation("IAudioClient::GetBufferSize (alignment)")?;
            drop(audio_client);
            // SAFETY: reading the sample rate field of a live header (copied out by value).
            let sample_rate_hz =
                unsafe { representation.as_wave_format_pointer().read_unaligned() }.nSamplesPerSec;
            let aligned_period =
                aligned_period_in_hundred_nanoseconds(aligned_frames, sample_rate_hz);
            // Steps 3 and 4: new client, Initialize again with the aligned period.
            let aligned_client = activate_audio_client(device)?;
            call_initialize_exclusive(&aligned_client, aligned_period, representation)?;
            Ok(InitializedClient {
                audio_client: aligned_client,
                period_in_hundred_nanoseconds: aligned_period,
            })
        }
        Err(error) => Err(error),
    }
}

/// "In exclusive mode ... with EVENTCALLBACK, hnsPeriodicity must be nonzero and equal to
/// hnsBufferDuration" (`Initialize` docs).
fn call_initialize_exclusive(
    audio_client: &IAudioClient,
    period_in_hundred_nanoseconds: i64,
    representation: &WaveFormatRepresentation,
) -> Result<(), WasapiCallError> {
    // SAFETY: the format pointer refers to `representation`, alive for the call; no session
    // GUID (default session).
    unsafe {
        audio_client.Initialize(
            AUDCLNT_SHAREMODE_EXCLUSIVE,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            period_in_hundred_nanoseconds,
            period_in_hundred_nanoseconds,
            representation.as_wave_format_pointer(),
            None,
        )
    }
    .for_operation("IAudioClient::Initialize")
}

struct StreamingObjects {
    render_client: IAudioRenderClient,
    buffer_event: AutoResetEvent,
    buffer_frame_count: u32,
}

/// Event handle, buffer size and render service of an initialized client.
fn prepare_streaming(audio_client: &IAudioClient) -> Result<StreamingObjects, WasapiCallError> {
    let buffer_event = AutoResetEvent::new()?;
    // SAFETY: valid initialized client and live event. On success the event moves into
    // `InitializedStream`, whose field order drops the client before the event. If a call
    // below fails, `buffer_event` is closed on return while the client still lives (the
    // caller drops it afterwards); that is harmless because the stream was never started, so
    // WASAPI has no reason to signal the closed handle before the client is released.
    unsafe { audio_client.SetEventHandle(buffer_event.raw_handle()) }
        .for_operation("IAudioClient::SetEventHandle")?;
    // SAFETY: valid initialized client.
    let buffer_frame_count =
        unsafe { audio_client.GetBufferSize() }.for_operation("IAudioClient::GetBufferSize")?;
    // SAFETY: valid initialized client; IAudioRenderClient is a documented service.
    let render_client = unsafe { audio_client.GetService::<IAudioRenderClient>() }
        .for_operation("IAudioClient::GetService(IAudioRenderClient)")?;
    Ok(StreamingObjects {
        render_client,
        buffer_event,
        buffer_frame_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_period_is_twenty_milliseconds_in_hundred_nanoseconds() {
        assert_eq!(
            duration_in_hundred_nanoseconds(PREFERRED_EXCLUSIVE_PERIOD),
            200_000
        );
    }

    #[test]
    fn requested_period_never_goes_below_the_device_minimum() {
        assert_eq!(
            requested_exclusive_period_in_hundred_nanoseconds(30_000),
            200_000
        );
        assert_eq!(
            requested_exclusive_period_in_hundred_nanoseconds(250_000),
            250_000
        );
    }

    #[test]
    fn aligned_period_matches_the_documented_rounding() {
        // 882 frames at 44.1 kHz are exactly 20 ms.
        assert_eq!(aligned_period_in_hundred_nanoseconds(882, 44_100), 200_000);
        // 896 frames at 44.1 kHz = 203 174.6 ticks, rounded up.
        assert_eq!(aligned_period_in_hundred_nanoseconds(896, 44_100), 203_175);
        // 1 frame at 192 kHz = 52.08 ticks, rounded down.
        assert_eq!(aligned_period_in_hundred_nanoseconds(1, 192_000), 52);
        // 3840 frames at 192 kHz are exactly 20 ms.
        assert_eq!(
            aligned_period_in_hundred_nanoseconds(3_840, 192_000),
            200_000
        );
    }
}
