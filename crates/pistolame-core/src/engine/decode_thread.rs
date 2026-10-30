//! The decode thread of one playback pipeline ("pistolame-decode", plan B2, B6, B7.2).
//!
//! It owns the decoder, the playback ring writer, the decoder-side hasher and the end-of-queue
//! flag. It pushes the decoded PCM into the ring in chunks the ring can hold, hashes exactly
//! what it pushed (stage before the push, commit after it), and tells the verifier and the
//! engine where each segment starts and ends. It may block: it sleeps `DECODE_RETRY_SLEEP`
//! while the ring is full and waits for a command once its segment has ended.
//!
//! Segment handshake (B7.2): `begin_segment` right before the first push of a generation; at the
//! end (or on a decode error) the report is sent first, then the zero-frame end marker is
//! pushed, then `is_playback_queue_finished` is set. So the verifier always holds the report
//! before the feeder can emit the matching `TrackEnded`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TryRecvError};

use crate::decoder::{AudioDecoder, DecodeError};
use crate::format::StreamFormat;
use crate::playback_ring::{
    PlaybackRingWriter, PushBlockError, SampleBlockHeader, TrackId, frames_in_duration,
};
use crate::timing::{DECODE_RETRY_SLEEP, VERIFICATION_PROGRESS_INTERVAL};
use crate::verification::{DecodedPcmHasher, DecodedSegmentEnd, VerifierCommand};

/// Name of the decode thread, as seen in debuggers and crash reports.
const DECODE_THREAD_NAME: &str = "pistolame-decode";

/// Engine → decode thread.
#[derive(Debug)]
pub(crate) enum DecodeCommand {
    /// Abandon the current segment and decode from `target_frame_index` under
    /// `flush_generation` (the engine bumped the shared generation before sending it).
    Seek {
        /// First frame the new segment must start at; earlier frames are dropped.
        target_frame_index: u64,
        /// Generation of the new segment.
        flush_generation: u64,
    },
    /// Abandon the current segment and return.
    Stop,
}

/// Decode thread → engine.
#[derive(Debug)]
pub(crate) enum DecodeEvent {
    /// `START_PREFILL` frames of this generation are in the ring, or its end marker is.
    PrefillReached {
        /// Generation the prefill belongs to.
        flush_generation: u64,
    },
    /// The end marker of this generation is in the ring; `error` is set when it ended early.
    SegmentEnded {
        /// Generation that ended.
        flush_generation: u64,
        /// Why it ended before the end of the stream, if it did.
        error: Option<SegmentFailure>,
    },
}

/// Why a generation ended before the end of the stream.
#[derive(Debug)]
pub(crate) enum SegmentFailure {
    /// The decoder failed, or the canonical layout rejected a block: the file is damaged
    /// (B8 "Corrupt file").
    Decode(DecodeError),
    /// The ring refused a block the split should have made fit (`SampleCountMismatch`,
    /// `BlockExceedsCapacity`): a bug, never a property of the file.
    Internal(String),
}

/// Everything the decode thread owns, handed over at spawn.
pub(crate) struct DecodeThreadSetup {
    /// Decoder of the pipeline's file, positioned at frame 0.
    pub decoder: Box<dyn AudioDecoder>,
    /// Track every pushed block belongs to.
    pub track_id: TrackId,
    /// Producer side of the pipeline's playback ring.
    pub ring_writer: PlaybackRingWriter,
    /// End-of-queue flag shared with the feeder; this thread alone writes it.
    pub is_playback_queue_finished: Arc<AtomicBool>,
    /// Latest generation buffered up to the prefill, read by the feeder; this thread alone
    /// writes it.
    pub prefilled_generation: Arc<AtomicU64>,
    /// Audio of a generation buffered before `PrefillReached` is sent.
    pub start_prefill_duration: Duration,
    /// Commands from the engine.
    pub commands: Receiver<DecodeCommand>,
    /// Events to the engine.
    pub events: Sender<DecodeEvent>,
    /// Decoder-side reports to the pipeline's verifier.
    pub verifier_commands: Sender<VerifierCommand>,
}

