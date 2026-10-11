//! The verifier thread of one playback pipeline (plan B7.2): it reads the verification tap
//! (device side) and the decoder reports (decoder side), and concludes both levels.
//!
//! The rules live in [`TrackVerifier`], a state machine with no thread and no channel, so tests
//! feed it events by hand. [`spawn_verifier`] only adds the polling loop around it.

use std::collections::BTreeMap;
use std::time::Instant;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use md5::{Digest, Md5};

use crate::format::{DeviceSampleFormat, StreamFormat};
use crate::output_contract::DeviceAccessMode;
use crate::pcm::write_canonical_md5_bytes;
use crate::pcm::{UnpackError, unpack_samples_from_container, validate_pcm_layout};
use crate::playback_ring::TrackId;
use crate::timing::{VERIFICATION_PROGRESS_INTERVAL, VERIFIER_POLL_INTERVAL};
use crate::verification::{
    DecodedSegmentEnd, DecodedSegmentReport, FileIntegrityNotVerifiableReason, FileIntegrityStatus,
    FileMismatchDetail, TapEvent, TransmissionMismatchDetail, TransmissionNotVerifiableReason,
    TransmissionStatus, VerificationStatus, VerificationTapReader, VerifierCommand,
};

/// Name of the verifier thread, as seen in debuggers and crash reports.
const VERIFIER_THREAD_NAME: &str = "pistolame-verifier";

/// Number of bits in a byte, for the canonical sample width.
const BITS_PER_BYTE: usize = 8;

/// The facts both levels compare against, fixed for the pipeline's life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedTrack {
    /// Track of the pipeline.
    pub track_id: TrackId,
    /// Source bits (unpacking, canonical layout) and channel count (frame arithmetic).
    pub source_format: StreamFormat,
    /// `SourceStreamInfo::embedded_md5`.
    pub expected_md5: Option<[u8; 16]>,
    /// `SourceStreamInfo::total_frames`.
    pub expected_total_frames: Option<u64>,
}

/// What the verifier publishes; the engine forwards it as `EngineEvent::VerificationChanged`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationUpdate {
    /// Track the status is about.
    pub track_id: TrackId,
    /// Both levels, as they stand now.
    pub status: VerificationStatus,
}

/// Spawns the verifier thread ("pistolame-verifier") of one pipeline. It returns after
/// `Shutdown`, or when `commands` disconnects (treated as `Shutdown`), or when `updates`
/// disconnects (the engine is gone). Never blocks on the tap.
pub fn spawn_verifier(
    track: VerifiedTrack,
    tap_reader: VerificationTapReader,
    commands: Receiver<VerifierCommand>,
    updates: Sender<VerificationUpdate>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(VERIFIER_THREAD_NAME.to_owned())
        .spawn(move || run_verifier(track, tap_reader, commands, updates))
}

/// The verifier thread's loop (B7.2 "Command rules": drain the tap before acting on an engine
/// command, since the engine sends it only after the feeder was flushed).
fn run_verifier(
    track: VerifiedTrack,
    mut tap_reader: VerificationTapReader,
    commands: Receiver<VerifierCommand>,
    updates: Sender<VerificationUpdate>,
) {
    let mut verifier = TrackVerifier::new(track);
    let mut device_bytes = Vec::new();
    loop {
        match commands.recv_timeout(VERIFIER_POLL_INTERVAL) {
            Ok(
                command @ (VerifierCommand::DecodedSegmentProgress { .. }
                | VerifierCommand::DecodedSegmentFinished(_)),
            ) => {
                verifier.on_command(command);
                drain_tap(&mut tap_reader, &mut verifier, &mut device_bytes);
            }
            Ok(VerifierCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                drain_tap(&mut tap_reader, &mut verifier, &mut device_bytes);
                verifier.on_command(VerifierCommand::Shutdown);
                // The final status is always sent; a send error only means nobody listens.
                let _ = updates.send(VerificationUpdate {
                    track_id: track.track_id,
                    status: verifier.status(),
                });
                return;
            }
            Ok(command) => {
                drain_tap(&mut tap_reader, &mut verifier, &mut device_bytes);
                verifier.on_command(command);
            }
            Err(RecvTimeoutError::Timeout) => {
                drain_tap(&mut tap_reader, &mut verifier, &mut device_bytes);
            }
        }
        if let Some(status) = verifier.take_update(Instant::now()) {
            let update = VerificationUpdate {
                track_id: track.track_id,
                status,
            };
            if updates.send(update).is_err() {
                return;
            }
        }
    }
}

/// Feeds every event waiting in the tap to `verifier`. For a `DeviceBytes`, exactly its
/// `byte_count` bytes are popped first (into the reused `device_bytes`), so the byte stream
/// stays in step even when the event's layout is wrong.
pub(crate) fn drain_tap(
    tap_reader: &mut VerificationTapReader,
    verifier: &mut TrackVerifier,
    device_bytes: &mut Vec<u8>,
) {
    while let Some(event) = tap_reader.pop_event() {
        match event {
            TapEvent::DeviceBytes { byte_count, .. } => {
                device_bytes.resize(byte_count, 0);
                match tap_reader.pop_device_bytes(device_bytes) {
                    Ok(()) => verifier.on_tap_event(event, Some(device_bytes)),
                    Err(_missing) => verifier.on_tap_event(event, None),
                }
            }
            _ => verifier.on_tap_event(event, None),
        }
    }
}

/// The render format and access mode of the latest `StreamBegan`.
#[derive(Clone, Copy, Debug)]
struct LatestStream {
    render_format: DeviceSampleFormat,
    access_mode: DeviceAccessMode,
    /// `validate_pcm_layout(source_bits, format)` passes and the channel counts agree.
    is_usable: bool,
}

/// Device-side state of the segment being played (B7.2 "Verifier state").
struct OpenSegment {
    flush_generation: u64,
    first_frame_index: u64,
    next_frame_index: u64,
    device_frames_hashed: u64,
    device_md5: Md5,
    /// The mixer has had the stream since the last tapped frame.
    is_mixer_in_effect: bool,
    has_frames_through_mixer: bool,
    is_tainted_by_overflow: bool,
    is_misaligned: bool,
    first_non_zero_padding_frame: Option<u64>,
    has_lost_queued_audio: bool,
    has_track_ended: bool,
}

/// Why the open segment is concluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConclusionCause {
    /// `has_track_ended` and the report of its generation is present.
    TrackEnded,
    /// `TrackStopped` or `Shutdown` before `TrackEnded`.
    Stopped,
    /// `Shutdown` after `TrackEnded` with no report: judged as it stands (row 6 at worst).
    ShutdownWithoutReport,
}

/// The B7.2 state machine of one pipeline: no thread, no channel. Feed it tap events and
/// commands in order; read `status`, or `take_update` for what to publish.
pub(crate) struct TrackVerifier {
    track: VerifiedTrack,
    latest_stream: Option<LatestStream>,
    open_segment: Option<OpenSegment>,
    /// A `TapOverflowed` (or a `DeviceBytes` only an overflow explains) arrived while no
    /// segment was open.
    has_unattributed_overflow: bool,
    /// A `TapBytesMissing`: the byte stream is out of step for good.
    is_byte_stream_out_of_step: bool,
    decoder_reports: BTreeMap<u64, DecodedSegmentReport>,
    transmission: TransmissionStatus,
    file_integrity: FileIntegrityStatus,
    last_concluded_transmission: Option<TransmissionStatus>,
    last_published: VerificationStatus,
    last_progress_published_at: Option<Instant>,
    unpacked_samples: Vec<i32>,
    canonical_bytes: Vec<u8>,
}

