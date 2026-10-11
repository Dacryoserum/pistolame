//! The single translation from WASAPI HRESULTs to the contract's errors (plan B5.8).
//!
//! Sources for the codes: the "Return value" sections of
//! <https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-initialize>,
//! <https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-isformatsupported>
//! and <https://learn.microsoft.com/en-us/windows/win32/api/mmdeviceapi/nf-mmdeviceapi-immdeviceenumerator-getdevice>.

use windows::Win32::Foundation::ERROR_NOT_FOUND;
use windows::Win32::Media::Audio::{
    AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_E_ENDPOINT_CREATE_FAILED,
    AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED, AUDCLNT_E_RESOURCES_INVALIDATED,
    AUDCLNT_E_SERVICE_NOT_RUNNING, AUDCLNT_E_UNSUPPORTED_FORMAT,
};
use windows::core::HRESULT;

use crate::{OutputError, OutputFailure};

/// `E_NOTFOUND`, returned by `IMMDeviceEnumerator::GetDevice` when "the device ID does not
/// identify an audio device that is in this system" (GetDevice docs). It is
/// `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`.
pub(crate) const E_NOTFOUND: HRESULT = HRESULT::from_win32(ERROR_NOT_FOUND.0);

/// A failed WASAPI or COM call: which call, and what it returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WasapiCallError {
    /// Name of the call, e.g. "IAudioClient::Initialize".
    pub(crate) operation: &'static str,
    /// The HRESULT it returned.
    pub(crate) hresult: HRESULT,
}

impl WasapiCallError {
    /// The contract error for this failure.
    pub(crate) fn to_output_error(self) -> OutputError {
        output_error_from_hresult(self.operation, self.hresult)
    }
}

impl From<WasapiCallError> for OutputError {
    fn from(call_error: WasapiCallError) -> Self {
        call_error.to_output_error()
    }
}

