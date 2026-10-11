//! Types of the two-level bit-perfect verification (B7.2), and the verification tap that carries
//! device bytes from the render thread to the verifier. The hasher and the verifier thread
//! belong to later tasks.

pub mod tap;

pub use tap::*;

use crate::format::DeviceSampleFormat;
use crate::output_contract::DeviceAccessMode;
use crate::playback_ring::TrackId;

/// Both verification levels for one track (B7.2). Sent whenever either level changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationStatus {
    /// Level 1, "Transmission vérifiée".
    pub transmission: TransmissionStatus,
    /// Level 2, "Fichier intègre".
    pub file_integrity: FileIntegrityStatus,
}

/// Level 1: do the bytes written to the device buffer, unpacked to the canonical layout, hash
/// to the same digest as the PCM the decoder produced, over the same frame range?
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransmissionStatus {
    /// Device-side frames hashed so far in the current segment.
    InProgress {
        /// Count of frames already fed to the hasher, for the progress display.
        device_frames_hashed: u64,
    },
    /// Both digests are equal over `[first_frame_index, first_frame_index + frame_count)`.
    /// `first_frame_index` is 0 unless the user seeked (then the range starts at the seek).
    Verified {
        /// First frame of the verified range.
        first_frame_index: u64,
        /// Number of frames in the verified range.
        frame_count: u64,
        /// MD5 of the verified frames, equal on both sides.
        md5: [u8; 16],
    },
    /// The digests differ.
    Mismatch {
        /// Digest of the decoder-side PCM.
        decoder_md5: [u8; 16],
        /// Digest of the device-side bytes.
        device_md5: [u8; 16],
        /// Which kind of difference was found, with its numbers.
        detail: TransmissionMismatchDetail,
    },
    /// The comparison could not be made.
    NotVerifiable {
        /// Cause that prevented the level-1 comparison.
        reason: TransmissionNotVerifiableReason,
    },
    /// The whole track played in shared mode: Windows owns the samples after us.
    NotApplicableInSharedMode,
}

/// What exactly differs in a level-1 mismatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransmissionMismatchDetail {
    /// Same frame count, different digest.
    DigestDiffers,
    /// The two sides hashed different numbers of frames.
    FrameCountDiffers {
        /// Frames hashed on the decoder side.
        decoder_frames: u64,
        /// Frames hashed on the device side.
        device_frames: u64,
    },
    /// A container padding bit read back from the device buffer was not zero.
    NonZeroPaddingBits {
        /// Frame where the bit was found.
        first_frame_index: u64,
    },
}

/// Why level 1 could not conclude.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransmissionNotVerifiableReason {
    /// Playback stopped before the track ended.
    StoppedBeforeTrackEnd,
    /// The verification tap overflowed, so some device bytes were not hashed.
    VerificationTapOverflowed,
    /// Part of the segment played in shared mode (fallback on resume, or mode switched).
    PartlyPlayedThroughSystemMixer,
    /// A pause could not play out the queued audio before releasing the device.
    QueuedAudioLostOnPause,
    /// Device-side frames had a gap or an overlap, or the two sides describe different
    /// segments. Never expected; a safety net so no gap can produce a false match.
    SegmentsNotAligned,
}

/// Level 2: does the decoded PCM hash to the file's STREAMINFO MD5? Runs in both modes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileIntegrityStatus {
    /// Decoded frames hashed so far.
    InProgress {
        /// Count of frames already fed to the hasher, for the progress display.
        decoded_frames_hashed: u64,
    },
    /// Decoded PCM of the whole track equals the STREAMINFO MD5.
    Verified {
        /// MD5 of the verified frames, equal on both sides.
        md5: [u8; 16],
    },
    /// The decoded PCM does not match the embedded signature.
    Mismatch {
        /// Digest stored in the file.
        expected_md5: [u8; 16],
        /// Digest of the decoded PCM.
        computed_md5: [u8; 16],
        /// Which kind of difference was found, with its numbers.
        detail: FileMismatchDetail,
    },
    /// STREAMINFO MD5 is all zeros (RFC 9639 §8.2: "not known"). Not a failure. The digest of
    /// the decoded PCM is given when the whole track was decoded, for comparison elsewhere.
    NoEmbeddedSignature {
        /// Digest of the decoded PCM, if the whole track was decoded.
        computed_md5: Option<[u8; 16]>,
    },
    /// The comparison could not be made.
    NotVerifiable {
        /// Cause that prevented the level-2 comparison.
        reason: FileIntegrityNotVerifiableReason,
    },
}

/// What exactly differs in a level-2 mismatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileMismatchDetail {
    /// Same frame count, different digest.
    DigestDiffers,
    /// The file declares a different number of frames than were decoded.
    FrameCountDiffers {
        /// Frames declared by the file.
        expected: u64,
        /// Frames actually decoded.
        decoded: u64,
    },
}

/// Why level 2 could not conclude.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileIntegrityNotVerifiableReason {
    /// A seek away from frame 0 happened.
    PlaybackRepositioned,
    /// The decoder had not reached the end.
    StoppedBeforeTrackEnd,
    /// The decoder reported an error inside the track.
    DecodeErrorInTrack,
}

/// How a decoder-side segment ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodedSegmentEnd {
    /// The decoder reached the end of the stream.
    EndOfStream,
    /// The decoder failed.
    DecodeError,
    /// Stop, seek or close.
    Abandoned,
}

