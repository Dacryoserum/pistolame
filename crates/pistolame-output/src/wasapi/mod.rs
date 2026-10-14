//! WASAPI backend (Windows): shared mode, the default, and exclusive mode with its automatic
//! fallback to shared mode (plan B5).
//!
//! The `windows` crate is used directly so that every candidate format, every call on the
//! real-time path and every byte written to the device buffer is under our control (B5.1).
//! All `unsafe` code of the output crate lives in this module; each block says why it is sound.

#![allow(unsafe_code)]

mod buffer_fill_rule;
mod com_apartment;
mod device_watch;
mod devices;
mod hresult_mapping;
mod mmcss;
mod negotiation;
mod output_stream;
mod render_thread;
mod shared_mode;
mod stream_control;
mod stream_initialization;
mod wave_format;
mod win32_event;

use std::sync::Arc;

use windows::Win32::Media::Audio::{AUDCLNT_SHAREMODE_EXCLUSIVE, IAudioClient};

use self::com_apartment::ComApartment;
use self::device_watch::EndpointWatch;
use self::devices::{DeviceEnumerator, activate_audio_client};
use self::hresult_mapping::NameWasapiCall;
pub use self::negotiation::ExclusiveCandidateProbe;
use self::negotiation::probe_exclusive_candidate;
use self::output_stream::open_wasapi_stream;
use self::shared_mode::read_system_mix_format;
use self::stream_control::WasapiDiagnosticCounters;
pub use self::stream_control::WasapiDiagnostics;
use crate::{
    AudioOutput, DeviceAccessMode, DeviceChangeListener, DeviceSampleFormat, FormatProbeResult,
    OpenStreamError, OpenStreamRequest, OutputDeviceId, OutputDeviceInfo, OutputError,
    OutputStream, RenderSource, SystemMixFormat,
};

/// Name reported by `backend_name` and carried in every `NegotiatedOutput` of this backend.
pub(crate) const WASAPI_BACKEND_NAME: &str = "WASAPI";

/// The device's periods as reported by `IAudioClient::GetDevicePeriod`, in 100 ns units.
/// <https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getdeviceperiod>
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DevicePeriods {
    /// Default period between processing passes (shared mode uses it).
    pub default_period_in_hundred_nanoseconds: i64,
    /// Shortest period the device accepts in exclusive mode.
    pub minimum_period_in_hundred_nanoseconds: i64,
}

/// The WASAPI output backend. Holds no COM object between calls, so it can move between
/// threads; each call initializes COM for its own thread (B5.3). The endpoint notifications
/// registered by `watch_device_changes` live on their own thread.
#[derive(Debug, Default)]
pub struct WasapiOutput {
    diagnostics: Arc<WasapiDiagnosticCounters>,
    endpoint_watch: Option<EndpointWatch>,
}

impl WasapiOutput {
    /// Creates the backend. Nothing touches the platform until a method is called.
    pub fn new() -> WasapiOutput {
        WasapiOutput::default()
    }

    /// Counters of non-fatal problems across every stream this backend opened.
    pub fn diagnostics(&self) -> WasapiDiagnostics {
        self.diagnostics.snapshot()
    }

    /// Asks the device about each candidate in exclusive mode, reporting both the
    /// WAVEFORMATEXTENSIBLE answer and, where B5.4 allows it, the WAVEFORMATEX retry.
    /// Opens no stream.
    pub fn probe_exclusive_candidates(
        &mut self,
        device_id: &OutputDeviceId,
        candidates: &[DeviceSampleFormat],
    ) -> Result<Vec<ExclusiveCandidateProbe>, OutputError> {
        with_audio_client(device_id, |audio_client| {
            let mut is_format_supported =
                |representation: &wave_format::WaveFormatRepresentation| {
                    // SAFETY: the pointer refers to `representation`, alive for the call; no
                    // closest match is requested in exclusive mode.
                    unsafe {
                        audio_client.IsFormatSupported(
                            AUDCLNT_SHAREMODE_EXCLUSIVE,
                            representation.as_wave_format_pointer(),
                            None,
                        )
                    }
                };
            Ok(candidates
                .iter()
                .map(|candidate| probe_exclusive_candidate(candidate, &mut is_format_supported))
                .collect())
        })
    }

