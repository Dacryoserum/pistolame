//! Platform-neutral state shared by the engine side and the real-time side of one output
//! stream (plan B6): the requested run state and the stream status packed in one word, so
//! both are lock-free. Used by every backend (WASAPI, CoreAudio).

// `StreamStateCell` serves the CoreAudio backend only; WASAPI keeps its own control block and
// Linux has no backend yet, so parts of this module are unused there.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use crate::{OutputFailure, OutputStreamStatus};

/// What the engine asks the real-time side to do. Written by the engine; read by the real-time
/// side at each wake-up or callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum RequestedStreamState {
    /// Nothing requested yet (stream opened and prefilled).
    Idle = 0,
    /// Start rendering.
    Run = 1,
    /// Stop feeding, play out the queued audio, then close (pause, mode switch).
    ClosePlayOut = 2,
    /// Stop and discard the queued audio at once (stop, seek, failure).
    CloseDiscard = 3,
}

impl RequestedStreamState {
    /// Inverse of `state as u8`; unknown values read as `Idle`.
    pub(crate) fn from_encoded(encoded: u8) -> RequestedStreamState {
        match encoded {
            1 => RequestedStreamState::Run,
            2 => RequestedStreamState::ClosePlayOut,
            3 => RequestedStreamState::CloseDiscard,
            _ => RequestedStreamState::Idle,
        }
    }
}

/// Status tags in the upper 32 bits of the encoded status; the lower 32 bits hold the platform
/// code of a `Platform` failure. One 64-bit word keeps tag and code consistent without a lock.
const STATUS_TAG_NOT_STARTED: u64 = 0;
const STATUS_TAG_RUNNING: u64 = 1;
const STATUS_TAG_FAILED_DEVICE_INVALIDATED: u64 = 2;
const STATUS_TAG_FAILED_DEVICE_TIMED_OUT: u64 = 3;
const STATUS_TAG_FAILED_PLATFORM: u64 = 4;
/// Position of the tag inside the encoded status.
const STATUS_TAG_SHIFT: u32 = 32;

/// Packs a status into one word.
pub(crate) fn encode_stream_status(status: OutputStreamStatus) -> u64 {
    let (tag, platform_code) = match status {
        OutputStreamStatus::NotStarted => (STATUS_TAG_NOT_STARTED, 0),
        OutputStreamStatus::Running => (STATUS_TAG_RUNNING, 0),
        OutputStreamStatus::Failed(OutputFailure::DeviceInvalidated) => {
            (STATUS_TAG_FAILED_DEVICE_INVALIDATED, 0)
        }
        OutputStreamStatus::Failed(OutputFailure::DeviceTimedOut) => {
            (STATUS_TAG_FAILED_DEVICE_TIMED_OUT, 0)
        }
        OutputStreamStatus::Failed(OutputFailure::Platform { platform_code }) => {
            (STATUS_TAG_FAILED_PLATFORM, platform_code)
        }
    };
    (tag << STATUS_TAG_SHIFT) | u64::from(platform_code.cast_unsigned())
}

/// Inverse of `encode_stream_status`.
pub(crate) fn decode_stream_status(encoded: u64) -> OutputStreamStatus {
    let platform_code = (encoded as u32).cast_signed();
    match encoded >> STATUS_TAG_SHIFT {
        STATUS_TAG_NOT_STARTED => OutputStreamStatus::NotStarted,
        STATUS_TAG_RUNNING => OutputStreamStatus::Running,
        STATUS_TAG_FAILED_DEVICE_INVALIDATED => {
            OutputStreamStatus::Failed(OutputFailure::DeviceInvalidated)
        }
        STATUS_TAG_FAILED_DEVICE_TIMED_OUT => {
            OutputStreamStatus::Failed(OutputFailure::DeviceTimedOut)
        }
        _ => OutputStreamStatus::Failed(OutputFailure::Platform { platform_code }),
    }
}

/// The requested state and the status of one stream, readable and writable from any thread
/// without locking. A failure already stored is never overwritten by a later status, so the
/// first cause wins.
#[derive(Debug)]
pub(crate) struct StreamStateCell {
    requested_state: AtomicU8,
    encoded_status: AtomicU64,
}

impl StreamStateCell {
    /// Nothing requested, status `NotStarted`.
    pub(crate) fn new() -> StreamStateCell {
        StreamStateCell {
            requested_state: AtomicU8::new(RequestedStreamState::Idle as u8),
            encoded_status: AtomicU64::new(encode_stream_status(OutputStreamStatus::NotStarted)),
        }
    }

    /// Records what the engine asks for.
    pub(crate) fn request(&self, state: RequestedStreamState) {
        self.requested_state.store(state as u8, Ordering::Release);
    }

    /// The latest request.
    pub(crate) fn requested_state(&self) -> RequestedStreamState {
        RequestedStreamState::from_encoded(self.requested_state.load(Ordering::Acquire))
    }

    /// Publishes `status`, unless a failure is already stored.
    pub(crate) fn store_status(&self, status: OutputStreamStatus) {
        let encoded = encode_stream_status(status);
        let _ =
            self.encoded_status
                .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    match decode_stream_status(current) {
                        OutputStreamStatus::Failed(_) => None,
                        _ => Some(encoded),
                    }
                });
    }

    /// The current status.
    pub(crate) fn status(&self) -> OutputStreamStatus {
        decode_stream_status(self.encoded_status.load(Ordering::Acquire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_survives_encoding() {
        for status in [
            OutputStreamStatus::NotStarted,
            OutputStreamStatus::Running,
            OutputStreamStatus::Failed(OutputFailure::DeviceInvalidated),
            OutputStreamStatus::Failed(OutputFailure::DeviceTimedOut),
            OutputStreamStatus::Failed(OutputFailure::Platform {
                platform_code: 0x8889_0004_u32.cast_signed(),
            }),
            OutputStreamStatus::Failed(OutputFailure::Platform { platform_code: 7 }),
        ] {
            assert_eq!(decode_stream_status(encode_stream_status(status)), status);
        }
    }

    #[test]
    fn the_first_failure_is_kept() {
        let cell = StreamStateCell::new();
        assert_eq!(cell.status(), OutputStreamStatus::NotStarted);
        cell.store_status(OutputStreamStatus::Running);
        cell.store_status(OutputStreamStatus::Failed(OutputFailure::DeviceInvalidated));
        cell.store_status(OutputStreamStatus::Failed(OutputFailure::DeviceTimedOut));
        cell.store_status(OutputStreamStatus::Running);
        assert_eq!(
            cell.status(),
            OutputStreamStatus::Failed(OutputFailure::DeviceInvalidated)
        );
    }

    #[test]
    fn requests_round_trip() {
        let cell = StreamStateCell::new();
        assert_eq!(cell.requested_state(), RequestedStreamState::Idle);
        cell.request(RequestedStreamState::ClosePlayOut);
        assert_eq!(cell.requested_state(), RequestedStreamState::ClosePlayOut);
    }
}
