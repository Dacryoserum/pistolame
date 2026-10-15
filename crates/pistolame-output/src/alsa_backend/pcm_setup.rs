//! Opening a PCM and fixing its format (`snd_pcm_hw_params`) and its start rule
//! (`snd_pcm_sw_params`).
//!
//! Sources: ALSA PCM hardware parameters
//! <https://www.alsa-project.org/alsa-doc/alsa-lib/group___p_c_m___h_w___params.html> and
//! software parameters
//! <https://www.alsa-project.org/alsa-doc/alsa-lib/group___p_c_m___s_w___params.html>.

use std::ffi::CString;

use alsa::pcm::{Access, Format, Frames, HwParams, PCM};
use alsa::{Direction, ValueOr};

use super::errors::{errno_of, output_error};
use crate::alsa_layouts::AlsaSampleLayout;
use crate::{
    DeviceAccessMode, DeviceSampleFormat, FormatProbeOutcome, FormatProbeResult, OutputError,
};

/// Device buffer asked for, in microseconds: 200 ms lets the render thread run at normal
/// priority (Linux gives no real-time scheduling without rtkit) without underruns.
const BUFFER_TIME_MICROSECONDS: u32 = 200_000;
/// Period asked for, in microseconds: the render thread wakes about every 20 ms.
const PERIOD_TIME_MICROSECONDS: u32 = 20_000;
/// Invalid argument: the PCM refuses a parameter value.
const EINVAL: i32 = 22;

/// A PCM set to one format, ready to be filled.
pub(super) struct ConfiguredPcm {
    pub(super) pcm: PCM,
    pub(super) format: DeviceSampleFormat,
    pub(super) buffer_frame_count: u32,
    pub(super) period_frame_count: u32,
}

/// Opens `name` for playback, in blocking mode.
pub(super) fn open_pcm(name: &str) -> Result<PCM, OutputError> {
    let name = CString::new(name).map_err(|_| OutputError::DeviceNotFound)?;
    PCM::open(&name, Direction::Playback, false)
        .map_err(|error| output_error("snd_pcm_open", error))
}

/// ALSA's format for `layout`.
fn alsa_format(layout: AlsaSampleLayout) -> Format {
    match layout {
        AlsaSampleLayout::Signed16 => Format::S16LE,
        AlsaSampleLayout::Signed24Packed => Format::S243LE,
        AlsaSampleLayout::Signed24InLow32 => Format::S24LE,
        AlsaSampleLayout::Signed32 => Format::S32LE,
    }
}

/// Hardware parameters restricted to `format` in `layout`, not applied. On a `hw:` PCM
/// (`is_hardware`) ALSA must not resample: the rate is the device's or the format is refused.
fn restricted_hw_params<'pcm>(
    pcm: &'pcm PCM,
    layout: AlsaSampleLayout,
    format: &DeviceSampleFormat,
    is_hardware: bool,
) -> Result<HwParams<'pcm>, alsa::Error> {
    let hw_params = HwParams::any(pcm)?;
    hw_params.set_access(Access::RWInterleaved)?;
    hw_params.set_format(alsa_format(layout))?;
    hw_params.set_channels(u32::from(format.channel_count))?;
    if is_hardware {
        hw_params.set_rate_resample(false)?;
    }
    hw_params.set_rate(format.sample_rate_hz, ValueOr::Nearest)?;
    if hw_params.get_rate()? != format.sample_rate_hz {
        return Err(alsa::Error::new("snd_pcm_hw_params_set_rate", -EINVAL));
    }
    Ok(hw_params)
}

/// The platform's answer to one candidate, for the negotiation log.
fn probe_outcome<Accepted>(result: &Result<Accepted, alsa::Error>) -> FormatProbeOutcome {
    match result {
        Ok(_) => FormatProbeOutcome::Accepted,
        Err(error) if errno_of(error) == EINVAL => FormatProbeOutcome::Refused,
        Err(error) => FormatProbeOutcome::Error {
            platform_code: errno_of(error),
        },
    }
}

/// Whether `pcm` accepts `format`, without applying anything ("Formats acceptés").
pub(super) fn probe(pcm: &PCM, format: &DeviceSampleFormat) -> FormatProbeOutcome {
    let Some(layout) = AlsaSampleLayout::of(format) else {
        return FormatProbeOutcome::Refused;
    };
    probe_outcome(&restricted_hw_params(pcm, layout, format, true))
}

/// Tries `candidates` in order on `pcm` and configures the first one it accepts. Returns the
/// configured PCM (or `None`) and every attempt.
pub(super) fn negotiate(
    pcm: PCM,
    candidates: &[(DeviceSampleFormat, AlsaSampleLayout)],
    access_mode: DeviceAccessMode,
) -> (Option<ConfiguredPcm>, Vec<FormatProbeResult>) {
    let is_hardware = access_mode == DeviceAccessMode::Exclusive;
    let mut log = Vec::with_capacity(candidates.len());
    for (format, layout) in candidates {
        let result = configure(&pcm, *layout, format, is_hardware);
        log.push(FormatProbeResult {
            candidate: *format,
            access_mode,
            outcome: probe_outcome(&result),
        });
        if let Ok((buffer_frame_count, period_frame_count)) = result {
            let configured = ConfiguredPcm {
                pcm,
                format: *format,
                buffer_frame_count,
                period_frame_count,
            };
            return (Some(configured), log);
        }
    }
    (None, log)
}

/// Applies `format` and the buffering to `pcm`. The PCM never starts by itself (start
/// threshold = boundary): `OutputStream::play` starts it after the prefill. Returns the
/// buffer and period sizes ALSA granted, in frames.
fn configure(
    pcm: &PCM,
    layout: AlsaSampleLayout,
    format: &DeviceSampleFormat,
    is_hardware: bool,
) -> Result<(u32, u32), alsa::Error> {
    let hw_params = restricted_hw_params(pcm, layout, format, is_hardware)?;
    hw_params.set_buffer_time_near(BUFFER_TIME_MICROSECONDS, ValueOr::Nearest)?;
    hw_params.set_period_time_near(PERIOD_TIME_MICROSECONDS, ValueOr::Nearest)?;
    pcm.hw_params(&hw_params)?;
    let (buffer_frame_count, period_frame_count) = pcm.get_params()?;
    let sw_params = pcm.sw_params_current()?;
    sw_params.set_start_threshold(sw_params.get_boundary()?)?;
    sw_params.set_avail_min(Frames::try_from(period_frame_count).unwrap_or(Frames::MAX))?;
    pcm.sw_params(&sw_params)?;
    let to_frame_count = |frames: u64| u32::try_from(frames).unwrap_or(u32::MAX);
    Ok((
        to_frame_count(buffer_frame_count),
        to_frame_count(period_frame_count),
    ))
}