    /// The device's default and minimum periods.
    pub fn device_periods(
        &mut self,
        device_id: &OutputDeviceId,
    ) -> Result<DevicePeriods, OutputError> {
        with_audio_client(device_id, |audio_client| {
            let mut default_period_in_hundred_nanoseconds = 0;
            let mut minimum_period_in_hundred_nanoseconds = 0;
            // SAFETY: valid client; both out pointers refer to live locals.
            unsafe {
                audio_client.GetDevicePeriod(
                    Some(&mut default_period_in_hundred_nanoseconds),
                    Some(&mut minimum_period_in_hundred_nanoseconds),
                )
            }
            .for_operation("IAudioClient::GetDevicePeriod")?;
            Ok(DevicePeriods {
                default_period_in_hundred_nanoseconds,
                minimum_period_in_hundred_nanoseconds,
            })
        })
    }

    /// The shared-mode mix format (`GetMixFormat`), the format the Windows audio engine converts
    /// shared-mode streams to, for display. Opens no stream.
    pub fn system_mix_format(
        &mut self,
        device_id: &OutputDeviceId,
    ) -> Result<SystemMixFormat, OutputError> {
        with_audio_client(device_id, |audio_client| {
            Ok(read_system_mix_format(audio_client)?)
        })
    }
}

/// Runs `action` with a freshly activated client of the device, on this thread, with COM
/// initialized for the duration of the call.
fn with_audio_client<T>(
    device_id: &OutputDeviceId,
    action: impl FnOnce(&IAudioClient) -> Result<T, OutputError>,
) -> Result<T, OutputError> {
    let _com_apartment = ComApartment::initialize_multithreaded()?;
    let enumerator = DeviceEnumerator::new()?;
    let (device, _info) = enumerator.find_active_render_device(device_id)?;
    let audio_client = activate_audio_client(&device)?;
    action(&audio_client)
}

impl AudioOutput for WasapiOutput {
    fn backend_name(&self) -> &'static str {
        WASAPI_BACKEND_NAME
    }

    fn list_devices(&mut self) -> Result<Vec<OutputDeviceInfo>, OutputError> {
        let _com_apartment = ComApartment::initialize_multithreaded()?;
        let enumerator = DeviceEnumerator::new()?;
        Ok(enumerator.list_active_render_devices()?)
    }

    /// Exclusive-mode answers only, one per candidate (the WAVEFORMATEX retry is folded in).
    fn probe_formats(
        &mut self,
        device_id: &OutputDeviceId,
        candidates: &[DeviceSampleFormat],
    ) -> Result<Vec<FormatProbeResult>, OutputError> {
        let probes = self.probe_exclusive_candidates(device_id, candidates)?;
        Ok(probes
            .iter()
            .map(|probe| FormatProbeResult {
                candidate: probe.candidate,
                access_mode: DeviceAccessMode::Exclusive,
                outcome: probe.final_outcome(),
            })
            .collect())
    }

    fn open_stream(
        &mut self,
        request: OpenStreamRequest,
        render_source: Box<dyn RenderSource>,
    ) -> Result<Box<dyn OutputStream>, OpenStreamError> {
        open_wasapi_stream(request, render_source, Arc::clone(&self.diagnostics))
    }

    /// Registers an `IMMNotificationClient` for default-device and device-state changes.
    fn watch_device_changes(&mut self, on_change: DeviceChangeListener) -> bool {
        self.endpoint_watch = EndpointWatch::start(on_change);
        self.endpoint_watch.is_some()
    }
}
