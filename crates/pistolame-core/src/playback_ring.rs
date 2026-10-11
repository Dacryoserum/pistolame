//! The playback ring: decoded blocks travelling from the decode thread to the real-time render
//! thread (plan B3.2).
//!
//! Two lock-free single-producer/single-consumer rings (`rtrb`): one of `SampleBlockHeader`s and
//! one of `i32` samples. The samples ring has a fixed element type, so the real-time side reads
//! plain slices with no per-block allocation or size limit; the header ring is small. Samples are
//! pushed before their header, so the reader never sees a header whose samples are missing.
//!
//! `i32` right-justified is the neutral representation: exactly what FLAC defines (RFC 9639 §9:
//! samples are signed integers of `bits_per_sample` bits), it holds anything up to 24 bits with
//! headroom, and conversion to any container is a shift plus a byte copy.

use std::time::Duration;

use crate::format::StreamFormat;

/// Smallest block a FLAC stream may use, except for its last block (RFC 9639 §8.2: the
/// STREAMINFO minimum block size, which excludes the last block, is at least 16). One header slot per
/// this many sample frames means valid FLAC streams never fill the header ring before the
/// samples ring. Smaller blocks would only shorten how much the ring buffers, never break it.
const MINIMUM_FLAC_BLOCK_FRAMES: usize = 16;

/// Header slots added on top of the per-frame share, for blocks that carry no samples (pure
/// end-of-track markers) and the short last block of a stream.
const EXTRA_HEADER_SLOTS: usize = 8;

/// Nanoseconds in one second, to turn a `Duration` into a frame count with integers only.
const NANOSECONDS_PER_SECOND: u128 = 1_000_000_000;

/// Identifies one playback of one track. Monotonic per engine run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TrackId(pub u64);

/// Describes the samples that follow it in the playback ring. Every block carries its format
/// and track markers (AGENTS.md: same format → keep streaming; different format → drain and
/// reopen).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleBlockHeader {
    /// Track this block belongs to.
    pub track_id: TrackId,
    /// Blocks whose generation is lower than the engine's current flush generation are
    /// discarded by the feeder (used for stop, seek and opening another file).
    pub flush_generation: u64,
    /// Format of the samples in this block.
    pub format: StreamFormat,
    /// Index of the first frame of this block within the track (0 at track start).
    pub first_frame_index: u64,
    /// Number of interleaved frames that follow (`frame_count * channel_count` samples).
    /// May be 0 for a pure end-of-track marker.
    pub frame_count: u32,
    /// True for the first block of a track.
    pub is_track_start: bool,
    /// True for the last block of a track.
    pub is_track_end: bool,
}

impl SampleBlockHeader {
    /// Number of interleaved samples this header announces, or `None` if it does not fit in
    /// `usize`.
    pub(crate) fn sample_count(&self) -> Option<usize> {
        usize::try_from(self.frame_count)
            .ok()?
            .checked_mul(usize::from(self.format.channel_count))
    }
}

/// Why `PlaybackRingWriter::try_push_block` wrote nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PushBlockError {
    /// Not enough room right now for the header and all its samples. Retry later.
    #[error("the playback ring is full")]
    RingFull,
    /// The block holds more samples than the whole ring, so it can never fit: retrying would
    /// loop forever. Split it (non-subset FLAC allows blocks of up to 65,535 frames, RFC 9639
    /// §8.2) or create a larger ring.
    #[error("block of {block_samples} samples exceeds the ring capacity of {capacity_samples}")]
    BlockExceedsCapacity {
        /// Samples in the refused block.
        block_samples: usize,
        /// Samples the ring holds when empty.
        capacity_samples: usize,
    },
    /// The slice does not hold `frame_count * channel_count` samples. Pushing it would
    /// desynchronize headers and samples, so it is refused.
    #[error("block announces {expected_samples} samples but {actual_samples} were given")]
    SampleCountMismatch {
        /// `frame_count * channel_count` from the header (`usize::MAX` if it overflows).
        expected_samples: usize,
        /// Length of the given slice.
        actual_samples: usize,
    },
}

