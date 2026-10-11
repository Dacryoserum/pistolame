//! Types describing blocks travelling through the playback ring. The ring itself
//! (`create_playback_ring`, writer and reader) belongs to a later task.

use crate::format::StreamFormat;

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
