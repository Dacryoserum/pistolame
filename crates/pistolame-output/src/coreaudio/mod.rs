//! CoreAudio backend (macOS), plan `docs/plans/coreaudio.md` §0:
//! - shared mode (default): AUHAL output unit; macOS converts the rate and mixes;
//! - exclusive mode: hog mode + device rate = source rate + exact float32 delivery, with an
//!   automatic fallback to shared mode when the device cannot run at the source format.
//!
//! All `unsafe` code of the CoreAudio backend lives in this module; each block says why it is
//! sound.

#![allow(unsafe_code)]

mod device_takeover;
mod device_watch;
mod devices;
mod negotiation;
mod output_unit;
mod properties;
mod stream;

use objc2_core_audio::AudioObjectID;
use objc2_core_audio_types::kAudioFormatFlagIsFloat;

use self::device_takeover::ExclusiveTakeover;
use self::device_watch::SystemDeviceWatch;
use self::devices::{
    available_nominal_sample_rates, find_output_device, io_buffer_frame_count, list_output_devices,
    nominal_sample_rate, output_channel_count, output_stream_formats,
};
use self::negotiation::{
    ExclusiveDeviceCapabilities, negotiate_exclusive_format, shared_render_format,
};
use self::stream::{StreamPlan, open_coreaudio_stream};
use crate::{
    AudioOutput, DeviceAccessMode, DeviceChangeListener, DeviceSampleDelivery, DeviceSampleFormat,
    FormatProbeOutcome, FormatProbeResult, NegotiatedOutput, OpenPolicy, OpenStreamError,
    OpenStreamRequest, OutputDeviceId, OutputDeviceInfo, OutputError, OutputStream, RenderSource,
    SharedModeCause, StreamFormat, SystemMixFormat,
};

/// Name reported by `backend_name` and carried in every `NegotiatedOutput` of this backend.
const COREAUDIO_BACKEND_NAME: &str = "CoreAudio";
/// Number of 100 ns units in one second, to express periods in the contract's WASAPI unit.
const HUNDRED_NANOSECONDS_PER_SECOND: f64 = 10_000_000.0;

/// The CoreAudio output backend. Holds no CoreAudio object between calls, except the system
/// listeners registered by `watch_device_changes`.
#[derive(Debug, Default)]
pub struct CoreAudioOutput {
    device_watch: Option<SystemDeviceWatch>,
}

impl CoreAudioOutput {
    /// Creates the backend. Nothing touches the platform until a method is called.
    pub fn new() -> CoreAudioOutput {
        CoreAudioOutput::default()
    }
}

/// A device found for a stream, with what negotiation needs from it.
struct OpenedDevice {
    object_id: AudioObjectID,
    info: OutputDeviceInfo,
}

impl AudioOutput for CoreAudioOutput {
    fn backend_name(&self) -> &'static str {
        COREAUDIO_BACKEND_NAME
    }

    fn list_devices(&mut self) -> Result<Vec<OutputDeviceInfo>, OutputError> {
        list_output_devices()
    }

    /// Exclusive-mode answers: a candidate is accepted when the device can run at its rate with
    /// its channel count and the exact float conversion supports it.
    fn probe_formats(
        &mut self,
        device_id: &OutputDeviceId,
        candidates: &[DeviceSampleFormat],
    ) -> Result<Vec<FormatProbeResult>, OutputError> {
        let (device, _info) = find_output_device(device_id)?;
        let capabilities = exclusive_capabilities(device)?;
        Ok(candidates
            .iter()
            .map(|candidate| {
                let source = StreamFormat {
                    sample_rate_hz: candidate.sample_rate_hz,
                    bits_per_sample: candidate.valid_bits_per_sample,
                    channel_count: candidate.channel_count,
                    channel_mask: candidate.channel_mask,
                };
                let accepted = negotiate_exclusive_format(&source, &capabilities).accepted;
                FormatProbeResult {
                    candidate: *candidate,
                    access_mode: DeviceAccessMode::Exclusive,
                    outcome: if accepted == Some(*candidate) {
                        FormatProbeOutcome::Accepted
                    } else {
                        FormatProbeOutcome::Refused
                    },
                }
            })
            .collect())
    }

    fn open_stream(
        &mut self,
        request: OpenStreamRequest,
        render_source: Box<dyn RenderSource>,
    ) -> Result<Box<dyn OutputStream>, OpenStreamError> {
        let plan = match plan_stream(&request) {
            Ok(plan) => plan,
            Err(error) => {
                return Err(OpenStreamError {
                    error,
                    render_source,
                });
            }
        };
        open_coreaudio_stream(plan, render_source).map_err(|(error, render_source)| {
            OpenStreamError {
                error,
                render_source,
            }
        })
    }

    /// Listens to `kAudioHardwarePropertyDefaultOutputDevice` and
    /// `kAudioHardwarePropertyDevices` on the system object.
    fn watch_device_changes(&mut self, on_change: DeviceChangeListener) -> bool {
        self.device_watch = SystemDeviceWatch::start(on_change);
        self.device_watch.is_some()
    }
}

/// Decides the mode and the format, and takes the device over in exclusive mode.
fn plan_stream(request: &OpenStreamRequest) -> Result<StreamPlan, OutputError> {
    let (object_id, info) = find_output_device(&request.device_id)?;
    let device = OpenedDevice { object_id, info };
    match request.policy {
        OpenPolicy::SharedOnly => plan_shared(
            &device,
            &request.source_format,
            SharedModeCause::UserPreference,
            Vec::new(),
        ),
        OpenPolicy::ExclusiveThenShared | OpenPolicy::ExclusiveOnly => {
            plan_exclusive(&device, &request.source_format, request.policy)
        }
    }
}

