//! ALSA backend (Linux), plan `walking-skeleton.md` (B3 "Fit with the other platforms"):
//! - exclusive mode: the card's `hw:` PCM, no conversion and no mixing, at the source rate in a
//!   lossless integer format (bit-perfect); the sound server cannot use the card meanwhile;
//! - shared mode: the `default` PCM (PipeWire or PulseAudio, which mix and convert), or the
//!   card behind ALSA's `plug` converter (`plughw:`) when a card was chosen.
//!
//! When exclusive mode cannot be had, the stream falls back to shared mode like on the other
//! platforms. Device changes are not watched (no notification API in libasound); the engine
//! re-reads the devices before every open.

mod devices;
mod errors;
mod pcm_setup;
mod stream;

use self::devices::{DeviceRoute, device_info, ensure_listed, list_output_devices};
use self::pcm_setup::{ConfiguredPcm, negotiate, open_pcm, probe};
use self::stream::{StreamPlan, open_alsa_stream};
use crate::alsa_layouts::alsa_candidates;
use crate::{
    AudioOutput, DeviceAccessMode, DeviceSampleDelivery, DeviceSampleFormat, FormatProbeOutcome,
    FormatProbeResult, NegotiatedOutput, OpenPolicy, OpenStreamError, OpenStreamRequest,
    OutputDeviceId, OutputDeviceInfo, OutputError, OutputStream, RenderSource, SharedModeCause,
    StreamFormat,
};

/// Name reported by `backend_name` and carried in every `NegotiatedOutput` of this backend.
const ALSA_BACKEND_NAME: &str = "ALSA";
/// Number of 100 ns units in one second, to express periods in the contract's WASAPI unit.
const HUNDRED_NANOSECONDS_PER_SECOND: u64 = 10_000_000;

/// The ALSA output backend. Holds no ALSA object between calls.
#[derive(Debug, Default)]
pub struct AlsaOutput;

impl AlsaOutput {
    /// Creates the backend. Nothing touches ALSA until a method is called.
    pub fn new() -> AlsaOutput {
        AlsaOutput
    }
}

impl AudioOutput for AlsaOutput {
    fn backend_name(&self) -> &'static str {
        ALSA_BACKEND_NAME
    }

    fn list_devices(&mut self) -> Result<Vec<OutputDeviceInfo>, OutputError> {
        list_output_devices()
    }

    /// Exclusive-mode answers, from the card's `hw:` PCM. The system default has no exclusive
    /// mode: every candidate is refused there.
    fn probe_formats(
        &mut self,
        device_id: &OutputDeviceId,
        candidates: &[DeviceSampleFormat],
    ) -> Result<Vec<FormatProbeResult>, OutputError> {
        let answer = |candidate: &DeviceSampleFormat, outcome| FormatProbeResult {
            candidate: *candidate,
            access_mode: DeviceAccessMode::Exclusive,
            outcome,
        };
        let DeviceRoute::Hardware {
            exclusive_pcm_name, ..
        } = ensure_listed(device_id)?
        else {
            return Ok(candidates
                .iter()
                .map(|candidate| answer(candidate, FormatProbeOutcome::Refused))
                .collect());
        };
        let pcm = open_pcm(&exclusive_pcm_name)?;
        Ok(candidates
            .iter()
            .map(|candidate| answer(candidate, probe(&pcm, candidate)))
            .collect())
    }

    fn open_stream(
        &mut self,
        request: OpenStreamRequest,
        render_source: Box<dyn RenderSource>,
    ) -> Result<Box<dyn OutputStream>, OpenStreamError> {
        match plan_stream(&request) {
            Ok(plan) => open_alsa_stream(plan, render_source),
            Err(error) => Err(OpenStreamError {
                error,
                render_source,
            }),
        }
    }
}