/// Spawns the decode thread. It returns on `DecodeCommand::Stop` or when the command channel
/// disconnects.
pub(crate) fn spawn_decode_thread(setup: DecodeThreadSetup) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(DECODE_THREAD_NAME.to_owned())
        .spawn(move || DecodeWorker::new(setup).run())
}

/// What the main loop does next.
enum Flow {
    /// Keep decoding (or waiting, once the segment ended).
    Continue,
    /// A command arrived while a push was waiting for room; handle it before anything else.
    Interrupted(DecodeCommand),
    /// Return from the thread.
    Stop,
}

/// Outcome of pushing one block, retrying while the ring is full.
enum PushOutcome {
    Pushed,
    Interrupted(DecodeCommand),
    Refused(PushBlockError),
}

struct DecodeWorker {
    decoder: Box<dyn AudioDecoder>,
    track_id: TrackId,
    format: StreamFormat,
    ring_writer: PlaybackRingWriter,
    hasher: DecodedPcmHasher,
    is_playback_queue_finished: Arc<AtomicBool>,
    prefilled_generation: Arc<AtomicU64>,
    commands: Receiver<DecodeCommand>,
    events: Sender<DecodeEvent>,
    verifier_commands: Sender<VerifierCommand>,
    flush_generation: u64,
    /// Frames before this index are dropped (the decoder may land before a seek target).
    trim_before_frame_index: u64,
    /// Index of the next frame to push; the end marker carries it.
    next_frame_index: u64,
    is_segment_open: bool,
    /// The end marker of the current generation is in the ring: wait for a command.
    has_segment_ended: bool,
    frames_pushed_in_generation: u64,
    prefill_frame_count: u64,
    has_sent_prefill: bool,
    last_progress_sent_at: Instant,
    /// Copy of the latest decoded block (the decoder lends its buffer only until the next call).
    decoded_samples: Vec<i32>,
}

impl DecodeWorker {
    fn new(setup: DecodeThreadSetup) -> Self {
        let format = setup.decoder.stream_info().format;
        let ring_capacity_frames = setup.ring_writer.capacity_frames(format.channel_count);
        let prefill_frame_count =
            frames_in_duration(format.sample_rate_hz, setup.start_prefill_duration)
                .min(ring_capacity_frames.max(1));
        Self {
            hasher: DecodedPcmHasher::new(setup.track_id, format),
            decoder: setup.decoder,
            track_id: setup.track_id,
            format,
            ring_writer: setup.ring_writer,
            is_playback_queue_finished: setup.is_playback_queue_finished,
            prefilled_generation: setup.prefilled_generation,
            commands: setup.commands,
            events: setup.events,
            verifier_commands: setup.verifier_commands,
            flush_generation: 0,
            trim_before_frame_index: 0,
            next_frame_index: 0,
            is_segment_open: false,
            has_segment_ended: false,
            frames_pushed_in_generation: 0,
            prefill_frame_count: u64::try_from(prefill_frame_count).unwrap_or(u64::MAX),
            has_sent_prefill: false,
            last_progress_sent_at: Instant::now(),
            decoded_samples: Vec::new(),
        }
    }

    fn run(mut self) {
        let mut interrupting_command = None;
        loop {
            let command = match interrupting_command.take() {
                Some(command) => Some(command),
                None if self.has_segment_ended => Some(self.receive_command_blocking()),
                None => self.receive_command_if_any(),
            };
            let flow = match command {
                Some(command) => self.handle_command(command),
                None => self.decode_next_block(),
            };
            match flow {
                Flow::Continue => {}
                Flow::Interrupted(command) => interrupting_command = Some(command),
                Flow::Stop => return,
            }
        }
    }

    /// A disconnected channel means the engine is gone: same as `Stop`.
    fn receive_command_blocking(&self) -> DecodeCommand {
        self.commands.recv().unwrap_or(DecodeCommand::Stop)
    }

