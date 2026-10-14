//! The engine thread ("pistolame-engine"): the command loop, the playback state machine and the
//! polling of the open stream (plan B3.7, B5.7, B8). The state diagram is in `engine/mod.rs`.
//!
//! It owns the `AudioOutput`, the open `OutputStream` (or, while no stream is open, the render
//! source inside the pipeline), the output-mode preference, the selected device and the
//! current pipeline's control handles. It is the slow path: it may block in `open_stream` and
//! `close`, and it wakes at least every `ENGINE_POLL_INTERVAL` to poll `status()` and the
//! counters (B13.3: no wake-up channel from the real-time thread).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded, never, select, unbounded};

use super::decode_thread::DecodeEvent;
use super::decode_thread::SegmentFailure;
use super::failure_mapping::{
    failure_for_segment, failure_kind_for_decode_error, failure_kind_for_output_error,
    failure_kind_for_stream_failure,
};
use super::handle::EngineTuning;
use super::pipeline::{Pipeline, PipelineSetup};
use crate::decoder::{DecoderRegistry, SourceStreamInfo};
use crate::engine::{
    EngineCommand, EngineEvent, EngineStartOptions, PlaybackCounters, PlaybackFailureKind,
    PlaybackState,
};
use crate::output_contract::{
    AudioOutput, OpenPolicy, OpenStreamRequest, OutputDeviceId, OutputDeviceInfo,
    OutputDeviceSelection, OutputError, OutputFailure, OutputModePreference, OutputStream,
    OutputStreamStatus, QueuedAudioOnClose,
};
use crate::playback_ring::TrackId;
use crate::signal_path::{ProcessingState, assess_signal_path};
use crate::timing::ENGINE_POLL_INTERVAL;
use crate::verification::{VerificationUpdate, VerifierCommand};

/// Nanoseconds in one second, to turn a seek position into a frame index with integers only.
const NANOSECONDS_PER_SECOND: u128 = 1_000_000_000;

/// Detail of the internal failure raised when the feeder stops at a format boundary inside one
/// file (B5.7: only a bug can cause it while M1 plays one file per pipeline).
const FORMAT_BOUNDARY_DETAIL: &str = "format boundary inside one file";

/// Pending device-change wake-ups the backend can post. One is enough: on waking, the engine
/// re-reads the whole device list, so further notifications in between add nothing.
const DEVICE_CHANGE_WAKE_UP_CAPACITY: usize = 1;

/// Everything the engine thread is started with.
pub(crate) struct EngineThreadSetup {
    pub(crate) output: Box<dyn AudioOutput>,
    pub(crate) decoders: DecoderRegistry,
    pub(crate) options: EngineStartOptions,
    pub(crate) tuning: EngineTuning,
    pub(crate) counters: Arc<PlaybackCounters>,
    pub(crate) commands: Receiver<EngineCommand>,
    pub(crate) events: Sender<EngineEvent>,
}

/// The engine thread's state. Methods are called in order by `run`; tests call them directly.
pub(crate) struct EngineThread {
    output: Box<dyn AudioOutput>,
    decoders: DecoderRegistry,
    tuning: EngineTuning,
    counters: Arc<PlaybackCounters>,
    commands: Receiver<EngineCommand>,
    events: Sender<EngineEvent>,
    verification_update_sender: Sender<VerificationUpdate>,
    verification_updates: Receiver<VerificationUpdate>,
    /// The user's choice: the system default or a specific device.
    device_selection: OutputDeviceSelection,
    devices: Vec<OutputDeviceInfo>,
    /// The device `device_selection` resolves to in `devices`: the next stream opens on it.
    selected_device_id: Option<OutputDeviceId>,
    /// Handed to the backend once, in `start_up`, to post device-change wake-ups.
    device_change_sender: Option<Sender<()>>,
    device_changes: Receiver<()>,
    /// Whether the backend notifies device changes; without it, the devices are re-read
    /// before each stream opens.
    is_watching_device_changes: bool,
    preference: OutputModePreference,
    shared_mode_volume: crate::volume::SharedModeVolume,
    state: PlaybackState,
    last_track_number: u64,
    last_opened_path: Option<PathBuf>,
    pipeline: Option<Pipeline>,
    stream: Option<Box<dyn OutputStream>>,
    /// When `Play` was received in `Paused`, until the stream plays (resume delay, B13.1 Q6).
    resume_requested_at: Option<Instant>,
    is_shutting_down: bool,
}

