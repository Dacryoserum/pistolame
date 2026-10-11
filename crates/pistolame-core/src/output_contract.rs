//! The contract every platform audio backend honors. Core logic depends on these traits and
//! types only; backends (WASAPI, later CoreAudio, ALSA, ASIO) are injected.

use crate::format::{DeviceSampleFormat, StreamFormat};

/// Opaque, backend-defined device identifier (WASAPI endpoint ID string, CoreAudio UID,
/// ALSA "hw:CARD=...,DEV=..."). Stable across runs where the platform allows.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OutputDeviceId(pub String);

/// A device a backend can open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputDeviceInfo {
    /// Backend-defined identifier.
    pub id: OutputDeviceId,
    /// Name to show to the user.
    pub display_name: String,
    /// Whether the operating system currently designates this device as its default.
    pub is_system_default: bool,
}

/// How the device is held. Only `Exclusive` can be bit-perfect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceAccessMode {
    /// WASAPI exclusive, CoreAudio hog mode with the physical format set, ALSA hw: device.
    Exclusive,
    /// Through the OS mixer (WASAPI shared, CoreAudio without hog, ALSA plug/dmix).
    SharedThroughSystemMixer,
}

/// The user's choice, persisted in the settings file. Shared is the first-launch default
/// (B13.1, 2026-10-11): exclusive mode silences every other app on the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OutputModePreference {
    /// Play through the system mixer (default).
    #[default]
    Shared,
    /// Try to hold the device exclusively.
    Exclusive,
}

/// What the engine asks a backend to do about the access mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenPolicy {
    /// Shared mode only (the user's default). Never tries exclusive mode.
    SharedOnly,
    /// The "Mode exclusif" toggle. Negotiates exclusive mode; falls back to shared mode when
    /// every lossless candidate is refused or exclusive mode is disabled in Windows (B5.4).
    ExclusiveThenShared,
    /// Exclusive or nothing. Used by the hardware tests and the `probe_formats` /
    /// `play_file --exclusive-only` examples, never by the app.
    ExclusiveOnly,
}

impl OpenPolicy {
    /// The single mapping from the user's preference to the policy the engine requests.
    pub fn for_preference(preference: OutputModePreference) -> OpenPolicy {
        match preference {
            OutputModePreference::Shared => OpenPolicy::SharedOnly,
            OutputModePreference::Exclusive => OpenPolicy::ExclusiveThenShared,
        }
    }
}

/// Everything a backend needs to open a stream.
pub struct OpenStreamRequest {
    /// Device to open.
    pub device_id: OutputDeviceId,
    /// Format of the decoded samples the stream will carry.
    pub source_format: StreamFormat,
    /// Access-mode policy.
    pub policy: OpenPolicy,
}

/// Why a stream runs through the system mixer. Shown in the reason line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedModeCause {
    /// The user's preference is shared mode.
    UserPreference,
    /// Exclusive mode was requested and the device refused every lossless candidate.
    ExclusiveFormatRefused,
    /// Exclusive mode was requested and is disabled for this device in the Windows settings.
    ExclusiveModeDisabledBySystem,
    /// Exclusive mode was requested while other applications play through the device in shared
    /// mode, and Windows does not let exclusive mode take it from them ("Give exclusive mode
    /// applications priority" is off). WASAPI `IAudioClient::Initialize` documents
    /// AUDCLNT_E_DEVICE_IN_USE as "The endpoint device is already in use. Either the device is
    /// being used in exclusive mode, or the device is being used in shared mode and the caller
    /// asked to use the device in exclusive mode." Shared mode then still works.
    DeviceBusyInSharedMode,
}

/// The system mixer's own format (WASAPI `GetMixFormat`), for display only. It may be float,
/// which is why it is not a `DeviceSampleFormat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemMixFormat {
    /// Sample rate in hertz.
    pub sample_rate_hz: u32,
    /// Bits per sample of the mix format.
    pub bits_per_sample: u8,
    /// Whether the samples are floating point.
    pub is_float: bool,
    /// Number of interleaved channels.
    pub channel_count: u8,
}

/// What the backend actually got. Read back from the platform after initialization, never
/// copied from the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NegotiatedOutput {
    /// Device that was opened.
    pub device: OutputDeviceInfo,
    /// How the device is held.
    pub access_mode: DeviceAccessMode,
    /// `Some` if and only if `access_mode` is `SharedThroughSystemMixer`.
    pub shared_mode_cause: Option<SharedModeCause>,
    /// `None` in shared mode, where the app does not control the device format.
    pub device_format: Option<DeviceSampleFormat>,
    /// `Some` in shared mode: the format the system mixer converts our stream to.
    pub system_mix_format: Option<SystemMixFormat>,
    /// Integer format the render source must produce. Equals `device_format` in exclusive
    /// mode; in shared mode it is the lossless candidate Windows accepted for conversion.
    pub render_format: DeviceSampleFormat,
    /// Size of the device buffer in frames.
    pub buffer_frame_count: u32,
    /// Device period in units of 100 ns (the unit WASAPI uses).
    pub device_period_in_hundred_nanoseconds: i64,
    /// Every candidate tried and the platform's answer, in both modes, for the signal-path
    /// popover and for diagnosing devices remotely. After a fallback it holds the refused
    /// exclusive candidates followed by the shared-mode attempt.
    pub negotiation_log: Vec<FormatProbeResult>,
}