    fn receive_command_if_any(&self) -> Option<DecodeCommand> {
        match self.commands.try_recv() {
            Ok(command) => Some(command),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(DecodeCommand::Stop),
        }
    }

    fn handle_command(&mut self, command: DecodeCommand) -> Flow {
        match command {
            DecodeCommand::Stop => {
                self.abandon_open_segment();
                Flow::Stop
            }
            DecodeCommand::Seek {
                target_frame_index,
                flush_generation,
            } => {
                self.abandon_open_segment();
                self.start_generation(target_frame_index, flush_generation)
            }
        }
    }

    /// `finish_segment(Abandoned)` and its report, only if a segment was begun and not ended.
    fn abandon_open_segment(&mut self) {
        if self.is_segment_open {
            self.is_segment_open = false;
            let report = self.hasher.finish_segment(DecodedSegmentEnd::Abandoned);
            self.send_to_verifier(VerifierCommand::DecodedSegmentFinished(report));
        }
    }

    /// Seek: clears the end flag (this thread is its only writer, so an old generation can
    /// never set it afterwards), adopts the generation and repositions the decoder. A seek
    /// error is a decode error of the new generation: an empty segment and its end marker.
    fn start_generation(&mut self, target_frame_index: u64, flush_generation: u64) -> Flow {
        self.is_playback_queue_finished
            .store(false, Ordering::Release);
        self.flush_generation = flush_generation;
        self.has_segment_ended = false;
        self.has_sent_prefill = false;
        self.frames_pushed_in_generation = 0;
        self.trim_before_frame_index = target_frame_index;
        match self.decoder.seek_to_frame(target_frame_index) {
            Ok(landed_frame_index) => {
                self.next_frame_index = landed_frame_index.max(target_frame_index);
                Flow::Continue
            }
            Err(error) => {
                self.next_frame_index = target_frame_index;
                self.end_segment(
                    DecodedSegmentEnd::DecodeError,
                    Some(SegmentFailure::Decode(error)),
                )
            }
        }
    }

    fn decode_next_block(&mut self) -> Flow {
        let first_frame_index = match self.decoder.next_block() {
            Ok(Some(block)) => {
                self.decoded_samples.clear();
                self.decoded_samples
                    .extend_from_slice(block.interleaved_samples);
                block.first_frame_index
            }
            Ok(None) => return self.end_segment(DecodedSegmentEnd::EndOfStream, None),
            Err(error) => {
                return self.end_segment(
                    DecodedSegmentEnd::DecodeError,
                    Some(SegmentFailure::Decode(error)),
                );
            }
        };
        let samples = std::mem::take(&mut self.decoded_samples);
        let flow = self.push_decoded_samples(first_frame_index, &samples);
        self.decoded_samples = samples;
        flow
    }

    /// Drops the frames before the seek target, then pushes the rest in chunks of at most the
    /// ring's capacity.
    fn push_decoded_samples(&mut self, first_frame_index: u64, samples: &[i32]) -> Flow {
        let channel_count = usize::from(self.format.channel_count.max(1));
        let block_frame_count = u64::try_from(samples.len() / channel_count).unwrap_or(u64::MAX);
        let skipped_frame_count = self
            .trim_before_frame_index
            .saturating_sub(first_frame_index)
            .min(block_frame_count);
        let skipped_sample_count =
            usize::try_from(skipped_frame_count).unwrap_or(usize::MAX) * channel_count;
        let kept_samples = samples.get(skipped_sample_count..).unwrap_or_default();
        if kept_samples.is_empty() {
            self.next_frame_index = self
                .next_frame_index
                .max(first_frame_index.saturating_add(block_frame_count));
            return Flow::Continue;
        }
        let chunk_sample_count = self
            .ring_writer
            .capacity_frames(self.format.channel_count)
            .max(1)
            * channel_count;
        let mut chunk_first_frame_index = first_frame_index + skipped_frame_count;
        for chunk in kept_samples.chunks(chunk_sample_count) {
            match self.push_audio_chunk(chunk_first_frame_index, chunk) {
                Flow::Continue => {}
                other => return other,
            }
            chunk_first_frame_index = self.next_frame_index;
        }
        Flow::Continue
    }

