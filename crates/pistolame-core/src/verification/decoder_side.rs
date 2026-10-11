//! Decode-thread side of both verification levels (plan B6 "Decoder-side hashing", B7.2).
//!
//! [`DecodedPcmHasher`] hashes, in the RFC 9639 section 8.2 canonical layout, exactly the samples
//! pushed into the playback ring for one segment (one flush generation of one track). The same
//! digest is the level-1 reference and, for a complete pass from frame 0, the level-2 candidate.

use md5::{Digest, Md5};

use crate::format::StreamFormat;
use crate::pcm::{CanonicalMd5Error, write_canonical_md5_bytes};
use crate::playback_ring::TrackId;
use crate::verification::{DecodedSegmentEnd, DecodedSegmentReport};

/// Number of bits in a byte, for the canonical sample width.
const BITS_PER_BYTE: usize = 8;

/// Why [`DecodedPcmHasher::stage_block`] staged nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecodedHashError {
    /// `begin_segment` was not called since the last `finish_segment`.
    #[error("no decoder-side segment is open")]
    NoSegmentOpen,
    /// The block does not start right after the frames already hashed in the segment.
    #[error(
        "block starts at frame {actual_first_frame_index}, expected {expected_first_frame_index}"
    )]
    NotContiguous {
        /// Frame the segment's next block must start at.
        expected_first_frame_index: u64,
        /// Frame the offered block starts at.
        actual_first_frame_index: u64,
    },
    /// The sample count is not a whole number of frames (or the format has no channel).
    #[error("{sample_count} samples are not whole frames of {channel_count} channels")]
    PartialFrame {
        /// Interleaved samples offered.
        sample_count: usize,
        /// Channel count of the source format.
        channel_count: u8,
    },
    /// The canonical layout rejected the samples (a sample out of range: a decoder bug).
    #[error("canonical MD5 layout rejected the block: {0}")]
    Canonical(#[from] CanonicalMd5Error),
}

/// A block written into the scratch buffer, waiting for its push to succeed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StagedBlock {
    canonical_byte_count: usize,
    frame_count: u64,
}

/// Identity and extent of the segment being hashed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HashedSegment {
    flush_generation: u64,
    first_frame_index: u64,
    committed_frame_count: u64,
}

/// Decode-thread hasher of one track's segments. Two phases (`stage_block` before the push,
/// `commit_staged_block` after it), so a sample the canonical layout rejects never reaches the
/// playback ring, and a `RingFull` retry never hashes a block twice. The scratch buffer grows to
/// the largest block once. Never used on the real-time thread.
pub struct DecodedPcmHasher {
    track_id: TrackId,
    source_format: StreamFormat,
    digest: Md5,
    canonical_scratch: Vec<u8>,
    staged_block: Option<StagedBlock>,
    open_segment: Option<HashedSegment>,
    /// Identity of the latest segment, reported even if `finish_segment` is called twice.
    latest_segment: HashedSegment,
}

impl DecodedPcmHasher {
    /// A hasher for the track `track_id`, whose samples have `source_format`. No segment is
    /// open yet.
    pub fn new(track_id: TrackId, source_format: StreamFormat) -> Self {
        Self {
            track_id,
            source_format,
            digest: Md5::new(),
            canonical_scratch: Vec::new(),
            staged_block: None,
            open_segment: None,
            latest_segment: HashedSegment {
                flush_generation: 0,
                first_frame_index: 0,
                committed_frame_count: 0,
            },
        }
    }

    /// Resets the digest and the frame count. `first_frame_index` is the `first_frame_index`
    /// of the first header pushed under `flush_generation` (after the seek trim): the feeder
    /// announces the same index in `SegmentStarted`.
    pub fn begin_segment(&mut self, flush_generation: u64, first_frame_index: u64) {
        Digest::reset(&mut self.digest);
        self.staged_block = None;
        let segment = HashedSegment {
            flush_generation,
            first_frame_index,
            committed_frame_count: 0,
        };
        self.open_segment = Some(segment);
        self.latest_segment = segment;
    }

