//! One ALSA stream: the engine-side handle and its render thread.
//!
//! `open_stream` opens and configures the PCM, then moves it into the render thread, which
//! calls `begin_stream`, prefills the whole device buffer, reports the open result and loops
//! until the engine asks it to close or the device fails. Failures reach the engine only through
//! `status()`, which it polls (same rule as the other backends).
//!
//! Real-time rules inside the loop: no allocation (the byte buffer is allocated before it), no
//! lock, no logging. The thread blocks only in `snd_pcm_wait`, in `snd_pcm_writei` (blocking
//! mode, with room known to be free), or in a bounded park while the stream waits for `play`.
//!
//! Source: ALSA PCM interface, transfer and state machine
//! <https://www.alsa-project.org/alsa-doc/alsa-lib/pcm.html>.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use alsa::PCM;
use alsa::pcm::{IO, State};
use crossbeam_channel::{Receiver, Sender, bounded};
use pistolame_core::timing::{DEVICE_EVENT_TIMEOUT, QUEUED_AUDIO_PLAY_OUT_TIMEOUT};

use super::devices::{DeviceRoute, is_device_listed};
use super::errors::{EPIPE, ESTRPIPE, errno_of, output_error, output_failure};
use super::pcm_setup::ConfiguredPcm;
use crate::sample_gain::apply_gain_to_integer_samples;
use crate::stream_state::{RequestedStreamState, StreamStateCell};
use crate::{
    ClosedStream, DeviceAccessMode, DeviceSampleFormat, NegotiatedOutput, OpenStreamError,
    OutputError, OutputFailure, OutputStream, OutputStreamStatus, QueuedAudioOnClose, RenderSource,
    RenderStreamDescription,
};

/// Name of the render threads, visible in `top -H` and debuggers.
const RENDER_THREAD_NAME: &str = "pistolame-alsa-render";
/// While the stream waits for `play`, the thread re-reads the request at least this often (the
/// engine also wakes it at once).
const IDLE_CHECK_INTERVAL: Duration = Duration::from_millis(50);
/// Longest single wait for free room in the device buffer, so a close request or a stalled
/// device is noticed quickly.
const ROOM_WAIT_MILLISECONDS: u32 = 100;
/// Pause between two checks while the queued audio plays out.
const PLAY_OUT_CHECK_INTERVAL: Duration = Duration::from_millis(5);
/// Writes of the same data retried after an underrun or a suspend was recovered.
const MAXIMUM_WRITE_ATTEMPTS: u32 = 3;
/// Platform code reported when the render thread panicked or vanished (no `errno` applies).
const RENDER_THREAD_LOST_CODE: i32 = -1;

/// What `open_stream` decided, moved into the render thread.
pub(super) struct StreamPlan {
    pub(super) configured: ConfiguredPcm,
    pub(super) negotiated: NegotiatedOutput,
    pub(super) route: DeviceRoute,
}

/// Engine ↔ render thread state of one stream. Lock-free.
struct StreamControl {
    state: StreamStateCell,
    /// Shared-mode volume as `f32` bits (unity by default).
    shared_mode_gain_bits: AtomicU32,
    /// Underruns and suspends recovered while playing (AGENTS.md: count, never swallow).
    recovered_device_errors: AtomicU64,
}

impl StreamControl {
    fn new() -> StreamControl {
        StreamControl {
            state: StreamStateCell::new(),
            shared_mode_gain_bits: AtomicU32::new(1.0_f32.to_bits()),
            recovered_device_errors: AtomicU64::new(0),
        }
    }

    fn shared_mode_gain(&self) -> f32 {
        f32::from_bits(self.shared_mode_gain_bits.load(Ordering::Acquire))
    }

    fn count_recovered_device_error(&self) {
        self.recovered_device_errors.fetch_add(1, Ordering::Relaxed);
    }
}

/// What the render thread hands back when it ends.
struct RenderThreadExit {
    render_source: Box<dyn RenderSource>,
    was_queued_audio_fully_played: bool,
}

/// An open ALSA stream. Dropping it closes it with `Discard`.
pub(super) struct AlsaOutputStream {
    negotiated: NegotiatedOutput,
    route: DeviceRoute,
    control: Arc<StreamControl>,
    render_thread: Option<JoinHandle<Option<RenderThreadExit>>>,
}