/// Why an attempt to open (or start) a stream failed.
struct OpenFailure {
    kind: PlaybackFailureKind,
    detail: String,
}

impl OpenFailure {
    fn from_output_error(error: &OutputError) -> Self {
        Self {
            kind: failure_kind_for_output_error(error),
            detail: error.to_string(),
        }
    }
}

impl EngineThread {
    pub(crate) fn new(setup: EngineThreadSetup) -> Self {
        let (verification_update_sender, verification_updates) = unbounded();
        let (device_change_sender, device_changes) = bounded(DEVICE_CHANGE_WAKE_UP_CAPACITY);
        Self {
            output: setup.output,
            decoders: setup.decoders,
            tuning: setup.tuning,
            counters: setup.counters,
            commands: setup.commands,
            events: setup.events,
            verification_update_sender,
            verification_updates,
            device_selection: setup.options.device_selection,
            devices: Vec::new(),
            selected_device_id: None,
            device_change_sender: Some(device_change_sender),
            device_changes,
            is_watching_device_changes: false,
            preference: setup.options.output_mode_preference,
            shared_mode_volume: setup.options.shared_mode_volume,
            state: PlaybackState::Stopped,
            last_track_number: 0,
            last_opened_path: None,
            pipeline: None,
            stream: None,
            resume_requested_at: None,
            is_shutting_down: false,
        }
    }

    /// The loop: one wake per UI command, decode event, verifier update or device change, and
    /// at least every `ENGINE_POLL_INTERVAL`; `poll` after every wake.
    pub(crate) fn run(mut self) {
        self.start_up();
        while !self.is_shutting_down {
            let decode_events = self
                .pipeline
                .as_ref()
                .map_or_else(never, |pipeline| pipeline.decode_events.clone());
            let device_changes = if self.is_watching_device_changes {
                self.device_changes.clone()
            } else {
                never()
            };
            select! {
                recv(self.commands) -> command => {
                    self.handle_command(command.unwrap_or(EngineCommand::Shutdown));
                }
                recv(decode_events) -> event => match event {
                    Ok(event) => self.handle_decode_event(event),
                    Err(_) => self.on_decode_thread_lost(),
                },
                recv(self.verification_updates) -> update => {
                    if let Ok(update) = update {
                        self.forward_verification_update(update);
                    }
                }
                recv(device_changes) -> notification => match notification {
                    Ok(()) => self.on_system_devices_changed(),
                    // The backend dropped its listener: fall back to re-reading at each open.
                    Err(_) => self.is_watching_device_changes = false,
                },
                default(ENGINE_POLL_INTERVAL) => {}
            }
            self.poll();
        }
    }

    /// Starts watching device changes, lists the devices and resolves the user's choice: the
    /// remembered device if listed, else the system default, with `PreferredDeviceUnavailable`
    /// when a remembered device is missing (B8, B13.1 Q9).
    pub(crate) fn start_up(&mut self) {
        self.watch_device_changes();
        if !self.list_devices() {
            self.selected_device_id = match &self.device_selection {
                OutputDeviceSelection::Specific(device_id) => Some(device_id.clone()),
                OutputDeviceSelection::SystemDefault => None,
            };
            self.emit_device_list();
            return;
        }
        if let OutputDeviceSelection::Specific(device_id) = &self.device_selection
            && !self.is_device_listed(device_id)
        {
            self.emit(EngineEvent::PreferredDeviceUnavailable {
                device_id: device_id.clone(),
            });
        }
        self.selected_device_id = self.device_selection.resolve(&self.devices);
        self.emit_device_list();
    }

    /// Gives the backend a listener that posts a wake-up to the engine loop. `try_send` never
    /// blocks the platform's notification thread; a full channel already holds a wake-up.
    fn watch_device_changes(&mut self) {
        let Some(sender) = self.device_change_sender.take() else {
            return;
        };
        self.is_watching_device_changes = self.output.watch_device_changes(Box::new(move || {
            let _ = sender.try_send(());
        }));
    }