    /// Stage, push (retrying while the ring is full), commit. A staging error is a decode
    /// error and a push refusal an internal one: either way nothing of the chunk is pushed or
    /// hashed.
    fn push_audio_chunk(&mut self, first_frame_index: u64, samples: &[i32]) -> Flow {
        let channel_count = usize::from(self.format.channel_count.max(1));
        let frame_count = u32::try_from(samples.len() / channel_count).unwrap_or(u32::MAX);
        if !self.is_segment_open {
            self.hasher
                .begin_segment(self.flush_generation, first_frame_index);
            self.is_segment_open = true;
        }
        if let Err(error) = self.hasher.stage_block(first_frame_index, samples) {
            let error = DecodeError::CorruptData {
                near_frame_index: first_frame_index,
                detail: error.to_string(),
            };
            return self.end_segment(
                DecodedSegmentEnd::DecodeError,
                Some(SegmentFailure::Decode(error)),
            );
        }
        let header = self.header(first_frame_index, frame_count, false);
        match self.push_with_retry(header, samples) {
            PushOutcome::Pushed => {}
            PushOutcome::Interrupted(command) => {
                self.hasher.discard_staged_block();
                return Flow::Interrupted(command);
            }
            PushOutcome::Refused(error) => {
                self.hasher.discard_staged_block();
                let detail = format!(
                    "the playback ring refused the block at frame {first_frame_index}: {error}"
                );
                return self.end_segment(
                    DecodedSegmentEnd::DecodeError,
                    Some(SegmentFailure::Internal(detail)),
                );
            }
        }
        self.hasher.commit_staged_block();
        self.next_frame_index = first_frame_index + u64::from(frame_count);
        self.frames_pushed_in_generation += u64::from(frame_count);
        self.send_progress_if_due();
        self.send_prefill_if_reached(false);
        Flow::Continue
    }

    /// End of stream or decode error: report, end marker, end flag, prefill if not yet sent,
    /// then `SegmentEnded`. An empty segment is begun here so its report names this generation.
    fn end_segment(&mut self, ended_by: DecodedSegmentEnd, error: Option<SegmentFailure>) -> Flow {
        if !self.is_segment_open {
            self.hasher
                .begin_segment(self.flush_generation, self.next_frame_index);
        }
        self.is_segment_open = false;
        let report = self.hasher.finish_segment(ended_by);
        self.send_to_verifier(VerifierCommand::DecodedSegmentFinished(report));
        let end_marker = self.header(self.next_frame_index, 0, true);
        if let PushOutcome::Interrupted(command) = self.push_with_retry(end_marker, &[]) {
            return Flow::Interrupted(command);
        }
        // A zero-frame block is never refused for its size. Should it be, the flag is still
        // set: the feeder then ends without `TrackEnded` and the verifier concludes the
        // segment as stopped, never as a match.
        self.is_playback_queue_finished
            .store(true, Ordering::Release);
        self.has_segment_ended = true;
        self.send_prefill_if_reached(true);
        self.send_event(DecodeEvent::SegmentEnded {
            flush_generation: self.flush_generation,
            error,
        });
        Flow::Continue
    }

    fn push_with_retry(&mut self, header: SampleBlockHeader, samples: &[i32]) -> PushOutcome {
        loop {
            match self.ring_writer.try_push_block(header, samples) {
                Ok(()) => return PushOutcome::Pushed,
                Err(PushBlockError::RingFull) => {
                    // The ring holds all it can: the prefill cannot grow any more.
                    self.send_prefill_if_reached(self.frames_pushed_in_generation > 0);
                    if let Some(command) = self.receive_command_if_any() {
                        return PushOutcome::Interrupted(command);
                    }
                    std::thread::sleep(DECODE_RETRY_SLEEP);
                }
                Err(error) => return PushOutcome::Refused(error),
            }
        }
    }