impl TrackVerifier {
    /// A verifier with both levels `InProgress { 0 }` and nothing seen yet.
    pub(crate) fn new(track: VerifiedTrack) -> Self {
        let initial = VerificationStatus {
            transmission: TransmissionStatus::InProgress {
                device_frames_hashed: 0,
            },
            file_integrity: FileIntegrityStatus::InProgress {
                decoded_frames_hashed: 0,
            },
        };
        Self {
            track,
            latest_stream: None,
            open_segment: None,
            has_unattributed_overflow: false,
            is_byte_stream_out_of_step: false,
            decoder_reports: BTreeMap::new(),
            transmission: initial.transmission.clone(),
            file_integrity: initial.file_integrity.clone(),
            last_concluded_transmission: None,
            last_published: initial,
            last_progress_published_at: None,
            unpacked_samples: Vec::new(),
            canonical_bytes: Vec::new(),
        }
    }

    /// Applies one tap event (B7.2 "Event rules"). `device_bytes` is `Some(bytes)` for
    /// `DeviceBytes` (exactly `byte_count` bytes popped by the caller), `None` for other
    /// events, and `None` with `DeviceBytes` when the bytes were missing (`TapBytesMissing`).
    pub(crate) fn on_tap_event(&mut self, event: TapEvent, device_bytes: Option<&[u8]>) {
        match event {
            TapEvent::StreamBegan {
                render_format,
                access_mode,
            } => self.on_stream_began(render_format, access_mode),
            TapEvent::SegmentStarted {
                track_id,
                flush_generation,
                first_frame_index,
            } => self.on_segment_started(track_id, flush_generation, first_frame_index),
            TapEvent::DeviceBytes {
                track_id,
                first_frame_index,
                frame_count,
                byte_count,
            } => {
                let complete_bytes = device_bytes.filter(|bytes| bytes.len() == byte_count);
                self.on_device_bytes(track_id, first_frame_index, frame_count, complete_bytes);
            }
            TapEvent::TrackEnded { track_id } => self.on_track_ended(track_id),
            TapEvent::TapOverflowed { track_id: _ } => self.on_tap_overflowed(),
        }
    }

    /// Applies one command (B7.2 "Command rules"). The caller drains the tap before
    /// `QueuedAudioLost`, `TrackStopped` and `Shutdown`.
    pub(crate) fn on_command(&mut self, command: VerifierCommand) {
        match command {
            VerifierCommand::DecodedSegmentProgress {
                track_id,
                frames_hashed,
                ..
            } => self.on_decoded_progress(track_id, frames_hashed),
            VerifierCommand::DecodedSegmentFinished(report) => self.on_decoded_report(report),
            VerifierCommand::QueuedAudioLost { .. } => self.on_queued_audio_lost(),
            VerifierCommand::TrackStopped { .. } => self.on_track_stopped(),
            VerifierCommand::Shutdown => self.on_shutdown(),
        }
    }

    /// Both levels as they stand now.
    pub(crate) fn status(&self) -> VerificationStatus {
        VerificationStatus {
            transmission: self.transmission.clone(),
            file_integrity: self.file_integrity.clone(),
        }
    }

    /// `Some` when the status changed in a way to publish now: a concluded value at once, an
    /// `InProgress` value only if `VERIFICATION_PROGRESS_INTERVAL` passed since the last
    /// publication (`now` is passed in, so tests control time).
    pub(crate) fn take_update(&mut self, now: Instant) -> Option<VerificationStatus> {
        let status = self.status();
        if status == self.last_published {
            return None;
        }
        let is_progress_only = without_progress(&status) == without_progress(&self.last_published);
        if is_progress_only {
            let is_interval_elapsed = self.last_progress_published_at.is_none_or(|published_at| {
                now.saturating_duration_since(published_at) >= VERIFICATION_PROGRESS_INTERVAL
            });
            if !is_interval_elapsed {
                return None;
            }
        }
        self.last_progress_published_at = Some(now);
        self.last_published = status.clone();
        Some(status)
    }

    fn source_bits(&self) -> u8 {
        self.track.source_format.bits_per_sample
    }

    fn channel_count(&self) -> u8 {
        self.track.source_format.channel_count
    }

    fn is_latest_stream_shared(&self) -> bool {
        self.latest_stream
            .is_some_and(|stream| stream.access_mode == DeviceAccessMode::SharedThroughSystemMixer)
    }

    /// The open segment, if it has not seen its `TrackEnded` yet.
    fn open_segment_not_ended(&mut self) -> Option<&mut OpenSegment> {
        self.open_segment
            .as_mut()
            .filter(|segment| !segment.has_track_ended)
    }

    fn on_stream_began(
        &mut self,
        render_format: DeviceSampleFormat,
        access_mode: DeviceAccessMode,
    ) {
        let is_usable = validate_pcm_layout(self.source_bits(), &render_format).is_ok()
            && render_format.channel_count == self.channel_count();
        self.latest_stream = Some(LatestStream {
            render_format,
            access_mode,
            is_usable,
        });
        if access_mode == DeviceAccessMode::SharedThroughSystemMixer
            && let Some(segment) = self.open_segment_not_ended()
        {
            segment.is_mixer_in_effect = true;
        }
    }

    fn on_segment_started(
        &mut self,
        track_id: TrackId,
        flush_generation: u64,
        first_frame_index: u64,
    ) {
        // An open segment is dropped with no status of its own: it was abandoned by a seek. One
        // that had already ended is dropped too: level 1 reports on the latest segment.
        self.open_segment = Some(OpenSegment {
            flush_generation,
            first_frame_index,
            next_frame_index: first_frame_index,
            device_frames_hashed: 0,
            device_md5: Md5::new(),
            is_mixer_in_effect: self.is_latest_stream_shared(),
            has_frames_through_mixer: false,
            is_tainted_by_overflow: false,
            is_misaligned: track_id != self.track.track_id || self.is_byte_stream_out_of_step,
            first_non_zero_padding_frame: None,
            has_lost_queued_audio: false,
            has_track_ended: false,
        });
        self.has_unattributed_overflow = false;
        self.transmission = TransmissionStatus::InProgress {
            device_frames_hashed: 0,
        };
    }

    fn on_device_bytes(
        &mut self,
        track_id: TrackId,
        first_frame_index: u64,
        frame_count: u32,
        device_bytes: Option<&[u8]>,
    ) {
        let Some(device_bytes) = device_bytes else {
            // `TapBytesMissing`: this and every later segment of the pipeline are misaligned.
            self.is_byte_stream_out_of_step = true;
            if let Some(segment) = self.open_segment.as_mut() {
                segment.is_misaligned = true;
            }
            return;
        };
        let latest_stream = self.latest_stream;
        let is_other_track = track_id != self.track.track_id;
        let Some(segment) = self.open_segment_not_ended() else {
            // Only an overflow can explain bytes outside a segment.
            self.has_unattributed_overflow = true;
            return;
        };
        if is_other_track {
            segment.is_tainted_by_overflow = true;
            return;
        }
        let is_layout_consistent = latest_stream.is_some_and(|stream| {
            stream.access_mode == DeviceAccessMode::Exclusive
                && stream.is_usable
                && usize::try_from(frame_count)
                    .ok()
                    .and_then(|frames| frames.checked_mul(stream.render_format.bytes_per_frame()))
                    == Some(device_bytes.len())
        });
        let mut should_hash = false;
        if !is_layout_consistent || first_frame_index < segment.next_frame_index {
            segment.is_misaligned = true;
        } else if first_frame_index > segment.next_frame_index {
            if segment.is_mixer_in_effect {
                segment.has_frames_through_mixer = true;
            } else {
                segment.is_misaligned = true;
            }
        } else {
            should_hash = !segment.is_misaligned && segment.first_non_zero_padding_frame.is_none();
        }
        if should_hash && let Some(stream) = latest_stream {
            self.hash_device_bytes(device_bytes, first_frame_index, &stream.render_format);
        }
        if let Some(segment) = self.open_segment.as_mut() {
            segment.next_frame_index = first_frame_index.saturating_add(u64::from(frame_count));
            segment.device_frames_hashed = segment
                .device_frames_hashed
                .saturating_add(u64::from(frame_count));
            segment.is_mixer_in_effect = false;
            self.transmission = TransmissionStatus::InProgress {
                device_frames_hashed: segment.device_frames_hashed,
            };
        }
    }

