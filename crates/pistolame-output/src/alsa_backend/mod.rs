//! ALSA backend (Linux), plan `walking-skeleton.md` (B3 "Fit with the other platforms"):
//! - exclusive mode: the chosen card's `hw:` PCM, no conversion and no mixing, at the source
//!   rate in a lossless integer format (bit-perfect); the sound server cannot use the card
//!   meanwhile;
//! - shared mode: the `default` PCM, that is the sound server (PipeWire or PulseAudio), which
//!   mixes, converts and picks the output device itself.
//!
//! When exclusive mode cannot be had, the stream falls back to shared mode like on the other
//! platforms. Device changes are not watched (libasound has no notification API); the engine
//! re-reads the devices before every open.
//!
//! Reference implementation consulted for ALSA usage: MPD's ALSA output plugin
//! (`src/output/plugins/AlsaOutputPlugin.cxx`, behavior only).

mod devices;
mod errors;
mod pcm_setup;
mod stream;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use self::devices::{
    DeviceRoute, SYSTEM_DEFAULT_PCM_NAME, device_info, ensure_listed, list_output_devices,
};
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
/// Most channels sent to a `hw:` PCM. Beyond stereo, the card's default channel order (ALSA:
/// front, rear, centre, LFE for 5.1) differs from the files' WAVE order (front, centre, LFE,
/// rear); until channel maps are set, multichannel files play through the sound server, which
/// maps them, rather than reach the wrong speakers while called bit-perfect.
const MAXIMUM_EXCLUSIVE_CHANNEL_COUNT: u8 = 2;

/// The ALSA output backend. Holds no ALSA object between calls.
#[derive(Debug, Default)]
pub struct AlsaOutput {
    /// Underruns and suspends recovered by every stream of this backend.
    recovered_device_errors: Arc<AtomicU64>,
}

impl AlsaOutput {
    /// Creates the backend. Nothing touches ALSA until a method is called.
    pub fn new() -> AlsaOutput {
        AlsaOutput::default()
    }

    /// Underruns and suspends the streams recovered from so far: playback went on, but the
    /// listener may have heard a gap.
    pub fn recovered_device_error_count(&self) -> u64 {
        self.recovered_device_errors.load(Ordering::Relaxed)
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
        let DeviceRoute::Hardware { exclusive_pcm_name } = ensure_listed(device_id)? else {
            return Ok(candidates
                .iter()
                .map(|candidate| answer(candidate, FormatProbeOutcome::Refused))
                .collect());
        };
        let pcm = open_pcm(&exclusive_pcm_name)?;
        Ok(candidates
            .iter()
            .map(|candidate| {
                let outcome = if candidate.channel_count > MAXIMUM_EXCLUSIVE_CHANNEL_COUNT {
                    FormatProbeOutcome::Refused
                } else {
                    probe(&pcm, candidate)
                };
                answer(candidate, outcome)
            })
            .collect())
    }

    fn open_stream(
        &mut self,
        request: OpenStreamRequest,
        render_source: Box<dyn RenderSource>,
    ) -> Result<Box<dyn OutputStream>, OpenStreamError> {
        match plan_stream(&request) {
            Ok(plan) => open_alsa_stream(
                plan,
                render_source,
                Arc::clone(&self.recovered_device_errors),
            ),
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
    let source = &request.source_format;
    let may_fall_back = request.policy == OpenPolicy::ExclusiveThenShared;
    let fall_back = |cause: SharedModeCause, log: Vec<FormatProbeResult>| {
        if may_fall_back {
            plan_shared(source, cause, log)
        } else {
            Err(match cause {
                SharedModeCause::DeviceBusyInSharedMode => OutputError::DeviceInUse,
                SharedModeCause::ExclusiveModeDisabledBySystem => {
                    OutputError::ExclusiveModeNotAllowed
                }
                _ => OutputError::FormatNotSupported { tried: log },
            })
        }
    };
    let exclusive_pcm_name = match (request.policy, &route) {
        (OpenPolicy::SharedOnly, _) => {
            return plan_shared(source, SharedModeCause::UserPreference, Vec::new());
        }
        // The sound server owns the system default: exclusive mode needs a chosen card.
        (_, DeviceRoute::SystemDefault) => {
            return fall_back(SharedModeCause::ExclusiveModeDisabledBySystem, Vec::new());
        }
        (_, DeviceRoute::Hardware { exclusive_pcm_name }) => exclusive_pcm_name,
    };
    if source.channel_count > MAXIMUM_EXCLUSIVE_CHANNEL_COUNT {
        return fall_back(SharedModeCause::ExclusiveFormatRefused, Vec::new());
    }
    let pcm = match open_pcm(exclusive_pcm_name) {
        Ok(pcm) => pcm,
        // The sound server (or another player) is playing through the card.
        Err(OutputError::DeviceInUse) => {
            return fall_back(SharedModeCause::DeviceBusyInSharedMode, Vec::new());
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
            route,
            device_info(&request.device_id)?,
            configured,
            DeviceAccessMode::Exclusive,
            None,
            log,
        )),
        None => fall_back(SharedModeCause::ExclusiveFormatRefused, log),
    }
}

/// Plays through the sound server (`default`), which then decides the output device.
fn plan_shared(
    source: &StreamFormat,
    cause: SharedModeCause,
    mut log: Vec<FormatProbeResult>,
) -> Result<StreamPlan, OutputError> {
    let pcm = open_pcm(SYSTEM_DEFAULT_PCM_NAME)?;
    let (configured, attempts) = negotiate(
        pcm,
        &alsa_candidates(source, false),
        DeviceAccessMode::SharedThroughSystemMixer,
    );
    log.extend(attempts);
    let Some(configured) = configured else {
        return Err(OutputError::FormatNotSupported { tried: log });
    };
    let system_default = OutputDeviceId(SYSTEM_DEFAULT_PCM_NAME.to_owned());
    Ok(stream_plan(
        DeviceRoute::SystemDefault,
        device_info(&system_default)?,
        configured,
        DeviceAccessMode::SharedThroughSystemMixer,
        Some(cause),
        log,
    ))
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
