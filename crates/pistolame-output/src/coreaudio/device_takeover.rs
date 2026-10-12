//! Exclusive takeover of a device: hog mode and the source sample rate, both read back, both
//! undone when the guard is dropped (pause, stop, error, panic). Plan coreaudio.md §0 item 1.
//!
//! Hog mode reference: `kAudioDevicePropertyHogMode` in `AudioHardware.h` ("the pid of the
//! process that has exclusive access ... -1 if the device is not in hog mode"). Setting it is
//! reported to toggle rather than set, so the owner is always read back.

use std::thread;
use std::time::{Duration, Instant};

use objc2_core_audio::AudioObjectID;

use super::devices::{
    HOG_MODE_FREE, hog_mode_owner, nominal_sample_rate, request_nominal_sample_rate,
    write_hog_mode_owner,
};
use crate::OutputError;

/// How long a hog or sample-rate change may take to show up when read back. Both are
/// asynchronous in CoreAudio; on the MacBook Air they settle within tens of milliseconds.
const PROPERTY_SETTLE_TIMEOUT: Duration = Duration::from_millis(1500);
/// Pause between two read-backs while waiting for a property to settle.
const PROPERTY_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Largest difference, in hertz, between a requested and a read-back rate that still counts as
/// the same rate (CoreAudio reports rates as `f64`).
const SAMPLE_RATE_TOLERANCE_HZ: f64 = 0.5;

/// Holds a device in hog mode at a given sample rate until dropped.
#[derive(Debug)]
pub(crate) struct ExclusiveTakeover {
    device: AudioObjectID,
    process_id: i32,
    /// The rate to restore on release, when this takeover changed it.
    rate_to_restore_hz: Option<f64>,
}

impl ExclusiveTakeover {
    /// Takes hog mode, then sets the nominal rate to `sample_rate_hz`, reading both back.
    ///
    /// Errors: `DeviceInUse` when another process holds the device or hog mode never shows
    /// this process as owner; `ExclusiveModeNotAllowed` when CoreAudio refuses hog mode;
    /// `FormatNotSupported` (with an empty log, filled by the caller) when the rate cannot be
    /// set or does not take effect. Every error leaves the device as it was.
    pub(crate) fn acquire(
        device: AudioObjectID,
        sample_rate_hz: u32,
    ) -> Result<ExclusiveTakeover, OutputError> {
        let process_id = current_process_id();
        let owner = hog_mode_owner(device)?;
        if owner != HOG_MODE_FREE && owner != process_id {
            return Err(OutputError::DeviceInUse);
        }
        // Built before any write, so dropping it on an early return releases whatever this
        // process ends up holding, even a hog toggle that lands after the read-back gave up.
        let mut takeover = ExclusiveTakeover {
            device,
            process_id,
            rate_to_restore_hz: None,
        };
        if owner != process_id {
            write_hog_mode_owner(device, process_id).map_err(map_hog_error)?;
            if !wait_until(|| hog_mode_owner(device).is_ok_and(|owner| owner == process_id)) {
                return Err(OutputError::DeviceInUse);
            }
        }
        let target_rate_hz = f64::from(sample_rate_hz);
        let previous_rate_hz = nominal_sample_rate(device)?;
        if !is_same_rate(previous_rate_hz, target_rate_hz) {
            takeover.rate_to_restore_hz = Some(previous_rate_hz);
            request_nominal_sample_rate(device, target_rate_hz)
                .map_err(|_| OutputError::FormatNotSupported { tried: Vec::new() })?;
            if !wait_for_rate(device, target_rate_hz) {
                return Err(OutputError::FormatNotSupported { tried: Vec::new() });
            }
        }
        Ok(takeover)
    }

    /// Whether this process still holds the device (read by the property listener).
    pub(crate) fn is_still_held(device: AudioObjectID) -> bool {
        hog_mode_owner(device).is_ok_and(|owner| owner == current_process_id())
    }
}

impl Drop for ExclusiveTakeover {
    /// Best effort: restores the rate and waits until it has taken effect (rate changes are
    /// asynchronous; a late one could land under the next stream), then releases hog mode.
    /// Failures cannot be reported from `drop`; the next takeover reads the real state back.
    fn drop(&mut self) {
        if let Some(rate_hz) = self.rate_to_restore_hz
            && request_nominal_sample_rate(self.device, rate_hz).is_ok()
        {
            let _ = wait_for_rate(self.device, rate_hz);
        }
        if hog_mode_owner(self.device).is_ok_and(|owner| owner == self.process_id) {
            let _ = write_hog_mode_owner(self.device, HOG_MODE_FREE);
        }
    }
}

fn current_process_id() -> i32 {
    i32::try_from(std::process::id()).unwrap_or(i32::MAX)
}

fn is_same_rate(first_hz: f64, second_hz: f64) -> bool {
    (first_hz - second_hz).abs() <= SAMPLE_RATE_TOLERANCE_HZ
}

/// Any refusal to enter hog mode means exclusive mode is not available for this device
/// (permissions error, or devices such as AirPlay and aggregates that do not support it), so
/// `ExclusiveThenShared` falls back to shared mode.
fn map_hog_error(error: OutputError) -> OutputError {
    match error {
        OutputError::Platform { .. } => OutputError::ExclusiveModeNotAllowed,
        other => other,
    }
}

fn wait_for_rate(device: AudioObjectID, rate_hz: f64) -> bool {
    wait_until(|| nominal_sample_rate(device).is_ok_and(|rate| is_same_rate(rate, rate_hz)))
}

/// Polls `condition` until it holds or `PROPERTY_SETTLE_TIMEOUT` elapses. Runs on the engine
/// thread, never on the real-time path.
fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
    let started_at = Instant::now();
    loop {
        if condition() {
            return true;
        }
        if started_at.elapsed() >= PROPERTY_SETTLE_TIMEOUT {
            return false;
        }
        thread::sleep(PROPERTY_POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_within_half_a_hertz_are_the_same() {
        assert!(is_same_rate(96_000.0, 96_000.3));
        assert!(!is_same_rate(96_000.0, 88_200.0));
    }

    #[test]
    fn any_hog_refusal_means_exclusive_mode_is_not_allowed() {
        let error = map_hog_error(OutputError::Platform {
            operation: "set kAudioDevicePropertyHogMode",
            platform_code: objc2_core_audio::kAudioDevicePermissionsError,
        });
        assert!(matches!(error, OutputError::ExclusiveModeNotAllowed));
    }
}