/// Attaches the name of the call to a `windows` result, so every failure says where it
/// happened.
pub(crate) trait NameWasapiCall<T> {
    /// Converts the error into a `WasapiCallError` naming `operation`.
    fn for_operation(self, operation: &'static str) -> Result<T, WasapiCallError>;
}

impl<T> NameWasapiCall<T> for windows::core::Result<T> {
    fn for_operation(self, operation: &'static str) -> Result<T, WasapiCallError> {
        self.map_err(|error| WasapiCallError {
            operation,
            hresult: error.code(),
        })
    }
}

/// Maps an HRESULT returned by `operation` to the contract's error (B5.8 table).
/// `AUDCLNT_E_UNSUPPORTED_FORMAT` maps to `FormatNotSupported` with an empty list; callers that
/// tried candidates replace it with their negotiation log.
pub(crate) fn output_error_from_hresult(operation: &'static str, hresult: HRESULT) -> OutputError {
    match hresult {
        AUDCLNT_E_DEVICE_INVALIDATED
        | AUDCLNT_E_RESOURCES_INVALIDATED
        | AUDCLNT_E_ENDPOINT_CREATE_FAILED => OutputError::DeviceInvalidated,
        AUDCLNT_E_DEVICE_IN_USE => OutputError::DeviceInUse,
        AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED => OutputError::ExclusiveModeNotAllowed,
        AUDCLNT_E_UNSUPPORTED_FORMAT => OutputError::FormatNotSupported { tried: Vec::new() },
        AUDCLNT_E_SERVICE_NOT_RUNNING => OutputError::AudioServiceNotRunning,
        E_NOTFOUND => OutputError::DeviceNotFound,
        _ => OutputError::Platform {
            operation,
            platform_code: hresult.0,
        },
    }
}

/// Maps an HRESULT raised on the real-time thread to the failure stored in the stream status.
/// Does not allocate.
pub(crate) fn output_failure_from_hresult(hresult: HRESULT) -> OutputFailure {
    match hresult {
        AUDCLNT_E_DEVICE_INVALIDATED
        | AUDCLNT_E_RESOURCES_INVALIDATED
        | AUDCLNT_E_ENDPOINT_CREATE_FAILED => OutputFailure::DeviceInvalidated,
        _ => OutputFailure::Platform {
            platform_code: hresult.0,
        },
    }
}

/// The contract error the engine gets from `play` once the real-time side has failed.
pub(crate) fn output_error_from_failure(failure: OutputFailure) -> OutputError {
    match failure {
        OutputFailure::DeviceInvalidated => OutputError::DeviceInvalidated,
        OutputFailure::DeviceTimedOut => OutputError::DeviceTimedOut,
        OutputFailure::Platform { platform_code } => OutputError::Platform {
            operation: "WASAPI render thread",
            platform_code,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Media::Audio::{
        AUDCLNT_E_BUFDURATION_PERIOD_NOT_EQUAL, AUDCLNT_E_BUFFER_SIZE_ERROR,
        AUDCLNT_E_INVALID_DEVICE_PERIOD,
    };

    const OPERATION: &str = "IAudioClient::Initialize";

    #[test]
    fn invalidated_device_codes_map_to_device_invalidated() {
        for hresult in [
            AUDCLNT_E_DEVICE_INVALIDATED,
            AUDCLNT_E_RESOURCES_INVALIDATED,
            AUDCLNT_E_ENDPOINT_CREATE_FAILED,
        ] {
            assert!(matches!(
                output_error_from_hresult(OPERATION, hresult),
                OutputError::DeviceInvalidated
            ));
            assert_eq!(
                output_failure_from_hresult(hresult),
                OutputFailure::DeviceInvalidated
            );
        }
    }

    #[test]
    fn device_in_use_maps_to_device_in_use() {
        assert!(matches!(
            output_error_from_hresult(OPERATION, AUDCLNT_E_DEVICE_IN_USE),
            OutputError::DeviceInUse
        ));
    }

    #[test]
    fn exclusive_mode_not_allowed_maps_to_its_own_error() {
        assert!(matches!(
            output_error_from_hresult(OPERATION, AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED),
            OutputError::ExclusiveModeNotAllowed
        ));
    }

    #[test]
    fn unsupported_format_maps_to_format_not_supported() {
        assert!(matches!(
            output_error_from_hresult(OPERATION, AUDCLNT_E_UNSUPPORTED_FORMAT),
            OutputError::FormatNotSupported { tried } if tried.is_empty()
        ));
    }

    #[test]
    fn service_not_running_maps_to_audio_service_not_running() {
        assert!(matches!(
            output_error_from_hresult(OPERATION, AUDCLNT_E_SERVICE_NOT_RUNNING),
            OutputError::AudioServiceNotRunning
        ));
    }

    #[test]
    fn unknown_device_id_maps_to_device_not_found() {
        assert_eq!(E_NOTFOUND.0 as u32, 0x8007_0490);
        assert!(matches!(
            output_error_from_hresult("IMMDeviceEnumerator::GetDevice", E_NOTFOUND),
            OutputError::DeviceNotFound
        ));
    }

    #[test]
    fn other_codes_keep_the_operation_and_the_code() {
        for hresult in [
            AUDCLNT_E_BUFFER_SIZE_ERROR,
            AUDCLNT_E_INVALID_DEVICE_PERIOD,
            AUDCLNT_E_BUFDURATION_PERIOD_NOT_EQUAL,
        ] {
            match output_error_from_hresult(OPERATION, hresult) {
                OutputError::Platform {
                    operation,
                    platform_code,
                } => {
                    assert_eq!(operation, OPERATION);
                    assert_eq!(platform_code, hresult.0);
                }
                other => panic!("{hresult:?} mapped to {other:?}"),
            }
            assert_eq!(
                output_failure_from_hresult(hresult),
                OutputFailure::Platform {
                    platform_code: hresult.0
                }
            );
        }
    }

    #[test]
    fn real_time_failures_map_back_to_errors_for_play() {
        assert!(matches!(
            output_error_from_failure(OutputFailure::DeviceTimedOut),
            OutputError::DeviceTimedOut
        ));
        assert!(matches!(
            output_error_from_failure(OutputFailure::DeviceInvalidated),
            OutputError::DeviceInvalidated
        ));
        assert!(matches!(
            output_error_from_failure(OutputFailure::Platform { platform_code: -5 }),
            OutputError::Platform {
                platform_code: -5,
                ..
            }
        ));
    }

    #[test]
    fn named_call_keeps_the_hresult() {
        let failing: windows::core::Result<()> =
            Err(windows::core::Error::from_hresult(AUDCLNT_E_DEVICE_IN_USE));
        assert_eq!(
            failing.for_operation(OPERATION),
            Err(WasapiCallError {
                operation: OPERATION,
                hresult: AUDCLNT_E_DEVICE_IN_USE
            })
        );
    }
}
