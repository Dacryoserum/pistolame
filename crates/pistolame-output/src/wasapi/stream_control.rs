//! State shared between the engine thread and one stream's render thread (plan B6): the
//! requested run state, the stream status, and the diagnostic counters. Lock-free atomics
//! only; the render thread never blocks on them.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use super::hresult_mapping::WasapiCallError;
use super::win32_event::AutoResetEvent;
use crate::OutputStreamStatus;

pub(crate) use crate::stream_state::{
    RequestedStreamState, decode_stream_status, encode_stream_status,
};

/// Engine ↔ render thread state for one stream.
pub(crate) struct StreamControl {
    requested_state: AtomicU8,
    encoded_status: AtomicU64,
    control_event: AutoResetEvent,
}

impl StreamControl {
    /// New control block: nothing requested, status `NotStarted`.
    pub(crate) fn new() -> Result<StreamControl, WasapiCallError> {
        Ok(StreamControl {
            requested_state: AtomicU8::new(RequestedStreamState::Idle as u8),
            encoded_status: AtomicU64::new(encode_stream_status(OutputStreamStatus::NotStarted)),
            control_event: AutoResetEvent::new()?,
        })
    }

    /// Engine side: records the request, then wakes the render thread. The request is stored
    /// even if the wake-up fails; the render thread re-reads it at every wake-up.
    pub(crate) fn request(&self, state: RequestedStreamState) -> Result<(), WasapiCallError> {
        self.requested_state.store(state as u8, Ordering::Release);
        self.control_event.signal()
    }

    /// Render side: the latest request.
    pub(crate) fn requested_state(&self) -> RequestedStreamState {
        RequestedStreamState::from_encoded(self.requested_state.load(Ordering::Acquire))
    }

    /// Render side: publishes the status.
    pub(crate) fn store_status(&self, status: OutputStreamStatus) {
        self.encoded_status
            .store(encode_stream_status(status), Ordering::Release);
    }

    /// Any side: the current status.
    pub(crate) fn status(&self) -> OutputStreamStatus {
        decode_stream_status(self.encoded_status.load(Ordering::Acquire))
    }

    /// The event the render thread waits on together with the buffer event.
    pub(crate) fn control_event(&self) -> &AutoResetEvent {
        &self.control_event
    }
}

/// Counters for problems that do not stop playback, shared by every stream of a
/// `WasapiOutput` (AGENTS.md: never swallow an error on the audio path, count it).
#[derive(Debug, Default)]
pub(crate) struct WasapiDiagnosticCounters {
    mmcss_registration_failures: AtomicU64,
    stream_close_errors: AtomicU64,
    control_signal_failures: AtomicU64,
}

/// A copy of the diagnostic counters at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WasapiDiagnostics {
    /// Streams that ran at normal priority because MMCSS refused the "Pro Audio" registration.
    pub mmcss_registration_failures: u64,
    /// `Stop` or `Reset` calls that failed while a stream was closing.
    pub stream_close_errors: u64,
    /// `SetEvent` calls on a control event that failed; the render thread still sees the
    /// request at its next wake-up.
    pub control_signal_failures: u64,
}

impl WasapiDiagnosticCounters {
    /// Counts one MMCSS refusal.
    pub(crate) fn count_mmcss_registration_failure(&self) {
        self.mmcss_registration_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Counts one failed `Stop` or `Reset` on close.
    pub(crate) fn count_stream_close_error(&self) {
        self.stream_close_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Counts one failed `SetEvent` on a control event.
    pub(crate) fn count_control_signal_failure(&self) {
        self.control_signal_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// Current values.
    pub(crate) fn snapshot(&self) -> WasapiDiagnostics {
        WasapiDiagnostics {
            mmcss_registration_failures: self.mmcss_registration_failures.load(Ordering::Relaxed),
            stream_close_errors: self.stream_close_errors.load(Ordering::Relaxed),
            control_signal_failures: self.control_signal_failures.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_state_round_trips_through_the_atomic() {
        let control = StreamControl::new().unwrap();
        assert_eq!(control.requested_state(), RequestedStreamState::Idle);
        for state in [
            RequestedStreamState::Run,
            RequestedStreamState::ClosePlayOut,
            RequestedStreamState::CloseDiscard,
        ] {
            control.request(state).unwrap();
            assert_eq!(control.requested_state(), state);
        }
    }

    #[test]
    fn stream_control_starts_not_started() {
        let control = StreamControl::new().unwrap();
        assert_eq!(control.status(), OutputStreamStatus::NotStarted);
        control.store_status(OutputStreamStatus::Running);
        assert_eq!(control.status(), OutputStreamStatus::Running);
    }

    #[test]
    fn diagnostic_counters_count() {
        let counters = WasapiDiagnosticCounters::default();
        counters.count_mmcss_registration_failure();
        counters.count_stream_close_error();
        counters.count_stream_close_error();
        counters.count_control_signal_failure();
        assert_eq!(
            counters.snapshot(),
            WasapiDiagnostics {
                mmcss_registration_failures: 1,
                stream_close_errors: 2,
                control_signal_failures: 1,
            }
        );
    }
}
