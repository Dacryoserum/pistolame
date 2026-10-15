//! Engine-side handle of a WASAPI stream: spawns the render thread, waits for the open
//! result, and drives the thread through the control event (plan B5.3, B5.6).
//!
//! Real-time failures reach the engine only through `status()`, which the engine polls; there
//! is no wake-up channel (decided for M1).

use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crossbeam_channel::bounded;
use windows::Win32::Foundation::E_UNEXPECTED;

use super::com_apartment::ComApartment;
use super::devices::DeviceEnumerator;
use super::hresult_mapping::output_error_from_failure;
use super::render_thread::{OpenResult, RenderThreadContext, RenderThreadExit, run_render_thread};
use super::stream_control::{RequestedStreamState, StreamControl, WasapiDiagnosticCounters};
use crate::{
    ClosedStream, NegotiatedOutput, OpenStreamError, OpenStreamRequest, OutputError, OutputStream,
    OutputStreamStatus, QueuedAudioOnClose, RenderSource,
};

/// Name of the render threads, visible in debuggers and profilers.
const RENDER_THREAD_NAME: &str = "pistolame-wasapi-render";

/// An open WASAPI stream. Dropping it closes it with `Discard`.
pub(crate) struct WasapiOutputStream {
    negotiated: NegotiatedOutput,
    control: Arc<StreamControl>,
    diagnostics: Arc<WasapiDiagnosticCounters>,
    render_thread: Option<JoinHandle<Option<RenderThreadExit>>>,
}

/// Spawns the render thread and waits until it has opened and prefilled the device, or
/// failed. On failure the render source comes back inside the error.
pub(crate) fn open_wasapi_stream(
    request: OpenStreamRequest,
    render_source: Box<dyn RenderSource>,
    diagnostics: Arc<WasapiDiagnosticCounters>,
) -> Result<Box<dyn OutputStream>, OpenStreamError> {
    let control = match StreamControl::new() {
        Ok(control) => Arc::new(control),
        Err(error) => {
            return Err(OpenStreamError {
                error: error.to_output_error(),
                render_source,
            });
        }
    };
    let (render_source_sender, render_source_receiver) = bounded(1);
    let (open_result_sender, open_result_receiver) = bounded::<OpenResult>(1);
    let context = RenderThreadContext {
        request,
        control: Arc::clone(&control),
        diagnostics: Arc::clone(&diagnostics),
        open_result_sender,
    };
    let spawned = thread::Builder::new()
        .name(RENDER_THREAD_NAME.to_owned())
        .spawn(move || run_render_thread(context, render_source_receiver));
    let render_thread = match spawned {
        Ok(handle) => handle,
        Err(spawn_error) => {
            return Err(OpenStreamError {
                error: OutputError::Platform {
                    operation: "spawn the WASAPI render thread",
                    platform_code: spawn_error.raw_os_error().unwrap_or(E_UNEXPECTED.0),
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
        Ok(Ok(negotiated)) => Ok(Box::new(WasapiOutputStream {
            negotiated,
            control,
            diagnostics,
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
        operation: "WASAPI render thread ended unexpectedly",
        platform_code: E_UNEXPECTED.0,
    }
}

/// Waits for the render thread and takes back what it returns.
fn join_render_thread(render_thread: JoinHandle<Option<RenderThreadExit>>) -> RenderThreadExit {
    render_thread.join().ok().flatten().expect(
        "the render thread receives its source before any fallible step and catches every panic",
    )
}

impl WasapiOutputStream {
    /// Asks the render thread to end in the given way and waits for it. The play-out is
    /// bounded by `QUEUED_AUDIO_PLAY_OUT_TIMEOUT` inside the thread.
    fn finish_render_thread(
        &mut self,
        queued_audio: QueuedAudioOnClose,
    ) -> Option<RenderThreadExit> {
        let render_thread = self.render_thread.take()?;
        let request = match queued_audio {
            QueuedAudioOnClose::PlayOut => RequestedStreamState::ClosePlayOut,
            QueuedAudioOnClose::Discard => RequestedStreamState::CloseDiscard,
        };
        // The request is stored before SetEvent, and the render thread re-reads it at every
        // wake-up (next buffer event when running, next 2 s timeout before start, play-out
        // budget during a play-out), so a failed SetEvent delays the close but cannot hang the
        // join. The failure is counted.
        if self.control.request(request).is_err() {
            self.diagnostics.count_control_signal_failure();
        }
        Some(join_render_thread(render_thread))
    }
}

impl OutputStream for WasapiOutputStream {
    /// Applied by the render thread to shared streams only; exclusive streams stay bit-perfect.
    fn set_shared_mode_volume(&mut self, linear_gain: f32) {
        self.control.set_shared_mode_gain(linear_gain);
    }

    fn negotiated(&self) -> &NegotiatedOutput {
        &self.negotiated
    }

    /// Asks the device enumerator whether the endpoint is still active (`IMMDevice::GetState`
    /// = `DEVICE_STATE_ACTIVE`), on the engine thread with its own COM initialization. Any
    /// failure counts as gone, so a doubtful device is never left for another one.
    fn is_device_still_present(&self) -> bool {
        let Ok(_com_apartment) = ComApartment::initialize_multithreaded() else {
            return false;
        };
        let Ok(enumerator) = DeviceEnumerator::new() else {
            return false;
        };
        enumerator
            .find_active_render_device(&self.negotiated.device.id)
            .is_ok()
    }

    fn play(&mut self) -> Result<(), OutputError> {
        if let OutputStreamStatus::Failed(failure) = self.control.status() {
            return Err(output_error_from_failure(failure));
        }
        // On a failed SetEvent the request is still stored; the thread starts at its next
        // timeout. The failure is counted, and returned so the engine knows.
        self.control
            .request(RequestedStreamState::Run)
            .map_err(|error| {
                self.diagnostics.count_control_signal_failure();
                error.to_output_error()
            })
    }

    fn status(&self) -> OutputStreamStatus {
        self.control.status()
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

impl Drop for WasapiOutputStream {
    fn drop(&mut self) {
        let _ = self.finish_render_thread(QueuedAudioOnClose::Discard);
    }
}