/// One candidate format and the platform's answer to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatProbeResult {
    /// The format that was tried.
    pub candidate: DeviceSampleFormat,
    /// The mode the candidate was tried in, so a fallback's log reads as one sequence.
    pub access_mode: DeviceAccessMode,
    /// What the platform answered.
    pub outcome: FormatProbeOutcome,
}

/// The platform's answer to a candidate format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatProbeOutcome {
    /// The device supports the format.
    Accepted,
    /// The device does not support the format.
    Refused,
    /// The platform call failed.
    Error {
        /// Platform error code (HRESULT on Windows).
        platform_code: i32,
    },
}

/// What the render source needs to know about the stream it is about to feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderStreamDescription {
    /// Integer format the render source must write.
    pub render_format: DeviceSampleFormat,
    /// How the device is held.
    pub access_mode: DeviceAccessMode,
}

/// Called by the backend's real-time thread to fill the device buffer.
/// Contract: never blocks, never allocates, never panics.
pub trait RenderSource: Send + 'static {
    /// Called once per opened stream, on the backend's thread, after the platform accepted a
    /// format and before the first `render` (prefill). A render source survives pause and
    /// reopen (`OutputStream::close` hands it back), so the format and mode can change
    /// between two calls. Must not allocate.
    fn begin_stream(&mut self, description: RenderStreamDescription);
    /// Fills the whole `destination` (silence where no data is available). `destination`
    /// holds a whole number of frames; its length may differ between calls (shared mode).
    fn render(&mut self, destination: &mut [u8]) -> RenderOutcome;
    /// Called by the engine, off the real-time thread, while this render source is in no
    /// stream (after `close`, after a flush generation change while paused). Pushes side
    /// information still waiting and drops data made stale by a flush. `true` when nothing is
    /// left waiting. Backends never call it.
    fn service_while_closed(&mut self) -> bool {
        true
    }
}

/// What a `render` call produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderOutcome {
    /// Destination fully filled with audio.
    Filled,
    /// Partly or fully filled with silence because the ring was empty (underrun, counted).
    FilledWithUnderrun,
    /// The next block has a different format; silence written, the engine must reopen.
    FormatBoundaryReached,
    /// The last track ended; the rest is silence.
    EndOfStream,
}

/// A platform audio output (WASAPI, later CoreAudio, ALSA, ASIO). Every implementation
/// honors the same contract (Liskov), documented per method.
pub trait AudioOutput: Send {
    /// Short stable name for diagnostics, e.g. "WASAPI".
    fn backend_name(&self) -> &'static str;
    /// Lists the output devices currently available.
    fn list_devices(&mut self) -> Result<Vec<OutputDeviceInfo>, OutputError>;
    /// Asks the device which candidates it accepts, without opening a stream. Used by the
    /// "Formats acceptés" diagnostic so we never guess hardware capabilities.
    fn probe_formats(
        &mut self,
        device_id: &OutputDeviceId,
        candidates: &[DeviceSampleFormat],
    ) -> Result<Vec<FormatProbeResult>, OutputError>;
    /// Negotiates a format according to `request.policy`, initializes the device, calls
    /// `render_source.begin_stream`, prefills, and leaves the backend's real-time thread in the
    /// `NotStarted` state. The render source is moved into that thread.
    fn open_stream(
        &mut self,
        request: OpenStreamRequest,
        render_source: Box<dyn RenderSource>,
    ) -> Result<Box<dyn OutputStream>, OpenStreamError>;
}

/// An open stream. There is no "paused but holding the device" state: pause closes the stream
/// with `QueuedAudioOnClose::PlayOut` so other apps get the device back (B13.1 Q6).
/// Dropping a stream equals `close(QueuedAudioOnClose::Discard)`.
///
/// Failure reporting (decided for M1): a failure on the real-time side is published only
/// through `status()`. There is no wake-up channel to the engine; the engine polls `status()`
/// (for example on its UI refresh tick) and closes the stream when it reads `Failed`.
pub trait OutputStream: Send {
    /// What the platform actually granted when the stream was opened.
    fn negotiated(&self) -> &NegotiatedOutput;
    /// Starts rendering after `open_stream`'s prefill.
    fn play(&mut self) -> Result<(), OutputError>;
    /// Health of the real-time side, readable at any time without locking. This is the only
    /// way a real-time failure reaches the engine, which polls it.
    fn status(&self) -> OutputStreamStatus;
    /// Stops, releases the device and returns the render source so the ring and the feeder's
    /// position survive a reopen (pause/resume, mode switch, gapless format change later).
    /// Blocks the caller (the engine thread) for at most `QUEUED_AUDIO_PLAY_OUT_TIMEOUT` with
    /// `PlayOut`. Errors during close are counted, not returned.
    fn close(self: Box<Self>, queued_audio: QueuedAudioOnClose) -> ClosedStream;
}