    pub(crate) fn handle_command(&mut self, command: EngineCommand) {
        match command {
            EngineCommand::RefreshDeviceList => self.refresh_device_list(),
            EngineCommand::SelectOutputDevice { selection } => {
                self.device_selection = selection;
                self.selected_device_id = self.device_selection.resolve(&self.devices);
                self.emit_device_list();
                self.move_stream_to_selected_device();
            }
            EngineCommand::SetOutputModePreference { preference } => {
                self.preference = preference;
                self.reopen_if_playing();
            }
            EngineCommand::OpenFileAndPlay { path } => self.open_file_and_play(path),
            EngineCommand::Play => self.play(),
            EngineCommand::Pause => self.pause(),
            EngineCommand::Stop => self.stop(),
            EngineCommand::SeekTo { position_in_track } => self.seek_to(position_in_track),
            EngineCommand::SetSharedModeVolume { volume } => {
                self.shared_mode_volume = volume;
                if let Some(stream) = self.stream.as_mut() {
                    stream.set_shared_mode_volume(volume.linear_gain());
                }
            }
            EngineCommand::Shutdown => {
                self.stop();
                self.is_shutting_down = true;
            }
        }
    }

    /// Events of an older generation are ignored: a seek made them irrelevant.
    pub(crate) fn handle_decode_event(&mut self, event: DecodeEvent) {
        let Some(pipeline) = self.pipeline.as_mut() else {
            return;
        };
        let current_generation = pipeline.current_generation();
        match event {
            DecodeEvent::PrefillReached { flush_generation }
                if flush_generation == current_generation =>
            {
                pipeline.is_prefill_reached = true;
                if self.state == PlaybackState::Opening {
                    self.open_stream_and_play();
                }
            }
            DecodeEvent::SegmentEnded {
                flush_generation,
                error,
            } if flush_generation == current_generation => {
                if error.is_some() {
                    pipeline.kept_segment_failure = error;
                }
            }
            DecodeEvent::PrefillReached { .. } | DecodeEvent::SegmentEnded { .. } => {}
        }
    }

    /// Reads the stream's health and the counters (B5.7, B8); while no stream is open, gives
    /// the held feeder a chance to drop stale blocks and push waiting tap events.
    pub(crate) fn poll(&mut self) {
        if self.state == PlaybackState::Playing
            && let Some(stream) = self.stream.as_ref()
        {
            if let OutputStreamStatus::Failed(failure) = stream.status() {
                self.on_stream_failure(failure);
                return;
            }
            let snapshot = self.counters.snapshot();
            let Some(pipeline) = self.pipeline.as_ref() else {
                return;
            };
            if snapshot.format_boundaries_reached > pipeline.format_boundaries_baseline {
                self.on_format_boundary();
            } else if snapshot.stream_ends_reached > pipeline.stream_ends_baseline {
                self.on_track_end();
            }
            return;
        }
        if self.stream.is_none()
            && let Some(pipeline) = self.pipeline.as_mut()
        {
            pipeline.service_render_source_once();
        }
    }

    fn open_file_and_play(&mut self, path: PathBuf) {
        self.end_current_playback(false);
        self.last_opened_path = Some(path.clone());
        let decoder = match self.decoders.open(&path) {
            Ok(decoder) => decoder,
            Err(error) => {
                self.emit_failure(failure_kind_for_decode_error(&error), error.to_string());
                self.set_state(PlaybackState::Stopped);
                return;
            }
        };
        self.last_track_number += 1;
        let track_id = TrackId(self.last_track_number);
        self.emit(EngineEvent::TrackLoaded {
            track_id,
            path,
            source: decoder.stream_info().clone(),
        });
        let pipeline = Pipeline::build(PipelineSetup {
            track_id,
            decoder,
            tuning: &self.tuning,
            counters: &self.counters,
            verification_updates: &self.verification_update_sender,
        });
        match pipeline {
            Ok(pipeline) => {
                self.pipeline = Some(pipeline);
                self.set_state(PlaybackState::Opening);
            }
            Err(error) => {
                self.emit_failure(
                    PlaybackFailureKind::Internal,
                    format!("cannot start the playback threads: {error}"),
                );
                self.set_state(PlaybackState::Stopped);
            }
        }
    }

    fn play(&mut self) {
        match self.state {
            PlaybackState::Stopped => {
                if let Some(path) = self.last_opened_path.clone() {
                    self.open_file_and_play(path);
                }
            }
            PlaybackState::Paused => {
                self.resume_requested_at = Some(Instant::now());
                let is_prefill_reached = self
                    .pipeline
                    .as_ref()
                    .is_some_and(|pipeline| pipeline.is_prefill_reached);
                if is_prefill_reached {
                    self.open_stream_and_play();
                } else {
                    self.set_state(PlaybackState::Opening);
                }
            }
            PlaybackState::Opening | PlaybackState::Playing => {}
        }
    }

