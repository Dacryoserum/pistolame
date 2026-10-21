//! `DeviceFeeder`: the `RenderSource` that turns playback-ring blocks into device bytes on the
//! backend's real-time thread (plan B6, B7.2).
//!
//! Per `render` call it pops headers and samples, packs them into the render format's
//! container, reports segments and track ends to the verification tap (both modes), copies the
//! packed bytes into the tap (exclusive mode only) and updates the lock-free counters.
//!
//! Real-time rules, for `begin_stream` and `render`:
//! - no allocation, no lock, no I/O, no logging: only `rtrb` ring operations, atomics and
//!   integer packing over the caller's buffer;
//! - bounded work: each loop step either writes at least one frame into `destination` or
//!   removes one header from the playback ring, so a call does at most
//!   `frames in destination + headers in the ring` steps;
//! - no panic path: slices are taken with `get`/`get_mut`/`split_at_mut_checked`, arithmetic
//!   on sizes is checked or saturating, and contract violations are counted, never asserted;
//! - integer only (lint below).

#![deny(clippy::float_arithmetic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::engine::PlaybackCounters;
use crate::format::{DeviceSampleFormat, StreamFormat};
use crate::output_contract::{
    DeviceAccessMode, RenderOutcome, RenderSource, RenderStreamDescription,
};
use crate::pcm::{pack_samples_into_container, validate_pcm_layout};
use crate::playback_ring::{PlaybackRingReader, SampleBlockHeader, TrackId};
use crate::verification::TapEvent;
use crate::verification::tap::{TapPushOutcome, VerificationTapWriter};

/// The render source of every stream: feeds the device from the playback ring and the
/// verification tap from the device bytes. Survives pause and reopen (`OutputStream::close`
/// hands it back): its ring position, block in progress and next frame index carry over to
/// the next `begin_stream`.
pub struct DeviceFeeder {
    ring_reader: PlaybackRingReader,
    tap_writer: VerificationTapWriter,
    flush_generation: Arc<AtomicU64>,
    /// Set by the decode thread once it has pushed the last block of the last queued track (the
    /// end marker); cleared when a seek makes it queue more. Only then is an empty ring the end
    /// of the stream.
    is_playback_queue_finished: Arc<AtomicBool>,
    counters: Arc<PlaybackCounters>,
    /// Set by `begin_stream`; `None` before it or when the render format is unusable.
    stream: Option<ActiveStream>,
    /// The block whose header was popped and whose remaining samples are still in the ring.
    block_in_progress: Option<BlockInProgress>,
    /// The latest segment announced on the tap with `SegmentStarted`.
    announced_segment: Option<SegmentIdentity>,
    /// The feeder is stopped at a format boundary (counted once per boundary).
    is_at_format_boundary: bool,
    /// The feeder is at the end of the stream (counted once per end, B5.7).
    is_at_end_of_stream: bool,
    /// Test hook run once right after the end-of-queue flag is read in `render`, to inject the
    /// decoder/engine race between that read and the ring check.
    #[cfg(test)]
    after_queue_state_read: Option<Box<dyn FnOnce() + Send>>,
}

/// What `begin_stream` established for the current stream.
#[derive(Clone, Copy, Debug)]
struct ActiveStream {
    render_format: DeviceSampleFormat,
    bytes_per_frame: usize,
    /// Device bytes are tapped in exclusive mode only: in shared mode Windows owns the samples
    /// after us (B7.2). Segment, track and stream events are tapped in both modes.
    is_device_bytes_tap_enabled: bool,
}

/// A block whose header has been popped.
#[derive(Clone, Copy, Debug)]
struct BlockInProgress {
    header: SampleBlockHeader,
    next_frame_index: u64,
    remaining_frame_count: u32,
}

/// One segment: the frames of one track under one flush generation (B7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SegmentIdentity {
    track_id: TrackId,
    flush_generation: u64,
}

/// What `render_with` does when the ring is empty after the last block of the track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndOfTrackHandling {
    /// The end of the stream: counted once in `stream_ends_reached` (B5.7).
    EndTheStream,
    /// Returned as `EndOfStream` without counting it, so a `TrackChain` can go on with the
    /// next track in the same buffer; it calls `enter_end_of_stream` when it cannot.
    ReportDrained,
}

/// Why the next block cannot be rendered now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoRenderableBlock {
    /// The ring holds no current block.
    RingEmpty,
    /// The next block's format cannot be packed into the render format.
    FormatBoundary,
}

/// Whether a block of `source` can be packed into `render` without changing any sample: same
/// rate and channel layout, and source bits within the valid bits. With a render format that
/// passed `validate_pcm_layout`, this makes `validate_pcm_layout(source bits, render)` pass too.
fn can_render_source_format(source: &StreamFormat, render: &DeviceSampleFormat) -> bool {
    source.sample_rate_hz == render.sample_rate_hz
        && source.channel_count == render.channel_count
        && source.channel_mask == render.channel_mask
        && (1..=render.valid_bits_per_sample).contains(&source.bits_per_sample)
}

/// Writes silence (zero bytes: signed PCM zero in every container and alignment).
fn fill_with_silence(bytes: Option<&mut [u8]>) {
    if let Some(bytes) = bytes {
        bytes.fill(0);
    }
}

impl DeviceFeeder {
    /// The render format is not known yet: the backend negotiates it and calls
    /// `begin_stream`, once per opened stream. The same feeder survives pause and reopen.
    /// `is_playback_queue_finished` is owned by the decode thread: it sets it once the last
    /// block of the last queued track (the end marker) is in the ring, and clears it on a seek,
    /// so a late decoder shows as a counted underrun, never as `EndOfStream`. Owning it in that
    /// single thread means an old generation can never set it after a seek.
    pub fn new(
        ring_reader: PlaybackRingReader,
        tap_writer: VerificationTapWriter,
        flush_generation: Arc<AtomicU64>,
        is_playback_queue_finished: Arc<AtomicBool>,
        counters: Arc<PlaybackCounters>,
    ) -> Self {
        Self {
            ring_reader,
            tap_writer,
            flush_generation,
            is_playback_queue_finished,
            counters,
            stream: None,
            block_in_progress: None,
            announced_segment: None,
            is_at_format_boundary: false,
            is_at_end_of_stream: false,
            #[cfg(test)]
            after_queue_state_read: None,
        }
    }

    /// Pushes tap events still waiting for room in the event ring (for example the last
    /// `TrackEnded` when the verifier lagged at the end of the queue). Returns `true` when
    /// nothing is left waiting. `render` does this on every call; once the stream is closed
    /// nothing calls `render`, so the engine must call this (off the real-time thread is fine)
    /// until it returns `true`, letting the verifier drain the tap in between. Never blocks.
    pub fn flush_verification_events(&mut self) -> bool {
        self.tap_writer.flush_pending_events()
    }

