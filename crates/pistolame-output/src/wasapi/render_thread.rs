//! The real-time render thread of one stream (plan B5.3, B5.6, B6).
//!
//! The thread owns every COM object of its stream: it initializes COM, opens the device,
//! calls `begin_stream`, prefills, registers with MMCSS, reports the open result, then runs the
//! event loop until the engine asks it to close or the device fails.
//!
//! Failures are reported only through the status atomic, which the engine polls with
//! `OutputStream::status()`; there is no wake-up channel to the engine (decided for M1).
//!
//! Real-time rules inside the loop: no allocation, no lock, no I/O, no logging. The only
//! blocking call is the wait on the two events. Each wake-up does bounded work: at most one
//! `GetBuffer`/`render`/`ReleaseBuffer` of one device buffer, plus `GetCurrentPadding` during a
//! play-out. `windows::core::Error` may allocate when a call fails; that only happens on the
//! exit path.
//!
//! Sources: Microsoft's exclusive-mode event-driven sample (prefill before `Start`, 2 s wait,
//! "Pro Audio") <https://learn.microsoft.com/en-us/windows/win32/coreaudio/exclusive-mode-streams>;
//! `GetBuffer` <https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudiorenderclient-getbuffer>;
//! `ReleaseBuffer` and AUDCLNT_BUFFERFLAGS_SILENT
//! <https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudiorenderclient-releasebuffer>.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use pistolame_core::timing::{DEVICE_EVENT_TIMEOUT, QUEUED_AUDIO_PLAY_OUT_TIMEOUT};
use windows::Win32::Foundation::{E_UNEXPECTED, WAIT_EVENT, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{AUDCLNT_BUFFERFLAGS_SILENT, IAudioClient, IAudioRenderClient};
use windows::Win32::System::Threading::WaitForMultipleObjects;
use windows::core::HRESULT;

use super::com_apartment::ComApartment;
use super::hresult_mapping::{NameWasapiCall, WasapiCallError, output_failure_from_hresult};
use super::mmcss::MmcssRegistration;
use super::stream_control::{RequestedStreamState, StreamControl, WasapiDiagnosticCounters};
use super::stream_initialization::{InitializedStream, initialize_stream};
use crate::sample_gain::apply_gain_to_integer_samples;
use crate::{
    DeviceAccessMode, DeviceSampleFormat, NegotiatedOutput, OpenStreamRequest, OutputError,
    OutputFailure, OutputStreamStatus, RenderOutcome, RenderSource, RenderStreamDescription,
};

/// Index of the control event in the wait array; it comes first so a close request wins over
/// a buffer event when both are signaled.
const CONTROL_EVENT_WAIT_INDEX: u32 = 0;
/// Index of the WASAPI buffer event in the wait array.
const BUFFER_EVENT_WAIT_INDEX: u32 = 1;

/// What the open sequence reports to `open_stream`.
pub(crate) type OpenResult = Result<NegotiatedOutput, OutputError>;

/// What the render thread hands back when it ends.
pub(crate) struct RenderThreadExit {
    /// The render source, for the next stream.
    pub(crate) render_source: Box<dyn RenderSource>,
    /// See `ClosedStream::was_queued_audio_fully_played`.
    pub(crate) was_queued_audio_fully_played: bool,
}

/// Everything the render thread needs, moved into it at spawn.
pub(crate) struct RenderThreadContext {
    /// The stream to open.
    pub(crate) request: OpenStreamRequest,
    /// Shared with the engine-side handle.
    pub(crate) control: Arc<StreamControl>,
    /// Shared with the `WasapiOutput`.
    pub(crate) diagnostics: Arc<WasapiDiagnosticCounters>,
    /// Receives the open result exactly once.
    pub(crate) open_result_sender: Sender<OpenResult>,
}

/// Entry point of the render thread. The render source arrives through a channel after the
/// spawn succeeded, so a failed spawn never loses it. Every panic is caught so the render source
/// always goes back to the engine.
pub(crate) fn run_render_thread(
    context: RenderThreadContext,
    render_source_receiver: Receiver<Box<dyn RenderSource>>,
) -> Option<RenderThreadExit> {
    let mut render_source = render_source_receiver.recv().ok()?;
    let run_result = catch_unwind(AssertUnwindSafe(|| {
        run_stream(&context, render_source.as_mut())
    }));
    // After a panic the default panic hook has already written its message to stderr from
    // this real-time thread. That is blocking I/O, but it happens only on this exit path, after
    // which the stream is dead; the normal loop never logs.
    let was_queued_audio_fully_played = run_result.unwrap_or_else(|_| {
        let failure_code = E_UNEXPECTED.0;
        context
            .control
            .store_status(OutputStreamStatus::Failed(OutputFailure::Platform {
                platform_code: failure_code,
            }));
        let _ = context
            .open_result_sender
            .try_send(Err(OutputError::Platform {
                operation: "WASAPI render thread (panic)",
                platform_code: failure_code,
            }));
        false
    });
    Some(RenderThreadExit {
        render_source,
        was_queued_audio_fully_played,
    })
}

/// Opens, prefills, reports, runs the loop and releases the device. Returns
/// `was_queued_audio_fully_played`.
fn run_stream(context: &RenderThreadContext, render_source: &mut dyn RenderSource) -> bool {
    let report_failure = |error: OutputError| {
        let _ = context.open_result_sender.try_send(Err(error));
        false
    };
    // Declared first so it is dropped last, after every COM object of the stream.
    let _com_apartment = match ComApartment::initialize_multithreaded() {
        Ok(apartment) => apartment,
        Err(error) => return report_failure(error),
    };
    let stream = match initialize_stream(&context.request) {
        Ok(stream) => stream,
        Err(error) => return report_failure(error),
    };
    let render_format = stream.negotiated.render_format;
    render_source.begin_stream(RenderStreamDescription {
        render_format,
        access_mode: stream.negotiated.access_mode,
    });
    let bytes_per_frame = render_format.bytes_per_frame();
    // Prefill the whole buffer before Start, as in Microsoft's exclusive-mode sample.
    let prefill = write_rendered_frames(
        &stream.render_client,
        render_source,
        stream.negotiated.buffer_frame_count,
        bytes_per_frame,
        shared_mode_gain(&stream, &context.control),
    );
    if let Err(error) = prefill {
        // `begin_stream` has run and `render` may have consumed frames into a device buffer
        // that is never played. The render source goes back to the engine through
        // `OpenStreamError`, but its position is past those frames: the engine (T9) must treat
        // it as a discontinuity (flush and reposition, or mark level 1 not verifiable), never
        // resume from it as if nothing had been rendered.
        return report_failure(error.to_output_error());
    }
    let mmcss_registration = MmcssRegistration::register_pro_audio_thread();
    if mmcss_registration.is_none() {
        context.diagnostics.count_mmcss_registration_failure();
    }
    if context
        .open_result_sender
        .try_send(Ok(stream.negotiated.clone()))
        .is_err()
    {
        // `open_stream` is gone: nobody will ever close this stream.
        stop_and_reset(&stream.audio_client, &context.diagnostics);
        return false;
    }
    let loop_exit = RenderLoop {
        stream: &stream,
        control: &context.control,
        bytes_per_frame,
    }
    .run(render_source);
    stop_and_reset(&stream.audio_client, &context.diagnostics);
    drop(mmcss_registration);
    loop_exit == LoopExit::QueuedAudioPlayedOut
}

/// Stops the stream and discards what is queued. Failures are counted, not returned (the
/// contract's `close` does not report them).
fn stop_and_reset(audio_client: &IAudioClient, diagnostics: &WasapiDiagnosticCounters) {
    // SAFETY: valid client owned by this thread; Stop on a stopped stream returns S_FALSE.
    if unsafe { audio_client.Stop() }.is_err() {
        diagnostics.count_stream_close_error();
    }
    // SAFETY: valid client, stopped just above.
    if unsafe { audio_client.Reset() }.is_err() {
        diagnostics.count_stream_close_error();
    }
}

/// The volume to apply to this stream's samples: `Some` only in shared mode below 0 dB.
/// Exclusive streams are never touched (bit-perfect; the DAC sets the level).
fn shared_mode_gain(
    stream: &InitializedStream,
    control: &StreamControl,
) -> Option<(f32, DeviceSampleFormat)> {
    if stream.negotiated.access_mode != DeviceAccessMode::SharedThroughSystemMixer {
        return None;
    }
    let gain = control.shared_mode_gain();
    (gain < 1.0).then_some((gain, stream.negotiated.render_format))
}

/// Asks the render source to fill `frame_count` frames of the device buffer, then applies the
/// shared-mode volume if any. Does not allocate.
fn write_rendered_frames(
    render_client: &IAudioRenderClient,
    render_source: &mut dyn RenderSource,
    frame_count: u32,
    bytes_per_frame: usize,
    shared_gain: Option<(f32, DeviceSampleFormat)>,
) -> Result<(), WasapiCallError> {
    if frame_count == 0 {
        return Ok(());
    }
    // SAFETY: valid render client; `frame_count` is never more than the buffer size (whole
    // buffer in exclusive mode, free space in shared mode).
    let buffer_start = unsafe { render_client.GetBuffer(frame_count) }
        .for_operation("IAudioRenderClient::GetBuffer")?;
    let byte_count = frame_count as usize * bytes_per_frame;
    // SAFETY: GetBuffer succeeded, so `buffer_start` points at `frame_count` frames of
    // `nBlockAlign` = `bytes_per_frame` bytes each, writable and owned by this thread alone
    // until ReleaseBuffer below; the slice does not outlive this function.
    let destination = unsafe { std::slice::from_raw_parts_mut(buffer_start, byte_count) };
    let outcome = render_source.render(destination);
    if let Some((gain, format)) = shared_gain {
        apply_gain_to_integer_samples(destination, &format, gain);
    }
    // The silent flag only when the whole buffer is silence (B5.6); a full buffer of audio
    // skips the scan.
    let is_whole_buffer_silent =
        outcome != RenderOutcome::Filled && destination.iter().all(|byte| *byte == 0);
    let flags = if is_whole_buffer_silent {
        AUDCLNT_BUFFERFLAGS_SILENT.0 as u32
    } else {
        0
    };
    // SAFETY: releases exactly the frames obtained above; `destination` is no longer used.
    unsafe { render_client.ReleaseBuffer(frame_count, flags) }
        .for_operation("IAudioRenderClient::ReleaseBuffer")
}

/// Writes `frame_count` frames of silence: zero bytes (exclusive mode has no audio engine to
/// honor the flag) and AUDCLNT_BUFFERFLAGS_SILENT.
fn write_silent_frames(
    render_client: &IAudioRenderClient,
    frame_count: u32,
    bytes_per_frame: usize,
) -> Result<(), WasapiCallError> {
    if frame_count == 0 {
        return Ok(());
    }
    // SAFETY: as in `write_rendered_frames`.
    let buffer_start = unsafe { render_client.GetBuffer(frame_count) }
        .for_operation("IAudioRenderClient::GetBuffer")?;
    // SAFETY: GetBuffer succeeded for `frame_count * bytes_per_frame` writable bytes owned by
    // this thread until ReleaseBuffer.
    unsafe { std::ptr::write_bytes(buffer_start, 0, frame_count as usize * bytes_per_frame) };
    // SAFETY: releases exactly the frames obtained above.
    unsafe { render_client.ReleaseBuffer(frame_count, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) }
        .for_operation("IAudioRenderClient::ReleaseBuffer")
}

/// Where the stream is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamPhase {
    /// Prefilled, `Start` not called.
    NotStarted,
    /// Rendering audio from the render source.
    Running,
    /// Writing silence until the queued audio has played (close with `PlayOut`).
    PlayingOut {
        began_at: Instant,
        silent_frames_written: u64,
    },
}