/// Spawns the render thread and waits until it has prefilled the device, or failed. On
/// failure the render source comes back inside the error.
pub(super) fn open_alsa_stream(
    plan: StreamPlan,
    render_source: Box<dyn RenderSource>,
) -> Result<Box<dyn OutputStream>, OpenStreamError> {
    let control = Arc::new(StreamControl::new());
    let negotiated = plan.negotiated.clone();
    let route = plan.route.clone();
    let (open_result_sender, open_result_receiver) = bounded::<Result<(), OutputError>>(1);
    let (render_source_sender, render_source_receiver) = bounded::<Box<dyn RenderSource>>(1);
    let thread_control = Arc::clone(&control);
    let spawned = thread::Builder::new()
        .name(RENDER_THREAD_NAME.to_owned())
        .spawn(move || {
            run_render_thread(
                plan,
                &render_source_receiver,
                &thread_control,
                &open_result_sender,
            )
        });
    let render_thread = match spawned {
        Ok(handle) => handle,
        Err(spawn_error) => {
            return Err(OpenStreamError {
                error: OutputError::Platform {
                    operation: "spawn the ALSA render thread",
                    platform_code: spawn_error
                        .raw_os_error()
                        .unwrap_or(RENDER_THREAD_LOST_CODE),
                },
                render_source,
            });
        }
    };
    if let Err(unsent) = render_source_sender.send(render_source) {
        // The thread ended before receiving the source; it still belongs to us.
        let _ = render_thread.join();
        return Err(OpenStreamError {
            error: thread_lost_error(),
            render_source: unsent.into_inner(),
        });
    }
    match open_result_receiver.recv() {
        Ok(Ok(())) => Ok(Box::new(AlsaOutputStream {
            negotiated,
            route,
            control,
            render_thread: Some(render_thread),
        })),
        Ok(Err(error)) => Err(OpenStreamError {
            error,
            render_source: join_render_thread(render_thread).render_source,
        }),
        Err(_) => Err(OpenStreamError {
            error: thread_lost_error(),
            render_source: join_render_thread(render_thread).render_source,
        }),
    }
}

fn thread_lost_error() -> OutputError {
    OutputError::Platform {
        operation: "ALSA render thread ended unexpectedly",
        platform_code: RENDER_THREAD_LOST_CODE,
    }
}

/// Waits for the render thread and takes back what it returns.
fn join_render_thread(render_thread: JoinHandle<Option<RenderThreadExit>>) -> RenderThreadExit {
    render_thread.join().ok().flatten().expect(
        "the render thread receives its source before any fallible step and catches every panic",
    )
}

/// Entry point of the render thread. Every panic is caught so the render source always goes
/// back to the engine.
fn run_render_thread(
    plan: StreamPlan,
    render_source_receiver: &Receiver<Box<dyn RenderSource>>,
    control: &StreamControl,
    open_result: &Sender<Result<(), OutputError>>,
) -> Option<RenderThreadExit> {
    let mut render_source = render_source_receiver.recv().ok()?;
    let run_result = catch_unwind(AssertUnwindSafe(|| {
        run_stream(&plan, render_source.as_mut(), control, open_result)
    }));
    let was_queued_audio_fully_played = run_result.unwrap_or_else(|_| {
        control
            .state
            .store_status(OutputStreamStatus::Failed(OutputFailure::Platform {
                platform_code: RENDER_THREAD_LOST_CODE,
            }));
        let _ = open_result.try_send(Err(OutputError::Platform {
            operation: "ALSA render thread (panic)",
            platform_code: RENDER_THREAD_LOST_CODE,
        }));
        false
    });
    // Releases the device for other programs (the PCM itself closes when `plan` drops).
    let _ = plan.configured.pcm.drop();
    Some(RenderThreadExit {
        render_source,
        was_queued_audio_fully_played,
    })
}

/// Prefills, reports, runs the loop. Returns `was_queued_audio_fully_played`.
fn run_stream(
    plan: &StreamPlan,
    render_source: &mut dyn RenderSource,
    control: &StreamControl,
    open_result: &Sender<Result<(), OutputError>>,
) -> bool {
    let configured = &plan.configured;
    let format = configured.format;
    render_source.begin_stream(RenderStreamDescription {
        render_format: format,
        access_mode: plan.negotiated.access_mode,
    });
    // The only allocation of the stream, before the real-time loop: one device buffer.
    let mut bytes = vec![0u8; configured.buffer_frame_count as usize * format.bytes_per_frame()];
    let writer = DeviceWriter {
        pcm: &configured.pcm,
        io: configured.pcm.io_bytes(),
        control,
        format,
        is_shared: plan.negotiated.access_mode == DeviceAccessMode::SharedThroughSystemMixer,
    };
    // Prefill the whole buffer; the PCM starts only on `play` (start threshold = boundary).
    if let Err(error) = writer.render_and_write(render_source, &mut bytes) {
        // `render` may already have consumed frames that never played: the engine treats the
        // next open as a discontinuity (contract of `OpenStreamError`).
        let _ = open_result.try_send(Err(output_error("snd_pcm_writei", error)));
        return false;
    }
    if open_result.try_send(Ok(())).is_err() {
        // `open_stream` is gone: nobody will ever close this stream.
        return false;
    }
    let exit = RenderLoop {
        writer: &writer,
        period_frame_count: configured.period_frame_count as usize,
    }
    .run(render_source, &mut bytes);
    exit == LoopExit::QueuedAudioPlayedOut
}