/// Number of frames of `sample_rate_hz` in `duration`, rounded up, at least 1. Integer only.
pub(crate) fn frames_in_duration(sample_rate_hz: u32, duration: Duration) -> usize {
    let frames = (u128::from(sample_rate_hz) * duration.as_nanos())
        .div_ceil(NANOSECONDS_PER_SECOND)
        .max(1);
    usize::try_from(frames).unwrap_or(usize::MAX)
}

/// Creates the two lock-free single-producer/single-consumer rings (headers and samples)
/// sized for `capacity_duration` of `format`. Allocates; call off the real-time thread.
pub fn create_playback_ring(
    format: &StreamFormat,
    capacity_duration: std::time::Duration,
) -> (PlaybackRingWriter, PlaybackRingReader) {
    let capacity_frames = frames_in_duration(format.sample_rate_hz, capacity_duration);
    let sample_capacity = capacity_frames.saturating_mul(usize::from(format.channel_count.max(1)));
    let header_capacity =
        (capacity_frames / MINIMUM_FLAC_BLOCK_FRAMES).saturating_add(EXTRA_HEADER_SLOTS);
    let (header_producer, header_consumer) = rtrb::RingBuffer::new(header_capacity);
    let (sample_producer, sample_consumer) = rtrb::RingBuffer::new(sample_capacity);
    (
        PlaybackRingWriter {
            headers: header_producer,
            samples: sample_producer,
        },
        PlaybackRingReader {
            headers: header_consumer,
            samples: sample_consumer,
        },
    )
}

/// Decode-thread side.
pub struct PlaybackRingWriter {
    headers: rtrb::Producer<SampleBlockHeader>,
    samples: rtrb::Producer<i32>,
}

impl PlaybackRingWriter {
    /// Number of whole frames of `channel_count` channels that fit right now. 0 when the header
    /// ring is full, since no block can be pushed then. 0 for `channel_count == 0`.
    pub fn free_frames(&self, channel_count: u8) -> usize {
        if channel_count == 0 || self.headers.slots() == 0 {
            return 0;
        }
        self.samples.slots() / usize::from(channel_count)
    }

    /// Largest block, in frames of `channel_count` channels, that can ever fit (the ring when
    /// empty). Larger blocks must be split by the caller.
    pub fn capacity_frames(&self, channel_count: u8) -> usize {
        if channel_count == 0 {
            return 0;
        }
        self.samples.buffer().capacity() / usize::from(channel_count)
    }

    /// Pushes a header and its samples atomically from the reader's point of view (samples
    /// first, header last). Returns an error without writing anything if it does not fit or if
    /// `interleaved_samples` does not hold `frame_count * channel_count` samples.
    pub fn try_push_block(
        &mut self,
        header: SampleBlockHeader,
        interleaved_samples: &[i32],
    ) -> Result<(), PushBlockError> {
        let expected_samples = header.sample_count().unwrap_or(usize::MAX);
        if interleaved_samples.len() != expected_samples {
            return Err(PushBlockError::SampleCountMismatch {
                expected_samples,
                actual_samples: interleaved_samples.len(),
            });
        }
        let capacity_samples = self.samples.buffer().capacity();
        if interleaved_samples.len() > capacity_samples {
            return Err(PushBlockError::BlockExceedsCapacity {
                block_samples: interleaved_samples.len(),
                capacity_samples,
            });
        }
        // Only this producer adds items, so free slots can only grow between this check and
        // the pushes below: both pushes succeed once the check passed.
        if self.headers.slots() == 0 || self.samples.slots() < interleaved_samples.len() {
            return Err(PushBlockError::RingFull);
        }
        self.samples
            .push_entire_slice(interleaved_samples)
            .map_err(|_| PushBlockError::RingFull)?;
        self.headers
            .push(header)
            .map_err(|_| PushBlockError::RingFull)
    }
}

/// Real-time side. Never allocates.
pub struct PlaybackRingReader {
    headers: rtrb::Consumer<SampleBlockHeader>,
    samples: rtrb::Consumer<i32>,
}