    fn header(
        &self,
        first_frame_index: u64,
        frame_count: u32,
        is_track_end: bool,
    ) -> SampleBlockHeader {
        SampleBlockHeader {
            track_id: self.track_id,
            flush_generation: self.flush_generation,
            format: self.format,
            first_frame_index,
            frame_count,
            is_track_start: first_frame_index == 0,
            is_track_end,
        }
    }

    fn send_prefill_if_reached(&mut self, is_forced: bool) {
        if self.has_sent_prefill {
            return;
        }
        if is_forced || self.frames_pushed_in_generation >= self.prefill_frame_count {
            self.has_sent_prefill = true;
            self.prefilled_generation
                .store(self.flush_generation, Ordering::Release);
            self.send_event(DecodeEvent::PrefillReached {
                flush_generation: self.flush_generation,
            });
        }
    }

    fn send_progress_if_due(&mut self) {
        if self.last_progress_sent_at.elapsed() < VERIFICATION_PROGRESS_INTERVAL {
            return;
        }
        self.last_progress_sent_at = Instant::now();
        self.send_to_verifier(VerifierCommand::DecodedSegmentProgress {
            track_id: self.track_id,
            flush_generation: self.flush_generation,
            frames_hashed: self.hasher.frames_hashed(),
        });
    }

    /// A send error means the pipeline is being torn down: nobody needs the message any more.
    fn send_to_verifier(&self, command: VerifierCommand) {
        let _ = self.verifier_commands.send(command);
    }

    /// As `send_to_verifier`, for the engine.
    fn send_event(&self, event: DecodeEvent) {
        let _ = self.events.send(event);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crossbeam_channel::unbounded;

    use super::*;
    use crate::decoder::{DecodedBlock, SourceStreamInfo};
    use crate::format::ChannelMask;
    use crate::playback_ring::{PlaybackRingReader, create_playback_ring};
    use crate::verification::DecodedSegmentReport;

    const RATE_HZ: u32 = 1_000;
    const TRACK: TrackId = TrackId(3);
    const WAIT: Duration = Duration::from_secs(10);

    fn format() -> StreamFormat {
        StreamFormat {
            sample_rate_hz: RATE_HZ,
            bits_per_sample: 16,
            channel_count: 2,
            channel_mask: ChannelMask(0b11),
        }
    }

    /// A decoder made of fixed blocks; it seeks to the start of the block holding the target,
    /// like a FLAC decoder landing on a frame boundary, and records the end flag it saw then.
    struct ScriptedDecoder {
        info: SourceStreamInfo,
        blocks: Vec<(u64, Vec<i32>)>,
        next_block_index: usize,
        end_flag: Arc<AtomicBool>,
        end_flag_seen_at_seek: Arc<AtomicBool>,
    }

    impl AudioDecoder for ScriptedDecoder {
        fn stream_info(&self) -> &SourceStreamInfo {
            &self.info
        }

        fn next_block(&mut self) -> Result<Option<DecodedBlock<'_>>, DecodeError> {
            let Some((first_frame_index, samples)) = self.blocks.get(self.next_block_index) else {
                return Ok(None);
            };
            self.next_block_index += 1;
            Ok(Some(DecodedBlock {
                first_frame_index: *first_frame_index,
                interleaved_samples: samples,
            }))
        }

        fn seek_to_frame(&mut self, frame_index: u64) -> Result<u64, DecodeError> {
            self.end_flag_seen_at_seek
                .store(self.end_flag.load(Ordering::Acquire), Ordering::Release);
            let block_index = self
                .blocks
                .iter()
                .rposition(|(first, _)| *first <= frame_index)
                .unwrap_or(0);
            self.next_block_index = block_index;
            Ok(self.blocks[block_index].0)
        }
    }

    /// Frames `first..first + count`, each sample derived from its frame and channel.
    fn frames(first: u64, count: u64) -> Vec<i32> {
        (first..first + count)
            .flat_map(|frame| [frame as i32, -(frame as i32) - 1])
            .collect()
    }

