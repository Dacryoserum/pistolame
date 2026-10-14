//! Messages exchanged between the UI and the engine. Nothing here is Slint-specific, so the
//! engine does not change if the UI toolkit does. `EngineHandle` belongs to a later task.

use crate::decoder::SourceStreamInfo;
use crate::output_contract::{
    OutputDeviceId, OutputDeviceInfo, OutputDeviceSelection, OutputModePreference,
};
use crate::playback_ring::TrackId;
use crate::signal_path::SignalPathAssessment;
use crate::verification::VerificationStatus;

/// Requests from the UI to the engine thread.
#[derive(Debug)]
pub enum EngineCommand {
    /// Re-read the list of output devices.
    RefreshDeviceList,
    /// Use another output device, or follow the system default. Applies at once: if a track is
    /// playing on another device, the stream closes with `PlayOut` and reopens on the new one at
    /// the first unplayed frame.
    SelectOutputDevice {
        /// The user's choice.
        selection: OutputDeviceSelection,
    },
    /// The "Mode exclusif" toggle. Applies at once: if a track is playing, the stream closes
    /// with `PlayOut` and reopens with the new policy at the first unplayed frame.
    SetOutputModePreference {
        /// Whether to ask for exclusive access or stay on the system mixer; mapped by `OpenPolicy::for_preference`.
        preference: OutputModePreference,
    },
    /// Open a file and start playing it.
    OpenFileAndPlay {
        /// File to play.
        path: std::path::PathBuf,
    },
    /// Resume or start playback.
    Play,
    /// Plays out the queued audio, then releases the device (B5.6, B13.1 Q6).
    Pause,
    /// Stop playback.
    Stop,
    /// Jump to a position in the current track.
    SeekTo {
        /// Position from the start of the track, converted to a frame index by the engine.
        position_in_track: std::time::Duration,
    },
    /// The shared-mode volume. Applied at once to a shared stream and to every stream opened
    /// later; exclusive streams ignore it (bit-perfect, the DAC sets the level).
    SetSharedModeVolume {
        /// The volume to apply.
        volume: crate::volume::SharedModeVolume,
    },
    /// Stop the engine thread.
    Shutdown,
}

/// Start-up choices read from the settings file by the app (task T15).
#[derive(Clone, Debug, Default)]
pub struct EngineStartOptions {
    /// Access-mode preference to start with.
    pub output_mode_preference: OutputModePreference,
    /// The user's device choice (system default unless a device was picked). A specific device
    /// absent at start-up makes the engine use the system default and emit
    /// `PreferredDeviceUnavailable`.
    pub device_selection: OutputDeviceSelection,
    /// Shared-mode volume to start with.
    pub shared_mode_volume: crate::volume::SharedModeVolume,
}

/// Notifications from the engine to the UI.
#[derive(Clone, Debug)]
pub enum EngineEvent {
    /// The device list, the user's choice or the device it resolves to changed.
    DeviceListChanged {
        /// Devices currently available.
        devices: Vec<OutputDeviceInfo>,
        /// The user's choice.
        selection: OutputDeviceSelection,
        /// The device the choice resolves to now (the next stream opens on it), if any.
        selected: Option<OutputDeviceId>,
    },
    /// The remembered device is not present; the system default is used instead (B13.1 Q9).
    PreferredDeviceUnavailable {
        /// Device that was remembered.
        device_id: OutputDeviceId,
    },
    /// A file was opened.
    TrackLoaded {
        /// Identifier of this playback.
        track_id: TrackId,
        /// File that was opened.
        path: std::path::PathBuf,
        /// Facts about the source.
        source: SourceStreamInfo,
    },
    /// The playback state changed.
    PlaybackStateChanged {
        /// State the engine is now in; see `PlaybackState` for what each holds.
        state: PlaybackState,
    },
    /// The signal path changed.
    SignalPathChanged {
        /// New assessment; `None` = no stream open.
        assessment: Option<SignalPathAssessment>,
    },
    /// A verification level changed.
    VerificationChanged {
        /// Playback whose verification changed.
        track_id: TrackId,
        /// Current state of level 1 (transmission) and level 2 (file integrity).
        status: VerificationStatus,
    },
    /// User-facing failure; the UI maps `kind` to French text.
    PlaybackFailed {
        /// Failure category the UI maps to French text.
        kind: PlaybackFailureKind,
        /// Technical detail, for diagnostics.
        detail: String,
    },
}

/// `Paused` holds no device: the stream is closed, the ring, the decoder and the feeder's
/// position are kept. `Opening` also covers the reopen on resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    /// Nothing is playing and no stream is open.
    Stopped,
    /// A stream is being opened.
    Opening,
    /// Audio is being rendered.
    Playing,
    /// Paused without holding the device.
    Paused,
}

/// `ExclusiveModeDisabled` and `FormatRefusedByDevice` are failures only when the shared-mode
/// fallback also fails (or under `OpenPolicy::ExclusiveOnly`); otherwise they appear as the
/// `SharedModeCause` of a playing stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackFailureKind {
    /// The file could not be read.
    FileUnreadable,
    /// No decoder handles the file.
    UnsupportedFile,
    /// The file is damaged.
    CorruptFile,
    /// The device does not exist.
    DeviceNotFound,
    /// The device was unplugged.
    DeviceUnplugged,
    /// Another application holds the device.
    DeviceInUse,
    /// Exclusive mode is disabled for the device.
    ExclusiveModeDisabled,
    /// The device refused every lossless format.
    FormatRefusedByDevice,
    /// The device stopped requesting data.
    DeviceStoppedResponding,
    /// The system audio service is down.
    AudioServiceNotRunning,
    /// Unexpected internal failure.
    Internal,
}

/// Plain copy of the lock-free counters written by the real-time thread, for the UI timer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaybackCountersSnapshot {
    /// Identifier of the track currently rendered.
    pub current_track_id: u64,
    /// Frames rendered in the track; position = frames / rate (excludes device latency).
    pub frames_rendered_in_track: u64,
    /// Number of underruns.
    pub underrun_events: u64,
    /// Number of silent frames inserted.
    pub silent_frames_inserted: u64,
    /// Number of verification tap overflows.
    pub verification_tap_overflow_events: u64,
    /// Number of stale frames discarded after a flush.
    pub stale_frames_discarded: u64,
    /// Time from `Play` after a pause to the first rendered buffer (reopen, renegotiate,
    /// prefill); 0 before the first resume. Measured for T14 (B13.1 Q6).
    pub last_resume_delay_milliseconds: u64,
    /// Times a backend broke the `RenderSource` contract (for example `render` before
    /// `begin_stream`) or the playback ring was inconsistent. Always 0 unless there is a bug;
    /// counted so the error is never silent (AGENTS.md).
    pub render_contract_violations: u64,
    /// Times the feeder stopped at a block whose format the open stream cannot carry
    /// (`RenderOutcome::FormatBoundaryReached`); counted once per boundary, not per buffer.
    pub format_boundaries_reached: u64,
    /// Times the feeder entered `RenderOutcome::EndOfStream`; counted once per end, not per
    /// buffer (re-armed when audio renders again or a stream begins). The engine reads it to
    /// learn that a track ended (B5.7).
    pub stream_ends_reached: u64,
}
