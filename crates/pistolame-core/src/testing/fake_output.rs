//! `FakeOutput`: an `AudioOutput` with no device behind it, for the engine integration tests
//! (plan B10.3). It negotiates like the WASAPI backend (B5.4 policy rules, with the B13.4
//! `DEVICE_IN_USE` row), runs a render thread that calls `begin_stream` once per open, prefills
//! and then calls `render` unpaced, and records every byte that "reaches the speaker".
//!
//! Model of the device: a queue of at most `queued_frame_capacity` frames. While playing, each
//! step either renders one buffer into the queue (when it has room) or lets the device play one
//! buffer out of it, in order, into the recorded bytes. `close(PlayOut)` plays the queue out
//! (or drops it with `play_out_should_time_out`); `close(Discard)` drops it. Dropped bytes are
//! recorded separately. Silence written by a backend during a play-out is not recorded.
//!
//! Deterministic stepping: the test can make the render thread hold after a given number of
//! `render` calls (it stops calling `render` until released or closed), so pause, seek, mode
//! switch, stop and failure happen at known frames. Locks and allocation here are fine: this is
//! the fake device's own thread, outside the feeder.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::format::{DeviceSampleFormat, ValidBitsAlignment, lossless_device_format_candidates};
use crate::output_contract::{
    AudioOutput, ClosedStream, DeviceAccessMode, DeviceChangeListener, DeviceSampleDelivery,
    FormatProbeOutcome, FormatProbeResult, NegotiatedOutput, OpenPolicy, OpenStreamError,
    OpenStreamRequest, OutputDeviceId, OutputDeviceInfo, OutputError, OutputFailure, OutputStream,
    OutputStreamStatus, QueuedAudioOnClose, RenderOutcome, RenderSource, RenderStreamDescription,
    SharedModeCause, SystemMixFormat,
};

/// Name the fake backend reports, in `backend_name` and in every `NegotiatedOutput`.
const FAKE_BACKEND_NAME: &str = "Fake";

/// Identifier of the single device of the default configuration.
pub const FAKE_DEVICE_ID: &str = "fake-device";
/// Pause after a buffer that was not all audio (end of stream, underrun), so an unpaced render
/// loop records a bounded amount of silence before the engine's next poll.
const SILENT_BUFFER_PAUSE: Duration = Duration::from_millis(1);
/// Device period reported in `NegotiatedOutput` (10 ms in units of 100 ns); display only.
const FAKE_DEVICE_PERIOD_IN_HUNDRED_NANOSECONDS: i64 = 100_000;
/// Number of distinct buffer lengths the shared-mode fake cycles through (quarters of the
/// buffer), like `GetCurrentPadding` leaving a varying amount of room.
const SHARED_BUFFER_LENGTH_STEPS: u32 = 4;

/// One render format a configuration accepts, by container and valid bits (rate and channels
/// always follow the source, B3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatChoice {
    /// Bytes per sample in the container.
    pub container_bytes: u8,
    /// Valid bits in the container.
    pub valid_bits_per_sample: u8,
}

impl FormatChoice {
    /// The choice that matches `format`.
    pub fn of(format: &DeviceSampleFormat) -> Self {
        Self {
            container_bytes: format.container_bytes,
            valid_bits_per_sample: format.valid_bits_per_sample,
        }
    }
}

/// Which lossless candidates a mode accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AcceptedFormats {
    /// Every candidate.
    All,
    /// Only these (the first candidate in the backend's order that is listed wins).
    Only(Vec<FormatChoice>),
    /// None.
    Nothing,
}

impl AcceptedFormats {
    fn accepts(&self, format: &DeviceSampleFormat) -> bool {
        match self {
            AcceptedFormats::All => true,
            AcceptedFormats::Only(choices) => choices.contains(&FormatChoice::of(format)),
            AcceptedFormats::Nothing => false,
        }
    }
}