    fn pause(&mut self) {
        match self.state {
            PlaybackState::Playing => {
                self.close_with_play_out();
                self.set_state(PlaybackState::Paused);
            }
            PlaybackState::Opening => {
                self.resume_requested_at = None;
                self.set_state(PlaybackState::Paused);
            }
            PlaybackState::Paused | PlaybackState::Stopped => {}
        }
    }

    fn stop(&mut self) {
        self.end_current_playback(false);
        self.resume_requested_at = None;
        self.set_state(PlaybackState::Stopped);
    }

    /// A seek always closes the stream with `Discard` and starts a new generation, so it never
    /// counts as an underrun. Paused (or still opening), it only changes the generation.
    fn seek_to(&mut self, position_in_track: Duration) {
        let Some(source) = self
            .pipeline
            .as_ref()
            .map(|pipeline| pipeline.source.clone())
        else {
            return;
        };
        let target_frame_index = target_frame_index(position_in_track, &source);
        if self.state == PlaybackState::Playing {
            self.close_with_discard();
            self.set_state(PlaybackState::Opening);
        }
        if let Some(pipeline) = self.pipeline.as_mut() {
            pipeline.seek(target_frame_index);
        }
    }

    /// Mode or device changed while playing: play out, then reopen with the new choice at the
    /// first unplayed frame. In any other state the choice is used at the next open.
    fn reopen_if_playing(&mut self) {
        if self.state == PlaybackState::Playing {
            self.close_with_play_out();
            self.open_stream_and_play();
        }
    }

    /// Opens a stream on the selected device with the kept render source and starts it.
    fn open_stream_and_play(&mut self) {
        if !self.is_watching_device_changes && self.pipeline.is_some() {
            // No notification tells this backend's engine that the default moved: look now.
            self.reread_devices();
        }
        let Some(pipeline) = self.pipeline.as_mut() else {
            return;
        };
        // Drops blocks a seek made stale that the decode thread pushed after the last service,
        // so the new stream starts on the current generation.
        pipeline.service_render_source_once();
        let Some(render_source) = pipeline.render_source.take() else {
            return;
        };
        let source_format = pipeline.source.format;
        let Some(device_id) = self.selected_device_id.clone() else {
            pipeline.render_source = Some(render_source);
            self.on_open_failure(
                OpenFailure {
                    kind: PlaybackFailureKind::DeviceNotFound,
                    detail: "no output device is selected".to_owned(),
                },
                None,
            );
            return;
        };
        let snapshot = self.counters.snapshot();
        pipeline.stream_ends_baseline = snapshot.stream_ends_reached;
        pipeline.format_boundaries_baseline = snapshot.format_boundaries_reached;
        let position_before_open = snapshot.frames_rendered_in_track;
        let request = OpenStreamRequest {
            device_id,
            source_format,
            policy: OpenPolicy::for_preference(self.preference),
        };
        let mut stream = match self.output.open_stream(request, render_source) {
            Ok(mut stream) => {
                stream.set_shared_mode_volume(self.shared_mode_volume.linear_gain());
                stream
            }
            Err(open_error) => {
                pipeline.render_source = Some(open_error.render_source);
                let failure = OpenFailure::from_output_error(&open_error.error);
                self.on_open_failure(failure, Some(position_before_open));
                return;
            }
        };
        if let Err(error) = stream.play() {
            let closed = stream.close(QueuedAudioOnClose::Discard);
            pipeline.render_source = Some(closed.render_source);
            let failure = OpenFailure::from_output_error(&error);
            self.on_open_failure(failure, Some(position_before_open));
            return;
        }
        pipeline.has_opened_stream = true;
        if let Some(requested_at) = self.resume_requested_at.take() {
            self.counters.record_resume_delay(requested_at.elapsed());
        }
        let assessment = assess_signal_path(
            &source_format,
            stream.negotiated(),
            &ProcessingState::default(),
        );
        self.stream = Some(stream);
        self.emit(EngineEvent::SignalPathChanged {
            assessment: Some(assessment),
        });
        self.set_state(PlaybackState::Playing);
    }

