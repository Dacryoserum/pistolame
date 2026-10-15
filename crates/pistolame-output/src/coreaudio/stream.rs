//! One CoreAudio output stream: the real-time render callback, the device listeners and the
//! `OutputStream` the engine drives (plan coreaudio.md §0, §4, §5).
//!
//! Threads: the AUHAL calls `render_callback` on CoreAudio's real-time IO thread; the engine
//! calls `play`, `status` and `close`; CoreAudio calls `device_listener` on its notification
//! thread. They share lock-free atomics only (`StreamStateCell`, counters).
//!
//! Real-time rules in the callback: no allocation, no lock, no I/O, no logging, no panic
//! escaping (caught and turned into a failure status).

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use objc2_audio_toolbox::{AURenderCallbackStruct, AudioUnitRenderActionFlags};
use objc2_core_audio::{
    AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectRemovePropertyListener, kAudioDevicePropertyDeviceIsAlive,
    kAudioDevicePropertyHogMode, kAudioDevicePropertyNominalSampleRate,
    kAudioObjectPropertyScopeGlobal,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
use pistolame_core::timing::{DEVICE_EVENT_TIMEOUT, QUEUED_AUDIO_PLAY_OUT_TIMEOUT};

use super::device_takeover::ExclusiveTakeover;
use super::devices::{is_device_alive, nominal_sample_rate};
use super::output_unit::{OutputUnit, float32_client_format};
use super::properties::{NO_ERROR, property_address};
use crate::float_conversion::convert_integer_samples_to_float32;
use crate::stream_state::{RequestedStreamState, StreamStateCell};
use crate::{
    ClosedStream, DeviceAccessMode, NegotiatedOutput, OutputError, OutputFailure, OutputStream,
    OutputStreamStatus, QueuedAudioOnClose, RenderSource, RenderStreamDescription,
};

/// Largest number of output frames the unit may ask for in one callback (4096 is the AUHAL
/// default, `kAudioUnitProperty_MaximumFramesPerSlice` docs). The integer scratch buffer is sized
/// for it; larger requests (upsampling in shared mode) are rendered in several chunks.
const MAXIMUM_FRAMES_PER_SLICE: u32 = 4096;
/// Platform code stored when CoreAudio hands a buffer smaller than the frames it asks for
/// (`kAudioUnitErr_InvalidParameter`; never expected).
const BUFFER_SMALLER_THAN_FRAME_COUNT: i32 = -10878;
/// Platform code stored when the render callback panicked (no OSStatus applies).
const RENDER_CALLBACK_PANICKED: i32 = -1;
/// Pause between two checks while the engine waits for the play-out or for callbacks to end.
const CLOSE_POLL_INTERVAL: Duration = Duration::from_millis(2);
/// Longest wait for the first callback after `play`: Bluetooth devices may take several
/// seconds to switch profile and start their IO.
const FIRST_CALLBACK_TIMEOUT: Duration = Duration::from_secs(8);
/// Longest wait for an in-flight callback after `AudioOutputUnitStop`; disposal follows anyway.
const CALLBACK_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

/// Everything one opened stream needs, decided by `CoreAudioOutput::open_stream`.
pub(crate) struct StreamPlan {
    /// The device to play to.
    pub(crate) device: AudioObjectID,
    /// What the engine will read back from `negotiated()`.
    pub(crate) negotiated: NegotiatedOutput,
    /// The hog-mode and sample-rate guard, exclusive mode only.
    pub(crate) takeover: Option<ExclusiveTakeover>,
}

/// State the render callback reads and writes. Allocated before the unit is configured and
/// freed only after the unit is disposed, so the callback never sees it dangle.
struct CallbackContext {
    state: Arc<StreamStateCell>,
    /// Touched only by the callback while the unit runs, and by the engine before the first
    /// start or after disposal.
    render_source: UnsafeCell<Option<Box<dyn RenderSource>>>,
    /// Integer bytes the render source writes before the exact float conversion. Sized for
    /// `MAXIMUM_FRAMES_PER_SLICE`; never reallocated.
    integer_scratch: UnsafeCell<Box<[u8]>>,
    bytes_per_frame: usize,
    /// Set by the engine before stopping: later callbacks write silence and touch nothing else.
    is_detached: AtomicBool,
    callbacks_in_progress: AtomicU32,
    callback_count: AtomicU64,
    /// Silent frames written since the engine requested a close.
    silent_frames_since_close_request: AtomicU64,
    /// Shared mode only: the user's volume as `f32` bits (unity in exclusive mode, never read).
    shared_mode_gain_bits: AtomicU32,
    is_shared_mode: bool,
}

// SAFETY: the `UnsafeCell` fields are accessed by one thread at a time: by the engine thread
// before `AudioOutputUnitStart` and after the unit is disposed, and by the single CoreAudio IO
// thread in between (see `CoreAudioStream::teardown`). Everything else is atomic.
unsafe impl Sync for CallbackContext {}

impl CallbackContext {
    /// The gain to apply, or `None` for unity (exclusive mode, or shared at 0 dB).
    fn shared_mode_gain(&self) -> Option<f32> {
        if !self.is_shared_mode {
            return None;
        }
        let gain = f32::from_bits(self.shared_mode_gain_bits.load(Ordering::Acquire));
        (gain != 1.0).then_some(gain)
    }

    fn store_failure(&self, failure: OutputFailure) {
        self.state.store_status(OutputStreamStatus::Failed(failure));
    }

    /// Fills the unit's buffer. Real-time safe.
    fn render(&self, frame_count: u32, buffer_list: *mut AudioBufferList) {
        // SAFETY: CoreAudio passes a valid buffer list for this callback; `output_bytes`
        // checks every pointer before building the slice.
        let Some(destination) = (unsafe { output_bytes(buffer_list) }) else {
            return;
        };
        if self.is_detached.load(Ordering::Acquire) {
            destination.fill(0);
            return;
        }
        let byte_count = frame_count as usize * self.bytes_per_frame;
        if byte_count > destination.len() {
            destination.fill(0);
            self.store_failure(OutputFailure::Platform {
                platform_code: BUFFER_SMALLER_THAN_FRAME_COUNT,
            });
            return;
        }
        let destination = &mut destination[..byte_count];
        match self.state.requested_state() {
            RequestedStreamState::Run => {
                // SAFETY: only this callback touches the scratch buffer and the render source
                // while the unit runs.
                let scratch = unsafe { &mut *self.integer_scratch.get() };
                // SAFETY: as above.
                let render_source = unsafe { &mut *self.render_source.get() };
                if let Some(render_source) = render_source.as_mut() {
                    // Chunks of the scratch capacity: an upsampling converter may pull more
                    // frames than the slice maximum, so no request size is too large.
                    let chunk_bytes = scratch.len() - scratch.len() % self.bytes_per_frame;
                    let gain = self.shared_mode_gain();
                    for destination_chunk in destination.chunks_mut(chunk_bytes.max(1)) {
                        let integer_bytes = &mut scratch[..destination_chunk.len()];
                        render_source.render(integer_bytes);
                        convert_integer_samples_to_float32(integer_bytes, destination_chunk);
                        if let Some(gain) = gain {
                            apply_gain_to_float32_samples(destination_chunk, gain);
                        }
                    }
                } else {
                    destination.fill(0);
                }
            }
            RequestedStreamState::Idle
            | RequestedStreamState::ClosePlayOut
            | RequestedStreamState::CloseDiscard => {
                destination.fill(0);
                self.silent_frames_since_close_request
                    .fetch_add(u64::from(frame_count), Ordering::AcqRel);
            }
        }
    }
}

/// Scales little-endian float32 samples in place. Shared mode only: the system mixer would
/// convert and mix them anyway, so nothing bit-perfect is lost. Real-time safe.
fn apply_gain_to_float32_samples(float_bytes: &mut [u8], gain: f32) {
    for sample in float_bytes.as_chunks_mut::<4>().0 {
        *sample = (f32::from_le_bytes(*sample) * gain).to_le_bytes();
    }
}

/// The interleaved output buffer of a callback as bytes, or `None` if CoreAudio passed none.
///
/// # Safety
/// `buffer_list` must be null or point at a valid `AudioBufferList` whose first buffer, if its
/// data pointer is not null, holds `mDataByteSize` writable bytes for the callback's duration.
unsafe fn output_bytes<'callback>(
    buffer_list: *mut AudioBufferList,
) -> Option<&'callback mut [u8]> {
    // SAFETY: guaranteed by the caller; null is checked by `as_mut`.
    let buffer_list = unsafe { buffer_list.as_mut() }?;
    if buffer_list.mNumberBuffers == 0 {
        return None;
    }
    let buffer = &mut buffer_list.mBuffers[0];
    let data = NonNull::new(buffer.mData.cast::<u8>())?;
    // SAFETY: guaranteed by the caller: `mDataByteSize` writable bytes at `data`.
    Some(unsafe { std::slice::from_raw_parts_mut(data.as_ptr(), buffer.mDataByteSize as usize) })
}