    /// Drops the block in progress and every queued block whose generation is older than the
    /// current flush generation (counted in `stale_frames_discarded`). Off the real-time
    /// thread only, while the feeder is in no stream: lets the decode thread refill a ring
    /// that a seek made stale without opening a stream first. Stops at the first current block,
    /// since generations only grow along the ring.
    pub fn discard_stale_blocks(&mut self) {
        let current_generation = self.flush_generation.load(Ordering::Acquire);
        if let Some(block) = self.block_in_progress {
            if block.header.flush_generation >= current_generation {
                return;
            }
            self.discard_block_in_progress(block);
        }
        while let Some(header) = self.ring_reader.peek_header().copied() {
            if header.flush_generation >= current_generation {
                return;
            }
            if self.ring_reader.pop_header().is_none() {
                return;
            }
            self.discard_block_in_progress(BlockInProgress {
                header,
                next_frame_index: header.first_frame_index,
                remaining_frame_count: header.frame_count,
            });
        }
    }

    /// Fills `destination` (whole frames of `stream`) from the ring. Returns how many frames
    /// were written with audio and the outcome to report if that is fewer than all of them.
    fn render_frames(
        &mut self,
        stream: ActiveStream,
        destination: &mut [u8],
        end_of_track: EndOfTrackHandling,
    ) -> (usize, RenderOutcome) {
        let frame_capacity = destination.len() / stream.bytes_per_frame;
        let current_generation = self.flush_generation.load(Ordering::Acquire);
        // Read before looking at the ring. The engine sets the flag only after the last block
        // is in the ring, so `true` here means an empty ring below really is the end. Reading
        // it after an empty ring could see a last block pushed in between as never played.
        let is_queue_finished = self.is_playback_queue_finished.load(Ordering::Acquire);
        #[cfg(test)]
        if let Some(inject_race) = self.after_queue_state_read.take() {
            inject_race();
        }
        let mut written_frame_count = 0usize;
        while written_frame_count < frame_capacity {
            let block = match self.renderable_block(current_generation, &stream) {
                Ok(block) => block,
                Err(NoRenderableBlock::FormatBoundary) => {
                    self.enter_format_boundary();
                    return (written_frame_count, RenderOutcome::FormatBoundaryReached);
                }
                Err(NoRenderableBlock::RingEmpty) => {
                    let silent_frame_count = frame_capacity - written_frame_count;
                    let outcome =
                        self.on_ring_empty(is_queue_finished, silent_frame_count, end_of_track);
                    return (written_frame_count, outcome);
                }
            };
            let frame_count = usize::try_from(block.remaining_frame_count)
                .unwrap_or(usize::MAX)
                .min(frame_capacity - written_frame_count);
            let first_byte = written_frame_count * stream.bytes_per_frame;
            let byte_range = first_byte..first_byte + frame_count * stream.bytes_per_frame;
            let Some(chunk_destination) = destination.get_mut(byte_range) else {
                return self.abandon_inconsistent_block(block, written_frame_count);
            };
            if !self.render_chunk(block, frame_count, chunk_destination, &stream) {
                return self.abandon_inconsistent_block(block, written_frame_count);
            }
            written_frame_count += frame_count;
        }
        (written_frame_count, RenderOutcome::Filled)
    }

    /// The ring has nothing to play: the end of the stream if the engine said the queue was
    /// finished (read before the ring was checked), otherwise an underrun (counted), even
    /// right after a track end.
    fn on_ring_empty(
        &mut self,
        is_queue_finished: bool,
        silent_frame_count: usize,
        end_of_track: EndOfTrackHandling,
    ) -> RenderOutcome {
        if is_queue_finished {
            if end_of_track == EndOfTrackHandling::EndTheStream {
                self.enter_end_of_stream();
            }
            return RenderOutcome::EndOfStream;
        }
        self.counters
            .record_underrun(u64::try_from(silent_frame_count).unwrap_or(u64::MAX));
        RenderOutcome::FilledWithUnderrun
    }

    /// Counts the end of the stream once (re-armed when audio renders again or a stream
    /// begins). Real-time safe.
    pub(crate) fn enter_end_of_stream(&mut self) {
        if !self.is_at_end_of_stream {
            self.is_at_end_of_stream = true;
            self.counters.record_stream_end_reached();
        }
    }

    /// Whether the end of the stream was counted and no audio rendered since.
    pub(crate) fn has_reached_end_of_stream(&self) -> bool {
        self.is_at_end_of_stream
    }

    /// Bytes per frame of the current stream; `None` before `begin_stream` or with an unusable
    /// render format.
    pub(crate) fn bytes_per_frame(&self) -> Option<usize> {
        self.stream.map(|stream| stream.bytes_per_frame)
    }

    /// `render`, with the end of the track handled as `end_of_track` says. Also returns how
    /// many whole frames at the start of `destination` hold audio; the rest is silence.
    pub(crate) fn render_with(
        &mut self,
        destination: &mut [u8],
        end_of_track: EndOfTrackHandling,
    ) -> (usize, RenderOutcome) {
        self.tap_writer.flush_pending_events();
        let Some(stream) = self.stream else {
            destination.fill(0);
            self.counters.record_render_contract_violation();
            return (0, RenderOutcome::FilledWithUnderrun);
        };
        let whole_frame_bytes = destination.len() - destination.len() % stream.bytes_per_frame;
        let has_partial_frame = whole_frame_bytes != destination.len();
        if has_partial_frame {
            self.counters.record_render_contract_violation();
            fill_with_silence(destination.get_mut(whole_frame_bytes..));
        }
        let Some(whole_frames) = destination.get_mut(..whole_frame_bytes) else {
            return (0, RenderOutcome::FilledWithUnderrun);
        };
        let (written_frame_count, outcome) = self.render_frames(stream, whole_frames, end_of_track);
        fill_with_silence(whole_frames.get_mut(written_frame_count * stream.bytes_per_frame..));
        if has_partial_frame && outcome == RenderOutcome::Filled {
            return (written_frame_count, RenderOutcome::FilledWithUnderrun);
        }
        (written_frame_count, outcome)
    }

    fn enter_format_boundary(&mut self) {
        if !self.is_at_format_boundary {
            self.is_at_format_boundary = true;
            self.counters.record_format_boundary_reached();
        }
    }