    /// Unpacks the device bytes (checking the padding bits), re-encodes them in the canonical
    /// layout and feeds the open segment's MD5.
    fn hash_device_bytes(
        &mut self,
        device_bytes: &[u8],
        first_frame_index: u64,
        render_format: &DeviceSampleFormat,
    ) {
        let source_bits = self.source_bits();
        let channel_count = usize::from(self.channel_count()).max(1);
        let sample_count = device_bytes.len() / usize::from(render_format.container_bytes).max(1);
        self.unpacked_samples.resize(sample_count, 0);
        let canonical_byte_count =
            sample_count.saturating_mul(usize::from(source_bits).div_ceil(BITS_PER_BYTE));
        self.canonical_bytes.resize(canonical_byte_count, 0);
        let unpacked = unpack_samples_from_container(
            device_bytes,
            source_bits,
            render_format,
            &mut self.unpacked_samples,
        );
        let Some(segment) = self.open_segment.as_mut() else {
            return;
        };
        match unpacked {
            Ok(()) => {}
            Err(UnpackError::InvalidPadding { sample_index }) => {
                let frame_offset = (sample_index / channel_count) as u64;
                segment.first_non_zero_padding_frame =
                    Some(first_frame_index.saturating_add(frame_offset));
                return;
            }
            Err(UnpackError::InvalidLayout(_) | UnpackError::LengthMismatch { .. }) => {
                segment.is_misaligned = true;
                return;
            }
        }
        match write_canonical_md5_bytes(
            &self.unpacked_samples,
            source_bits,
            &mut self.canonical_bytes,
        ) {
            Ok(()) => segment.device_md5.update(&self.canonical_bytes),
            Err(_never_expected) => segment.is_misaligned = true,
        }
    }

    fn on_track_ended(&mut self, track_id: TrackId) {
        let is_other_track = track_id != self.track.track_id;
        let Some(segment) = self.open_segment_not_ended() else {
            let reason = if self.has_unattributed_overflow {
                TransmissionNotVerifiableReason::VerificationTapOverflowed
            } else {
                TransmissionNotVerifiableReason::SegmentsNotAligned
            };
            self.open_segment = None;
            self.set_concluded_transmission(TransmissionStatus::NotVerifiable { reason });
            return;
        };
        if is_other_track {
            segment.is_tainted_by_overflow = true;
        }
        segment.has_track_ended = true;
        self.try_conclude_ended_segment();
    }

    fn on_tap_overflowed(&mut self) {
        match self.open_segment_not_ended() {
            Some(segment) => segment.is_tainted_by_overflow = true,
            None => self.has_unattributed_overflow = true,
        }
    }

    fn on_decoded_progress(&mut self, track_id: TrackId, frames_hashed: u64) {
        if track_id != self.track.track_id {
            return;
        }
        if matches!(self.file_integrity, FileIntegrityStatus::InProgress { .. }) {
            self.file_integrity = FileIntegrityStatus::InProgress {
                decoded_frames_hashed: frames_hashed,
            };
        }
    }

    fn on_decoded_report(&mut self, report: DecodedSegmentReport) {
        // A report of another track cannot describe this pipeline's segments; ignoring it can
        // only leave a level unconcluded, never verified.
        if report.track_id != self.track.track_id {
            return;
        }
        // The first report of a generation is the real one; a later one (a teardown calling
        // `finish_segment` again) would replace it with an empty segment.
        if self.decoder_reports.contains_key(&report.flush_generation) {
            return;
        }
        self.decoder_reports.insert(report.flush_generation, report);
        self.apply_file_integrity_table(&report);
        self.try_conclude_ended_segment();
    }

    fn on_queued_audio_lost(&mut self) {
        if let Some(segment) = self.open_segment.as_mut() {
            segment.has_lost_queued_audio = true;
            return;
        }
        if matches!(
            self.last_concluded_transmission,
            Some(TransmissionStatus::Verified { .. })
        ) {
            self.set_concluded_transmission(TransmissionStatus::NotVerifiable {
                reason: TransmissionNotVerifiableReason::QueuedAudioLostOnPause,
            });
        }
    }

    fn on_track_stopped(&mut self) {
        if self.open_segment_not_ended().is_some() {
            self.conclude_open_segment(ConclusionCause::Stopped);
        } else if self.open_segment.is_none() && self.last_concluded_transmission.is_none() {
            // Stopped before any segment was announced: nothing reached the device, so level 1
            // must not stay `InProgress` for good.
            self.set_concluded_transmission(TransmissionStatus::NotVerifiable {
                reason: TransmissionNotVerifiableReason::StoppedBeforeTrackEnd,
            });
        }
        self.conclude_file_integrity_on_stop();
    }

    /// Level 2 after a stop: keep a final verdict, and keep `PlaybackRepositioned` when the
    /// decoder did reach the end (a stop right at the end of a sought track); otherwise the
    /// decoder had not reached the end.
    fn conclude_file_integrity_on_stop(&mut self) {
        if self.has_final_file_integrity_verdict() {
            return;
        }
        let has_decoder_reached_the_end = self
            .decoder_reports
            .values()
            .next_back()
            .is_some_and(|report| report.ended_by == DecodedSegmentEnd::EndOfStream);
        let is_repositioned = self.file_integrity
            == FileIntegrityStatus::NotVerifiable {
                reason: FileIntegrityNotVerifiableReason::PlaybackRepositioned,
            };
        if is_repositioned && has_decoder_reached_the_end {
            return;
        }
        self.file_integrity = FileIntegrityStatus::NotVerifiable {
            reason: FileIntegrityNotVerifiableReason::StoppedBeforeTrackEnd,
        };
    }

    fn on_shutdown(&mut self) {
        if self.open_segment_not_ended().is_some() {
            self.on_track_stopped();
        } else if self.open_segment.is_some() {
            // Ended, but the report never came: never a match, judged as it stands.
            self.conclude_open_segment(ConclusionCause::ShutdownWithoutReport);
        }
        // Shutdown publishes the final status: no level may stay `InProgress`.
        if matches!(self.transmission, TransmissionStatus::InProgress { .. }) {
            self.set_concluded_transmission(TransmissionStatus::NotVerifiable {
                reason: TransmissionNotVerifiableReason::StoppedBeforeTrackEnd,
            });
        }
        self.conclude_file_integrity_on_stop();
    }

    /// Concludes the open segment if it has ended and the report of its generation is here.
    fn try_conclude_ended_segment(&mut self) {
        let is_ready = self.open_segment.as_ref().is_some_and(|segment| {
            segment.has_track_ended && self.decoder_reports.contains_key(&segment.flush_generation)
        });
        if is_ready {
            self.conclude_open_segment(ConclusionCause::TrackEnded);
        }
    }

    /// Closes the open segment and sets level 1 from the B7.2 level-1 table.
    fn conclude_open_segment(&mut self, cause: ConclusionCause) {
        let Some(segment) = self.open_segment.take() else {
            return;
        };
        let report = self.decoder_reports.get(&segment.flush_generation).copied();
        let outcome = level_one_outcome(segment, report, cause);
        self.set_concluded_transmission(outcome);
    }