    /// A failed start tears the pipeline down; a failed resume stays paused (the user can
    /// retry), except when the device is gone (B8). Frames the failed attempt took from the
    /// feeder (prefill) were never played: the verifier is told, so level 1 cannot read
    /// `Verified` over audio that never reached the device.
    fn on_open_failure(&mut self, failure: OpenFailure, position_before_open: Option<u64>) {
        self.resume_requested_at = None;
        let can_stay_paused = failure.kind != PlaybackFailureKind::DeviceNotFound
            && self
                .pipeline
                .as_ref()
                .is_some_and(|pipeline| pipeline.has_opened_stream);
        self.emit_failure(failure.kind, failure.detail);
        if !can_stay_paused {
            self.end_current_playback(false);
            self.set_state(PlaybackState::Stopped);
            return;
        }
        let has_feeder_moved = position_before_open
            .is_some_and(|before| self.counters.snapshot().frames_rendered_in_track != before);
        if has_feeder_moved {
            if let Some(pipeline) = self.pipeline.as_mut() {
                pipeline.service_render_source();
            }
            self.report_queued_audio_lost();
        }
        self.set_state(PlaybackState::Paused);
    }

    /// The device failed while playing (unplugged, stopped responding): no automatic
    /// re-activation; `Play` restarts the track (B8).
    fn on_stream_failure(&mut self, failure: OutputFailure) {
        self.end_current_playback(false);
        self.emit_failure(
            failure_kind_for_stream_failure(failure),
            format!("{failure:?}"),
        );
        self.refresh_device_list();
        self.set_state(PlaybackState::Stopped);
    }

    fn on_format_boundary(&mut self) {
        self.end_current_playback(false);
        self.emit_failure(
            PlaybackFailureKind::Internal,
            FORMAT_BOUNDARY_DETAIL.to_owned(),
        );
        self.set_state(PlaybackState::Stopped);
    }

    /// The feeder reached the end: the device tail plays out, the pipeline goes without
    /// `TrackStopped`, and the failure that ended the decoding early, if any, is reported now
    /// (kept from its event, or found unread at the teardown on a short track).
    fn on_track_end(&mut self) {
        self.close_with_play_out();
        if let Some(failure) = self.end_current_playback(true) {
            let (kind, detail) = failure_for_segment(&failure);
            self.emit_failure(kind, detail);
        }
        self.set_state(PlaybackState::Stopped);
    }

    /// The decode thread ended without being asked (it panicked): the pipeline cannot go on.
    fn on_decode_thread_lost(&mut self) {
        self.end_current_playback(false);
        self.emit_failure(
            PlaybackFailureKind::Internal,
            "the decode thread stopped unexpectedly".to_owned(),
        );
        self.set_state(PlaybackState::Stopped);
    }

    /// `close(PlayOut)`, then the feeder is serviced, then `QueuedAudioLost` if the play-out
    /// did not finish (sent after the service, so the verifier drains every event first).
    fn close_with_play_out(&mut self) {
        let was_queued_audio_fully_played = self.close_stream(QueuedAudioOnClose::PlayOut);
        if let Some(pipeline) = self.pipeline.as_mut() {
            pipeline.service_render_source();
        }
        if !was_queued_audio_fully_played {
            self.report_queued_audio_lost();
        }
    }

    fn close_with_discard(&mut self) {
        self.close_stream(QueuedAudioOnClose::Discard);
    }

    /// Closes the open stream, if any, and keeps its render source in the pipeline. Returns
    /// `was_queued_audio_fully_played` (`true` when no stream was open).
    fn close_stream(&mut self, queued_audio: QueuedAudioOnClose) -> bool {
        let Some(stream) = self.stream.take() else {
            return true;
        };
        let closed = stream.close(queued_audio);
        if let Some(pipeline) = self.pipeline.as_mut() {
            pipeline.render_source = Some(closed.render_source);
        }
        self.emit(EngineEvent::SignalPathChanged { assessment: None });
        closed.was_queued_audio_fully_played
    }

    fn report_queued_audio_lost(&self) {
        if let Some(pipeline) = self.pipeline.as_ref() {
            pipeline.send_to_verifier(VerifierCommand::QueuedAudioLost {
                track_id: pipeline.track_id,
            });
        }
    }