/// The AUHAL render callback.
unsafe extern "C-unwind" fn render_callback(
    reference: NonNull<c_void>,
    _action_flags: NonNull<AudioUnitRenderActionFlags>,
    _time_stamp: NonNull<AudioTimeStamp>,
    _bus_number: u32,
    frame_count: u32,
    buffer_list: *mut AudioBufferList,
) -> i32 {
    // SAFETY: `reference` is the `CallbackContext` registered with the unit; it is freed only
    // after the unit is disposed, which ends every callback.
    let context = unsafe { reference.cast::<CallbackContext>().as_ref() };
    context.callbacks_in_progress.fetch_add(1, Ordering::AcqRel);
    let rendered = catch_unwind(AssertUnwindSafe(|| {
        context.render(frame_count, buffer_list);
    }));
    if rendered.is_err() {
        context.store_failure(OutputFailure::Platform {
            platform_code: RENDER_CALLBACK_PANICKED,
        });
    }
    context.callback_count.fetch_add(1, Ordering::Release);
    context.callbacks_in_progress.fetch_sub(1, Ordering::AcqRel);
    NO_ERROR
}

/// What the device listener checks. Deliberately leaked once registered (a few dozen bytes per
/// stream): CoreAudio documents no guarantee that a notification in flight has finished when
/// `AudioObjectRemovePropertyListener` returns, so freeing it could leave a dangling pointer.
struct ListenerState {
    state: Arc<StreamStateCell>,
    device: AudioObjectID,
    /// `Some(rate)` in exclusive mode: the hog owner and this rate must not change.
    exclusive_sample_rate_hz: Option<f64>,
}