/// Fills byte buffers from the render source and writes them to the PCM.
struct DeviceWriter<'stream> {
    pcm: &'stream PCM,
    io: IO<'stream, u8>,
    control: &'stream StreamControl,
    format: DeviceSampleFormat,
    is_shared: bool,
}

impl DeviceWriter<'_> {
    /// Renders `bytes` (a whole number of frames), applies the shared-mode volume, and writes
    /// it all. After an underrun or a suspend, ALSA is recovered and the same data is written
    /// again, so no rendered frame is lost.
    fn render_and_write(
        &self,
        render_source: &mut dyn RenderSource,
        bytes: &mut [u8],
    ) -> Result<(), alsa::Error> {
        render_source.render(bytes);
        let gain = self.control.shared_mode_gain();
        if self.is_shared && gain < 1.0 {
            apply_gain_to_integer_samples(bytes, &self.format, gain);
        }
        let bytes_per_frame = self.format.bytes_per_frame();
        let mut remaining: &[u8] = bytes;
        let mut failed_attempts = 0;
        while !remaining.is_empty() {
            match self.io.writei(remaining) {
                Ok(frames_written) => {
                    remaining = remaining
                        .get(frames_written * bytes_per_frame..)
                        .unwrap_or_default();
                }
                Err(error) => {
                    failed_attempts += 1;
                    if failed_attempts >= MAXIMUM_WRITE_ATTEMPTS {
                        return Err(error);
                    }
                    self.recover(error)?;
                }
            }
        }
        Ok(())
    }

    /// Recovers from an underrun (`EPIPE`) or a suspend (`ESTRPIPE`); the PCM is then
    /// prepared again and must be restarted once refilled. Any other error is returned.
    fn recover(&self, error: alsa::Error) -> Result<(), alsa::Error> {
        let errno = errno_of(&error);
        if errno != EPIPE && errno != ESTRPIPE {
            return Err(error);
        }
        self.pcm.try_recover(error, true)?;
        self.control.count_recovered_device_error();
        Ok(())
    }
}

/// Why the loop ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoopExit {
    QueuedAudioPlayedOut,
    PlayOutTimedOut,
    Discarded,
    Failed,
}

struct RenderLoop<'stream> {
    writer: &'stream DeviceWriter<'stream>,
    period_frame_count: usize,
}

impl RenderLoop<'_> {
    fn run(&self, render_source: &mut dyn RenderSource, bytes: &mut [u8]) -> LoopExit {
        let pcm = self.writer.pcm;
        let control = self.writer.control;
        let mut is_started = false;
        let mut last_progress = Instant::now();
        loop {
            match control.state.requested_state() {
                RequestedStreamState::CloseDiscard => return LoopExit::Discarded,
                RequestedStreamState::ClosePlayOut => {
                    // A stream never started must start, or its prefilled audio never plays.
                    if !is_started && let Err(error) = pcm.start() {
                        return self.fail_with(&error);
                    }
                    return self.play_out();
                }
                RequestedStreamState::Idle => {
                    thread::park_timeout(IDLE_CHECK_INTERVAL);
                    continue;
                }
                RequestedStreamState::Run if !is_started => {
                    if let Err(error) = pcm.start() {
                        return self.fail_with(&error);
                    }
                    is_started = true;
                    last_progress = Instant::now();
                    control.state.store_status(OutputStreamStatus::Running);
                }
                RequestedStreamState::Run => {}
            }
            match self.feed_once(render_source, bytes) {
                Ok(true) => last_progress = Instant::now(),
                Ok(false) if last_progress.elapsed() > DEVICE_EVENT_TIMEOUT => {
                    control
                        .state
                        .store_status(OutputStreamStatus::Failed(OutputFailure::DeviceTimedOut));
                    return LoopExit::Failed;
                }
                Ok(false) => {}
                Err(error) => return self.fail_with(&error),
            }
        }
    }

    /// Writes one batch when at least a period of room is free (or the PCM was recovered and
    /// waits to be refilled), otherwise waits for room. `Ok(true)` when audio was written.
    fn feed_once(
        &self,
        render_source: &mut dyn RenderSource,
        bytes: &mut [u8],
    ) -> Result<bool, alsa::Error> {
        let pcm = self.writer.pcm;
        let bytes_per_frame = self.writer.format.bytes_per_frame();
        let free_frames = match pcm.avail_update() {
            Ok(frames) => usize::try_from(frames).unwrap_or(0),
            Err(error) => {
                self.writer.recover(error)?;
                bytes.len() / bytes_per_frame
            }
        };
        let is_waiting_for_restart = pcm.state() == State::Prepared;
        if free_frames < self.period_frame_count && !is_waiting_for_restart {
            if let Err(error) = pcm.wait(Some(ROOM_WAIT_MILLISECONDS)) {
                self.writer.recover(error)?;
            }
            return Ok(false);
        }
        let frame_count = free_frames.min(bytes.len() / bytes_per_frame);
        let batch = &mut bytes[..frame_count * bytes_per_frame];
        self.writer.render_and_write(render_source, batch)?;
        if pcm.state() == State::Prepared {
            // Refilled after a recovery: play again.
            pcm.start()?;
        }
        Ok(true)
    }

    /// Lets the device play every queued frame, then stops (`PlayOut`). Nothing more is
    /// written: when the buffer runs dry ALSA stops the PCM (`XRUN`), so everything played.
    fn play_out(&self) -> LoopExit {
        let pcm = self.writer.pcm;
        let began_at = Instant::now();
        loop {
            let is_drained = match pcm.state() {
                State::XRun | State::Setup | State::Prepared | State::Open => true,
                State::Running | State::Draining => pcm.delay().map_or(true, |delay| delay <= 0),
                State::Paused | State::Suspended | State::Disconnected => false,
            };
            if is_drained {
                return LoopExit::QueuedAudioPlayedOut;
            }
            let elapsed = began_at.elapsed();
            if elapsed >= QUEUED_AUDIO_PLAY_OUT_TIMEOUT {
                return LoopExit::PlayOutTimedOut;
            }
            thread::sleep(PLAY_OUT_CHECK_INTERVAL.min(QUEUED_AUDIO_PLAY_OUT_TIMEOUT - elapsed));
        }
    }

    fn fail_with(&self, error: &alsa::Error) -> LoopExit {
        self.writer
            .control
            .state
            .store_status(OutputStreamStatus::Failed(output_failure(error)));
        LoopExit::Failed
    }
}