/// What other applications do with the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceOccupancy {
    /// Nobody else uses it.
    Free,
    /// Others play through it in shared mode and Windows does not let exclusive mode take it:
    /// exclusive is refused with "in use", shared is accepted (B13.4).
    BusyInSharedMode,
    /// Another application holds it exclusively: both modes are refused with "in use".
    HeldExclusivelyByAnotherApp,
}

/// The fake device's behavior. May change between two opens (`FakeOutputController`).
#[derive(Clone, Debug)]
pub struct FakeOutputConfig {
    /// What `list_devices` returns; `open_stream` refuses other ids with `DeviceNotFound`.
    pub devices: Vec<OutputDeviceInfo>,
    /// Candidates accepted in exclusive mode.
    pub exclusive_formats: AcceptedFormats,
    /// Candidates accepted in shared mode (with Windows' conversion).
    pub shared_formats: AcceptedFormats,
    /// Exclusive mode is disabled for the device in the system settings.
    pub is_exclusive_mode_disabled: bool,
    /// What other applications do with the device.
    pub occupancy: DeviceOccupancy,
    /// The mix format reported in shared mode.
    pub system_mix_format: SystemMixFormat,
    /// Frames per `render` call (exclusive) or the largest call (shared).
    pub buffer_frame_count: u32,
    /// Frames the simulated device queue holds; at least `buffer_frame_count`.
    pub queued_frame_capacity: u32,
    /// `close(PlayOut)` times out: the queued frames are dropped, not played.
    pub play_out_should_time_out: bool,
    /// Whether `watch_device_changes` accepts a listener (like CoreAudio and WASAPI) or
    /// refuses it (like a backend without notifications). Read once, when the engine starts.
    pub can_watch_device_changes: bool,
    /// After each successful exclusive open, the system default moves to this device and the
    /// listener is called, like a system that moves its default away from a device another
    /// process holds.
    pub exclusive_open_moves_default_to: Option<OutputDeviceId>,
}

/// The single device of the default configuration.
pub fn fake_device() -> OutputDeviceInfo {
    OutputDeviceInfo {
        id: OutputDeviceId(FAKE_DEVICE_ID.to_owned()),
        display_name: "Fake DAC".to_owned(),
        is_system_default: true,
    }
}

impl Default for FakeOutputConfig {
    fn default() -> Self {
        Self {
            devices: vec![fake_device()],
            exclusive_formats: AcceptedFormats::All,
            shared_formats: AcceptedFormats::All,
            is_exclusive_mode_disabled: false,
            occupancy: DeviceOccupancy::Free,
            system_mix_format: SystemMixFormat {
                sample_rate_hz: 48_000,
                bits_per_sample: 32,
                is_float: true,
                channel_count: 2,
            },
            buffer_frame_count: 441,
            queued_frame_capacity: 882,
            play_out_should_time_out: false,
            can_watch_device_changes: true,
            exclusive_open_moves_default_to: None,
        }
    }
}

/// What the engine asked of the open stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamRequest {
    NotStarted,
    Run,
    Close(QueuedAudioOnClose),
}

/// State shared by the fake output, its streams and the test's controller.
struct FakeState {
    config: FakeOutputConfig,
    shared_mode_volume_calls: Vec<f32>,
    played_bytes: Vec<u8>,
    dropped_bytes: Vec<u8>,
    render_call_count: u64,
    hold_at_render_call: Option<u64>,
    is_held: bool,
    begin_stream_count: u64,
    negotiated_streams: Vec<NegotiatedOutput>,
    injected_failure: Option<OutputFailure>,
    stream_request: StreamRequest,
    device_change_listener: Option<Arc<dyn Fn() + Send + Sync>>,
    is_stream_device_gone: bool,
}

struct FakeShared {
    state: Mutex<FakeState>,
    changed: Condvar,
}

impl FakeShared {
    fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn update<Output>(&self, change: impl FnOnce(&mut FakeState) -> Output) -> Output {
        let output = change(&mut self.lock());
        self.changed.notify_all();
        output
    }
}