/// Device property listener: marks the stream failed when the device dies or, in exclusive
/// mode, when another process takes it or changes its rate.
unsafe extern "C-unwind" fn device_listener(
    _object: AudioObjectID,
    _address_count: u32,
    _addresses: NonNull<AudioObjectPropertyAddress>,
    client_data: *mut c_void,
) -> i32 {
    // SAFETY: `client_data` is the leaked `ListenerState` registered with this listener; it is
    // never freed.
    let Some(listener) = (unsafe { client_data.cast::<ListenerState>().as_ref() }) else {
        return NO_ERROR;
    };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let is_takeover_intact = listener.exclusive_sample_rate_hz.is_none_or(|rate_hz| {
            ExclusiveTakeover::is_still_held(listener.device)
                && nominal_sample_rate(listener.device).is_ok_and(|now| (now - rate_hz).abs() < 1.0)
        });
        if !is_device_alive(listener.device) || !is_takeover_intact {
            listener
                .state
                .store_status(OutputStreamStatus::Failed(OutputFailure::DeviceInvalidated));
        }
    }));
    NO_ERROR
}

/// The properties the listener watches.
fn watched_properties(is_exclusive: bool) -> Vec<AudioObjectPropertyAddress> {
    let mut selectors = vec![kAudioDevicePropertyDeviceIsAlive];
    if is_exclusive {
        selectors.extend([
            kAudioDevicePropertyHogMode,
            kAudioDevicePropertyNominalSampleRate,
        ]);
    }
    selectors
        .into_iter()
        .map(|selector| property_address(selector, kAudioObjectPropertyScopeGlobal))
        .collect()
}

/// Frames of silence to write after the last audio frame before it has surely left the
/// speaker: the device's presentation latency converted to the client rate, plus the unit's
/// own latency, rounded up.
pub(crate) fn play_out_frame_count(
    device_presentation_latency_frames: u32,
    device_sample_rate_hz: f64,
    unit_latency_seconds: f64,
    client_sample_rate_hz: u32,
) -> u64 {
    if device_sample_rate_hz <= 0.0 {
        return 0;
    }
    let seconds = f64::from(device_presentation_latency_frames) / device_sample_rate_hz
        + unit_latency_seconds;
    (seconds.max(0.0) * f64::from(client_sample_rate_hz)).ceil() as u64
}

