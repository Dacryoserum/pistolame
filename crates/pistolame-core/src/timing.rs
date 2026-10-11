//! Named durations of the playback pipeline (walking-skeleton plan, section B6). Each value is
//! defined once here and says why it was chosen.

use std::time::Duration;

/// Capacity of the playback ring: absorbs decoder and disk stalls (3 MiB at 24/192 stereo as `i32`).
pub const PLAYBACK_RING_DURATION: Duration = Duration::from_secs(2);
/// Audio buffered before starting a stream (or the whole track if shorter): avoids an underrun at start.
pub const START_PREFILL_DURATION: Duration = Duration::from_millis(500);
/// Capacity of the verification tap in device bytes: the verifier can lag 2 s before overflow.
pub const VERIFICATION_TAP_DURATION: Duration = Duration::from_secs(2);
/// Minimum spacing of `InProgress` verification updates: at most twice per second.
pub const VERIFICATION_PROGRESS_INTERVAL: Duration = Duration::from_millis(500);
/// Preferred WASAPI exclusive-mode period: robustness over latency (B5.5).
pub const PREFERRED_EXCLUSIVE_PERIOD: Duration = Duration::from_millis(20);
/// Longest wait for a play-out on pause. Play-out needs about two periods (at most 40 ms in
/// exclusive mode); a much longer wait means the device is not consuming (B5.6).
pub const QUEUED_AUDIO_PLAY_OUT_TIMEOUT: Duration = Duration::from_millis(500);
/// Longest wait for the device event before declaring a timeout (Microsoft exclusive-mode sample).
pub const DEVICE_EVENT_TIMEOUT: Duration = Duration::from_millis(2000);
/// Decode thread back-off when the playback ring is full.
pub const DECODE_RETRY_SLEEP: Duration = Duration::from_millis(5);
/// UI refresh period, about 30 Hz, as in ADR 0001.
pub const UI_REFRESH_INTERVAL: Duration = Duration::from_millis(33);
/// The verifier's longest wait for a command before draining the tap again: 200 times shorter
/// than the tap (`VERIFICATION_TAP_DURATION`), so polling never causes an overflow.
pub const VERIFIER_POLL_INTERVAL: Duration = Duration::from_millis(10);