/// The fake backend, handed to the engine.
pub struct FakeOutput {
    shared: Arc<FakeShared>,
}

/// The test's side of a `FakeOutput`: configuration, stepping, failure injection and the
/// recorded bytes.
#[derive(Clone)]
pub struct FakeOutputController {
    shared: Arc<FakeShared>,
}

impl FakeOutput {
    /// A fake backend and the controller that drives it.
    pub fn new(config: FakeOutputConfig) -> (FakeOutput, FakeOutputController) {
        let shared = Arc::new(FakeShared {
            state: Mutex::new(FakeState {
                config,
                shared_mode_volume_calls: Vec::new(),
                played_bytes: Vec::new(),
                dropped_bytes: Vec::new(),
                render_call_count: 0,
                hold_at_render_call: None,
                is_held: false,
                begin_stream_count: 0,
                negotiated_streams: Vec::new(),
                injected_failure: None,
                stream_request: StreamRequest::NotStarted,
                device_change_listener: None,
                is_stream_device_gone: false,
            }),
            changed: Condvar::new(),
        });
        (
            FakeOutput {
                shared: Arc::clone(&shared),
            },
            FakeOutputController { shared },
        )
    }
}

impl FakeOutputController {
    /// Changes the configuration; it applies from the next open.
    pub fn update_config(&self, change: impl FnOnce(&mut FakeOutputConfig)) {
        self.shared.update(|state| change(&mut state.config));
    }

    /// The open stream's `is_device_still_present` answers `false` from now on (the device is
    /// dying but still listed and its stream has not failed yet). Reset by the next open.
    pub fn mark_stream_device_gone(&self) {
        self.shared
            .update(|state| state.is_stream_device_gone = true);
    }

    /// Changes the device list (for example which device is the system default), then calls
    /// the engine's device-change listener, like a platform notification, if one was
    /// registered. Returns whether a listener was called.
    pub fn change_devices(&self, change: impl FnOnce(&mut Vec<OutputDeviceInfo>)) -> bool {
        let listener = self.shared.update(|state| {
            change(&mut state.config.devices);
            state.device_change_listener.clone()
        });
        // Called outside the lock: the listener may run engine code that lists the devices.
        listener.map(|listener| listener()).is_some()
    }

    /// The open stream reports `Failed(failure)` from now on and stops rendering, like a
    /// WASAPI render thread that stored a failure (no wake-up: the engine polls).
    pub fn fail_stream(&self, failure: OutputFailure) {
        self.shared
            .update(|state| state.injected_failure = Some(failure));
    }

    /// The render thread stops calling `render` once `count` more calls were made.
    pub fn hold_after_more_render_calls(&self, count: u64) {
        self.shared.update(|state| {
            state.hold_at_render_call = Some(state.render_call_count + count);
            state.is_held = false;
        });
    }

    /// Lets the render thread run freely again.
    pub fn release(&self) {
        self.shared.update(|state| {
            state.hold_at_render_call = None;
            state.is_held = false;
        });
    }