/// Detects a stopped IO thread: `Running` with no callback for `DEVICE_EVENT_TIMEOUT`.
#[derive(Debug)]
struct Watchdog {
    created_at: Instant,
    last_seen_callback_count: AtomicU64,
    last_progress_milliseconds: AtomicU64,
}

impl Watchdog {
    fn new() -> Watchdog {
        Watchdog {
            created_at: Instant::now(),
            last_seen_callback_count: AtomicU64::new(0),
            last_progress_milliseconds: AtomicU64::new(0),
        }
    }

    /// Records progress; `true` when the callback count has not moved for the timeout.
    fn has_timed_out(&self, callback_count: u64) -> bool {
        let now_milliseconds =
            u64::try_from(self.created_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        if self
            .last_seen_callback_count
            .swap(callback_count, Ordering::AcqRel)
            != callback_count
        {
            self.last_progress_milliseconds
                .store(now_milliseconds, Ordering::Release);
            return false;
        }
        let silent_for = now_milliseconds
            .saturating_sub(self.last_progress_milliseconds.load(Ordering::Acquire));
        let timeout = if callback_count == 0 {
            FIRST_CALLBACK_TIMEOUT
        } else {
            DEVICE_EVENT_TIMEOUT
        };
        u128::from(silent_for) > timeout.as_millis()
    }

    fn restart(&self) {
        let now_milliseconds =
            u64::try_from(self.created_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_progress_milliseconds
            .store(now_milliseconds, Ordering::Release);
    }
}

/// An open CoreAudio stream.
pub(crate) struct CoreAudioStream {
    negotiated: NegotiatedOutput,
    /// `None` once torn down.
    unit: Option<OutputUnit>,
    /// Owned through a raw pointer because the callback holds one too; freed in `teardown`.
    context: NonNull<CallbackContext>,
    takeover: Option<ExclusiveTakeover>,
    device: AudioObjectID,
    listener: NonNull<ListenerState>,
    watched_properties: Vec<AudioObjectPropertyAddress>,
    play_out_frames: u64,
    watchdog: Watchdog,
    is_torn_down: bool,
}

// SAFETY: the raw pointers refer to heap state that is either immutable or atomic from the
// engine's side (see `CallbackContext`'s `Sync` impl); the engine uses the stream from one
// thread at a time.
unsafe impl Send for CoreAudioStream {}

/// Opens the stream described by `plan`. On failure the render source is handed back.
pub(crate) fn open_coreaudio_stream(
    plan: StreamPlan,
    mut render_source: Box<dyn RenderSource>,
) -> Result<Box<dyn OutputStream>, (OutputError, Box<dyn RenderSource>)> {
    let render_format = plan.negotiated.render_format;
    let bytes_per_frame = render_format.bytes_per_frame();
    let state = Arc::new(StreamStateCell::new());
    let context = Box::new(CallbackContext {
        state: Arc::clone(&state),
        render_source: UnsafeCell::new(None),
        integer_scratch: UnsafeCell::new(
            vec![0u8; MAXIMUM_FRAMES_PER_SLICE as usize * bytes_per_frame].into_boxed_slice(),
        ),
        bytes_per_frame,
        is_detached: AtomicBool::new(false),
        callbacks_in_progress: AtomicU32::new(0),
        callback_count: AtomicU64::new(0),
        silent_frames_since_close_request: AtomicU64::new(0),
        shared_mode_gain_bits: AtomicU32::new(1.0_f32.to_bits()),
        is_shared_mode: plan.negotiated.access_mode == DeviceAccessMode::SharedThroughSystemMixer,
    });
    let context = NonNull::from(Box::leak(context));
    let unit = match configure_unit(&plan, context) {
        Ok(unit) => unit,
        Err(error) => {
            // SAFETY: the unit that could reference the context is already dropped (disposed);
            // nothing else holds the pointer.
            drop(unsafe { Box::from_raw(context.as_ptr()) });
            return Err((error, render_source));
        }
    };
    let play_out_frames = match play_out_frames_for(&plan, &unit) {
        Ok(frames) => frames,
        Err(error) => {
            drop(unit);
            // SAFETY: as above, the unit is disposed.
            drop(unsafe { Box::from_raw(context.as_ptr()) });
            return Err((error, render_source));
        }
    };
    render_source.begin_stream(RenderStreamDescription {
        render_format,
        access_mode: plan.negotiated.access_mode,
    });
    // SAFETY: the unit is configured but not started, so no callback runs yet.
    unsafe { *context.as_ref().render_source.get() = Some(render_source) };
    let is_exclusive = plan.negotiated.access_mode == DeviceAccessMode::Exclusive;
    let listener = NonNull::from(Box::leak(Box::new(ListenerState {
        state,
        device: plan.device,
        exclusive_sample_rate_hz: is_exclusive.then_some(f64::from(render_format.sample_rate_hz)),
    })));
    let watched_properties = watched_properties(is_exclusive);
    for address in &watched_properties {
        // SAFETY: valid device and address; the listener state is leaked, so it outlives any
        // notification. A failed registration only loses failure detection; it is not fatal.
        let _ = unsafe {
            AudioObjectAddPropertyListener(
                plan.device,
                NonNull::from(address),
                Some(device_listener),
                listener.as_ptr().cast::<c_void>(),
            )
        };
    }
    Ok(Box::new(CoreAudioStream {
        negotiated: plan.negotiated,
        unit: Some(unit),
        context,
        takeover: plan.takeover,
        device: plan.device,
        listener,
        watched_properties,
        play_out_frames,
        watchdog: Watchdog::new(),
        is_torn_down: false,
    }))
}

fn configure_unit(
    plan: &StreamPlan,
    context: NonNull<CallbackContext>,
) -> Result<OutputUnit, OutputError> {
    let render_format = plan.negotiated.render_format;
    let mut unit = OutputUnit::new(plan.device)?;
    let callback = AURenderCallbackStruct {
        inputProc: Some(render_callback),
        inputProcRefCon: context.as_ptr().cast::<c_void>(),
    };
    // SAFETY: the context outlives the unit: `teardown` (and every error path) drops the unit
    // before freeing the context.
    unsafe {
        unit.configure(
            &float32_client_format(render_format.sample_rate_hz, render_format.channel_count),
            MAXIMUM_FRAMES_PER_SLICE,
            &callback,
        )?;
    }
    Ok(unit)
}

fn play_out_frames_for(plan: &StreamPlan, unit: &OutputUnit) -> Result<u64, OutputError> {
    Ok(play_out_frame_count(
        super::devices::output_presentation_latency_frames(plan.device)?,
        nominal_sample_rate(plan.device)?,
        unit.latency_seconds()?,
        plan.negotiated.render_format.sample_rate_hz,
    ))
}

impl CoreAudioStream {
    fn context(&self) -> &CallbackContext {
        // SAFETY: the context lives until `teardown`, after which the stream is never used.
        unsafe { self.context.as_ref() }
    }

    /// Waits for the play-out of the queued audio. `true` when it completed in time.
    fn play_out(&self) -> bool {
        let context = self.context();
        if context.state.status() != OutputStreamStatus::Running {
            // Never started: CoreAudio pulls audio, nothing was queued yet.
            return context.state.status() == OutputStreamStatus::NotStarted;
        }
        context
            .silent_frames_since_close_request
            .store(0, Ordering::Release);
        context.state.request(RequestedStreamState::ClosePlayOut);
        let started_at = Instant::now();
        while started_at.elapsed() < QUEUED_AUDIO_PLAY_OUT_TIMEOUT {
            if context
                .silent_frames_since_close_request
                .load(Ordering::Acquire)
                >= self.play_out_frames
            {
                return true;
            }
            if matches!(context.state.status(), OutputStreamStatus::Failed(_)) {
                return false;
            }
            thread::sleep(CLOSE_POLL_INTERVAL);
        }
        false
    }

    /// Stops the unit, waits for in-flight callbacks, disposes the unit, then frees the context
    /// and hands the render source back. Releases the takeover last. Idempotent.
    fn teardown(&mut self) -> Option<Box<dyn RenderSource>> {
        if self.is_torn_down {
            return None;
        }
        self.is_torn_down = true;
        let context = self.context();
        context.state.request(RequestedStreamState::CloseDiscard);
        context.is_detached.store(true, Ordering::Release);
        if let Some(unit) = &self.unit {
            let _ = unit.stop();
        }
        let started_at = Instant::now();
        while context.callbacks_in_progress.load(Ordering::Acquire) != 0
            && started_at.elapsed() < CALLBACK_DRAIN_TIMEOUT
        {
            thread::sleep(CLOSE_POLL_INTERVAL);
        }
        // Disposing the unit ends every callback; only then is the context free to drop.
        self.unit = None;
        for address in &self.watched_properties {
            // SAFETY: same arguments as the registration; the listener state stays leaked.
            let _ = unsafe {
                AudioObjectRemovePropertyListener(
                    self.device,
                    NonNull::from(address),
                    Some(device_listener),
                    self.listener.as_ptr().cast::<c_void>(),
                )
            };
        }
        // SAFETY: the unit is disposed, so no callback can run; the context came from
        // `Box::leak` in `open_coreaudio_stream` and is freed exactly once here.
        let context = unsafe { Box::from_raw(self.context.as_ptr()) };
        self.takeover = None;
        context.render_source.into_inner()
    }
}

impl OutputStream for CoreAudioStream {
    fn negotiated(&self) -> &NegotiatedOutput {
        &self.negotiated
    }

    /// Reads `kAudioDevicePropertyDeviceIsAlive` now: the device listener may not have marked
    /// the stream failed yet when the default-device notification arrives (unplugged
    /// headphones). An unreadable property counts as gone.
    fn is_device_still_present(&self) -> bool {
        is_device_alive(self.device)
    }

    fn play(&mut self) -> Result<(), OutputError> {
        let context = self.context();
        context.state.request(RequestedStreamState::Run);
        self.watchdog.restart();
        let unit = self.unit.as_ref().ok_or(OutputError::DeviceInvalidated)?;
        unit.start()?;
        context.state.store_status(OutputStreamStatus::Running);
        Ok(())
    }

    fn set_shared_mode_volume(&mut self, linear_gain: f32) {
        self.context()
            .shared_mode_gain_bits
            .store(linear_gain.clamp(0.0, 1.0).to_bits(), Ordering::Release);
    }

    fn status(&self) -> OutputStreamStatus {
        let context = self.context();
        let status = context.state.status();
        if status == OutputStreamStatus::Running
            && self
                .watchdog
                .has_timed_out(context.callback_count.load(Ordering::Acquire))
        {
            context.store_failure(OutputFailure::DeviceTimedOut);
            return context.state.status();
        }
        status
    }

    fn close(mut self: Box<Self>, queued_audio: QueuedAudioOnClose) -> ClosedStream {
        let was_queued_audio_fully_played = match queued_audio {
            QueuedAudioOnClose::PlayOut => self.play_out(),
            QueuedAudioOnClose::Discard => false,
        };
        let render_source = self
            .teardown()
            .expect("close is the only teardown of a stream that was never torn down");
        ClosedStream {
            render_source,
            was_queued_audio_fully_played,
        }
    }
}

impl Drop for CoreAudioStream {
    /// Dropping without `close` equals `close(Discard)`.
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_out_covers_device_and_unit_latency_at_the_client_rate() {
        // 1024 device frames at 48 kHz = 21.33 ms, plus 1 ms of unit latency, at 96 kHz.
        assert_eq!(play_out_frame_count(1024, 48_000.0, 0.001, 96_000), 2144);
        assert_eq!(play_out_frame_count(512, 0.0, 0.0, 96_000), 0);
    }

    #[test]
    fn watchdog_times_out_only_without_progress() {
        let watchdog = Watchdog::new();
        assert!(!watchdog.has_timed_out(1));
        assert!(!watchdog.has_timed_out(1));
        watchdog
            .last_progress_milliseconds
            .store(0, Ordering::Release);
        let elapsed_past_timeout = DEVICE_EVENT_TIMEOUT + Duration::from_millis(10);
        let watchdog = Watchdog {
            created_at: Instant::now() - elapsed_past_timeout,
            last_seen_callback_count: AtomicU64::new(5),
            last_progress_milliseconds: AtomicU64::new(0),
        };
        assert!(watchdog.has_timed_out(5));
        assert!(!watchdog.has_timed_out(6));
    }

    #[test]
    fn gain_scales_every_float_sample() {
        let mut bytes: Vec<u8> = [1.0_f32, -0.5]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        apply_gain_to_float32_samples(&mut bytes, 0.5);
        let values: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        assert_eq!(values, vec![0.5, -0.25]);
    }

    #[test]
    fn watchdog_waits_longer_for_the_first_callback() {
        let watchdog = Watchdog {
            created_at: Instant::now() - (DEVICE_EVENT_TIMEOUT + Duration::from_millis(10)),
            last_seen_callback_count: AtomicU64::new(0),
            last_progress_milliseconds: AtomicU64::new(0),
        };
        assert!(!watchdog.has_timed_out(0));
    }
}