    /// Closes the stream with `Discard` if one is open and tears the pipeline down (B5.7);
    /// the verifier's final status is forwarded before returning. Returns why the decoding
    /// ended early, if it did.
    ///
    /// If the feeder had already reached the end of the stream, the verifier may have
    /// concluded `Verified` on the `TrackEnded` while the last periods were still queued in
    /// the device; the `Discard` dropped them, so the verifier is told (`QueuedAudioLost`)
    /// and revises the result instead of keeping a false `Verified`.
    fn end_current_playback(&mut self, has_track_ended: bool) -> Option<SegmentFailure> {
        let had_open_stream = self.stream.is_some();
        self.close_with_discard();
        let mut pipeline = self.pipeline.take()?;
        let has_stream_ended =
            self.counters.snapshot().stream_ends_reached > pipeline.stream_ends_baseline;
        if had_open_stream && has_stream_ended {
            pipeline.service_render_source();
            pipeline.send_to_verifier(VerifierCommand::QueuedAudioLost {
                track_id: pipeline.track_id,
            });
        }
        let outcome = pipeline.tear_down(has_track_ended);
        self.forward_pending_verification_updates();
        for thread_name in outcome.panicked_threads {
            self.emit_failure(
                PlaybackFailureKind::Internal,
                format!("the {thread_name} thread panicked"),
            );
        }
        outcome.segment_failure
    }

    /// The "Actualiser" button and the refresh after a device failure: re-lists, re-resolves
    /// and always tells the UI.
    fn refresh_device_list(&mut self) {
        if self.list_devices() {
            self.selected_device_id = self.device_selection.resolve(&self.devices);
        }
        self.emit_device_list();
        self.move_stream_to_selected_device();
    }

    /// The backend reported a new default device or a device added or removed: re-resolve the
    /// choice and, when it now names another device, move the playing stream there.
    pub(crate) fn on_system_devices_changed(&mut self) {
        self.reread_devices();
        self.move_stream_to_selected_device();
    }

    /// Re-lists the devices and re-resolves the choice, telling the UI only when something
    /// changed. A listing failure keeps the previous list: it is not a playback failure, and
    /// the next notification or open reads the list again.
    fn reread_devices(&mut self) {
        let Ok(devices) = self.output.list_devices() else {
            return;
        };
        let selected_device_id = self.device_selection.resolve(&devices);
        if devices != self.devices || selected_device_id != self.selected_device_id {
            self.devices = devices;
            self.selected_device_id = selected_device_id;
            self.emit_device_list();
        }
    }

    /// While playing on a device that is still present but is no longer the one the choice
    /// resolves to: play out, then reopen on the selected device at the first unplayed frame,
    /// with the same access-mode policy (`reopen_if_playing`). A device that disappeared is left
    /// to the stream-failure path, so unplugging headphones stops playback instead of moving it
    /// to the speakers (B8: no automatic re-activation).
    fn move_stream_to_selected_device(&mut self) {
        let Some(device_in_use) = self
            .stream
            .as_ref()
            .map(|stream| stream.negotiated().device.id.clone())
        else {
            return;
        };
        let Some(selected_device_id) = self.selected_device_id.as_ref() else {
            return;
        };
        if *selected_device_id != device_in_use && self.is_device_listed(&device_in_use) {
            self.reopen_if_playing();
        }
    }

    /// `false` (and a `PlaybackFailed`) when the backend cannot list its devices.
    fn list_devices(&mut self) -> bool {
        match self.output.list_devices() {
            Ok(devices) => {
                self.devices = devices;
                true
            }
            Err(error) => {
                self.devices.clear();
                self.emit_failure(failure_kind_for_output_error(&error), error.to_string());
                false
            }
        }
    }

    fn is_device_listed(&self, device_id: &OutputDeviceId) -> bool {
        self.devices.iter().any(|device| &device.id == device_id)
    }

    fn emit_device_list(&self) {
        self.emit(EngineEvent::DeviceListChanged {
            devices: self.devices.clone(),
            selection: self.device_selection.clone(),
            selected: self.selected_device_id.clone(),
        });
    }

    /// Updates from any pipeline, including one being torn down: the UI filters by track.
    fn forward_verification_update(&self, update: VerificationUpdate) {
        self.emit(EngineEvent::VerificationChanged {
            track_id: update.track_id,
            status: update.status,
        });
    }

    fn forward_pending_verification_updates(&self) {
        while let Ok(update) = self.verification_updates.try_recv() {
            self.forward_verification_update(update);
        }
    }