/// Digest and extent of one decoder-side segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedSegmentReport {
    /// Track the segment belongs to.
    pub track_id: TrackId,
    /// Flush generation of the segment.
    pub flush_generation: u64,
    /// First frame of the segment within the track.
    pub first_frame_index: u64,
    /// Number of frames hashed.
    pub frame_count: u64,
    /// Digest in the canonical layout.
    pub md5: [u8; 16],
    /// How the segment ended.
    pub ended_by: DecodedSegmentEnd,
}

/// Messages to the verifier thread, from the engine and the decode thread.
#[derive(Debug)]
pub enum VerifierCommand {
    /// A track starts; carries the facts level 2 compares against.
    BeginTrack {
        /// Track that starts.
        track_id: TrackId,
        /// Embedded MD5, if the file has one.
        expected_md5: Option<[u8; 16]>,
        /// Frame count declared by the file, if any.
        expected_total_frames: Option<u64>,
        /// Source bits per sample, for the canonical layout.
        source_bits: u8,
    },
    /// The decode thread finished a segment.
    DecodedSegmentFinished(DecodedSegmentReport),
    /// From the engine, when `ClosedStream::was_queued_audio_fully_played` is false.
    QueuedAudioLost {
        /// Track that lost audio.
        track_id: TrackId,
    },
    /// From the engine, when the track stops before its end (stop, device failure).
    TrackStopped {
        /// Track that stopped.
        track_id: TrackId,
    },
    /// Ends the verifier thread.
    Shutdown,
}

/// Events the feeder pushes into the verification tap, next to the copied bytes
/// (`pistolame-core/src/verification/tap.rs`). Small `Copy` values; pushing never blocks.
///
/// Rules the verifier (T7) relies on, in both access modes:
/// - Every segment is announced by `SegmentStarted` (with its real first frame) and closed by
///   `TrackEnded` when its track ends; only `DeviceBytes` are limited to exclusive mode.
/// - A segment was played (partly) through the system mixer if the latest `StreamBegan` before
///   its `SegmentStarted` is in shared mode, or if a shared-mode `StreamBegan` falls between its
///   `SegmentStarted` and its `TrackEnded`. If every frame of it played in shared mode it has no
///   `DeviceBytes` at all. Frames played in shared mode leave a gap in the `DeviceBytes` frame
///   indices; that gap is explained by the shared `StreamBegan`, not a misalignment.
/// - `TapOverflowed` means something was lost at that point (bytes or events). It taints every
///   segment open at that point, whatever its `track_id`. A `DeviceBytes` or `TrackEnded` for a
///   segment whose `SegmentStarted` was never received only follows a `TapOverflowed`.
/// - The latest `StreamBegan` is never lost: when the verifier lags, an older unsent one can only
///   be replaced by a newer one, and a `TapOverflowed` then follows it. If the replaced one was
///   a shared-mode start, the segment's level 1 reads `VerificationTapOverflowed` rather than
///   `PartlyPlayedThroughSystemMixer`: still `NotVerifiable`, only the stated reason differs,
///   which is acceptable for a case that needs the verifier to lag behind several reopens.
/// - `DeviceBytes` and `TrackEnded` belong to the segment of the latest `SegmentStarted`. One
///   that names another track never opens an implicit segment: the open segment becomes
///   `NotVerifiable { VerificationTapOverflowed }` (only an overflow can cause it).
/// - A new `SegmentStarted` for the same track (new flush generation) ends the previous
///   segment without a `TrackEnded`: that segment was abandoned by a seek.
/// - Tap events and `VerifierCommand`s travel on separate, mutually unordered channels. Before
///   acting on a `TrackStopped` or `QueuedAudioLost` for a track, the verifier drains the tap,
///   so device-side events emitted before the command are not judged after it.
/// - After the stream closes, the engine calls `DeviceFeeder::flush_verification_events` until
///   it returns `true`, so a final `TrackEnded` that was waiting for room is not stranded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapEvent {
    /// Emitted from `begin_stream`: a stream (re)opened. The `DeviceBytes` that follow use this
    /// layout. In shared mode no `DeviceBytes` follow until the next exclusive stream.
    StreamBegan {
        /// Layout of the following device bytes.
        render_format: DeviceSampleFormat,
        /// Access mode of the new stream.
        access_mode: DeviceAccessMode,
    },
    /// A block of a new (track, flush generation) pair is about to be rendered.
    SegmentStarted {
        /// Track of the segment.
        track_id: TrackId,
        /// Flush generation of the segment.
        flush_generation: u64,
        /// First frame of the segment within the track.
        first_frame_index: u64,
    },
    /// `frame_count` frames of the current segment, starting at `first_frame_index`, were
    /// copied into the bytes ring as `byte_count` bytes, pushed before this event. Silence the
    /// feeder inserts is never described here.
    DeviceBytes {
        /// Track of the frames.
        track_id: TrackId,
        /// First copied frame within the track.
        first_frame_index: u64,
        /// Number of copied frames.
        frame_count: u32,
        /// Number of bytes to read from the bytes ring for these frames. Lets the verifier
        /// consume exactly the right bytes even if it misread the layout.
        byte_count: usize,
    },
    /// After the last frame of a block with `is_track_end`.
    TrackEnded {
        /// Track that ended.
        track_id: TrackId,
    },
    /// Bytes or events were dropped (tap full). Emitted as soon as the event ring has room;
    /// sticky until then. Taints every segment open at this point (see the rules above).
    TapOverflowed {
        /// Track of the first dropped item, for diagnostics; `None` if no track had been
        /// rendered yet. Not used to decide which segments are tainted.
        track_id: Option<TrackId>,
    },
}