impl PlaybackRingReader {
    /// The oldest header not yet popped. Its samples are all in the ring.
    pub(crate) fn peek_header(&self) -> Option<&SampleBlockHeader> {
        self.headers.peek().ok()
    }

    /// Removes the oldest header. Its samples stay in the ring until read or discarded.
    pub(crate) fn pop_header(&mut self) -> Option<SampleBlockHeader> {
        self.headers.pop().ok()
    }

    /// Lends exactly `sample_count` samples to `use_samples` as one or two slices (two when the
    /// ring wraps), then releases them. `None`, with nothing consumed, if fewer are available.
    pub(crate) fn read_samples<Output>(
        &mut self,
        sample_count: usize,
        use_samples: impl FnOnce(&[i32], &[i32]) -> Output,
    ) -> Option<Output> {
        let chunk = self.samples.read_chunk(sample_count).ok()?;
        let (first, second) = chunk.as_slices();
        let result = use_samples(first, second);
        chunk.commit_all();
        Some(result)
    }

    /// Drops up to `sample_count` samples without reading them. Returns how many were dropped.
    pub(crate) fn discard_samples(&mut self, sample_count: usize) -> usize {
        let dropped = sample_count.min(self.samples.slots());
        if let Ok(chunk) = self.samples.read_chunk(dropped) {
            chunk.commit_all();
        }
        dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::ChannelMask;

    fn stereo_format() -> StreamFormat {
        StreamFormat {
            sample_rate_hz: 1_000,
            bits_per_sample: 16,
            channel_count: 2,
            channel_mask: ChannelMask(0b11),
        }
    }

    fn header(first_frame_index: u64, frame_count: u32) -> SampleBlockHeader {
        SampleBlockHeader {
            track_id: TrackId(1),
            flush_generation: 0,
            format: stereo_format(),
            first_frame_index,
            frame_count,
            is_track_start: first_frame_index == 0,
            is_track_end: false,
        }
    }

    fn read_all(reader: &mut PlaybackRingReader, sample_count: usize) -> Vec<i32> {
        reader
            .read_samples(sample_count, |first, second| [first, second].concat())
            .expect("samples are available")
    }

    #[test]
    fn capacity_follows_duration_and_format() {
        // 10 ms at 1 kHz = 10 frames.
        let (writer, _reader) = create_playback_ring(&stereo_format(), Duration::from_millis(10));
        assert_eq!(writer.capacity_frames(2), 10);
        assert_eq!(writer.free_frames(2), 10);
        assert_eq!(writer.free_frames(0), 0);
    }

    #[test]
    fn frames_in_duration_rounds_up_and_is_never_zero() {
        assert_eq!(frames_in_duration(44_100, Duration::from_secs(2)), 88_200);
        assert_eq!(frames_in_duration(44_100, Duration::from_nanos(1)), 1);
        assert_eq!(frames_in_duration(44_100, Duration::ZERO), 1);
    }

    #[test]
    fn push_and_pop_with_wrap_around() {
        let (mut writer, mut reader) =
            create_playback_ring(&stereo_format(), Duration::from_millis(5));
        // 5 frames of capacity; blocks of 3 frames force the samples ring to wrap.
        let mut next_value = 0;
        for block_index in 0..10u64 {
            let samples: Vec<i32> = (next_value..next_value + 6).collect();
            next_value += 6;
            writer
                .try_push_block(header(block_index * 3, 3), &samples)
                .expect("ring was drained");
            let popped = reader.pop_header().expect("header pushed");
            assert_eq!(popped.first_frame_index, block_index * 3);
            assert_eq!(read_all(&mut reader, 6), samples);
            assert!(reader.peek_header().is_none());
        }
    }

    #[test]
    fn try_push_block_is_all_or_nothing_when_samples_do_not_fit() {
        let (mut writer, mut reader) =
            create_playback_ring(&stereo_format(), Duration::from_millis(5));
        writer
            .try_push_block(header(0, 4), &[0; 8])
            .expect("fits in 5 frames");
        assert_eq!(writer.free_frames(2), 1);
        assert_eq!(
            writer.try_push_block(header(4, 2), &[0; 4]),
            Err(PushBlockError::RingFull)
        );
        // Nothing of the refused block is visible.
        assert!(reader.pop_header().is_some());
        assert!(reader.pop_header().is_none());
        assert_eq!(reader.discard_samples(100), 8);
    }

    #[test]
    fn try_push_block_is_all_or_nothing_when_headers_do_not_fit() {
        let (mut writer, mut reader) =
            create_playback_ring(&stereo_format(), Duration::from_millis(5));
        // 5 frames → 0 + EXTRA_HEADER_SLOTS header slots; fill them with end markers.
        for _ in 0..EXTRA_HEADER_SLOTS {
            writer.try_push_block(header(0, 0), &[]).expect("room");
        }
        assert_eq!(writer.free_frames(2), 0);
        assert_eq!(
            writer.try_push_block(header(0, 1), &[7, 7]),
            Err(PushBlockError::RingFull)
        );
        assert_eq!(reader.discard_samples(100), 0, "no sample was written");
    }

    #[test]
    fn try_push_block_refuses_a_slice_of_the_wrong_length() {
        let (mut writer, mut reader) =
            create_playback_ring(&stereo_format(), Duration::from_millis(5));
        assert_eq!(
            writer.try_push_block(header(0, 2), &[1, 2, 3]),
            Err(PushBlockError::SampleCountMismatch {
                expected_samples: 4,
                actual_samples: 3
            })
        );
        assert!(reader.peek_header().is_none());
        assert_eq!(reader.discard_samples(100), 0);
    }

    #[test]
    fn a_block_larger_than_the_ring_is_refused_for_good() {
        let (mut writer, reader) = create_playback_ring(&stereo_format(), Duration::from_millis(5));
        assert_eq!(
            writer.try_push_block(header(0, 6), &[0; 12]),
            Err(PushBlockError::BlockExceedsCapacity {
                block_samples: 12,
                capacity_samples: 10
            })
        );
        assert!(reader.peek_header().is_none());
        // A block of exactly the capacity still fits in the empty ring.
        writer
            .try_push_block(header(0, 5), &[0; 10])
            .expect("fits exactly");
    }

    #[test]
    fn read_samples_consumes_nothing_when_too_few_are_available() {
        let (mut writer, mut reader) =
            create_playback_ring(&stereo_format(), Duration::from_millis(5));
        writer.try_push_block(header(0, 1), &[1, 2]).expect("room");
        assert!(reader.read_samples(3, |_, _| ()).is_none());
        assert_eq!(read_all(&mut reader, 2), vec![1, 2]);
    }

    /// Stress test across threads: every header the reader sees has all its samples present,
    /// and the samples arrive in order.
    #[test]
    fn header_is_never_visible_before_its_samples() {
        const BLOCK_COUNT: u64 = 20_000;
        let (mut writer, mut reader) =
            create_playback_ring(&stereo_format(), Duration::from_millis(50));
        let producer = std::thread::spawn(move || {
            let mut block_index = 0u64;
            while block_index < BLOCK_COUNT {
                let frame_count = (block_index % 7) as u32 + 1;
                let value = block_index as i32;
                let samples = vec![value; frame_count as usize * 2];
                match writer.try_push_block(header(block_index, frame_count), &samples) {
                    Ok(()) => block_index += 1,
                    Err(PushBlockError::RingFull) => std::thread::yield_now(),
                    Err(other) => panic!("unexpected {other}"),
                }
            }
        });
        let mut expected_index = 0u64;
        while expected_index < BLOCK_COUNT {
            let Some(popped) = reader.pop_header() else {
                std::thread::yield_now();
                continue;
            };
            assert_eq!(popped.first_frame_index, expected_index);
            let sample_count = popped.sample_count().expect("small block");
            let samples = reader
                .read_samples(sample_count, |first, second| [first, second].concat())
                .expect("samples are pushed before their header");
            assert!(
                samples
                    .iter()
                    .all(|sample| *sample == expected_index as i32)
            );
            expected_index += 1;
        }
        producer.join().expect("producer thread");
    }
}