/// What a control request asks the loop to do in the current phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlAction {
    /// Nothing to do (repeated or meaningless request).
    Ignore,
    /// Call `Start` and render.
    Start,
    /// Stop feeding and play out the queued audio. A stream that never started must start
    /// first, otherwise its prefilled audio would never play.
    BeginPlayOut { must_start_first: bool },
    /// Leave at once, discarding what is queued.
    Discard,
}

fn control_action(phase: StreamPhase, request: RequestedStreamState) -> ControlAction {
    match (request, phase) {
        (RequestedStreamState::CloseDiscard, _) => ControlAction::Discard,
        (RequestedStreamState::Run, StreamPhase::NotStarted) => ControlAction::Start,
        (RequestedStreamState::ClosePlayOut, StreamPhase::NotStarted) => {
            ControlAction::BeginPlayOut {
                must_start_first: true,
            }
        }
        (RequestedStreamState::ClosePlayOut, StreamPhase::Running) => ControlAction::BeginPlayOut {
            must_start_first: false,
        },
        _ => ControlAction::Ignore,
    }
}

/// How long to wait for the next event: the device timeout, or what is left of the play-out
/// budget.
fn wait_timeout(phase: StreamPhase, now: Instant) -> Duration {
    match phase {
        StreamPhase::PlayingOut { began_at, .. } => {
            QUEUED_AUDIO_PLAY_OUT_TIMEOUT.saturating_sub(now.saturating_duration_since(began_at))
        }
        StreamPhase::NotStarted | StreamPhase::Running => DEVICE_EVENT_TIMEOUT,
    }
}