    fn set_concluded_transmission(&mut self, outcome: TransmissionStatus) {
        self.last_concluded_transmission = Some(outcome.clone());
        self.transmission = outcome;
    }

    /// One of the last four rows of the level-2 table, or `DecodeErrorInTrack`.
    fn has_final_file_integrity_verdict(&self) -> bool {
        match self.file_integrity {
            FileIntegrityStatus::InProgress { .. } => false,
            FileIntegrityStatus::Verified { .. }
            | FileIntegrityStatus::Mismatch { .. }
            | FileIntegrityStatus::NoEmbeddedSignature { .. } => true,
            FileIntegrityStatus::NotVerifiable { reason } => {
                reason == FileIntegrityNotVerifiableReason::DecodeErrorInTrack
            }
        }
    }

    /// The B7.2 level-2 table, first matching row wins.
    fn apply_file_integrity_table(&mut self, report: &DecodedSegmentReport) {
        if self.has_final_file_integrity_verdict() {
            return;
        }
        let computed_md5 = report.md5;
        self.file_integrity = match report.ended_by {
            DecodedSegmentEnd::Abandoned => return,
            DecodedSegmentEnd::DecodeError => FileIntegrityStatus::NotVerifiable {
                reason: FileIntegrityNotVerifiableReason::DecodeErrorInTrack,
            },
            DecodedSegmentEnd::EndOfStream if report.first_frame_index != 0 => {
                FileIntegrityStatus::NotVerifiable {
                    reason: FileIntegrityNotVerifiableReason::PlaybackRepositioned,
                }
            }
            DecodedSegmentEnd::EndOfStream => {
                match (self.track.expected_total_frames, self.track.expected_md5) {
                    (Some(expected), expected_md5) if expected != report.frame_count => {
                        FileIntegrityStatus::Mismatch {
                            // `None` means the file stores all zeros (RFC 9639 section 8.2).
                            expected_md5: expected_md5.unwrap_or([0; 16]),
                            computed_md5,
                            detail: FileMismatchDetail::FrameCountDiffers {
                                expected,
                                decoded: report.frame_count,
                            },
                        }
                    }
                    (_, None) => FileIntegrityStatus::NoEmbeddedSignature {
                        computed_md5: Some(computed_md5),
                    },
                    (_, Some(expected_md5)) if expected_md5 == computed_md5 => {
                        FileIntegrityStatus::Verified { md5: computed_md5 }
                    }
                    (_, Some(expected_md5)) => FileIntegrityStatus::Mismatch {
                        expected_md5,
                        computed_md5,
                        detail: FileMismatchDetail::DigestDiffers,
                    },
                }
            }
        };
    }
}

/// The B7.2 level-1 table, first matching row wins.
fn level_one_outcome(
    segment: OpenSegment,
    report: Option<DecodedSegmentReport>,
    cause: ConclusionCause,
) -> TransmissionStatus {
    use TransmissionNotVerifiableReason as Reason;
    let not_verifiable = |reason| TransmissionStatus::NotVerifiable { reason };
    let had_mixer = segment.is_mixer_in_effect || segment.has_frames_through_mixer;
    // Row 1.
    if segment.device_frames_hashed == 0 && had_mixer && !segment.is_tainted_by_overflow {
        return TransmissionStatus::NotApplicableInSharedMode;
    }
    // Row 2.
    if cause == ConclusionCause::Stopped {
        return not_verifiable(Reason::StoppedBeforeTrackEnd);
    }
    // Row 3.
    if segment.is_tainted_by_overflow {
        return not_verifiable(Reason::VerificationTapOverflowed);
    }
    let decoder_end =
        report.map(|report| report.first_frame_index.saturating_add(report.frame_count));
    // Row 4.
    let is_short_under_mixer =
        segment.is_mixer_in_effect && decoder_end.is_some_and(|end| segment.next_frame_index < end);
    if segment.has_frames_through_mixer || is_short_under_mixer {
        return not_verifiable(Reason::PartlyPlayedThroughSystemMixer);
    }
    // Row 5.
    if segment.has_lost_queued_audio {
        return not_verifiable(Reason::QueuedAudioLostOnPause);
    }
    // Row 6.
    let Some(report) = report.filter(|report| {
        !segment.is_misaligned && report.first_frame_index == segment.first_frame_index
    }) else {
        return not_verifiable(Reason::SegmentsNotAligned);
    };
    // Row 7.
    if report.frame_count == 0 {
        return not_verifiable(Reason::EmptySegment);
    }
    let device_md5: [u8; 16] = segment.device_md5.finalize().into();
    let mismatch = |detail| TransmissionStatus::Mismatch {
        decoder_md5: report.md5,
        device_md5,
        detail,
    };
    // Row 8.
    if let Some(first_frame_index) = segment.first_non_zero_padding_frame {
        return mismatch(TransmissionMismatchDetail::NonZeroPaddingBits { first_frame_index });
    }
    // Row 9.
    let decoder_end = report.first_frame_index.saturating_add(report.frame_count);
    if segment.next_frame_index != decoder_end {
        return mismatch(TransmissionMismatchDetail::FrameCountDiffers {
            decoder_frames: report.frame_count,
            device_frames: segment.device_frames_hashed,
        });
    }
    // Rows 10 and 11.
    if device_md5 != report.md5 {
        return mismatch(TransmissionMismatchDetail::DigestDiffers);
    }
    TransmissionStatus::Verified {
        first_frame_index: segment.first_frame_index,
        frame_count: report.frame_count,
        md5: device_md5,
    }
}