    /// Before the push: writes the block's canonical bytes into the scratch buffer. Fails,
    /// staging nothing, if no segment is open, if the block does not start at the segment's
    /// next frame, if `interleaved_samples` is not whole frames, or on `CanonicalMd5Error`.
    pub fn stage_block(
        &mut self,
        first_frame_index: u64,
        interleaved_samples: &[i32],
    ) -> Result<(), DecodedHashError> {
        self.staged_block = None;
        let segment = self.open_segment.ok_or(DecodedHashError::NoSegmentOpen)?;
        let expected_first_frame_index = segment
            .first_frame_index
            .saturating_add(segment.committed_frame_count);
        if first_frame_index != expected_first_frame_index {
            return Err(DecodedHashError::NotContiguous {
                expected_first_frame_index,
                actual_first_frame_index: first_frame_index,
            });
        }
        let channel_count = self.source_format.channel_count;
        let sample_count = interleaved_samples.len();
        if channel_count == 0 || !sample_count.is_multiple_of(usize::from(channel_count)) {
            return Err(DecodedHashError::PartialFrame {
                sample_count,
                channel_count,
            });
        }
        let bytes_per_sample =
            usize::from(self.source_format.bits_per_sample).div_ceil(BITS_PER_BYTE);
        let canonical_byte_count = sample_count.saturating_mul(bytes_per_sample);
        if self.canonical_scratch.len() < canonical_byte_count {
            self.canonical_scratch.resize(canonical_byte_count, 0);
        }
        let destination = self
            .canonical_scratch
            .get_mut(..canonical_byte_count)
            .unwrap_or_default();
        write_canonical_md5_bytes(
            interleaved_samples,
            self.source_format.bits_per_sample,
            destination,
        )?;
        self.staged_block = Some(StagedBlock {
            canonical_byte_count,
            frame_count: (sample_count / usize::from(channel_count)) as u64,
        });
        Ok(())
    }

    /// After `try_push_block` succeeded: feeds the staged bytes to the MD5 and advances the
    /// frame count. Does nothing if nothing is staged.
    pub fn commit_staged_block(&mut self) {
        let (Some(staged), Some(segment)) = (self.staged_block.take(), self.open_segment.as_mut())
        else {
            return;
        };
        if let Some(canonical_bytes) = self.canonical_scratch.get(..staged.canonical_byte_count) {
            self.digest.update(canonical_bytes);
            segment.committed_frame_count = segment
                .committed_frame_count
                .saturating_add(staged.frame_count);
            self.latest_segment = *segment;
        }
    }

    /// The staged block will not be pushed (seek or stop during a `RingFull` retry).
    pub fn discard_staged_block(&mut self) {
        self.staged_block = None;
    }

    /// Frames committed so far in the open segment (0 when none is open).
    pub fn frames_hashed(&self) -> u64 {
        self.open_segment
            .map_or(0, |segment| segment.committed_frame_count)
    }