    fn set_state(&mut self, state: PlaybackState) {
        if self.state != state {
            self.state = state;
            self.emit(EngineEvent::PlaybackStateChanged { state });
        }
    }

    fn emit_failure(&self, kind: PlaybackFailureKind, detail: String) {
        self.emit(EngineEvent::PlaybackFailed { kind, detail });
    }

    /// A send error means the UI dropped its handle; the engine is about to be shut down.
    fn emit(&self, event: EngineEvent) {
        let _ = self.events.send(event);
    }
}

/// `position × rate` in integer nanoseconds, clamped to the track's length when known.
fn target_frame_index(position_in_track: Duration, source: &SourceStreamInfo) -> u64 {
    let frames = position_in_track.as_nanos() * u128::from(source.format.sample_rate_hz)
        / NANOSECONDS_PER_SECOND;
    let frames = u64::try_from(frames).unwrap_or(u64::MAX);
    source
        .total_frames
        .map_or(frames, |total_frames| frames.min(total_frames))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{ChannelMask, StreamFormat};
    use crate::testing::{
        CD_STEREO, CORRUPT, FakeOutput, FakeOutputConfig, FakeOutputController, FixtureSpec,
        generate_fixture,
    };
    use crate::verification::{TransmissionNotVerifiableReason, TransmissionStatus};

    const WAIT: Duration = Duration::from_secs(20);

    fn cd_source(total_frames: Option<u64>) -> SourceStreamInfo {
        SourceStreamInfo {
            format: StreamFormat {
                sample_rate_hz: 44_100,
                bits_per_sample: 16,
                channel_count: 2,
                channel_mask: ChannelMask(0b11),
            },
            total_frames,
            embedded_md5: None,
        }
    }

    /// An engine driven by hand on the test thread: nothing polls unless the test does, so
    /// a command can land between a stream end and the poll that would see it.
    struct ManualEngine {
        engine: EngineThread,
        controller: FakeOutputController,
        counters: Arc<PlaybackCounters>,
        events: Receiver<EngineEvent>,
    }

    /// Ring, prefill and tap hold the whole fixture: no underrun, no tap overflow.
    fn whole_track_tuning(spec: &FixtureSpec) -> EngineTuning {
        let track_duration = Duration::from_secs(u64::from(spec.duration_seconds) + 1);
        EngineTuning {
            playback_ring_duration: track_duration,
            start_prefill_duration: track_duration * 2,
            verification_tap_duration: track_duration,
        }
    }

    fn manual_engine(config: FakeOutputConfig, tuning: EngineTuning) -> ManualEngine {
        let (output, controller) = FakeOutput::new(config);
        let (_command_sender, commands) = unbounded();
        let (events, event_receiver) = unbounded();
        let counters = Arc::new(PlaybackCounters::new());
        let mut engine = EngineThread::new(EngineThreadSetup {
            output: Box::new(output),
            decoders: DecoderRegistry::with_default_providers(),
            options: EngineStartOptions {
                output_mode_preference: OutputModePreference::Exclusive,
                device_selection: OutputDeviceSelection::SystemDefault,
                shared_mode_volume: Default::default(),
            },
            tuning,
            counters: Arc::clone(&counters),
            commands,
            events,
        });
        engine.start_up();
        ManualEngine {
            engine,
            controller,
            counters,
            events: event_receiver,
        }
    }

    impl ManualEngine {
        /// Opens `path` and handles decode events until the stream plays; later decode
        /// events stay unread.
        fn open_until_playing(&mut self, path: &std::path::Path) {
            self.engine.handle_command(EngineCommand::OpenFileAndPlay {
                path: path.to_path_buf(),
            });
            while self.engine.state != PlaybackState::Playing {
                let event = self
                    .engine
                    .pipeline
                    .as_ref()
                    .expect("pipeline built")
                    .decode_events
                    .recv_timeout(WAIT)
                    .expect("prefill arrives");
                self.engine.handle_decode_event(event);
            }
        }

        /// Lets the fake render past the end of the track, then holds it there.
        fn render_past_the_end(&self) {
            self.controller.hold_after_more_render_calls(1_000);
            assert!(self.controller.wait_until_held(WAIT));
            assert!(self.counters.snapshot().stream_ends_reached > 0);
        }

        fn emitted(&self) -> Vec<EngineEvent> {
            self.events.try_iter().collect()
        }
    }

    fn last_transmission(events: &[EngineEvent]) -> TransmissionStatus {
        events
            .iter()
            .rev()
            .find_map(|event| match event {
                EngineEvent::VerificationChanged { status, .. } => {
                    Some(status.transmission.clone())
                }
                _ => None,
            })
            .expect("a verification status")
    }

    #[test]
    fn seek_targets_use_integer_math_and_clamp_to_the_track() {
        assert_eq!(
            target_frame_index(Duration::from_millis(1_500), &cd_source(None)),
            66_150
        );
        assert_eq!(
            target_frame_index(Duration::from_nanos(22_675), &cd_source(None)),
            0,
            "just under one frame"
        );
        assert_eq!(
            target_frame_index(Duration::from_secs(10), &cd_source(Some(44_100))),
            44_100
        );
    }

    #[test]
    fn a_format_boundary_seen_on_the_poll_tears_down_and_fails_internally() {
        let fixture = generate_fixture(&CD_STEREO);
        let mut manual = manual_engine(FakeOutputConfig::default(), EngineTuning::default());
        // The fake holds right after the prefill, so the track cannot end before the polls.
        manual.controller.hold_after_more_render_calls(1);
        manual.open_until_playing(&fixture.path);
        manual.engine.poll();
        assert_eq!(
            manual.engine.state,
            PlaybackState::Playing,
            "nothing to react to"
        );

        manual.counters.record_format_boundary_reached();
        manual.engine.poll();

        assert_eq!(manual.engine.state, PlaybackState::Stopped);
        assert!(manual.engine.pipeline.is_none() && manual.engine.stream.is_none());
        let events = manual.emitted();
        assert!(events.iter().any(|event| matches!(
            event,
            EngineEvent::PlaybackFailed {
                kind: PlaybackFailureKind::Internal,
                detail
            } if detail == FORMAT_BOUNDARY_DETAIL
        )));
        assert!(matches!(
            events.last(),
            Some(EngineEvent::PlaybackStateChanged {
                state: PlaybackState::Stopped
            })
        ));
    }

    /// Regression: the end marker was rendered (the verifier saw `TrackEnded`) but the last
    /// periods were still queued in the device when Stop discarded them, before any poll.
    #[test]
    fn a_stop_that_discards_the_queued_end_of_the_track_is_never_verified() {
        let fixture = generate_fixture(&CD_STEREO);
        let config = FakeOutputConfig {
            queued_frame_capacity: 10 * FakeOutputConfig::default().buffer_frame_count,
            ..FakeOutputConfig::default()
        };
        let mut manual = manual_engine(config, whole_track_tuning(&CD_STEREO));
        manual.controller.hold_after_more_render_calls(1);
        manual.open_until_playing(&fixture.path);
        manual.render_past_the_end();

        manual.engine.handle_command(EngineCommand::Stop);

        assert!(!manual.controller.dropped_bytes().is_empty());
        assert_eq!(
            last_transmission(&manual.emitted()),
            TransmissionStatus::NotVerifiable {
                reason: TransmissionNotVerifiableReason::QueuedAudioLostOnPause
            }
        );
    }

    /// Regression: on a short damaged file the track end can be polled before the decode
    /// thread's `SegmentEnded` is read; the error must still be reported.
    #[test]
    fn a_decode_error_still_unread_at_the_track_end_is_reported() {
        let short_corrupt = FixtureSpec {
            duration_seconds: 1,
            ..CORRUPT
        };
        let fixture = generate_fixture(&short_corrupt);
        let mut manual = manual_engine(FakeOutputConfig::default(), whole_track_tuning(&CORRUPT));
        manual.controller.hold_after_more_render_calls(1);
        manual.open_until_playing(&fixture.path);
        manual.render_past_the_end();
        assert!(
            manual.engine.pipeline.as_ref().is_some_and(|pipeline| {
                pipeline.kept_segment_failure.is_none() && !pipeline.decode_events.is_empty()
            }),
            "the SegmentEnded event is still unread"
        );

        manual.engine.poll();

        assert_eq!(manual.engine.state, PlaybackState::Stopped);
        assert!(manual.emitted().iter().any(|event| matches!(
            event,
            EngineEvent::PlaybackFailed {
                kind: PlaybackFailureKind::CorruptFile,
                ..
            }
        )));
    }
}
