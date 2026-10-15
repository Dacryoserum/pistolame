//! ALSA error codes as the contract's errors and failures.
//!
//! ALSA returns negative `errno` values. The numbers are Linux's (`asm-generic/errno-base.h`,
//! `asm-generic/errno.h`); they are the same on every Linux architecture Pistolame builds for.

use crate::{OutputError, OutputFailure};

/// No such file or directory: the PCM name does not exist.
const ENOENT: i32 = 2;
/// No such device or address.
const ENXIO: i32 = 6;
/// Device or resource busy: another program holds the device.
const EBUSY: i32 = 16;
/// No such device: unplugged.
const ENODEV: i32 = 19;
/// Broken pipe: underrun.
pub(super) const EPIPE: i32 = 32;
/// File descriptor in bad state: the device went away under an open stream.
const EBADFD: i32 = 77;
/// Streams pipe error: the device was suspended.
pub(super) const ESTRPIPE: i32 = 86;

/// The positive `errno` of an ALSA error.
pub(super) fn errno_of(error: &alsa::Error) -> i32 {
    error.errno().saturating_abs()
}

/// `OutputError` for an ALSA call that failed while opening.
pub(super) fn output_error(operation: &'static str, error: alsa::Error) -> OutputError {
    match errno_of(&error) {
        EBUSY => OutputError::DeviceInUse,
        ENOENT | ENXIO => OutputError::DeviceNotFound,
        ENODEV | EBADFD => OutputError::DeviceInvalidated,
        errno => OutputError::Platform {
            operation,
            platform_code: errno,
        },
    }
}

/// `OutputFailure` for an ALSA call that failed while playing (after recovery was tried).
pub(super) fn output_failure(error: &alsa::Error) -> OutputFailure {
    match errno_of(error) {
        ENODEV | EBADFD | ENXIO => OutputFailure::DeviceInvalidated,
        errno => OutputFailure::Platform {
            platform_code: errno,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(errno: i32) -> alsa::Error {
        alsa::Error::new("snd_pcm_open", -errno)
    }

    #[test]
    fn opening_errors_map_to_contract_errors() {
        assert!(matches!(
            output_error("open", failed(EBUSY)),
            OutputError::DeviceInUse
        ));
        assert!(matches!(
            output_error("open", failed(ENOENT)),
            OutputError::DeviceNotFound
        ));
        assert!(matches!(
            output_error("open", failed(ENODEV)),
            OutputError::DeviceInvalidated
        ));
        assert!(matches!(
            output_error("open", failed(22)),
            OutputError::Platform {
                platform_code: 22,
                ..
            }
        ));
    }

    #[test]
    fn an_unplugged_device_is_invalidated() {
        assert_eq!(
            output_failure(&failed(ENODEV)),
            OutputFailure::DeviceInvalidated
        );
        assert_eq!(
            output_failure(&failed(EPIPE)),
            OutputFailure::Platform {
                platform_code: EPIPE
            }
        );
    }
}