    fn blocks_of(block_frames: u64, block_count: u64) -> Vec<(u64, Vec<i32>)> {
        (0..block_count)
            .map(|index| {
                (
                    index * block_frames,
                    frames(index * block_frames, block_frames),
                )
            })
            .collect()
    }

    struct Harness {
        reader: PlaybackRingReader,
        commands: Sender<DecodeCommand>,
        events: Receiver<DecodeEvent>,
        verifier: Receiver<VerifierCommand>,
        end_flag: Arc<AtomicBool>,
        end_flag_seen_at_seek: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    fn start(blocks: Vec<(u64, Vec<i32>)>, ring_duration: Duration) -> Harness {
        let (writer, reader) = create_playback_ring(&format(), ring_duration);
        let end_flag = Arc::new(AtomicBool::new(false));
        let end_flag_seen_at_seek = Arc::new(AtomicBool::new(true));
        let (command_sender, command_receiver) = unbounded();
        let (event_sender, event_receiver) = unbounded();
        let (verifier_sender, verifier_receiver) = unbounded();
        let decoder = ScriptedDecoder {
            info: SourceStreamInfo {
                format: format(),
                total_frames: None,
                is_total_frames_exact: true,
                embedded_md5: None,
                codec: crate::decoder::SourceCodec::FLAC,
            },
            blocks,
            next_block_index: 0,
            end_flag: Arc::clone(&end_flag),
            end_flag_seen_at_seek: Arc::clone(&end_flag_seen_at_seek),
        };
        let thread = spawn_decode_thread(DecodeThreadSetup {
            decoder: Box::new(decoder),
            track_id: TRACK,
            ring_writer: writer,
            is_playback_queue_finished: Arc::clone(&end_flag),
            prefilled_generation: Arc::new(AtomicU64::new(0)),
            start_prefill_duration: Duration::from_millis(50),
            commands: command_receiver,
            events: event_sender,
            verifier_commands: verifier_sender,
        })
        .expect("thread spawns");
        Harness {
            reader,
            commands: command_sender,
            events: event_receiver,
            verifier: verifier_receiver,
            end_flag,
            end_flag_seen_at_seek,
            thread: Some(thread),
        }
    }

    impl Harness {
        /// Pops every block until the end marker of `generation`, keeping the ring drained so
        /// the decode thread never waits for room.
        fn read_until_end_marker(&mut self, generation: u64) -> Vec<(SampleBlockHeader, Vec<i32>)> {
            let deadline = Instant::now() + WAIT;
            let mut blocks = Vec::new();
            loop {
                assert!(Instant::now() < deadline, "no end marker");
                let Some(header) = self.reader.pop_header() else {
                    std::thread::yield_now();
                    continue;
                };
                let sample_count = header.sample_count().expect("fits");
                let samples = self
                    .reader
                    .read_samples(sample_count, |first, second| [first, second].concat())
                    .expect("samples present");
                let is_end = header.is_track_end && header.flush_generation == generation;
                blocks.push((header, samples));
                if is_end {
                    return blocks;
                }
            }
        }

        fn next_event(&self) -> DecodeEvent {
            self.events.recv_timeout(WAIT).expect("an event")
        }

        fn reports(&self) -> Vec<DecodedSegmentReport> {
            self.verifier
                .try_iter()
                .filter_map(|command| match command {
                    VerifierCommand::DecodedSegmentFinished(report) => Some(report),
                    _ => None,
                })
                .collect()
        }

        fn stop(&mut self) {
            self.commands
                .send(DecodeCommand::Stop)
                .expect("thread alive");
            self.thread
                .take()
                .expect("running")
                .join()
                .expect("no panic");
        }
    }

    fn audio_of(blocks: &[(SampleBlockHeader, Vec<i32>)]) -> Vec<i32> {
        blocks
            .iter()
            .flat_map(|(_, samples)| samples.iter().copied())
            .collect()
    }