/// What happens to audio already written to the device buffer when a stream closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuedAudioOnClose {
    /// Pause and mode switch: stop feeding, write silence until the device has played every
    /// queued audio frame, then stop. Nothing is lost, nothing is played twice (B5.6).
    PlayOut,
    /// Stop, seek-by-reopen, device failure: stop and discard immediately (`Reset`).
    Discard,
}

/// What `OutputStream::close` hands back.
pub struct ClosedStream {
    /// The render source, so the caller can reuse it for the next stream.
    pub render_source: Box<dyn RenderSource>,
    /// `false` when `PlayOut` timed out or failed, so some queued audio was never played and
    /// the feeder's position is past the last played frame. The engine reports it to the
    /// verifier (`QueuedAudioLostOnPause`). Always `false` after `Discard`, which drops the
    /// queued audio by definition; the engine ignores it on stop and seek.
    pub was_queued_audio_fully_played: bool,
}

/// Health of a stream's real-time side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputStreamStatus {
    /// Opened and prefilled, `play` not called yet.
    NotStarted,
    /// Rendering.
    Running,
    /// Fatal error on the real-time thread; the stream must be closed.
    Failed(OutputFailure),
}

/// Fatal error raised on the real-time thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFailure {
    /// The device was unplugged or reconfigured.
    DeviceInvalidated,
    /// The device stopped requesting data.
    DeviceTimedOut,
    /// Any other platform error.
    Platform {
        /// Platform error code (HRESULT on Windows).
        platform_code: i32,
    },
}

/// `open_stream` failure; carries the render source back so the caller can retry.
pub struct OpenStreamError {
    /// Why the stream could not be opened.
    pub error: OutputError,
    /// The render source that was passed to `open_stream`, handed back. Usually unused, but
    /// if the open failed after `begin_stream` (for example while prefilling), `render` may
    /// already have consumed frames that were never played: its position is then past them,
    /// and the engine must treat the next open as a discontinuity, not a seamless resume.
    pub render_source: Box<dyn RenderSource>,
}

/// Errors a backend reports.
#[derive(Debug, thiserror::Error)]
pub enum OutputError {
    /// The requested device does not exist.
    #[error("device not found")]
    DeviceNotFound,
    /// The device disappeared or changed configuration.
    #[error("device was unplugged or reconfigured")]
    DeviceInvalidated,
    /// Another application holds the device.
    #[error("device is used by another application")]
    DeviceInUse,
    /// Exclusive mode is turned off for this device by the system.
    #[error("exclusive mode is disabled for this device in the system settings")]
    ExclusiveModeNotAllowed,
    /// The device refused every lossless candidate.
    #[error("the device accepts none of the lossless formats for this stream")]
    FormatNotSupported {
        /// Every candidate tried and the answer it got.
        tried: Vec<FormatProbeResult>,
    },
    /// The device stopped requesting data.
    #[error("the device stopped requesting data")]
    DeviceTimedOut,
    /// The operating system audio service is down.
    #[error("the system audio service is not running")]
    AudioServiceNotRunning,
    /// Any other platform failure.
    #[error("{operation} failed with platform code {platform_code:#010x}")]
    Platform {
        /// Name of the platform call that failed.
        operation: &'static str,
        /// Platform error code (HRESULT on Windows).
        platform_code: i32,
    },
}

impl std::fmt::Debug for ClosedStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClosedStream")
            .field(
                "was_queued_audio_fully_played",
                &self.was_queued_audio_fully_played,
            )
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for OpenStreamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenStreamError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_preference_maps_to_shared_only_policy() {
        assert_eq!(
            OpenPolicy::for_preference(OutputModePreference::Shared),
            OpenPolicy::SharedOnly
        );
    }

    #[test]
    fn exclusive_preference_maps_to_exclusive_then_shared_policy() {
        assert_eq!(
            OpenPolicy::for_preference(OutputModePreference::Exclusive),
            OpenPolicy::ExclusiveThenShared
        );
    }

    #[test]
    fn default_output_mode_preference_is_shared() {
        assert_eq!(
            OutputModePreference::default(),
            OutputModePreference::Shared
        );
    }
}