    /// The block to render next: the one in progress, or the next current one from the ring.
    /// Drops stale blocks (older flush generation) and finishes empty blocks on the way.
    fn renderable_block(
        &mut self,
        current_generation: u64,
        stream: &ActiveStream,
    ) -> Result<BlockInProgress, NoRenderableBlock> {
        loop {
            let Some(block) = self.block_in_progress else {
                self.take_next_block(current_generation, stream)?;
                continue;
            };
            if block.header.flush_generation < current_generation {
                self.discard_block_in_progress(block);
                continue;
            }
            if !can_render_source_format(&block.header.format, &stream.render_format) {
                return Err(NoRenderableBlock::FormatBoundary);
            }
            if block.remaining_frame_count == 0 {
                self.finish_block(block);
                continue;
            }
            return Ok(block);
        }
    }

    /// Pops the next header that is not stale into `block_in_progress`, announcing a new
    /// segment. Stale headers and their samples are dropped and counted. Leaves a header of an
    /// incompatible format in the ring, so it is never packed in the wrong format.
    fn take_next_block(
        &mut self,
        current_generation: u64,
        stream: &ActiveStream,
    ) -> Result<(), NoRenderableBlock> {
        loop {
            let header = *self
                .ring_reader
                .peek_header()
                .ok_or(NoRenderableBlock::RingEmpty)?;
            if header.flush_generation >= current_generation
                && !can_render_source_format(&header.format, &stream.render_format)
            {
                return Err(NoRenderableBlock::FormatBoundary);
            }
            if self.ring_reader.pop_header().is_none() {
                return Err(NoRenderableBlock::RingEmpty);
            }
            let block = BlockInProgress {
                header,
                next_frame_index: header.first_frame_index,
                remaining_frame_count: header.frame_count,
            };
            if header.flush_generation < current_generation {
                self.discard_block_in_progress(block);
                continue;
            }
            self.counters
                .record_position(header.track_id, header.first_frame_index);
            self.announce_segment(&header);
            self.block_in_progress = Some(block);
            return Ok(());
        }
    }

    /// Drops the remaining samples of a stale block and counts them.
    fn discard_block_in_progress(&mut self, block: BlockInProgress) {
        self.block_in_progress = None;
        let channel_count = usize::from(block.header.format.channel_count);
        let remaining_samples = usize::try_from(block.remaining_frame_count)
            .unwrap_or(usize::MAX)
            .saturating_mul(channel_count);
        let dropped_samples = self.ring_reader.discard_samples(remaining_samples);
        if dropped_samples != remaining_samples {
            self.counters.record_render_contract_violation();
        }
        self.counters
            .record_stale_frames_discarded(u64::from(block.remaining_frame_count));
    }

    /// Packs `frame_count` frames of `block` into `chunk_destination`, taps them (exclusive
    /// mode), and advances the block. `false` if the ring did not hold the samples the header
    /// announced.
    fn render_chunk(
        &mut self,
        block: BlockInProgress,
        frame_count: usize,
        chunk_destination: &mut [u8],
        stream: &ActiveStream,
    ) -> bool {
        let Ok(frame_count_u32) = u32::try_from(frame_count) else {
            return false;
        };
        let source_bits = block.header.format.bits_per_sample;
        let container_bytes = usize::from(stream.render_format.container_bytes);
        let sample_count = frame_count * usize::from(block.header.format.channel_count);
        let render_format = stream.render_format;
        let was_packed = self
            .ring_reader
            .read_samples(sample_count, |first_samples, second_samples| {
                let (first_bytes, second_bytes) = chunk_destination
                    .split_at_mut_checked(first_samples.len() * container_bytes)?;
                pack_samples_into_container(
                    first_samples,
                    source_bits,
                    &render_format,
                    first_bytes,
                );
                pack_samples_into_container(
                    second_samples,
                    source_bits,
                    &render_format,
                    second_bytes,
                );
                Some(())
            })
            .flatten()
            .is_some();
        if !was_packed {
            return false;
        }
        self.is_at_format_boundary = false;
        self.is_at_end_of_stream = false;
        if stream.is_device_bytes_tap_enabled {
            self.tap_device_bytes(&block, frame_count_u32, chunk_destination);
        }
        self.advance_block(block, frame_count_u32);
        true
    }

    /// Copies the bytes just packed (read back from the device buffer, B7.2) into the tap.
    fn tap_device_bytes(&mut self, block: &BlockInProgress, frame_count: u32, packed: &[u8]) {
        let outcome = self.tap_writer.push_device_bytes(
            packed,
            block.header.track_id,
            block.next_frame_index,
            frame_count,
        );
        self.count_tap_outcome(outcome);
    }

    /// Records the frames rendered and finishes the block when it is exhausted.
    fn advance_block(&mut self, block: BlockInProgress, frame_count: u32) {
        let advanced = BlockInProgress {
            next_frame_index: block
                .next_frame_index
                .saturating_add(u64::from(frame_count)),
            remaining_frame_count: block.remaining_frame_count.saturating_sub(frame_count),
            ..block
        };
        self.counters
            .record_position(advanced.header.track_id, advanced.next_frame_index);
        if advanced.remaining_frame_count == 0 {
            self.finish_block(advanced);
        } else {
            self.block_in_progress = Some(advanced);
        }
    }

    /// Leaves an exhausted block; emits `TrackEnded` (both modes) after the last frame of a
    /// track.
    fn finish_block(&mut self, block: BlockInProgress) {
        self.block_in_progress = None;
        if block.header.is_track_end {
            self.push_tap_event(TapEvent::TrackEnded {
                track_id: block.header.track_id,
            });
        }
    }

    /// Emits `SegmentStarted`, in both modes, when the first block of a new (track,
    /// generation) is taken: `first_frame_index` is the segment's real first frame.
    fn announce_segment(&mut self, header: &SampleBlockHeader) {
        let segment = SegmentIdentity {
            track_id: header.track_id,
            flush_generation: header.flush_generation,
        };
        if self.announced_segment == Some(segment) {
            return;
        }
        // Marked even if dropped: the `TapOverflowed` that follows tells the verifier, and
        // announcing later at another frame index would describe the segment wrongly.
        self.announced_segment = Some(segment);
        self.push_tap_event(TapEvent::SegmentStarted {
            track_id: segment.track_id,
            flush_generation: segment.flush_generation,
            first_frame_index: header.first_frame_index,
        });
    }

    fn push_tap_event(&mut self, event: TapEvent) {
        let outcome = self.tap_writer.push_event(event);
        self.count_tap_outcome(outcome);
    }

    fn count_tap_outcome(&self, outcome: TapPushOutcome) {
        if outcome == TapPushOutcome::Dropped {
            self.counters.record_verification_tap_overflow();
        }
    }