fn plan_exclusive(
    device: &OpenedDevice,
    source_format: &StreamFormat,
    policy: OpenPolicy,
) -> Result<StreamPlan, OutputError> {
    let may_fall_back = policy == OpenPolicy::ExclusiveThenShared;
    let negotiation =
        negotiate_exclusive_format(source_format, &exclusive_capabilities(device.object_id)?);
    let Some(render_format) = negotiation.accepted else {
        return if may_fall_back {
            plan_shared(
                device,
                source_format,
                SharedModeCause::ExclusiveFormatRefused,
                negotiation.log,
            )
        } else {
            Err(OutputError::FormatNotSupported {
                tried: negotiation.log,
            })
        };
    };
    let takeover = match ExclusiveTakeover::acquire(device.object_id, render_format.sample_rate_hz)
    {
        Ok(takeover) => takeover,
        // Another application holds the device: a failure, never a fallback (same rule as
        // WASAPI exclusive mode held elsewhere).
        Err(OutputError::DeviceInUse) => return Err(OutputError::DeviceInUse),
        Err(error) if may_fall_back => {
            let cause = match error {
                OutputError::ExclusiveModeNotAllowed => {
                    SharedModeCause::ExclusiveModeDisabledBySystem
                }
                _ => SharedModeCause::ExclusiveFormatRefused,
            };
            return plan_shared(device, source_format, cause, negotiation.log);
        }
        Err(OutputError::FormatNotSupported { .. }) => {
            return Err(OutputError::FormatNotSupported {
                tried: negotiation.log,
            });
        }
        Err(error) => return Err(error),
    };
    let (buffer_frame_count, device_period_in_hundred_nanoseconds) =
        device_buffering(device.object_id)?;
    Ok(StreamPlan {
        device: device.object_id,
        negotiated: NegotiatedOutput {
            backend_name: COREAUDIO_BACKEND_NAME,
            device: device.info.clone(),
            access_mode: DeviceAccessMode::Exclusive,
            shared_mode_cause: None,
            device_format: Some(render_format),
            system_mix_format: None,
            render_format,
            sample_delivery: DeviceSampleDelivery::ExactFloat32Conversion,
            buffer_frame_count,
            device_period_in_hundred_nanoseconds,
            negotiation_log: negotiation.log,
        },
        takeover: Some(takeover),
    })
}

fn plan_shared(
    device: &OpenedDevice,
    source_format: &StreamFormat,
    cause: SharedModeCause,
    mut negotiation_log: Vec<FormatProbeResult>,
) -> Result<StreamPlan, OutputError> {
    let Some(render_format) = shared_render_format(source_format) else {
        return Err(OutputError::FormatNotSupported {
            tried: negotiation_log,
        });
    };
    negotiation_log.push(FormatProbeResult {
        candidate: render_format,
        access_mode: DeviceAccessMode::SharedThroughSystemMixer,
        outcome: FormatProbeOutcome::Accepted,
    });
    let (buffer_frame_count, device_period_in_hundred_nanoseconds) =
        device_buffering(device.object_id)?;
    Ok(StreamPlan {
        device: device.object_id,
        negotiated: NegotiatedOutput {
            backend_name: COREAUDIO_BACKEND_NAME,
            device: device.info.clone(),
            access_mode: DeviceAccessMode::SharedThroughSystemMixer,
            shared_mode_cause: Some(cause),
            device_format: None,
            system_mix_format: Some(system_mix_format(device.object_id)?),
            render_format,
            sample_delivery: DeviceSampleDelivery::ExactFloat32Conversion,
            buffer_frame_count,
            device_period_in_hundred_nanoseconds,
            negotiation_log,
        },
        takeover: None,
    })
}

fn exclusive_capabilities(
    device: AudioObjectID,
) -> Result<ExclusiveDeviceCapabilities, OutputError> {
    Ok(ExclusiveDeviceCapabilities {
        nominal_sample_rate_ranges_hz: available_nominal_sample_rates(device)?
            .iter()
            .map(|range| (range.mMinimum, range.mMaximum))
            .collect(),
        output_channel_count: output_channel_count(device)?,
    })
}

/// The device's IO buffer in frames and the same duration in 100 ns units.
fn device_buffering(device: AudioObjectID) -> Result<(u32, i64), OutputError> {
    let frame_count = io_buffer_frame_count(device)?;
    let sample_rate_hz = nominal_sample_rate(device)?;
    let period = if sample_rate_hz > 0.0 {
        (f64::from(frame_count) / sample_rate_hz * HUNDRED_NANOSECONDS_PER_SECOND).round() as i64
    } else {
        0
    };
    Ok((frame_count, period))
}

/// The format macOS mixes in for this device: its current rate and its first output stream's
/// virtual format.
fn system_mix_format(device: AudioObjectID) -> Result<SystemMixFormat, OutputError> {
    let sample_rate_hz = nominal_sample_rate(device)?.round() as u32;
    let first_stream = output_stream_formats(device)?
        .into_iter()
        .next()
        .ok_or(OutputError::DeviceNotFound)?;
    Ok(SystemMixFormat {
        sample_rate_hz,
        bits_per_sample: u8::try_from(first_stream.mBitsPerChannel).unwrap_or(u8::MAX),
        is_float: first_stream.mFormatFlags & kAudioFormatFlagIsFloat != 0,
        channel_count: u8::try_from(output_channel_count(device)?).unwrap_or(u8::MAX),
    })
}