fn duration_in_wait_milliseconds(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

/// Why the loop ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoopExit {
    /// Close with `PlayOut` completed.
    QueuedAudioPlayedOut,
    /// Close with `PlayOut` ran out of time.
    PlayOutTimedOut,
    /// Close with `Discard`.
    Discarded,
    /// The device failed; the status holds the failure.
    Failed,
}

struct RenderLoop<'stream> {
    stream: &'stream InitializedStream,
    control: &'stream StreamControl,
    bytes_per_frame: usize,
}

impl RenderLoop<'_> {
    fn run(&self, render_source: &mut dyn RenderSource) -> LoopExit {
        let wait_handles = [
            self.control.control_event().raw_handle(),
            self.stream.buffer_event.raw_handle(),
        ];
        let mut phase = StreamPhase::NotStarted;
        loop {
            let timeout_milliseconds =
                duration_in_wait_milliseconds(wait_timeout(phase, Instant::now()));
            // SAFETY: both handles are live events owned by `self.control` and `self.stream`,
            // which outlive the loop.
            let wait_result =
                unsafe { WaitForMultipleObjects(&wait_handles, false, timeout_milliseconds) };
            let step = self.handle_wake_up(wait_result, phase, render_source);
            match step {
                Ok(next_phase) => phase = next_phase,
                Err(exit) => return exit,
            }
            if let StreamPhase::PlayingOut { began_at, .. } = phase
                && began_at.elapsed() > QUEUED_AUDIO_PLAY_OUT_TIMEOUT
            {
                return LoopExit::PlayOutTimedOut;
            }
        }
    }

    /// One wake-up. `Ok` carries the next phase, `Err` ends the loop.
    ///
    /// The requested state is re-read on every wake-up, not only when the control event fires.
    /// A missed `SetEvent` therefore cannot hang the stream: a running stream sees the request
    /// at its next buffer event, a stream that has not started at its next 2 s timeout, and a
    /// play-out ends on its own budget. Reading the atomic is one load; `control_action` is
    /// idempotent, so re-reading an already applied request does nothing.
    fn handle_wake_up(
        &self,
        wait_result: WAIT_EVENT,
        phase_before_wake_up: StreamPhase,
        render_source: &mut dyn RenderSource,
    ) -> Result<StreamPhase, LoopExit> {
        let phase = self.on_control_request(phase_before_wake_up)?;
        if wait_result == WAIT_OBJECT_0 {
            debug_assert_eq!(wait_result.0, WAIT_OBJECT_0.0 + CONTROL_EVENT_WAIT_INDEX);
            return Ok(phase);
        }
        if wait_result.0 == WAIT_OBJECT_0.0 + BUFFER_EVENT_WAIT_INDEX {
            return self.on_buffer_ready(phase, render_source);
        }
        if wait_result == WAIT_TIMEOUT {
            // Judged on the phase the wait was made in: a stream that was not started has no
            // buffer events to miss, even if the request read just above started it.
            return match phase_before_wake_up {
                StreamPhase::NotStarted => Ok(phase),
                StreamPhase::Running => Err(self.fail(OutputFailure::DeviceTimedOut)),
                StreamPhase::PlayingOut { .. } => Err(LoopExit::PlayOutTimedOut),
            };
        }
        Err(self.fail(output_failure_from_hresult(HRESULT::from_thread())))
    }

    fn on_control_request(&self, phase: StreamPhase) -> Result<StreamPhase, LoopExit> {
        match control_action(phase, self.control.requested_state()) {
            ControlAction::Ignore => Ok(phase),
            ControlAction::Discard => Err(LoopExit::Discarded),
            ControlAction::Start => {
                self.start()?;
                Ok(StreamPhase::Running)
            }
            ControlAction::BeginPlayOut { must_start_first } => {
                if must_start_first {
                    self.start()?;
                }
                Ok(StreamPhase::PlayingOut {
                    began_at: Instant::now(),
                    silent_frames_written: 0,
                })
            }
        }
    }

    fn start(&self) -> Result<(), LoopExit> {
        // SAFETY: valid initialized client owned by this thread, prefilled before this call.
        let started = unsafe { self.stream.audio_client.Start() };
        match started {
            Ok(()) => {
                self.control.store_status(OutputStreamStatus::Running);
                Ok(())
            }
            Err(error) => Err(self.fail(output_failure_from_hresult(error.code()))),
        }
    }

    fn on_buffer_ready(
        &self,
        phase: StreamPhase,
        render_source: &mut dyn RenderSource,
    ) -> Result<StreamPhase, LoopExit> {
        let frame_count = self
            .frames_to_write()
            .map_err(|error| self.fail_call(error))?;
        match phase {
            StreamPhase::NotStarted => Ok(phase),
            StreamPhase::Running => {
                write_rendered_frames(
                    &self.stream.render_client,
                    render_source,
                    frame_count,
                    self.bytes_per_frame,
                    shared_mode_gain(self.stream, self.control),
                )
                .map_err(|error| self.fail_call(error))?;
                Ok(phase)
            }
            StreamPhase::PlayingOut {
                began_at,
                silent_frames_written,
            } => self.continue_play_out(frame_count, began_at, silent_frames_written),
        }
    }

    fn continue_play_out(
        &self,
        frame_count: u32,
        began_at: Instant,
        silent_frames_written: u64,
    ) -> Result<StreamPhase, LoopExit> {
        write_silent_frames(
            &self.stream.render_client,
            frame_count,
            self.bytes_per_frame,
        )
        .map_err(|error| self.fail_call(error))?;
        let silent_frames_written = silent_frames_written + u64::from(frame_count);
        let current_padding_frames = self
            .current_padding_frames()
            .map_err(|error| self.fail_call(error))?;
        if self.stream.fill_rule.is_queued_audio_played_out(
            self.stream.negotiated.buffer_frame_count,
            current_padding_frames,
            silent_frames_written,
        ) {
            return Err(LoopExit::QueuedAudioPlayedOut);
        }
        Ok(StreamPhase::PlayingOut {
            began_at,
            silent_frames_written,
        })
    }

    fn frames_to_write(&self) -> Result<u32, WasapiCallError> {
        let fill_rule = self.stream.fill_rule;
        let current_padding_frames = if fill_rule.needs_current_padding() {
            self.current_padding_frames()?
        } else {
            0
        };
        Ok(fill_rule.frames_to_write(
            self.stream.negotiated.buffer_frame_count,
            current_padding_frames,
        ))
    }

    fn current_padding_frames(&self) -> Result<u32, WasapiCallError> {
        // SAFETY: valid initialized client owned by this thread.
        unsafe { self.stream.audio_client.GetCurrentPadding() }
            .for_operation("IAudioClient::GetCurrentPadding")
    }

    fn fail_call(&self, error: WasapiCallError) -> LoopExit {
        self.fail(output_failure_from_hresult(error.hresult))
    }

    /// Publishes the failure in the status atomic; the engine sees it through `status()`.
    fn fail(&self, failure: OutputFailure) -> LoopExit {
        self.control
            .store_status(OutputStreamStatus::Failed(failure));
        LoopExit::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing_out() -> StreamPhase {
        StreamPhase::PlayingOut {
            began_at: Instant::now(),
            silent_frames_written: 0,
        }
    }

    #[test]
    fn run_starts_only_a_stream_that_has_not_started() {
        assert_eq!(
            control_action(StreamPhase::NotStarted, RequestedStreamState::Run),
            ControlAction::Start
        );
        assert_eq!(
            control_action(StreamPhase::Running, RequestedStreamState::Run),
            ControlAction::Ignore
        );
        assert_eq!(
            control_action(playing_out(), RequestedStreamState::Run),
            ControlAction::Ignore
        );
    }

    #[test]
    fn play_out_starts_a_prefilled_stream_first() {
        assert_eq!(
            control_action(StreamPhase::NotStarted, RequestedStreamState::ClosePlayOut),
            ControlAction::BeginPlayOut {
                must_start_first: true
            }
        );
        assert_eq!(
            control_action(StreamPhase::Running, RequestedStreamState::ClosePlayOut),
            ControlAction::BeginPlayOut {
                must_start_first: false
            }
        );
        assert_eq!(
            control_action(playing_out(), RequestedStreamState::ClosePlayOut),
            ControlAction::Ignore
        );
    }

    #[test]
    fn discard_wins_in_every_phase() {
        for phase in [StreamPhase::NotStarted, StreamPhase::Running, playing_out()] {
            assert_eq!(
                control_action(phase, RequestedStreamState::CloseDiscard),
                ControlAction::Discard
            );
        }
        assert_eq!(
            control_action(StreamPhase::Running, RequestedStreamState::Idle),
            ControlAction::Ignore
        );
    }

    #[test]
    fn wait_uses_the_device_timeout_until_a_play_out_begins() {
        let now = Instant::now();
        assert_eq!(
            wait_timeout(StreamPhase::Running, now),
            DEVICE_EVENT_TIMEOUT
        );
        assert_eq!(
            duration_in_wait_milliseconds(wait_timeout(StreamPhase::NotStarted, now)),
            2_000
        );
        let began_at = now;
        let later = began_at + Duration::from_millis(100);
        assert_eq!(
            wait_timeout(
                StreamPhase::PlayingOut {
                    began_at,
                    silent_frames_written: 0
                },
                later
            ),
            QUEUED_AUDIO_PLAY_OUT_TIMEOUT - Duration::from_millis(100)
        );
        let much_later = began_at + QUEUED_AUDIO_PLAY_OUT_TIMEOUT * 2;
        assert_eq!(
            wait_timeout(
                StreamPhase::PlayingOut {
                    began_at,
                    silent_frames_written: 0
                },
                much_later
            ),
            Duration::ZERO
        );
    }
}