/// Decides the mode and the format, and opens the PCM.
fn plan_stream(request: &OpenStreamRequest) -> Result<StreamPlan, OutputError> {
    let route = ensure_listed(&request.device_id)?;
    let device = device_info(&request.device_id)?;
    let source = &request.source_format;
    let may_fall_back = request.policy == OpenPolicy::ExclusiveThenShared;
    match (request.policy, &route) {
        (OpenPolicy::SharedOnly, _) => plan_shared(
            route.clone(),
            device,
            source,
            SharedModeCause::UserPreference,
            Vec::new(),
        ),
        // The sound server owns the system default: exclusive mode needs a chosen card.
        (_, DeviceRoute::SystemDefault) if may_fall_back => plan_shared(
            route.clone(),
            device,
            source,
            SharedModeCause::ExclusiveModeDisabledBySystem,
            Vec::new(),
        ),
        (_, DeviceRoute::SystemDefault) => Err(OutputError::ExclusiveModeNotAllowed),
        (
            _,
            DeviceRoute::Hardware {
                exclusive_pcm_name, ..
            },
        ) => plan_exclusive(&route, exclusive_pcm_name, device, source, may_fall_back),
    }
}

fn plan_exclusive(
    route: &DeviceRoute,
    exclusive_pcm_name: &str,
    device: OutputDeviceInfo,
    source: &StreamFormat,
    may_fall_back: bool,
) -> Result<StreamPlan, OutputError> {
    let pcm = match open_pcm(exclusive_pcm_name) {
        Ok(pcm) => pcm,
        // The sound server (or another player) plays through the card.
        Err(OutputError::DeviceInUse) if may_fall_back => {
            return plan_shared(
                route.clone(),
                device,
                source,
                SharedModeCause::DeviceBusyInSharedMode,
                Vec::new(),
            );
        }
        Err(error) => return Err(error),
    };
    let (configured, log) = negotiate(
        pcm,
        &alsa_candidates(source, true),
        DeviceAccessMode::Exclusive,
    );
    match configured {
        Some(configured) => Ok(stream_plan(
            route.clone(),
            device,
            configured,
            DeviceAccessMode::Exclusive,
            None,
            log,
        )),
        None if may_fall_back => plan_shared(
            route.clone(),
            device,
            source,
            SharedModeCause::ExclusiveFormatRefused,
            log,
        ),
        None => Err(OutputError::FormatNotSupported { tried: log }),
    }
}

fn plan_shared(
    route: DeviceRoute,
    device: OutputDeviceInfo,
    source: &StreamFormat,
    cause: SharedModeCause,
    mut log: Vec<FormatProbeResult>,
) -> Result<StreamPlan, OutputError> {
    let pcm = open_pcm(route.shared_pcm_name())?;
    let (configured, attempts) = negotiate(
        pcm,
        &alsa_candidates(source, false),
        DeviceAccessMode::SharedThroughSystemMixer,
    );
    log.extend(attempts);
    match configured {
        Some(configured) => Ok(stream_plan(
            route,
            device,
            configured,
            DeviceAccessMode::SharedThroughSystemMixer,
            Some(cause),
            log,
        )),
        None => Err(OutputError::FormatNotSupported { tried: log }),
    }
}

/// The plan for a configured PCM. In shared mode the sound server's own format is unknown to
/// ALSA clients, so `system_mix_format` stays empty.
fn stream_plan(
    route: DeviceRoute,
    device: OutputDeviceInfo,
    configured: ConfiguredPcm,
    access_mode: DeviceAccessMode,
    shared_mode_cause: Option<SharedModeCause>,
    negotiation_log: Vec<FormatProbeResult>,
) -> StreamPlan {
    let format = configured.format;
    let period_in_hundred_nanoseconds = u64::from(configured.period_frame_count)
        * HUNDRED_NANOSECONDS_PER_SECOND
        / u64::from(format.sample_rate_hz.max(1));
    StreamPlan {
        negotiated: NegotiatedOutput {
            backend_name: ALSA_BACKEND_NAME,
            device,
            access_mode,
            shared_mode_cause,
            device_format: (access_mode == DeviceAccessMode::Exclusive).then_some(format),
            system_mix_format: None,
            render_format: format,
            sample_delivery: DeviceSampleDelivery::Unchanged,
            buffer_frame_count: configured.buffer_frame_count,
            device_period_in_hundred_nanoseconds: i64::try_from(period_in_hundred_nanoseconds)
                .unwrap_or(i64::MAX),
            negotiation_log,
        },
        configured,
        route,
    }
}