    /// The ring did not hold the samples a header announced: never expected. The block is
    /// dropped (its samples cannot be located any more) and the rest of the buffer is silence.
    /// The lost frames are marked on the tap with `TapOverflowed`, and a track end the block
    /// carried is still reported, so the verifier concludes instead of waiting forever.
    fn abandon_inconsistent_block(
        &mut self,
        block: BlockInProgress,
        written_frame_count: usize,
    ) -> (usize, RenderOutcome) {
        self.block_in_progress = None;
        self.counters.record_render_contract_violation();
        self.tap_writer.mark_overflow(Some(block.header.track_id));
        if block.header.is_track_end {
            self.push_tap_event(TapEvent::TrackEnded {
                track_id: block.header.track_id,
            });
        }
        (written_frame_count, RenderOutcome::FilledWithUnderrun)
    }
}

impl RenderSource for DeviceFeeder {
    /// Stores the description, enables the device-bytes tap only for
    /// `DeviceAccessMode::Exclusive`, and pushes `TapEvent::StreamBegan` (in both modes, so the
    /// verifier knows when part of a segment goes through the system mixer). The ring
    /// position, block in progress and next frame index are kept, so frame indices continue
    /// across a reopen. A render format that fails `validate_pcm_layout` (or has no channel) is
    /// counted, and `render` then writes silence until the next `begin_stream`.
    fn begin_stream(&mut self, description: RenderStreamDescription) {
        let render_format = description.render_format;
        let is_render_format_usable = render_format.channel_count > 0
            && validate_pcm_layout(render_format.valid_bits_per_sample, &render_format).is_ok();
        self.stream = if is_render_format_usable {
            Some(ActiveStream {
                render_format,
                bytes_per_frame: render_format.bytes_per_frame(),
                is_device_bytes_tap_enabled: description.access_mode == DeviceAccessMode::Exclusive,
            })
        } else {
            self.counters.record_render_contract_violation();
            None
        };
        self.is_at_format_boundary = false;
        // A new stream reports its own end, so the engine sees a track end reached again after a
        // pause at the very end (it compares with the count taken when it opened the stream).
        self.is_at_end_of_stream = false;
        self.push_tap_event(TapEvent::StreamBegan {
            render_format,
            access_mode: description.access_mode,
        });
    }

    /// Fills the whole `destination`. Outcomes: `Filled` only when every byte is audio, so any
    /// other outcome tells the backend to check for an all-silent buffer
    /// (`AUDCLNT_BUFFERFLAGS_SILENT`). Silence is never tapped. Underrun silence is counted.
    /// Before `begin_stream`, or with an unusable render format, the whole buffer is silence,
    /// counted as a contract violation, and the outcome is `FilledWithUnderrun`; so is a
    /// buffer ending in a partial frame (that partial frame is silenced and counted).
    fn render(&mut self, destination: &mut [u8]) -> RenderOutcome {
        self.render_with(destination, EndOfTrackHandling::EndTheStream)
            .1
    }

