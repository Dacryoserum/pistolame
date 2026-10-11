//! Pure mappings from the errors of the output contract and of the decoder to the user-facing
//! `PlaybackFailureKind` (B3.7, B8). One place, so every path reports the same kind for the
//! same cause.

use super::decode_thread::SegmentFailure;
use crate::decoder::DecodeError;
use crate::engine::PlaybackFailureKind;
use crate::output_contract::{OutputError, OutputFailure};

/// Kind of a failure to open (or start) a stream.
pub(crate) fn failure_kind_for_output_error(error: &OutputError) -> PlaybackFailureKind {
    match error {
        OutputError::DeviceNotFound => PlaybackFailureKind::DeviceNotFound,
        OutputError::DeviceInvalidated => PlaybackFailureKind::DeviceUnplugged,
        OutputError::DeviceInUse => PlaybackFailureKind::DeviceInUse,
        OutputError::ExclusiveModeNotAllowed => PlaybackFailureKind::ExclusiveModeDisabled,
        OutputError::FormatNotSupported { .. } => PlaybackFailureKind::FormatRefusedByDevice,
        OutputError::DeviceTimedOut => PlaybackFailureKind::DeviceStoppedResponding,
        OutputError::AudioServiceNotRunning => PlaybackFailureKind::AudioServiceNotRunning,
        OutputError::Platform { .. } => PlaybackFailureKind::Internal,
    }
}

/// Kind of a failure of a running stream, read from `OutputStream::status`.
pub(crate) fn failure_kind_for_stream_failure(failure: OutputFailure) -> PlaybackFailureKind {
    match failure {
        OutputFailure::DeviceInvalidated => PlaybackFailureKind::DeviceUnplugged,
        OutputFailure::DeviceTimedOut => PlaybackFailureKind::DeviceStoppedResponding,
        OutputFailure::Platform { .. } => PlaybackFailureKind::Internal,
    }
}

/// Kind of a decoder failure, at open time or inside the track.
pub(crate) fn failure_kind_for_decode_error(error: &DecodeError) -> PlaybackFailureKind {
    match error {
        DecodeError::Io { .. } => PlaybackFailureKind::FileUnreadable,
        DecodeError::UnsupportedFileType | DecodeError::UnsupportedBitDepth { .. } => {
            PlaybackFailureKind::UnsupportedFile
        }
        DecodeError::CorruptData { .. } | DecodeError::FormatChangedMidStream => {
            PlaybackFailureKind::CorruptFile
        }
    }
}

/// Kind and detail of a generation that ended early: a damaged file maps like any decode
/// error, a ring refusal is an internal failure (a bug, not the file).
pub(crate) fn failure_for_segment(failure: &SegmentFailure) -> (PlaybackFailureKind, String) {
    match failure {
        SegmentFailure::Decode(error) => (failure_kind_for_decode_error(error), error.to_string()),
        SegmentFailure::Internal(detail) => (PlaybackFailureKind::Internal, detail.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_output_error_has_its_kind() {
        let cases = [
            (
                OutputError::DeviceNotFound,
                PlaybackFailureKind::DeviceNotFound,
            ),
            (
                OutputError::DeviceInvalidated,
                PlaybackFailureKind::DeviceUnplugged,
            ),
            (OutputError::DeviceInUse, PlaybackFailureKind::DeviceInUse),
            (
                OutputError::ExclusiveModeNotAllowed,
                PlaybackFailureKind::ExclusiveModeDisabled,
            ),
            (
                OutputError::FormatNotSupported { tried: Vec::new() },
                PlaybackFailureKind::FormatRefusedByDevice,
            ),
            (
                OutputError::DeviceTimedOut,
                PlaybackFailureKind::DeviceStoppedResponding,
            ),
            (
                OutputError::AudioServiceNotRunning,
                PlaybackFailureKind::AudioServiceNotRunning,
            ),
            (
                OutputError::Platform {
                    operation: "Initialize",
                    platform_code: -1,
                },
                PlaybackFailureKind::Internal,
            ),
        ];
        for (error, kind) in cases {
            assert_eq!(failure_kind_for_output_error(&error), kind, "{error}");
        }
    }

    #[test]
    fn every_stream_failure_has_its_kind() {
        assert_eq!(
            failure_kind_for_stream_failure(OutputFailure::DeviceInvalidated),
            PlaybackFailureKind::DeviceUnplugged
        );
        assert_eq!(
            failure_kind_for_stream_failure(OutputFailure::DeviceTimedOut),
            PlaybackFailureKind::DeviceStoppedResponding
        );
        assert_eq!(
            failure_kind_for_stream_failure(OutputFailure::Platform { platform_code: 5 }),
            PlaybackFailureKind::Internal
        );
    }

    #[test]
    fn a_ring_refusal_is_internal_and_a_damaged_block_is_a_corrupt_file() {
        let internal = SegmentFailure::Internal("refused".to_owned());
        assert_eq!(
            failure_for_segment(&internal),
            (PlaybackFailureKind::Internal, "refused".to_owned())
        );
        let damaged = SegmentFailure::Decode(DecodeError::CorruptData {
            near_frame_index: 3,
            detail: "sample out of range".to_owned(),
        });
        assert_eq!(
            failure_for_segment(&damaged).0,
            PlaybackFailureKind::CorruptFile
        );
    }

    #[test]
    fn every_decode_error_has_its_kind() {
        let cases = [
            (
                DecodeError::Io {
                    path: "missing.flac".into(),
                    source: std::io::Error::from(std::io::ErrorKind::NotFound),
                },
                PlaybackFailureKind::FileUnreadable,
            ),
            (
                DecodeError::UnsupportedFileType,
                PlaybackFailureKind::UnsupportedFile,
            ),
            (
                DecodeError::UnsupportedBitDepth {
                    bits_per_sample: 32,
                },
                PlaybackFailureKind::UnsupportedFile,
            ),
            (
                DecodeError::CorruptData {
                    near_frame_index: 10,
                    detail: "bad CRC".to_owned(),
                },
                PlaybackFailureKind::CorruptFile,
            ),
            (
                DecodeError::FormatChangedMidStream,
                PlaybackFailureKind::CorruptFile,
            ),
        ];
        for (error, kind) in cases {
            assert_eq!(failure_kind_for_decode_error(&error), kind, "{error}");
        }
    }
}