impl AlsaOutputStream {
    /// Asks the render thread to end in the given way and waits for it (the play-out is
    /// bounded by `QUEUED_AUDIO_PLAY_OUT_TIMEOUT` inside the thread).
    fn finish_render_thread(
        &mut self,
        queued_audio: QueuedAudioOnClose,
    ) -> Option<RenderThreadExit> {
        let render_thread = self.render_thread.take()?;
        self.control.state.request(match queued_audio {
            QueuedAudioOnClose::PlayOut => RequestedStreamState::ClosePlayOut,
            QueuedAudioOnClose::Discard => RequestedStreamState::CloseDiscard,
        });
        render_thread.thread().unpark();
        Some(join_render_thread(render_thread))
    }
}

impl OutputStream for AlsaOutputStream {
    fn negotiated(&self) -> &NegotiatedOutput {
        &self.negotiated
    }

    fn play(&mut self) -> Result<(), OutputError> {
        if let OutputStreamStatus::Failed(failure) = self.control.state.status() {
            return Err(match failure {
                OutputFailure::DeviceInvalidated => OutputError::DeviceInvalidated,
                OutputFailure::DeviceTimedOut => OutputError::DeviceTimedOut,
                OutputFailure::Platform { platform_code } => OutputError::Platform {
                    operation: "ALSA render thread",
                    platform_code,
                },
            });
        }
        self.control.state.request(RequestedStreamState::Run);
        if let Some(render_thread) = &self.render_thread {
            render_thread.thread().unpark();
        }
        Ok(())
    }

    fn status(&self) -> OutputStreamStatus {
        self.control.state.status()
    }

    /// Applied by the render thread to shared streams only; exclusive streams stay bit-perfect.
    fn set_shared_mode_volume(&mut self, linear_gain: f32) {
        self.control
            .shared_mode_gain_bits
            .store(linear_gain.clamp(0.0, 1.0).to_bits(), Ordering::Release);
    }

    /// The sound server is always there; a card must still be listed.
    fn is_device_still_present(&self) -> bool {
        match self.route {
            DeviceRoute::SystemDefault => true,
            DeviceRoute::Hardware { .. } => is_device_listed(&self.negotiated.device.id),
        }
    }

    fn close(mut self: Box<Self>, queued_audio: QueuedAudioOnClose) -> ClosedStream {
        let exit = self
            .finish_render_thread(queued_audio)
            .expect("the render thread handle is only taken by close or drop, and close consumes the stream");
        ClosedStream {
            render_source: exit.render_source,
            was_queued_audio_fully_played: exit.was_queued_audio_fully_played,
        }
    }
}

impl Drop for AlsaOutputStream {
    fn drop(&mut self) {
        let _ = self.finish_render_thread(QueuedAudioOnClose::Discard);
    }
}