    /// `discard_stale_blocks`, then `flush_verification_events`.
    fn service_while_closed(&mut self) -> bool {
        self.discard_stale_blocks();
        self.flush_verification_events()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::format::{ChannelMask, ValidBitsAlignment};
    use crate::playback_ring::{PlaybackRingWriter, create_playback_ring};
    use crate::verification::tap::{
        VerificationTapReader, create_verification_tap, tap_with_capacities,
    };

    const SAMPLE_RATE_HZ: u32 = 1_000;
    const CHANNEL_COUNT: u8 = 2;
    const TRACK: TrackId = TrackId(5);
    const EXCLUSIVE: DeviceAccessMode = DeviceAccessMode::Exclusive;
    const SHARED: DeviceAccessMode = DeviceAccessMode::SharedThroughSystemMixer;

    fn source_format() -> StreamFormat {
        StreamFormat {
            sample_rate_hz: SAMPLE_RATE_HZ,
            bits_per_sample: 24,
            channel_count: CHANNEL_COUNT,
            channel_mask: ChannelMask(0b11),
        }
    }

    fn render_format(container_bytes: u8, valid_bits_per_sample: u8) -> DeviceSampleFormat {
        DeviceSampleFormat {
            sample_rate_hz: SAMPLE_RATE_HZ,
            container_bytes,
            valid_bits_per_sample,
            alignment: ValidBitsAlignment::MostSignificant,
            channel_count: CHANNEL_COUNT,
            channel_mask: ChannelMask(0b11),
        }
    }

    fn packed_3_bytes() -> DeviceSampleFormat {
        render_format(3, 24)
    }

    fn packed_4_bytes() -> DeviceSampleFormat {
        render_format(4, 32)
    }

    struct Harness {
        ring_writer: PlaybackRingWriter,
        tap_reader: VerificationTapReader,
        feeder: DeviceFeeder,
        flush_generation: Arc<AtomicU64>,
        is_playback_queue_finished: Arc<AtomicBool>,
        counters: Arc<PlaybackCounters>,
    }

    fn harness_with_tap_duration(tap_duration: Duration) -> Harness {
        harness_with_tap(create_verification_tap(
            SAMPLE_RATE_HZ,
            4 * usize::from(CHANNEL_COUNT),
            tap_duration,
        ))
    }

    fn harness_with_tap(
        (tap_writer, tap_reader): (VerificationTapWriter, VerificationTapReader),
    ) -> Harness {
        let (ring_writer, ring_reader) =
            create_playback_ring(&source_format(), Duration::from_secs(1));
        let flush_generation = Arc::new(AtomicU64::new(0));
        let is_playback_queue_finished = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(PlaybackCounters::new());
        let feeder = DeviceFeeder::new(
            ring_reader,
            tap_writer,
            Arc::clone(&flush_generation),
            Arc::clone(&is_playback_queue_finished),
            Arc::clone(&counters),
        );
        Harness {
            ring_writer,
            tap_reader,
            feeder,
            flush_generation,
            is_playback_queue_finished,
            counters,
        }
    }

    fn harness() -> Harness {
        harness_with_tap_duration(Duration::from_secs(1))
    }

    /// Distinct 24-bit values, negative and positive, so any misplaced byte shows.
    fn samples(first_frame_index: u64, frame_count: u32) -> Vec<i32> {
        let first_sample = first_frame_index as i32 * i32::from(CHANNEL_COUNT);
        let sample_count = frame_count as i32 * i32::from(CHANNEL_COUNT);
        (first_sample..first_sample + sample_count)
            .map(|index| (index * 40_503 - 3_000_000) % 8_388_607)
            .collect()
    }

    fn block_header(
        track_id: TrackId,
        flush_generation: u64,
        first_frame_index: u64,
        frame_count: u32,
        is_track_end: bool,
    ) -> SampleBlockHeader {
        SampleBlockHeader {
            track_id,
            flush_generation,
            format: source_format(),
            first_frame_index,
            frame_count,
            is_track_start: first_frame_index == 0,
            is_track_end,
        }
    }

    impl Harness {
        fn push(&mut self, header: SampleBlockHeader) -> Vec<i32> {
            let block_samples = samples(header.first_frame_index, header.frame_count);
            self.ring_writer
                .try_push_block(header, &block_samples)
                .expect("ring has room");
            block_samples
        }

        fn begin(&mut self, format: DeviceSampleFormat, access_mode: DeviceAccessMode) {
            self.feeder.begin_stream(RenderStreamDescription {
                render_format: format,
                access_mode,
            });
        }

        fn render(
            &mut self,
            frame_count: usize,
            format: &DeviceSampleFormat,
        ) -> (Vec<u8>, RenderOutcome) {
            let mut destination = vec![0xAA; frame_count * format.bytes_per_frame()];
            let outcome = self.feeder.render(&mut destination);
            (destination, outcome)
        }

        fn tap_events(&mut self) -> Vec<TapEvent> {
            std::iter::from_fn(|| self.tap_reader.pop_event()).collect()
        }

        fn tap_bytes(&mut self, byte_count: usize) -> Vec<u8> {
            let mut bytes = vec![0; byte_count];
            self.tap_reader
                .pop_device_bytes(&mut bytes)
                .expect("tapped bytes present");
            bytes
        }

        fn has_no_tapped_bytes(&mut self) -> bool {
            self.tap_reader.pop_device_bytes(&mut [0u8; 1]).is_err()
        }
    }

    fn packed(samples: &[i32], format: &DeviceSampleFormat) -> Vec<u8> {
        let mut bytes = vec![0; samples.len() * usize::from(format.container_bytes)];
        pack_samples_into_container(samples, 24, format, &mut bytes);
        bytes
    }

    fn stream_began(format: DeviceSampleFormat, access_mode: DeviceAccessMode) -> TapEvent {
        TapEvent::StreamBegan {
            render_format: format,
            access_mode,
        }
    }

    fn segment_started(flush_generation: u64, first_frame_index: u64) -> TapEvent {
        TapEvent::SegmentStarted {
            track_id: TRACK,
            flush_generation,
            first_frame_index,
        }
    }

    fn device_bytes(
        first_frame_index: u64,
        frame_count: u32,
        format: &DeviceSampleFormat,
    ) -> TapEvent {
        TapEvent::DeviceBytes {
            track_id: TRACK,
            first_frame_index,
            frame_count,
            byte_count: frame_count as usize * format.bytes_per_frame(),
        }
    }

    fn track_ended(track_id: TrackId) -> TapEvent {
        TapEvent::TrackEnded { track_id }
    }

    #[test]
    fn renders_exact_bytes_and_taps_them_in_exclusive_mode() {
        let mut harness = harness();
        let format = packed_3_bytes();
        let track_samples = harness.push(block_header(TRACK, 0, 0, 4, true));
        harness.begin(format, EXCLUSIVE);
        let (bytes, outcome) = harness.render(4, &format);
        assert_eq!(outcome, RenderOutcome::Filled);
        assert_eq!(bytes, packed(&track_samples, &format));
        assert_eq!(
            harness.tap_events(),
            vec![
                stream_began(format, EXCLUSIVE),
                segment_started(0, 0),
                device_bytes(0, 4, &format),
                track_ended(TRACK),
            ]
        );
        assert_eq!(harness.tap_bytes(bytes.len()), bytes);

        harness
            .is_playback_queue_finished
            .store(true, Ordering::Release);
        let (bytes, outcome) = harness.render(3, &format);
        assert_eq!(outcome, RenderOutcome::EndOfStream);
        assert!(bytes.iter().all(|byte| *byte == 0));
        assert!(harness.tap_events().is_empty(), "silence is never tapped");
        let snapshot = harness.counters.snapshot();
        assert_eq!(snapshot.underrun_events, 0);
        assert_eq!(snapshot.current_track_id, TRACK.0);
        assert_eq!(snapshot.frames_rendered_in_track, 4);
    }

    #[test]
    fn variable_buffer_lengths_across_blocks_keep_bytes_and_frame_indices_exact() {
        let mut harness = harness();
        let format = packed_3_bytes();
        let mut all_samples = harness.push(block_header(TRACK, 0, 0, 5, false));
        all_samples.extend(harness.push(block_header(TRACK, 0, 5, 3, false)));
        all_samples.extend(harness.push(block_header(TRACK, 0, 8, 7, true)));
        harness.begin(format, EXCLUSIVE);
        let mut rendered = Vec::new();
        for buffer_frames in [2, 6, 1, 4, 2] {
            let (bytes, outcome) = harness.render(buffer_frames, &format);
            assert_eq!(outcome, RenderOutcome::Filled);
            rendered.extend(bytes);
        }
        assert_eq!(rendered, packed(&all_samples, &format));
        let mut next_frame_index = 0u64;
        for event in harness.tap_events() {
            if let TapEvent::DeviceBytes {
                first_frame_index,
                frame_count,
                byte_count,
                ..
            } = event
            {
                assert_eq!(first_frame_index, next_frame_index, "contiguous frames");
                assert_eq!(byte_count, frame_count as usize * format.bytes_per_frame());
                next_frame_index += u64::from(frame_count);
            }
        }
        assert_eq!(next_frame_index, 15);
        assert_eq!(harness.tap_bytes(rendered.len()), rendered);
    }

    #[test]
    fn reopen_with_another_container_continues_frame_indices() {
        let mut harness = harness();
        let track_samples = harness.push(block_header(TRACK, 0, 0, 6, true));
        harness.begin(packed_3_bytes(), EXCLUSIVE);
        let (first_bytes, _) = harness.render(3, &packed_3_bytes());
        // Pause by release, then resume on a stream that got a 4-byte container.
        harness.begin(packed_4_bytes(), EXCLUSIVE);
        let (second_bytes, outcome) = harness.render(3, &packed_4_bytes());
        assert_eq!(outcome, RenderOutcome::Filled);
        assert_eq!(first_bytes, packed(&track_samples[..6], &packed_3_bytes()));
        assert_eq!(second_bytes, packed(&track_samples[6..], &packed_4_bytes()));
        assert_eq!(
            harness.tap_events(),
            vec![
                stream_began(packed_3_bytes(), EXCLUSIVE),
                segment_started(0, 0),
                device_bytes(0, 3, &packed_3_bytes()),
                stream_began(packed_4_bytes(), EXCLUSIVE),
                device_bytes(3, 3, &packed_4_bytes()),
                track_ended(TRACK),
            ]
        );
        assert_eq!(harness.tap_bytes(first_bytes.len()), first_bytes);
        assert_eq!(harness.tap_bytes(second_bytes.len()), second_bytes);
    }

    #[test]
    fn shared_mode_taps_segment_and_track_events_but_no_bytes() {
        let mut harness = harness();
        let format = packed_4_bytes();
        let track_samples = harness.push(block_header(TRACK, 0, 0, 4, true));
        harness.begin(format, SHARED);
        let (bytes, outcome) = harness.render(4, &format);
        assert_eq!(outcome, RenderOutcome::Filled);
        assert_eq!(bytes, packed(&track_samples, &format));
        assert_eq!(
            harness.tap_events(),
            vec![
                stream_began(format, SHARED),
                segment_started(0, 0),
                track_ended(TRACK),
            ]
        );
        assert!(harness.has_no_tapped_bytes());
    }

    #[test]
    fn switching_from_exclusive_to_shared_mid_track_keeps_the_segment_complete() {
        let mut harness = harness();
        let format = packed_3_bytes();
        let track_samples = harness.push(block_header(TRACK, 0, 0, 6, true));
        harness.begin(format, EXCLUSIVE);
        let (exclusive_bytes, _) = harness.render(2, &format);
        harness.begin(format, SHARED);
        let (shared_bytes, outcome) = harness.render(4, &format);
        assert_eq!(outcome, RenderOutcome::Filled);
        assert_eq!(
            [exclusive_bytes.clone(), shared_bytes].concat(),
            packed(&track_samples, &format)
        );
        assert_eq!(
            harness.tap_events(),
            vec![
                stream_began(format, EXCLUSIVE),
                segment_started(0, 0),
                device_bytes(0, 2, &format),
                stream_began(format, SHARED),
                track_ended(TRACK),
            ]
        );
        assert_eq!(harness.tap_bytes(exclusive_bytes.len()), exclusive_bytes);
        assert!(harness.has_no_tapped_bytes());
    }

    #[test]
    fn switching_from_shared_to_exclusive_mid_track_announces_the_real_first_frame() {
        let mut harness = harness();
        let format = packed_3_bytes();
        let track_samples = harness.push(block_header(TRACK, 0, 0, 6, true));
        harness.begin(format, SHARED);
        let (shared_bytes, _) = harness.render(2, &format);
        harness.begin(packed_4_bytes(), EXCLUSIVE);
        let (exclusive_bytes, outcome) = harness.render(4, &packed_4_bytes());
        assert_eq!(outcome, RenderOutcome::Filled);
        assert_eq!(shared_bytes, packed(&track_samples[..4], &format));
        assert_eq!(
            exclusive_bytes,
            packed(&track_samples[4..], &packed_4_bytes())
        );
        assert_eq!(
            harness.tap_events(),
            vec![
                stream_began(format, SHARED),
                segment_started(0, 0),
                stream_began(packed_4_bytes(), EXCLUSIVE),
                device_bytes(2, 4, &packed_4_bytes()),
                track_ended(TRACK),
            ]
        );
        assert_eq!(harness.tap_bytes(exclusive_bytes.len()), exclusive_bytes);
    }

    #[test]
    fn underrun_inserts_counted_silence_that_is_never_tapped() {
        let mut harness = harness();
        let format = packed_3_bytes();
        let first_samples = harness.push(block_header(TRACK, 0, 0, 2, false));
        harness.begin(format, EXCLUSIVE);
        let (bytes, outcome) = harness.render(5, &format);
        assert_eq!(outcome, RenderOutcome::FilledWithUnderrun);
        let audio_length = 2 * format.bytes_per_frame();
        assert_eq!(bytes[..audio_length], packed(&first_samples, &format));
        assert!(bytes[audio_length..].iter().all(|byte| *byte == 0));
        let snapshot = harness.counters.snapshot();
        assert_eq!(snapshot.underrun_events, 1);
        assert_eq!(snapshot.silent_frames_inserted, 3);

        let second_samples = harness.push(block_header(TRACK, 0, 2, 2, true));
        let (bytes, outcome) = harness.render(2, &format);
        assert_eq!(outcome, RenderOutcome::Filled);
        assert_eq!(bytes, packed(&second_samples, &format));
        let events = harness.tap_events();
        assert_eq!(events[2], device_bytes(0, 2, &format));
        assert_eq!(
            events[3],
            device_bytes(2, 2, &format),
            "no gap: silence is not described"
        );
    }

    #[test]
    fn a_late_next_track_is_an_underrun_not_the_end_of_the_stream() {
        let mut harness = harness();
        let format = packed_3_bytes();
        let next_track = TrackId(6);
        harness.push(block_header(TRACK, 0, 0, 2, true));
        harness.begin(format, EXCLUSIVE);
        assert_eq!(harness.render(2, &format).1, RenderOutcome::Filled);
        // The track ended but the queue is not finished: the next decoder is just late.
        let (bytes, outcome) = harness.render(3, &format);
        assert_eq!(outcome, RenderOutcome::FilledWithUnderrun);
        assert!(bytes.iter().all(|byte| *byte == 0));
        assert_eq!(harness.counters.snapshot().underrun_events, 1);
        assert_eq!(harness.counters.snapshot().silent_frames_inserted, 3);
        let next_samples = harness.push(block_header(next_track, 0, 0, 2, true));
        let (bytes, outcome) = harness.render(2, &format);
        assert_eq!(outcome, RenderOutcome::Filled);
        assert_eq!(bytes, packed(&next_samples, &format));
    }

    #[test]
    fn an_empty_ring_is_the_end_of_the_stream_only_once_the_queue_is_finished() {
        let mut harness = harness();
        let format = packed_3_bytes();
        let track_samples = harness.push(block_header(TRACK, 0, 0, 2, true));
        harness
            .is_playback_queue_finished
            .store(true, Ordering::Release);
        harness.begin(format, EXCLUSIVE);
        let (bytes, outcome) = harness.render(5, &format);
        assert_eq!(outcome, RenderOutcome::EndOfStream);
        let audio_length = 2 * format.bytes_per_frame();
        assert_eq!(bytes[..audio_length], packed(&track_samples, &format));
        assert!(bytes[audio_length..].iter().all(|byte| *byte == 0));
        assert_eq!(harness.counters.snapshot().underrun_events, 0);
        assert_eq!(harness.counters.snapshot().silent_frames_inserted, 0);
    }

    #[test]
    fn stale_blocks_are_discarded_and_a_new_segment_starts() {
        let mut harness = harness();
        let format = packed_3_bytes();
        harness.push(block_header(TRACK, 0, 0, 4, false));
        harness.push(block_header(TRACK, 0, 4, 3, false));
        harness.begin(format, EXCLUSIVE);
        harness.render(2, &format);
        // Seek: the engine bumps the generation, the decoder pushes from the target.
        harness.flush_generation.store(1, Ordering::Release);
        let sought_samples = harness.push(block_header(TRACK, 1, 100, 2, true));
        let (bytes, outcome) = harness.render(2, &format);
        assert_eq!(outcome, RenderOutcome::Filled);
        assert_eq!(bytes, packed(&sought_samples, &format));
        assert_eq!(harness.counters.snapshot().stale_frames_discarded, 2 + 3);
        let events = harness.tap_events();
        assert_eq!(
            &events[3..5],
            &[segment_started(1, 100), device_bytes(100, 2, &format)]
        );
    }

    #[test]
    fn a_zero_frame_end_marker_and_a_following_track_play_gaplessly() {
        let mut harness = harness();
        let format = packed_3_bytes();
        let next_track = TrackId(6);
        let mut expected = harness.push(block_header(TRACK, 0, 0, 3, false));
        harness.push(block_header(TRACK, 0, 3, 0, true));
        expected.extend(harness.push(block_header(next_track, 0, 0, 2, true)));
        harness.begin(format, EXCLUSIVE);
        let (bytes, outcome) = harness.render(5, &format);
        assert_eq!(outcome, RenderOutcome::Filled);
        assert_eq!(bytes, packed(&expected, &format));
        let events = harness.tap_events();
        assert_eq!(
            &events[3..],
            &[
                track_ended(TRACK),
                TapEvent::SegmentStarted {
                    track_id: next_track,
                    flush_generation: 0,
                    first_frame_index: 0
                },
                TapEvent::DeviceBytes {
                    track_id: next_track,
                    first_frame_index: 0,
                    frame_count: 2,
                    byte_count: 2 * format.bytes_per_frame()
                },
                track_ended(next_track),
            ]
        );
    }

    #[test]
    fn render_before_begin_stream_writes_silence_and_counts_a_violation() {
        let mut harness = harness();
        harness.push(block_header(TRACK, 0, 0, 2, false));
        let (bytes, outcome) = harness.render(2, &packed_3_bytes());
        assert_eq!(outcome, RenderOutcome::FilledWithUnderrun);
        assert!(bytes.iter().all(|byte| *byte == 0));
        assert_eq!(harness.counters.snapshot().render_contract_violations, 1);
        assert!(harness.tap_events().is_empty());
    }

    #[test]
    fn an_unusable_render_format_gives_silence_and_counts_a_violation() {
        let mut harness = harness();
        harness.push(block_header(TRACK, 0, 0, 2, false));
        let no_channel = DeviceSampleFormat {
            channel_count: 0,
            ..packed_3_bytes()
        };
        harness.begin(no_channel, EXCLUSIVE);
        let mut destination = [0xAAu8; 12];
        assert_eq!(
            harness.feeder.render(&mut destination),
            RenderOutcome::FilledWithUnderrun
        );
        assert_eq!(destination, [0; 12]);
        assert_eq!(harness.counters.snapshot().render_contract_violations, 2);
    }

    #[test]
    fn a_partial_trailing_frame_is_silenced_counted_and_not_reported_as_filled() {
        let mut harness = harness();
        let format = packed_3_bytes();
        let track_samples = harness.push(block_header(TRACK, 0, 0, 2, false));
        harness.begin(format, EXCLUSIVE);
        let mut destination = vec![0xAAu8; 2 * format.bytes_per_frame() + 1];
        assert_eq!(
            harness.feeder.render(&mut destination),
            RenderOutcome::FilledWithUnderrun
        );
        assert_eq!(destination[..12], packed(&track_samples, &format));
        assert_eq!(destination[12], 0);
        let snapshot = harness.counters.snapshot();
        assert_eq!(snapshot.render_contract_violations, 1);
        assert_eq!(snapshot.underrun_events, 0);
    }

    #[test]
    fn tap_overflow_never_blocks_and_is_marked_and_counted() {
        // Room for 2 frames of 8 bytes in the tap.
        let mut harness = harness_with_tap_duration(Duration::from_millis(2));
        let format = packed_4_bytes();
        let mut track_samples = harness.push(block_header(TRACK, 0, 0, 4, false));
        track_samples.extend(harness.push(block_header(TRACK, 0, 4, 2, true)));
        harness.begin(format, EXCLUSIVE);
        let (bytes, outcome) = harness.render(4, &format);
        assert_eq!(outcome, RenderOutcome::Filled, "playback is unaffected");
        assert_eq!(bytes, packed(&track_samples[..8], &format));
        assert_eq!(
            harness.counters.snapshot().verification_tap_overflow_events,
            1
        );
        assert_eq!(
            harness.tap_events(),
            vec![
                stream_began(format, EXCLUSIVE),
                segment_started(0, 0),
                TapEvent::TapOverflowed {
                    track_id: Some(TRACK)
                },
            ]
        );
        let (bytes, _) = harness.render(2, &format);
        assert_eq!(bytes, packed(&track_samples[8..], &format));
        assert_eq!(
            harness.tap_events(),
            vec![device_bytes(4, 2, &format), track_ended(TRACK)]
        );
    }
    #[test]
    fn a_last_block_pushed_while_the_flag_is_set_is_played_before_the_end() {
        let mut harness = harness();
        let format = packed_3_bytes();
        harness.begin(format, EXCLUSIVE);
        assert_eq!(
            harness.render(2, &format).1,
            RenderOutcome::FilledWithUnderrun
        );
        // The decoder pushes the last block, then the engine sets the flag.
        let last_samples = harness.push(block_header(TRACK, 0, 0, 2, true));
        harness
            .is_playback_queue_finished
            .store(true, Ordering::Release);
        let (bytes, outcome) = harness.render(2, &format);
        assert_eq!(outcome, RenderOutcome::Filled);
        assert_eq!(bytes, packed(&last_samples, &format));
        assert_eq!(harness.render(2, &format).1, RenderOutcome::EndOfStream);
        assert_eq!(harness.tap_events().last(), Some(&track_ended(TRACK)));
    }

    /// Regression test for the end-of-queue race: the last block arrives and the flag is set
    /// between the flag read and the ring check. The block must still play and end its track.
    #[test]
    fn the_end_of_queue_race_never_drops_the_last_block() {
        let Harness {
            mut ring_writer,
            mut tap_reader,
            mut feeder,
            is_playback_queue_finished,
            counters,
            ..
        } = harness();
        let format = packed_3_bytes();
        feeder.begin_stream(RenderStreamDescription {
            render_format: format,
            access_mode: EXCLUSIVE,
        });
        let last_samples = samples(0, 2);
        let flag_for_engine = Arc::clone(&is_playback_queue_finished);
        let samples_for_decoder = last_samples.clone();
        feeder.after_queue_state_read = Some(Box::new(move || {
            ring_writer
                .try_push_block(block_header(TRACK, 0, 0, 2, true), &samples_for_decoder)
                .expect("ring has room");
            flag_for_engine.store(true, Ordering::Release);
        }));
        let mut destination = vec![0xAA; 4 * format.bytes_per_frame()];
        assert_eq!(
            feeder.render(&mut destination),
            RenderOutcome::FilledWithUnderrun,
            "the flag was read before the block arrived: not the end yet"
        );
        let audio_length = 2 * format.bytes_per_frame();
        assert_eq!(destination[..audio_length], packed(&last_samples, &format));
        assert!(is_playback_queue_finished.load(Ordering::Acquire));
        assert_eq!(feeder.render(&mut destination), RenderOutcome::EndOfStream);
        assert_eq!(counters.snapshot().underrun_events, 1);
        let events: Vec<TapEvent> = std::iter::from_fn(|| tap_reader.pop_event()).collect();
        assert_eq!(events.last(), Some(&track_ended(TRACK)));
    }

    #[test]
    fn stream_ends_are_counted_once_per_end_and_again_after_a_reopen() {
        let mut harness = harness();
        let format = packed_3_bytes();
        harness.push(block_header(TRACK, 0, 0, 2, true));
        harness
            .is_playback_queue_finished
            .store(true, Ordering::Release);
        harness.begin(format, EXCLUSIVE);
        for _ in 0..5 {
            harness.render(4, &format);
        }
        assert_eq!(harness.counters.snapshot().stream_ends_reached, 1);
        // Pause at the very end and resume: the new stream reports the end again.
        harness.begin(format, EXCLUSIVE);
        assert_eq!(harness.render(4, &format).1, RenderOutcome::EndOfStream);
        assert_eq!(harness.render(4, &format).1, RenderOutcome::EndOfStream);
        assert_eq!(harness.counters.snapshot().stream_ends_reached, 2);
        // Audio rendered again (after a seek) re-arms the count.
        harness.flush_generation.store(1, Ordering::Release);
        harness
            .is_playback_queue_finished
            .store(false, Ordering::Release);
        harness.push(block_header(TRACK, 1, 0, 2, true));
        assert_eq!(harness.render(2, &format).1, RenderOutcome::Filled);
        harness
            .is_playback_queue_finished
            .store(true, Ordering::Release);
        assert_eq!(harness.render(2, &format).1, RenderOutcome::EndOfStream);
        assert_eq!(harness.counters.snapshot().stream_ends_reached, 3);
        assert_eq!(harness.counters.snapshot().underrun_events, 0);
    }

    #[test]
    fn discard_stale_blocks_drops_only_older_generations_while_closed() {
        let mut harness = harness();
        let format = packed_3_bytes();
        harness.push(block_header(TRACK, 0, 0, 4, false));
        harness.push(block_header(TRACK, 0, 4, 3, false));
        harness.begin(format, EXCLUSIVE);
        harness.render(1, &format);
        // Closed and paused: a seek bumps the generation and the decoder refills.
        harness.flush_generation.store(1, Ordering::Release);
        harness.feeder.discard_stale_blocks();
        assert_eq!(harness.counters.snapshot().stale_frames_discarded, 3 + 3);
        let sought_samples = harness.push(block_header(TRACK, 1, 50, 2, true));
        harness.feeder.discard_stale_blocks();
        assert_eq!(
            harness.counters.snapshot().stale_frames_discarded,
            6,
            "a current block is kept"
        );
        assert!(harness.feeder.service_while_closed());
        harness.begin(format, EXCLUSIVE);
        let (bytes, outcome) = harness.render(2, &format);
        assert_eq!(outcome, RenderOutcome::Filled);
        assert_eq!(bytes, packed(&sought_samples, &format));
        let events = harness.tap_events();
        assert_eq!(events.last(), Some(&track_ended(TRACK)));
        assert!(events.contains(&segment_started(1, 50)));
    }

    #[test]
    fn discard_stale_blocks_keeps_a_current_block_in_progress() {
        let mut harness = harness();
        let format = packed_3_bytes();
        let track_samples = harness.push(block_header(TRACK, 0, 0, 4, true));
        harness.begin(format, EXCLUSIVE);
        let (first_bytes, _) = harness.render(1, &format);
        harness.feeder.discard_stale_blocks();
        let (rest_bytes, outcome) = harness.render(3, &format);
        assert_eq!(outcome, RenderOutcome::Filled);
        assert_eq!(
            [first_bytes, rest_bytes].concat(),
            packed(&track_samples, &format)
        );
        assert_eq!(harness.counters.snapshot().stale_frames_discarded, 0);
    }

    #[test]
    fn an_inconsistent_block_is_marked_and_still_ends_its_track() {
        let mut harness = harness();
        let format = packed_3_bytes();
        harness.push(block_header(TRACK, 0, 0, 2, true));
        // Corrupt the ring: the header now announces samples that are gone.
        harness.feeder.ring_reader.discard_samples(4);
        harness.begin(format, EXCLUSIVE);
        let (bytes, outcome) = harness.render(2, &format);
        assert_eq!(outcome, RenderOutcome::FilledWithUnderrun);
        assert!(bytes.iter().all(|byte| *byte == 0));
        assert_eq!(harness.counters.snapshot().render_contract_violations, 1);
        assert_eq!(
            harness.tap_events(),
            vec![
                stream_began(format, EXCLUSIVE),
                segment_started(0, 0),
                TapEvent::TapOverflowed {
                    track_id: Some(TRACK)
                },
                track_ended(TRACK),
            ]
        );
    }

    #[test]
    fn events_left_waiting_at_the_end_are_flushed_after_close() {
        // The event ring holds 2 events: the stream start and the segment start fill it.
        let mut harness = harness_with_tap(tap_with_capacities(64, 2));
        let format = packed_3_bytes();
        harness.push(block_header(TRACK, 0, 0, 2, true));
        harness
            .is_playback_queue_finished
            .store(true, Ordering::Release);
        harness.begin(format, SHARED);
        assert_eq!(harness.render(2, &format).1, RenderOutcome::Filled);
        // Stream closed: nothing calls render any more, the last TrackEnded still waits.
        assert!(!harness.feeder.flush_verification_events());
        assert_eq!(
            harness.tap_reader.pop_event(),
            Some(stream_began(format, SHARED))
        );
        assert!(harness.feeder.flush_verification_events());
        assert_eq!(
            harness.tap_events(),
            vec![segment_started(0, 0), track_ended(TRACK)]
        );
    }
}