/// `status` with every progress counter set to zero: two statuses that differ only in their
/// counters are the same "kind" of status, published at most every progress interval.
fn without_progress(status: &VerificationStatus) -> VerificationStatus {
    let transmission = match status.transmission {
        TransmissionStatus::InProgress { .. } => TransmissionStatus::InProgress {
            device_frames_hashed: 0,
        },
        ref concluded => concluded.clone(),
    };
    let file_integrity = match status.file_integrity {
        FileIntegrityStatus::InProgress { .. } => FileIntegrityStatus::InProgress {
            decoded_frames_hashed: 0,
        },
        ref concluded => concluded.clone(),
    };
    VerificationStatus {
        transmission,
        file_integrity,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::format::{ChannelMask, ValidBitsAlignment};
    use crate::pcm::pack_samples_into_container;
    use crate::verification::DecodedPcmHasher;
    use crate::verification::tap::tap_with_capacities;

    const TRACK: TrackId = TrackId(9);
    const OTHER_TRACK: TrackId = TrackId(10);
    const SOURCE_BITS: u8 = 24;
    const CHANNEL_COUNT: u8 = 2;
    const SAMPLE_RATE_HZ: u32 = 48_000;
    /// Length of the test track, in frames.
    const TRACK_FRAMES: u64 = 400;
    const EXCLUSIVE: DeviceAccessMode = DeviceAccessMode::Exclusive;
    const SHARED: DeviceAccessMode = DeviceAccessMode::SharedThroughSystemMixer;
    /// Long enough for a loaded test machine; the thread answers in a few milliseconds.
    const THREAD_TEST_TIMEOUT: Duration = Duration::from_secs(10);

    use DecodedSegmentEnd::{Abandoned, DecodeError, EndOfStream};
    use TransmissionNotVerifiableReason as Reason;

    fn source_format() -> StreamFormat {
        StreamFormat {
            sample_rate_hz: SAMPLE_RATE_HZ,
            bits_per_sample: SOURCE_BITS,
            channel_count: CHANNEL_COUNT,
            channel_mask: ChannelMask(0b11),
        }
    }

    fn render_format(container_bytes: u8) -> DeviceSampleFormat {
        DeviceSampleFormat {
            sample_rate_hz: SAMPLE_RATE_HZ,
            container_bytes,
            valid_bits_per_sample: container_bytes * 8,
            alignment: ValidBitsAlignment::MostSignificant,
            channel_count: CHANNEL_COUNT,
            channel_mask: ChannelMask(0b11),
        }
    }

    /// Deterministic 24-bit samples covering the whole range.
    fn samples(first_frame_index: u64, frame_count: u64) -> Vec<i32> {
        let first_sample = first_frame_index * u64::from(CHANNEL_COUNT);
        let sample_count = frame_count * u64::from(CHANNEL_COUNT);
        (first_sample..first_sample + sample_count)
            .map(|index| ((index * 7_919 + 12_345) % (1 << 24)) as i32 - (1 << 23))
            .collect()
    }

    fn decoder_report(
        flush_generation: u64,
        first_frame_index: u64,
        frame_count: u64,
        ended_by: DecodedSegmentEnd,
    ) -> DecodedSegmentReport {
        let mut hasher = DecodedPcmHasher::new(TRACK, source_format());
        hasher.begin_segment(flush_generation, first_frame_index);
        if frame_count > 0 {
            hasher
                .stage_block(first_frame_index, &samples(first_frame_index, frame_count))
                .expect("test samples fit");
            hasher.commit_staged_block();
        }
        hasher.finish_segment(ended_by)
    }

    fn whole_track_md5() -> [u8; 16] {
        decoder_report(0, 0, TRACK_FRAMES, EndOfStream).md5
    }

    fn track_facts(expected_md5: Option<[u8; 16]>) -> VerifiedTrack {
        VerifiedTrack {
            track_id: TRACK,
            source_format: source_format(),
            expected_md5,
            expected_total_frames: Some(TRACK_FRAMES),
        }
    }

    fn packed(first_frame_index: u64, frame_count: u64, container_bytes: u8) -> Vec<u8> {
        let source = samples(first_frame_index, frame_count);
        let mut bytes = vec![0u8; source.len() * usize::from(container_bytes)];
        pack_samples_into_container(
            &source,
            SOURCE_BITS,
            &render_format(container_bytes),
            &mut bytes,
        );
        bytes
    }

    /// Drives a `TrackVerifier` with the events a pipeline would produce.
    struct Session {
        verifier: TrackVerifier,
        container_bytes: u8,
    }

    impl Session {
        fn new() -> Self {
            Self::with_track(track_facts(Some(whole_track_md5())))
        }

        fn with_track(track: VerifiedTrack) -> Self {
            Self {
                verifier: TrackVerifier::new(track),
                container_bytes: 3,
            }
        }

        fn stream(&mut self, container_bytes: u8, access_mode: DeviceAccessMode) {
            self.container_bytes = container_bytes;
            self.verifier.on_tap_event(
                TapEvent::StreamBegan {
                    render_format: render_format(container_bytes),
                    access_mode,
                },
                None,
            );
        }

        fn exclusive(&mut self, container_bytes: u8) {
            self.stream(container_bytes, EXCLUSIVE);
        }

        fn shared(&mut self) {
            self.stream(4, SHARED);
        }

        fn segment_of(&mut self, track_id: TrackId, flush_generation: u64, first: u64) {
            self.verifier.on_tap_event(
                TapEvent::SegmentStarted {
                    track_id,
                    flush_generation,
                    first_frame_index: first,
                },
                None,
            );
        }

        fn segment(&mut self, flush_generation: u64, first: u64) {
            self.segment_of(TRACK, flush_generation, first);
        }

        fn play_bytes_of(&mut self, track_id: TrackId, first: u64, frames: u64, bytes: &[u8]) {
            self.verifier.on_tap_event(
                TapEvent::DeviceBytes {
                    track_id,
                    first_frame_index: first,
                    frame_count: frames as u32,
                    byte_count: bytes.len(),
                },
                Some(bytes),
            );
        }

        fn play_bytes(&mut self, first: u64, frames: u64, bytes: &[u8]) {
            self.play_bytes_of(TRACK, first, frames, bytes);
        }

        fn play(&mut self, first: u64, frames: u64) {
            let bytes = packed(first, frames, self.container_bytes);
            self.play_bytes(first, frames, &bytes);
        }

        fn end_of(&mut self, track_id: TrackId) {
            self.verifier
                .on_tap_event(TapEvent::TrackEnded { track_id }, None);
        }

        fn end(&mut self) {
            self.end_of(TRACK);
        }

        fn overflow(&mut self) {
            self.verifier
                .on_tap_event(TapEvent::TapOverflowed { track_id: None }, None);
        }

        fn report(&mut self, report: DecodedSegmentReport) {
            self.verifier
                .on_command(VerifierCommand::DecodedSegmentFinished(report));
        }

        fn finished(&mut self, flush_generation: u64, first: u64, frames: u64) {
            self.report(decoder_report(flush_generation, first, frames, EndOfStream));
        }

        fn queued_audio_lost(&mut self) {
            self.verifier
                .on_command(VerifierCommand::QueuedAudioLost { track_id: TRACK });
        }

        fn stop(&mut self) {
            self.verifier
                .on_command(VerifierCommand::TrackStopped { track_id: TRACK });
        }

        /// The whole track in exclusive mode, in two blocks, under `flush_generation`.
        fn play_whole_track(&mut self, flush_generation: u64) {
            self.exclusive(3);
            self.segment(flush_generation, 0);
            self.play(0, 150);
            self.play(150, TRACK_FRAMES - 150);
            self.finished(flush_generation, 0, TRACK_FRAMES);
            self.end();
        }

        fn transmission(&self) -> TransmissionStatus {
            self.verifier.status().transmission
        }

        fn file_integrity(&self) -> FileIntegrityStatus {
            self.verifier.status().file_integrity
        }
    }

    fn not_verifiable(reason: Reason) -> TransmissionStatus {
        TransmissionStatus::NotVerifiable { reason }
    }

    fn verified(first: u64, frames: u64) -> TransmissionStatus {
        TransmissionStatus::Verified {
            first_frame_index: first,
            frame_count: frames,
            md5: decoder_report(0, first, frames, EndOfStream).md5,
        }
    }

    fn mismatch_detail(status: TransmissionStatus) -> Option<TransmissionMismatchDetail> {
        match status {
            TransmissionStatus::Mismatch { detail, .. } => Some(detail),
            _ => None,
        }
    }

    #[test]
    fn shutdown_before_any_segment_concludes_both_levels() {
        let mut session = Session::new();
        session.exclusive(3);
        session.verifier.on_command(VerifierCommand::Shutdown);
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::StoppedBeforeTrackEnd)
        );
        assert_eq!(
            session.file_integrity(),
            FileIntegrityStatus::NotVerifiable {
                reason: FileIntegrityNotVerifiableReason::StoppedBeforeTrackEnd,
            }
        );
    }

    #[test]
    fn a_second_report_of_the_same_generation_is_ignored() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, TRACK_FRAMES);
        session.finished(1, 0, TRACK_FRAMES);
        // A teardown finishing the hasher again sends an empty, abandoned report.
        session.report(decoder_report(1, 0, 0, Abandoned));
        session.end();
        assert_eq!(session.transmission(), verified(0, TRACK_FRAMES));
    }

    #[test]
    fn a_stop_right_at_the_end_of_a_sought_track_keeps_playback_repositioned() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 100);
        session.play(100, TRACK_FRAMES - 100);
        session.finished(1, 100, TRACK_FRAMES - 100);
        session.end();
        session.stop();
        assert_eq!(
            session.file_integrity(),
            FileIntegrityStatus::NotVerifiable {
                reason: FileIntegrityNotVerifiableReason::PlaybackRepositioned,
            }
        );
    }

    #[test]
    fn stopping_before_any_segment_is_announced_concludes_level_one() {
        let mut session = Session::new();
        session.exclusive(3);
        session.stop();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::StoppedBeforeTrackEnd)
        );
    }

    // ---- Level-1 table, one test per row ----

    #[test]
    fn level_one_row_1_segment_ended_entirely_in_shared_mode_is_not_applicable() {
        let mut session = Session::new();
        session.shared();
        session.segment(1, 0);
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            session.transmission(),
            TransmissionStatus::NotApplicableInSharedMode
        );
    }

    #[test]
    fn level_one_row_1_segment_stopped_entirely_in_shared_mode_is_not_applicable() {
        let mut session = Session::new();
        session.shared();
        session.segment(1, 0);
        session.stop();
        assert_eq!(
            session.transmission(),
            TransmissionStatus::NotApplicableInSharedMode
        );
    }

    #[test]
    fn level_one_row_2_stopped_before_track_end() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, 100);
        session.stop();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::StoppedBeforeTrackEnd)
        );
        assert_eq!(
            session.file_integrity(),
            FileIntegrityStatus::NotVerifiable {
                reason: FileIntegrityNotVerifiableReason::StoppedBeforeTrackEnd
            }
        );
    }

    #[test]
    fn level_one_row_3_overflow_with_a_segment_open() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, 100);
        session.overflow();
        session.play(100, TRACK_FRAMES - 100);
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::VerificationTapOverflowed)
        );
    }

    /// Also the case "shared `StreamBegan` mid-segment, then exclusive again with a gap".
    #[test]
    fn level_one_row_4_frames_through_the_mixer_mid_segment() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, 100);
        session.shared();
        session.exclusive(4);
        session.play(150, TRACK_FRAMES - 150);
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::PartlyPlayedThroughSystemMixer)
        );
    }

    #[test]
    fn level_one_row_4_shortfall_at_the_end_while_the_mixer_has_the_stream() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, 300);
        session.shared();
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::PartlyPlayedThroughSystemMixer)
        );
    }

    #[test]
    fn level_one_row_5_queued_audio_lost_during_the_segment() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, 100);
        session.queued_audio_lost();
        session.play(100, TRACK_FRAMES - 100);
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::QueuedAudioLostOnPause)
        );
    }

    #[test]
    fn level_one_row_6_report_with_another_first_frame_is_not_aligned() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, TRACK_FRAMES);
        session.report(decoder_report(1, 10, TRACK_FRAMES - 10, EndOfStream));
        session.end();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::SegmentsNotAligned)
        );
    }

    #[test]
    fn level_one_row_6_no_report_by_shutdown_is_not_aligned() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            session.transmission(),
            TransmissionStatus::InProgress {
                device_frames_hashed: TRACK_FRAMES
            }
        );
        session.verifier.on_command(VerifierCommand::Shutdown);
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::SegmentsNotAligned)
        );
    }

    #[test]
    fn level_one_row_7_empty_segment() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, TRACK_FRAMES);
        session.finished(1, TRACK_FRAMES, 0);
        session.end();
        assert_eq!(session.transmission(), not_verifiable(Reason::EmptySegment));
    }

    #[test]
    fn level_one_row_8_non_zero_padding_bits_at_the_right_frame() {
        let mut session = Session::new();
        session.exclusive(4);
        session.segment(1, 0);
        session.play(0, 100);
        let mut bytes = packed(100, 100, 4);
        // Frame 105, right channel: the low byte of a 24-in-32 container must be zero.
        bytes[(5 * 2 + 1) * 4] |= 0x01;
        session.play_bytes(100, 100, &bytes);
        session.play(200, TRACK_FRAMES - 200);
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            mismatch_detail(session.transmission()),
            Some(TransmissionMismatchDetail::NonZeroPaddingBits {
                first_frame_index: 105
            })
        );
    }

    #[test]
    fn level_one_row_9_one_frame_missing_at_the_end() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, TRACK_FRAMES - 1);
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            mismatch_detail(session.transmission()),
            Some(TransmissionMismatchDetail::FrameCountDiffers {
                decoder_frames: TRACK_FRAMES,
                device_frames: TRACK_FRAMES - 1
            })
        );
    }

    #[test]
    fn level_one_row_10_one_altered_sample_byte() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        let mut bytes = packed(0, TRACK_FRAMES, 3);
        bytes[(10 * 2) * 3 + 2] ^= 0x01;
        session.play_bytes(0, TRACK_FRAMES, &bytes);
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        let TransmissionStatus::Mismatch {
            decoder_md5,
            device_md5,
            detail,
        } = session.transmission()
        else {
            panic!("expected a mismatch, got {:?}", session.transmission());
        };
        assert_eq!(detail, TransmissionMismatchDetail::DigestDiffers);
        assert_eq!(decoder_md5, whole_track_md5());
        assert_ne!(device_md5, decoder_md5);
    }

    #[test]
    fn level_one_row_11_equal_digests_are_verified() {
        let mut session = Session::new();
        session.play_whole_track(1);
        assert_eq!(session.transmission(), verified(0, TRACK_FRAMES));
    }

    // ---- Level-2 table, one test per row ----

    #[test]
    fn level_two_row_1_a_final_verdict_is_kept() {
        let mut session = Session::with_track(track_facts(Some([1; 16])));
        session.finished(1, 0, TRACK_FRAMES);
        let verdict = session.file_integrity();
        assert!(matches!(verdict, FileIntegrityStatus::Mismatch { .. }));
        session.report(decoder_report(2, 0, 10, DecodeError));
        session.finished(3, 0, TRACK_FRAMES);
        session.stop();
        assert_eq!(session.file_integrity(), verdict);
    }

    #[test]
    fn level_two_row_2_an_abandoned_report_changes_nothing() {
        let mut session = Session::new();
        session
            .verifier
            .on_command(VerifierCommand::DecodedSegmentProgress {
                track_id: TRACK,
                flush_generation: 1,
                frames_hashed: 50,
            });
        session.report(decoder_report(1, 0, 50, Abandoned));
        assert_eq!(
            session.file_integrity(),
            FileIntegrityStatus::InProgress {
                decoded_frames_hashed: 50
            }
        );
    }

    #[test]
    fn level_two_row_3_decode_error() {
        let mut session = Session::new();
        session.report(decoder_report(1, 0, 200, DecodeError));
        assert_eq!(
            session.file_integrity(),
            FileIntegrityStatus::NotVerifiable {
                reason: FileIntegrityNotVerifiableReason::DecodeErrorInTrack
            }
        );
    }

    #[test]
    fn level_two_row_4_pass_started_after_frame_zero_is_repositioned() {
        let mut session = Session::new();
        session.finished(1, 100, TRACK_FRAMES - 100);
        assert_eq!(
            session.file_integrity(),
            FileIntegrityStatus::NotVerifiable {
                reason: FileIntegrityNotVerifiableReason::PlaybackRepositioned
            }
        );
    }

    #[test]
    fn level_two_row_5_frame_count_differs_from_the_declared_total() {
        let mut session = Session::new();
        session.finished(1, 0, TRACK_FRAMES - 1);
        assert_eq!(
            session.file_integrity(),
            FileIntegrityStatus::Mismatch {
                expected_md5: whole_track_md5(),
                computed_md5: decoder_report(1, 0, TRACK_FRAMES - 1, EndOfStream).md5,
                detail: FileMismatchDetail::FrameCountDiffers {
                    expected: TRACK_FRAMES,
                    decoded: TRACK_FRAMES - 1
                }
            }
        );
    }

    #[test]
    fn level_two_row_6_no_embedded_signature_gives_the_computed_digest() {
        let mut session = Session::with_track(track_facts(None));
        session.finished(1, 0, TRACK_FRAMES);
        assert_eq!(
            session.file_integrity(),
            FileIntegrityStatus::NoEmbeddedSignature {
                computed_md5: Some(whole_track_md5())
            }
        );
    }

    #[test]
    fn level_two_row_7_equal_digests_are_verified() {
        let mut session = Session::new();
        session.finished(1, 0, TRACK_FRAMES);
        assert_eq!(
            session.file_integrity(),
            FileIntegrityStatus::Verified {
                md5: whole_track_md5()
            }
        );
    }

    #[test]
    fn level_two_row_8_different_digests_mismatch() {
        let mut session = Session::with_track(track_facts(Some([1; 16])));
        session.finished(1, 0, TRACK_FRAMES);
        assert_eq!(
            session.file_integrity(),
            FileIntegrityStatus::Mismatch {
                expected_md5: [1; 16],
                computed_md5: whole_track_md5(),
                detail: FileMismatchDetail::DigestDiffers
            }
        );
    }

    // ---- Listed cases ----

    #[test]
    fn initial_status_is_in_progress_on_both_levels() {
        let session = Session::new();
        assert_eq!(
            session.verifier.status(),
            VerificationStatus {
                transmission: TransmissionStatus::InProgress {
                    device_frames_hashed: 0
                },
                file_integrity: FileIntegrityStatus::InProgress {
                    decoded_frames_hashed: 0
                },
            }
        );
    }

    #[test]
    fn exclusive_pause_reopened_in_another_container_is_verified() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, 170);
        session.exclusive(4);
        session.play(170, TRACK_FRAMES - 170);
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(session.transmission(), verified(0, TRACK_FRAMES));
    }

    #[test]
    fn a_gap_under_an_exclusive_stream_is_not_aligned() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, 100);
        session.play(120, TRACK_FRAMES - 120);
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::SegmentsNotAligned)
        );
    }

    #[test]
    fn an_overlap_is_not_aligned() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, 100);
        session.play(50, TRACK_FRAMES - 50);
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::SegmentsNotAligned)
        );
    }

    #[test]
    fn byte_count_inconsistent_with_the_layout_is_not_aligned_and_the_tap_resyncs() {
        let (mut writer, mut reader) = tap_with_capacities(1 << 16, 64);
        let mut verifier = TrackVerifier::new(track_facts(Some(whole_track_md5())));
        let mut device_bytes = Vec::new();
        writer.push_event(TapEvent::StreamBegan {
            render_format: render_format(3),
            access_mode: EXCLUSIVE,
        });
        writer.push_event(TapEvent::SegmentStarted {
            track_id: TRACK,
            flush_generation: 1,
            first_frame_index: 0,
        });
        // Three stray bytes: `byte_count` no longer matches 10 frames of 6 bytes.
        let mut wrong = packed(0, 10, 3);
        wrong.extend_from_slice(&[0xAA; 3]);
        writer.push_device_bytes(&wrong, TRACK, 0, 10);
        writer.push_device_bytes(&packed(10, TRACK_FRAMES - 10, 3), TRACK, 10, 390);
        writer.push_event(TapEvent::TrackEnded { track_id: TRACK });
        drain_tap(&mut reader, &mut verifier, &mut device_bytes);
        verifier.on_command(VerifierCommand::DecodedSegmentFinished(decoder_report(
            1,
            0,
            TRACK_FRAMES,
            EndOfStream,
        )));
        assert_eq!(
            verifier.status().transmission,
            not_verifiable(Reason::SegmentsNotAligned)
        );
        // The next segment is read from the right byte: verified.
        writer.push_event(TapEvent::SegmentStarted {
            track_id: TRACK,
            flush_generation: 2,
            first_frame_index: 0,
        });
        writer.push_device_bytes(&packed(0, TRACK_FRAMES, 3), TRACK, 0, 400);
        writer.push_event(TapEvent::TrackEnded { track_id: TRACK });
        verifier.on_command(VerifierCommand::DecodedSegmentFinished(decoder_report(
            2,
            0,
            TRACK_FRAMES,
            EndOfStream,
        )));
        drain_tap(&mut reader, &mut verifier, &mut device_bytes);
        assert_eq!(verifier.status().transmission, verified(0, TRACK_FRAMES));
    }

    #[test]
    fn missing_tap_bytes_misalign_this_and_every_later_segment() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.verifier.on_tap_event(
            TapEvent::DeviceBytes {
                track_id: TRACK,
                first_frame_index: 0,
                frame_count: 400,
                byte_count: 2_400,
            },
            None,
        );
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::SegmentsNotAligned)
        );
        session.segment(2, 0);
        session.play(0, TRACK_FRAMES);
        session.finished(2, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::SegmentsNotAligned)
        );
    }

    #[test]
    fn shared_stream_began_with_no_frame_played_before_the_end_is_still_verified() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, TRACK_FRAMES);
        session.shared();
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(session.transmission(), verified(0, TRACK_FRAMES));
    }

    #[test]
    fn segment_started_in_shared_mode_then_exclusive_is_partly_through_the_mixer() {
        let mut session = Session::new();
        session.shared();
        session.segment(1, 0);
        session.exclusive(3);
        session.play(50, TRACK_FRAMES - 50);
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::PartlyPlayedThroughSystemMixer)
        );
    }

    #[test]
    fn a_seek_judges_only_the_new_segment_from_its_target() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, 100);
        session.report(decoder_report(1, 0, 100, Abandoned));
        session.segment(2, 250);
        assert_eq!(
            session.transmission(),
            TransmissionStatus::InProgress {
                device_frames_hashed: 0
            }
        );
        session.play(250, TRACK_FRAMES - 250);
        session.finished(2, 250, TRACK_FRAMES - 250);
        session.end();
        assert_eq!(session.transmission(), verified(250, TRACK_FRAMES - 250));
        assert_eq!(
            session.file_integrity(),
            FileIntegrityStatus::NotVerifiable {
                reason: FileIntegrityNotVerifiableReason::PlaybackRepositioned
            }
        );
    }

    #[test]
    fn a_seek_back_to_zero_that_reaches_the_end_gives_a_level_two_verdict() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 300);
        session.play(300, 100);
        session.finished(1, 300, 100);
        session.end();
        assert_eq!(
            session.file_integrity(),
            FileIntegrityStatus::NotVerifiable {
                reason: FileIntegrityNotVerifiableReason::PlaybackRepositioned
            }
        );
        session.segment(2, 0);
        session.play(0, TRACK_FRAMES);
        session.finished(2, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            session.file_integrity(),
            FileIntegrityStatus::Verified {
                md5: whole_track_md5()
            }
        );
        assert_eq!(session.transmission(), verified(0, TRACK_FRAMES));
    }

    #[test]
    fn a_level_two_verdict_reached_before_a_later_seek_is_kept() {
        let mut session = Session::new();
        session.play_whole_track(1);
        session.segment(2, 200);
        session.play(200, 200);
        session.finished(2, 200, 200);
        session.end();
        assert_eq!(
            session.file_integrity(),
            FileIntegrityStatus::Verified {
                md5: whole_track_md5()
            }
        );
        assert_eq!(session.transmission(), verified(200, 200));
    }

    #[test]
    fn overflow_with_no_segment_open_then_bytes_and_track_end_is_overflowed() {
        let mut session = Session::new();
        session.exclusive(3);
        session.overflow();
        let bytes = packed(390, 10, 3);
        session.play_bytes(390, 10, &bytes);
        session.end();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::VerificationTapOverflowed)
        );
        // A fresh segment is judged normally.
        session.play_whole_track(2);
        assert_eq!(session.transmission(), verified(0, TRACK_FRAMES));
    }

    #[test]
    fn device_bytes_naming_another_track_taint_the_segment() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, TRACK_FRAMES);
        let bytes = packed(0, 10, 3);
        session.play_bytes_of(OTHER_TRACK, 0, 10, &bytes);
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::VerificationTapOverflowed)
        );
    }

    #[test]
    fn track_ended_naming_another_track_taints_the_segment() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, TRACK_FRAMES);
        session.finished(1, 0, TRACK_FRAMES);
        session.end_of(OTHER_TRACK);
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::VerificationTapOverflowed)
        );
    }

    #[test]
    fn segment_started_naming_another_track_is_not_aligned() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment_of(OTHER_TRACK, 1, 0);
        session.play(0, TRACK_FRAMES);
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::SegmentsNotAligned)
        );
    }

    #[test]
    fn queued_audio_lost_after_a_verified_conclusion_revises_it() {
        let mut session = Session::new();
        session.play_whole_track(1);
        assert_eq!(session.transmission(), verified(0, TRACK_FRAMES));
        session.queued_audio_lost();
        assert_eq!(
            session.transmission(),
            not_verifiable(Reason::QueuedAudioLostOnPause)
        );
    }

    #[test]
    fn queued_audio_lost_after_a_mismatch_changes_nothing() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, TRACK_FRAMES - 1);
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        let mismatch = session.transmission();
        assert!(mismatch_detail(mismatch.clone()).is_some());
        session.queued_audio_lost();
        assert_eq!(session.transmission(), mismatch);
    }

    #[test]
    fn track_stopped_right_after_the_track_end_keeps_the_conclusion() {
        let mut session = Session::new();
        session.play_whole_track(1);
        session.stop();
        assert_eq!(session.transmission(), verified(0, TRACK_FRAMES));
        assert_eq!(
            session.file_integrity(),
            FileIntegrityStatus::Verified {
                md5: whole_track_md5()
            }
        );
    }

    #[test]
    fn track_ended_before_its_report_concludes_when_the_report_arrives() {
        let mut session = Session::new();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, TRACK_FRAMES);
        session.end();
        session.finished(1, 0, TRACK_FRAMES);
        assert_eq!(session.transmission(), verified(0, TRACK_FRAMES));
    }

    #[test]
    fn progress_is_published_at_most_once_per_interval_and_conclusions_at_once() {
        let mut session = Session::new();
        let start = Instant::now();
        session.exclusive(3);
        session.segment(1, 0);
        session.play(0, 10);
        assert_eq!(
            session.verifier.take_update(start).map(|s| s.transmission),
            Some(TransmissionStatus::InProgress {
                device_frames_hashed: 10
            })
        );
        session.play(10, 10);
        let just_before = start + VERIFICATION_PROGRESS_INTERVAL - Duration::from_millis(1);
        assert_eq!(session.verifier.take_update(just_before), None);
        let interval_later = start + VERIFICATION_PROGRESS_INTERVAL;
        assert_eq!(
            session
                .verifier
                .take_update(interval_later)
                .map(|s| s.transmission),
            Some(TransmissionStatus::InProgress {
                device_frames_hashed: 20
            })
        );
        assert_eq!(session.verifier.take_update(interval_later), None);
        session.play(20, TRACK_FRAMES - 20);
        session.finished(1, 0, TRACK_FRAMES);
        session.end();
        let concluded = session
            .verifier
            .take_update(interval_later + Duration::from_millis(1))
            .expect("a conclusion is published at once");
        assert_eq!(concluded.transmission, verified(0, TRACK_FRAMES));
    }

    // ---- Thread ----

    fn exclusive_stream_began() -> TapEvent {
        TapEvent::StreamBegan {
            render_format: render_format(3),
            access_mode: EXCLUSIVE,
        }
    }

    fn segment_started(flush_generation: u64) -> TapEvent {
        TapEvent::SegmentStarted {
            track_id: TRACK,
            flush_generation,
            first_frame_index: 0,
        }
    }

    fn receive_update(updates: &Receiver<VerificationUpdate>) -> VerificationUpdate {
        updates
            .recv_timeout(THREAD_TEST_TIMEOUT)
            .expect("the verifier publishes")
    }

    #[test]
    fn thread_drains_the_tap_before_a_stop_so_the_track_end_concludes() {
        let (mut writer, reader) = tap_with_capacities(1 << 16, 64);
        let (command_sender, command_receiver) = crossbeam_channel::unbounded();
        let (update_sender, update_receiver) = crossbeam_channel::unbounded();
        writer.push_event(exclusive_stream_began());
        writer.push_event(segment_started(1));
        writer.push_device_bytes(&packed(0, TRACK_FRAMES, 3), TRACK, 0, 400);
        let handle = spawn_verifier(
            track_facts(Some(whole_track_md5())),
            reader,
            command_receiver,
            update_sender,
        )
        .expect("thread spawns");
        command_sender
            .send(VerifierCommand::DecodedSegmentFinished(decoder_report(
                1,
                0,
                TRACK_FRAMES,
                EndOfStream,
            )))
            .expect("verifier listens");
        // Wait until the report is applied (level 2 concludes at once).
        while !matches!(
            receive_update(&update_receiver).status.file_integrity,
            FileIntegrityStatus::Verified { .. }
        ) {}
        // The track end is in the tap, then the stop arrives at once.
        writer.push_event(TapEvent::TrackEnded { track_id: TRACK });
        command_sender
            .send(VerifierCommand::TrackStopped { track_id: TRACK })
            .expect("verifier listens");
        command_sender
            .send(VerifierCommand::Shutdown)
            .expect("verifier listens");
        handle.join().expect("verifier thread ends cleanly");
        let published: Vec<VerificationUpdate> = update_receiver.try_iter().collect();
        assert!(published.iter().all(|update| update.track_id == TRACK));
        assert!(
            published.iter().all(|update| update.status.transmission
                != not_verifiable(Reason::StoppedBeforeTrackEnd))
        );
        assert_eq!(
            published
                .last()
                .map(|update| update.status.transmission.clone()),
            Some(verified(0, TRACK_FRAMES))
        );
    }

    #[test]
    fn thread_shutdown_with_an_open_segment_is_stopped_before_track_end() {
        let (mut writer, reader) = tap_with_capacities(1 << 16, 64);
        let (command_sender, command_receiver) = crossbeam_channel::unbounded();
        let (update_sender, update_receiver) = crossbeam_channel::unbounded();
        writer.push_event(exclusive_stream_began());
        writer.push_event(segment_started(1));
        writer.push_device_bytes(&packed(0, 100, 3), TRACK, 0, 100);
        let handle = spawn_verifier(
            track_facts(Some(whole_track_md5())),
            reader,
            command_receiver,
            update_sender,
        )
        .expect("thread spawns");
        command_sender
            .send(VerifierCommand::Shutdown)
            .expect("verifier listens");
        handle.join().expect("verifier thread ends cleanly");
        let last = update_receiver
            .try_iter()
            .last()
            .expect("the final status is published");
        assert_eq!(
            last.status.transmission,
            not_verifiable(Reason::StoppedBeforeTrackEnd)
        );
        assert_eq!(
            last.status.file_integrity,
            FileIntegrityStatus::NotVerifiable {
                reason: FileIntegrityNotVerifiableReason::StoppedBeforeTrackEnd
            }
        );
    }

    #[test]
    fn thread_treats_a_disconnected_command_channel_as_shutdown() {
        let (_writer, reader) = tap_with_capacities(64, 8);
        let (command_sender, command_receiver) = crossbeam_channel::unbounded();
        let (update_sender, update_receiver) = crossbeam_channel::unbounded();
        let handle = spawn_verifier(track_facts(None), reader, command_receiver, update_sender)
            .expect("thread spawns");
        drop(command_sender);
        handle.join().expect("verifier thread ends cleanly");
        assert!(update_receiver.try_iter().last().is_some());
    }
}