    /// Waits until the render thread holds; `false` on timeout.
    pub fn wait_until_held(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.lock();
        while !state.is_held {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    /// Every byte the simulated device played, in order.
    pub fn played_bytes(&self) -> Vec<u8> {
        self.shared.lock().played_bytes.clone()
    }

    /// Bytes dropped from the device queue by `close(Discard)` or a timed-out play-out.
    pub fn dropped_bytes(&self) -> Vec<u8> {
        self.shared.lock().dropped_bytes.clone()
    }

    /// `begin_stream` calls so far (one per successful open).
    pub fn begin_stream_count(&self) -> u64 {
        self.shared.lock().begin_stream_count
    }

    /// What each successful open negotiated, in order.
    pub fn negotiated_streams(&self) -> Vec<NegotiatedOutput> {
        self.shared.lock().negotiated_streams.clone()
    }

    /// Every shared-mode volume the engine applied to a stream, in order.
    pub fn shared_mode_volume_calls(&self) -> Vec<f32> {
        self.shared.lock().shared_mode_volume_calls.clone()
    }

    /// `render` calls so far, prefills included.
    pub fn render_call_count(&self) -> u64 {
        self.shared.lock().render_call_count
    }
}

impl AudioOutput for FakeOutput {
    fn backend_name(&self) -> &'static str {
        FAKE_BACKEND_NAME
    }

    fn list_devices(&mut self) -> Result<Vec<OutputDeviceInfo>, OutputError> {
        Ok(self.shared.lock().config.devices.clone())
    }

    fn probe_formats(
        &mut self,
        device_id: &OutputDeviceId,
        candidates: &[DeviceSampleFormat],
    ) -> Result<Vec<FormatProbeResult>, OutputError> {
        let config = self.shared.lock().config.clone();
        find_device(&config, device_id)?;
        Ok(candidates
            .iter()
            .map(|candidate| probe_result(candidate, DeviceAccessMode::Exclusive, &config))
            .collect())
    }

    fn open_stream(
        &mut self,
        request: OpenStreamRequest,
        mut render_source: Box<dyn RenderSource>,
    ) -> Result<Box<dyn OutputStream>, OpenStreamError> {
        let config = self.shared.lock().config.clone();
        let negotiated = match negotiate(&config, &request) {
            Ok(negotiated) => negotiated,
            Err(error) => {
                return Err(OpenStreamError {
                    error,
                    render_source,
                });
            }
        };
        render_source.begin_stream(RenderStreamDescription {
            render_format: negotiated.render_format,
            access_mode: negotiated.access_mode,
        });
        self.shared.update(|state| {
            state.begin_stream_count += 1;
            state.negotiated_streams.push(negotiated.clone());
            state.injected_failure = None;
            state.stream_request = StreamRequest::NotStarted;
            state.is_stream_device_gone = false;
        });
        if negotiated.access_mode == DeviceAccessMode::Exclusive
            && let Some(new_default) = config.exclusive_open_moves_default_to.clone()
        {
            let controller = FakeOutputController {
                shared: Arc::clone(&self.shared),
            };
            controller.change_devices(|devices| {
                for device in devices {
                    device.is_system_default = device.id == new_default;
                }
            });
        }
        let mut device = FakeDevice::new(&config, &negotiated);
        device.render_buffer(render_source.as_mut(), &self.shared);
        let shared = Arc::clone(&self.shared);
        let thread = std::thread::Builder::new()
            .name("fake-output-render".to_owned())
            .spawn(move || device.run(render_source, &shared))
            .expect("the fake render thread spawns");
        Ok(Box::new(FakeStream {
            negotiated,
            shared: Arc::clone(&self.shared),
            thread: Some(thread),
        }))
    }

    fn watch_device_changes(&mut self, on_change: DeviceChangeListener) -> bool {
        self.shared.update(|state| {
            if state.config.can_watch_device_changes {
                state.device_change_listener = Some(Arc::from(on_change));
            }
            state.config.can_watch_device_changes
        })
    }
}

fn find_device(
    config: &FakeOutputConfig,
    device_id: &OutputDeviceId,
) -> Result<OutputDeviceInfo, OutputError> {
    config
        .devices
        .iter()
        .find(|device| &device.id == device_id)
        .cloned()
        .ok_or(OutputError::DeviceNotFound)
}

fn probe_result(
    candidate: &DeviceSampleFormat,
    access_mode: DeviceAccessMode,
    config: &FakeOutputConfig,
) -> FormatProbeResult {
    let accepted = match access_mode {
        DeviceAccessMode::Exclusive => &config.exclusive_formats,
        DeviceAccessMode::SharedThroughSystemMixer => &config.shared_formats,
    };
    FormatProbeResult {
        candidate: *candidate,
        access_mode,
        outcome: if accepted.accepts(candidate) {
            FormatProbeOutcome::Accepted
        } else {
            FormatProbeOutcome::Refused
        },
    }
}

/// B5.4 policy rules with the B13.4 `DEVICE_IN_USE` row, as the WASAPI backend applies them.
fn negotiate(
    config: &FakeOutputConfig,
    request: &OpenStreamRequest,
) -> Result<NegotiatedOutput, OutputError> {
    let device = find_device(config, &request.device_id)?;
    let candidates = lossless_device_format_candidates(
        &request.source_format,
        ValidBitsAlignment::MostSignificant,
    );
    let may_fall_back = request.policy == OpenPolicy::ExclusiveThenShared;
    let mut negotiation_log = Vec::new();
    let fallback_cause = match request.policy {
        OpenPolicy::SharedOnly => SharedModeCause::UserPreference,
        OpenPolicy::ExclusiveThenShared | OpenPolicy::ExclusiveOnly => {
            match exclusive_refusal(config) {
                Some(OutputError::DeviceInUse) if may_fall_back => {
                    SharedModeCause::DeviceBusyInSharedMode
                }
                Some(OutputError::ExclusiveModeNotAllowed) if may_fall_back => {
                    SharedModeCause::ExclusiveModeDisabledBySystem
                }
                Some(error) => return Err(error),
                None => {
                    for candidate in &candidates {
                        let result = probe_result(candidate, DeviceAccessMode::Exclusive, config);
                        negotiation_log.push(result.clone());
                        if result.outcome == FormatProbeOutcome::Accepted {
                            return Ok(exclusive_output(
                                config,
                                device,
                                *candidate,
                                negotiation_log,
                            ));
                        }
                    }
                    if !may_fall_back {
                        return Err(OutputError::FormatNotSupported {
                            tried: negotiation_log,
                        });
                    }
                    SharedModeCause::ExclusiveFormatRefused
                }
            }
        }
    };
    open_shared(config, device, &candidates, fallback_cause, negotiation_log)
}

/// Why exclusive mode cannot even be tried.
fn exclusive_refusal(config: &FakeOutputConfig) -> Option<OutputError> {
    if config.occupancy != DeviceOccupancy::Free {
        return Some(OutputError::DeviceInUse);
    }
    if config.is_exclusive_mode_disabled {
        return Some(OutputError::ExclusiveModeNotAllowed);
    }
    None
}

fn open_shared(
    config: &FakeOutputConfig,
    device: OutputDeviceInfo,
    candidates: &[DeviceSampleFormat],
    cause: SharedModeCause,
    mut negotiation_log: Vec<FormatProbeResult>,
) -> Result<NegotiatedOutput, OutputError> {
    if config.occupancy == DeviceOccupancy::HeldExclusivelyByAnotherApp {
        return Err(OutputError::DeviceInUse);
    }
    for candidate in candidates {
        let result = probe_result(
            candidate,
            DeviceAccessMode::SharedThroughSystemMixer,
            config,
        );
        negotiation_log.push(result.clone());
        if result.outcome == FormatProbeOutcome::Accepted {
            return Ok(NegotiatedOutput {
                backend_name: FAKE_BACKEND_NAME,
                device,
                access_mode: DeviceAccessMode::SharedThroughSystemMixer,
                shared_mode_cause: Some(cause),
                device_format: None,
                system_mix_format: Some(config.system_mix_format),
                render_format: *candidate,
                sample_delivery: DeviceSampleDelivery::Unchanged,
                buffer_frame_count: config.buffer_frame_count,
                device_period_in_hundred_nanoseconds: FAKE_DEVICE_PERIOD_IN_HUNDRED_NANOSECONDS,
                negotiation_log,
            });
        }
    }
    Err(OutputError::FormatNotSupported {
        tried: negotiation_log,
    })
}

fn exclusive_output(
    config: &FakeOutputConfig,
    device: OutputDeviceInfo,
    format: DeviceSampleFormat,
    negotiation_log: Vec<FormatProbeResult>,
) -> NegotiatedOutput {
    NegotiatedOutput {
        backend_name: FAKE_BACKEND_NAME,
        device,
        access_mode: DeviceAccessMode::Exclusive,
        shared_mode_cause: None,
        device_format: Some(format),
        system_mix_format: None,
        render_format: format,
        sample_delivery: DeviceSampleDelivery::Unchanged,
        buffer_frame_count: config.buffer_frame_count,
        device_period_in_hundred_nanoseconds: FAKE_DEVICE_PERIOD_IN_HUNDRED_NANOSECONDS,
        negotiation_log,
    }
}

/// The simulated device of one open stream.
struct FakeDevice {
    access_mode: DeviceAccessMode,
    bytes_per_frame: usize,
    buffer_frame_count: usize,
    queue_capacity_bytes: usize,
    queue: VecDeque<u8>,
    buffer: Vec<u8>,
    play_out_should_time_out: bool,
    call_index: u32,
}

/// What the render thread does next.
enum DeviceStep {
    Render,
    Close(QueuedAudioOnClose),
}

impl FakeDevice {
    fn new(config: &FakeOutputConfig, negotiated: &NegotiatedOutput) -> Self {
        let bytes_per_frame = negotiated.render_format.bytes_per_frame().max(1);
        let buffer_frame_count = usize::try_from(config.buffer_frame_count)
            .unwrap_or(usize::MAX)
            .max(1);
        let queued_frame_capacity = usize::try_from(config.queued_frame_capacity)
            .unwrap_or(usize::MAX)
            .max(buffer_frame_count);
        Self {
            access_mode: negotiated.access_mode,
            bytes_per_frame,
            buffer_frame_count,
            queue_capacity_bytes: queued_frame_capacity * bytes_per_frame,
            queue: VecDeque::new(),
            buffer: vec![0; buffer_frame_count * bytes_per_frame],
            play_out_should_time_out: config.play_out_should_time_out,
            call_index: 0,
        }
    }