    #[test]
    fn blocks_larger_than_the_ring_are_split_and_the_end_marker_follows_the_report() {
        // A 100 ms ring holds 100 frames; the decoder yields blocks of 250.
        let mut harness = start(blocks_of(250, 3), Duration::from_millis(100));
        let blocks = harness.read_until_end_marker(0);
        // The marker is visible, so its report must already be in the verifier's channel.
        let reports = harness.reports();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].ended_by, DecodedSegmentEnd::EndOfStream);
        assert_eq!(reports[0].frame_count, 750);
        assert!(blocks.iter().all(|(header, _)| header.frame_count <= 100));
        assert_eq!(audio_of(&blocks), frames(0, 750));
        let (marker, _) = blocks.last().expect("marker");
        assert_eq!((marker.first_frame_index, marker.frame_count), (750, 0));
        assert!(matches!(
            harness.next_event(),
            DecodeEvent::PrefillReached {
                flush_generation: 0
            }
        ));
        assert!(matches!(
            harness.next_event(),
            DecodeEvent::SegmentEnded {
                flush_generation: 0,
                error: None
            }
        ));
        assert!(harness.end_flag.load(Ordering::Acquire));
        harness.stop();
    }

    #[test]
    fn a_seek_clears_the_end_flag_and_trims_the_frames_before_the_target() {
        let mut harness = start(blocks_of(100, 4), Duration::from_secs(1));
        harness.read_until_end_marker(0);
        while !matches!(harness.next_event(), DecodeEvent::SegmentEnded { .. }) {}
        assert!(harness.end_flag.load(Ordering::Acquire));
        harness
            .commands
            .send(DecodeCommand::Seek {
                target_frame_index: 150,
                flush_generation: 1,
            })
            .expect("thread alive");
        let blocks = harness.read_until_end_marker(1);
        assert!(
            !harness.end_flag_seen_at_seek.load(Ordering::Acquire),
            "the flag is cleared before the decoder moves"
        );
        assert_eq!(blocks[0].0.first_frame_index, 150);
        assert!(
            blocks
                .iter()
                .all(|(header, _)| header.flush_generation == 1)
        );
        assert_eq!(audio_of(&blocks), frames(150, 250));
        let reports = harness.reports();
        assert_eq!(reports.len(), 2, "no abandoned report for an ended segment");
        assert_eq!(
            (reports[1].flush_generation, reports[1].first_frame_index),
            (1, 150)
        );
        assert_eq!(reports[1].frame_count, 250);
        harness.stop();
    }

    #[test]
    fn a_block_the_canonical_layout_rejects_is_a_decode_error_and_never_pushed() {
        let mut blocks = blocks_of(10, 1);
        // 40,000 does not fit in 16 bits.
        blocks.push((10, vec![40_000; 20]));
        blocks.push((20, frames(20, 10)));
        let mut harness = start(blocks, Duration::from_secs(1));
        let pushed = harness.read_until_end_marker(0);
        assert_eq!(audio_of(&pushed), frames(0, 10));
        assert_eq!(pushed.last().expect("marker").0.first_frame_index, 10);
        let reports = harness.reports();
        assert_eq!(reports[0].ended_by, DecodedSegmentEnd::DecodeError);
        assert_eq!(reports[0].frame_count, 10);
        let ended = loop {
            if let DecodeEvent::SegmentEnded { error, .. } = harness.next_event() {
                break error;
            }
        };
        assert!(matches!(
            ended,
            Some(SegmentFailure::Decode(DecodeError::CorruptData { .. }))
        ));
        harness.stop();
    }

    #[test]
    fn stop_while_the_ring_is_full_sends_an_abandoned_report() {
        let mut harness = start(blocks_of(100, 50), Duration::from_millis(200));
        assert!(matches!(
            harness.next_event(),
            DecodeEvent::PrefillReached {
                flush_generation: 0
            }
        ));
        harness.stop();
        let reports = harness.reports();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].ended_by, DecodedSegmentEnd::Abandoned);
        assert!(!harness.end_flag.load(Ordering::Acquire));
    }
}