    /// Closes the segment (a staged, uncommitted block is discarded) and returns its report.
    /// An empty segment reports the MD5 of zero bytes and `frame_count: 0`. Called with no
    /// segment open, it reports the latest segment's identity with no frame and always as
    /// `Abandoned`: with nothing hashed it cannot prove an end of stream, and an `EndOfStream`
    /// report of zero frames would read as a frame-count mismatch.
    pub fn finish_segment(&mut self, ended_by: DecodedSegmentEnd) -> DecodedSegmentReport {
        self.staged_block = None;
        let mut ended_by = ended_by;
        let segment = match self.open_segment.take() {
            Some(segment) => segment,
            None => {
                ended_by = DecodedSegmentEnd::Abandoned;
                Digest::reset(&mut self.digest);
                HashedSegment {
                    committed_frame_count: 0,
                    ..self.latest_segment
                }
            }
        };
        let md5: [u8; 16] = self.digest.finalize_reset().into();
        DecodedSegmentReport {
            track_id: self.track_id,
            flush_generation: segment.flush_generation,
            first_frame_index: segment.first_frame_index,
            frame_count: segment.committed_frame_count,
            md5,
            ended_by,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::ChannelMask;

    const TRACK: TrackId = TrackId(11);

    fn stereo_format(bits_per_sample: u8) -> StreamFormat {
        StreamFormat {
            sample_rate_hz: 48_000,
            bits_per_sample,
            channel_count: 2,
            channel_mask: ChannelMask(0b11),
        }
    }

    fn canonical_md5(samples: &[i32], bits_per_sample: u8) -> [u8; 16] {
        let bytes_per_sample = usize::from(bits_per_sample).div_ceil(8);
        let mut hasher = Md5::new();
        for sample in samples {
            hasher.update(&sample.to_le_bytes()[..bytes_per_sample]);
        }
        hasher.finalize().into()
    }

    fn stage_and_commit(hasher: &mut DecodedPcmHasher, first_frame_index: u64, samples: &[i32]) {
        hasher
            .stage_block(first_frame_index, samples)
            .expect("block stages");
        hasher.commit_staged_block();
    }

    #[test]
    fn committed_blocks_hash_to_the_canonical_md5_of_their_samples() {
        let samples: Vec<i32> = (0..40).map(|value| value * 1_000 - 20_000).collect();
        let mut hasher = DecodedPcmHasher::new(TRACK, stereo_format(20));
        hasher.begin_segment(3, 100);
        stage_and_commit(&mut hasher, 100, &samples[..16]);
        stage_and_commit(&mut hasher, 108, &samples[16..]);
        assert_eq!(hasher.frames_hashed(), 20);
        let report = hasher.finish_segment(DecodedSegmentEnd::EndOfStream);
        assert_eq!(
            report,
            DecodedSegmentReport {
                track_id: TRACK,
                flush_generation: 3,
                first_frame_index: 100,
                frame_count: 20,
                md5: canonical_md5(&samples, 20),
                ended_by: DecodedSegmentEnd::EndOfStream,
            }
        );
    }

    #[test]
    fn finishing_with_no_open_segment_never_claims_an_end_of_stream() {
        let mut hasher = DecodedPcmHasher::new(TRACK, stereo_format(16));
        hasher.begin_segment(1, 0);
        stage_and_commit(&mut hasher, 0, &[1, 2]);
        let _ = hasher.finish_segment(DecodedSegmentEnd::EndOfStream);
        let second = hasher.finish_segment(DecodedSegmentEnd::EndOfStream);
        assert_eq!(second.ended_by, DecodedSegmentEnd::Abandoned);
        assert_eq!(second.frame_count, 0);
    }

    #[test]
    fn a_staged_block_is_hashed_once_however_many_push_retries() {
        let samples = [1, -1, 2, -2];
        let mut hasher = DecodedPcmHasher::new(TRACK, stereo_format(16));
        hasher.begin_segment(1, 0);
        hasher.stage_block(0, &samples).expect("block stages");
        // Pushes failing with `RingFull` do not touch the hasher; the commit comes once.
        hasher.commit_staged_block();
        hasher.commit_staged_block();
        let report = hasher.finish_segment(DecodedSegmentEnd::EndOfStream);
        assert_eq!(report.frame_count, 2);
        assert_eq!(report.md5, canonical_md5(&samples, 16));
    }

    #[test]
    fn a_discarded_or_uncommitted_block_is_never_hashed() {
        let mut hasher = DecodedPcmHasher::new(TRACK, stereo_format(16));
        hasher.begin_segment(1, 0);
        stage_and_commit(&mut hasher, 0, &[5, 6]);
        hasher.stage_block(1, &[7, 8]).expect("block stages");
        hasher.discard_staged_block();
        hasher.commit_staged_block();
        hasher.stage_block(1, &[9, 10]).expect("block stages");
        let report = hasher.finish_segment(DecodedSegmentEnd::Abandoned);
        assert_eq!(report.frame_count, 1);
        assert_eq!(report.md5, canonical_md5(&[5, 6], 16));
    }

    #[test]
    fn an_empty_segment_reports_the_md5_of_zero_bytes() {
        let mut hasher = DecodedPcmHasher::new(TRACK, stereo_format(24));
        hasher.begin_segment(2, 500);
        let report = hasher.finish_segment(DecodedSegmentEnd::EndOfStream);
        assert_eq!(report.frame_count, 0);
        assert_eq!(report.first_frame_index, 500);
        assert_eq!(report.md5, canonical_md5(&[], 24));
    }

    #[test]
    fn a_new_segment_starts_a_fresh_digest() {
        let mut hasher = DecodedPcmHasher::new(TRACK, stereo_format(16));
        hasher.begin_segment(1, 0);
        stage_and_commit(&mut hasher, 0, &[1, 2, 3, 4]);
        hasher.begin_segment(2, 40);
        assert_eq!(hasher.frames_hashed(), 0);
        stage_and_commit(&mut hasher, 40, &[7, 8]);
        let report = hasher.finish_segment(DecodedSegmentEnd::EndOfStream);
        assert_eq!(report.flush_generation, 2);
        assert_eq!(report.frame_count, 1);
        assert_eq!(report.md5, canonical_md5(&[7, 8], 16));
    }

    #[test]
    fn staging_without_an_open_segment_fails() {
        let mut hasher = DecodedPcmHasher::new(TRACK, stereo_format(16));
        assert_eq!(
            hasher.stage_block(0, &[1, 2]),
            Err(DecodedHashError::NoSegmentOpen)
        );
        hasher.begin_segment(1, 0);
        hasher.finish_segment(DecodedSegmentEnd::EndOfStream);
        assert_eq!(
            hasher.stage_block(0, &[1, 2]),
            Err(DecodedHashError::NoSegmentOpen)
        );
    }

    #[test]
    fn a_block_that_is_not_contiguous_fails_and_stages_nothing() {
        let mut hasher = DecodedPcmHasher::new(TRACK, stereo_format(16));
        hasher.begin_segment(1, 10);
        stage_and_commit(&mut hasher, 10, &[1, 2]);
        assert_eq!(
            hasher.stage_block(12, &[3, 4]),
            Err(DecodedHashError::NotContiguous {
                expected_first_frame_index: 11,
                actual_first_frame_index: 12,
            })
        );
        hasher.commit_staged_block();
        assert_eq!(hasher.frames_hashed(), 1);
    }

    #[test]
    fn a_partial_frame_fails() {
        let mut hasher = DecodedPcmHasher::new(TRACK, stereo_format(16));
        hasher.begin_segment(1, 0);
        assert_eq!(
            hasher.stage_block(0, &[1, 2, 3]),
            Err(DecodedHashError::PartialFrame {
                sample_count: 3,
                channel_count: 2,
            })
        );
    }

    #[test]
    fn an_out_of_range_sample_fails_and_a_previously_staged_block_is_dropped() {
        let mut hasher = DecodedPcmHasher::new(TRACK, stereo_format(16));
        hasher.begin_segment(1, 0);
        hasher.stage_block(0, &[1, 2]).expect("block stages");
        assert_eq!(
            hasher.stage_block(0, &[1, 0x8000]),
            Err(DecodedHashError::Canonical(
                CanonicalMd5Error::SampleOutOfRange { sample_index: 1 }
            ))
        );
        hasher.commit_staged_block();
        assert_eq!(hasher.frames_hashed(), 0);
    }

    #[test]
    fn finishing_discards_a_staged_block() {
        let mut hasher = DecodedPcmHasher::new(TRACK, stereo_format(16));
        hasher.begin_segment(1, 0);
        hasher.stage_block(0, &[1, 2]).expect("block stages");
        let report = hasher.finish_segment(DecodedSegmentEnd::Abandoned);
        assert_eq!(report.frame_count, 0);
        hasher.commit_staged_block();
        assert_eq!(hasher.frames_hashed(), 0);
    }
}