    fn run(
        mut self,
        mut render_source: Box<dyn RenderSource>,
        shared: &FakeShared,
    ) -> (Box<dyn RenderSource>, bool) {
        loop {
            match self.wait_for_step(shared) {
                DeviceStep::Render => self.step(render_source.as_mut(), shared),
                DeviceStep::Close(queued_audio) => {
                    let was_queued_audio_fully_played = self.close(queued_audio, shared);
                    return (render_source, was_queued_audio_fully_played);
                }
            }
        }
    }

    /// Blocks while not started, failed or held.
    fn wait_for_step(&self, shared: &FakeShared) -> DeviceStep {
        let mut state = shared.lock();
        loop {
            match state.stream_request {
                StreamRequest::Close(queued_audio) => return DeviceStep::Close(queued_audio),
                StreamRequest::Run if state.injected_failure.is_none() => {
                    let is_hold_reached = state
                        .hold_at_render_call
                        .is_some_and(|hold_at| state.render_call_count >= hold_at);
                    if !is_hold_reached {
                        return DeviceStep::Render;
                    }
                    if !state.is_held {
                        state.is_held = true;
                        shared.changed.notify_all();
                    }
                }
                StreamRequest::Run | StreamRequest::NotStarted => {}
            }
            state = shared
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Renders one buffer into the queue if it has room, otherwise plays one buffer out of it.
    fn step(&mut self, render_source: &mut dyn RenderSource, shared: &FakeShared) {
        let frame_count = self.frames_for_next_call();
        let free_bytes = self.queue_capacity_bytes.saturating_sub(self.queue.len());
        if free_bytes >= frame_count * self.bytes_per_frame {
            self.render_frames(render_source, frame_count, shared);
        } else {
            let played_byte_count = (frame_count * self.bytes_per_frame).min(self.queue.len());
            let mut state = shared.lock();
            state
                .played_bytes
                .extend(self.queue.drain(..played_byte_count));
        }
    }

    /// The prefill: one full buffer.
    fn render_buffer(&mut self, render_source: &mut dyn RenderSource, shared: &FakeShared) {
        self.render_frames(render_source, self.buffer_frame_count, shared);
    }

    fn render_frames(
        &mut self,
        render_source: &mut dyn RenderSource,
        frame_count: usize,
        shared: &FakeShared,
    ) {
        let byte_count = frame_count * self.bytes_per_frame;
        let destination = &mut self.buffer[..byte_count];
        let outcome = render_source.render(destination);
        self.queue.extend(destination.iter().copied());
        shared.update(|state| state.render_call_count += 1);
        if outcome != RenderOutcome::Filled {
            std::thread::sleep(SILENT_BUFFER_PAUSE);
        }
    }

    /// Exclusive: the whole buffer every time. Shared: a varying share of it.
    fn frames_for_next_call(&mut self) -> usize {
        self.call_index = self.call_index.wrapping_add(1);
        match self.access_mode {
            DeviceAccessMode::Exclusive => self.buffer_frame_count,
            DeviceAccessMode::SharedThroughSystemMixer => {
                let step_frames =
                    (self.buffer_frame_count / SHARED_BUFFER_LENGTH_STEPS as usize).max(1);
                step_frames * (1 + (self.call_index % SHARED_BUFFER_LENGTH_STEPS) as usize)
            }
        }
    }

    fn close(&mut self, queued_audio: QueuedAudioOnClose, shared: &FakeShared) -> bool {
        let is_played_out =
            queued_audio == QueuedAudioOnClose::PlayOut && !self.play_out_should_time_out;
        let mut state = shared.lock();
        if is_played_out {
            state.played_bytes.extend(self.queue.drain(..));
        } else {
            state.dropped_bytes.extend(self.queue.drain(..));
        }
        is_played_out
    }
}

/// A stream of the fake backend.
struct FakeStream {
    negotiated: NegotiatedOutput,
    shared: Arc<FakeShared>,
    thread: Option<JoinHandle<(Box<dyn RenderSource>, bool)>>,
}

impl FakeStream {
    fn close_and_join(&mut self, queued_audio: QueuedAudioOnClose) -> Option<ClosedStream> {
        let thread = self.thread.take()?;
        self.shared
            .update(|state| state.stream_request = StreamRequest::Close(queued_audio));
        let (render_source, was_queued_audio_fully_played) =
            thread.join().expect("the fake render thread never panics");
        self.shared.update(|state| state.injected_failure = None);
        Some(ClosedStream {
            render_source,
            was_queued_audio_fully_played,
        })
    }
}

impl OutputStream for FakeStream {
    fn negotiated(&self) -> &NegotiatedOutput {
        &self.negotiated
    }

    fn set_shared_mode_volume(&mut self, linear_gain: f32) {
        self.shared
            .update(|state| state.shared_mode_volume_calls.push(linear_gain));
    }

    fn play(&mut self) -> Result<(), OutputError> {
        self.shared
            .update(|state| state.stream_request = StreamRequest::Run);
        Ok(())
    }

    fn is_device_still_present(&self) -> bool {
        !self.shared.lock().is_stream_device_gone
    }

    fn status(&self) -> OutputStreamStatus {
        let state = self.shared.lock();
        match (state.injected_failure, state.stream_request) {
            (Some(failure), _) => OutputStreamStatus::Failed(failure),
            (None, StreamRequest::Run) => OutputStreamStatus::Running,
            (None, _) => OutputStreamStatus::NotStarted,
        }
    }

    fn close(mut self: Box<Self>, queued_audio: QueuedAudioOnClose) -> ClosedStream {
        self.close_and_join(queued_audio)
            .expect("a stream is closed once")
    }
}

impl Drop for FakeStream {
    /// Dropping equals `close(Discard)` (output contract).
    fn drop(&mut self) {
        let _ = self.close_and_join(QueuedAudioOnClose::Discard);
    }
}
