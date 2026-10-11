//! Lock-free playback counters: written by the real-time thread, read by the UI timer (B6).
//!
//! Every field is an independent `AtomicU64` with `Relaxed` ordering: each value is a
//! statistic or a position read for display, never used to order other memory accesses.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::messages::PlaybackCountersSnapshot;
use crate::playback_ring::TrackId;

/// Lock-free counters written by the real-time thread (Relaxed atomics), read by the UI timer.
/// Shared through an `Arc`; writing never blocks, allocates or fails.
#[derive(Debug, Default)]
pub struct PlaybackCounters {
    current_track_id: AtomicU64,
    frames_rendered_in_track: AtomicU64,
    underrun_events: AtomicU64,
    silent_frames_inserted: AtomicU64,
    verification_tap_overflow_events: AtomicU64,
    stale_frames_discarded: AtomicU64,
    last_resume_delay_milliseconds: AtomicU64,
    render_contract_violations: AtomicU64,
    format_boundaries_reached: AtomicU64,
}

impl PlaybackCounters {
    /// All counters at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Plain copy of every counter, for the UI. Fields are read one by one, so two fields may
    /// come from different instants; each one is exact on its own.
    pub fn snapshot(&self) -> PlaybackCountersSnapshot {
        PlaybackCountersSnapshot {
            current_track_id: self.current_track_id.load(Ordering::Relaxed),
            frames_rendered_in_track: self.frames_rendered_in_track.load(Ordering::Relaxed),
            underrun_events: self.underrun_events.load(Ordering::Relaxed),
            silent_frames_inserted: self.silent_frames_inserted.load(Ordering::Relaxed),
            verification_tap_overflow_events: self
                .verification_tap_overflow_events
                .load(Ordering::Relaxed),
            stale_frames_discarded: self.stale_frames_discarded.load(Ordering::Relaxed),
            last_resume_delay_milliseconds: self
                .last_resume_delay_milliseconds
                .load(Ordering::Relaxed),
            render_contract_violations: self.render_contract_violations.load(Ordering::Relaxed),
            format_boundaries_reached: self.format_boundaries_reached.load(Ordering::Relaxed),
        }
    }

    /// Engine side: time from `Play` after a pause to the first rendered buffer (B13.1 Q6).
    pub fn record_resume_delay(&self, resume_delay: Duration) {
        let milliseconds = u64::try_from(resume_delay.as_millis()).unwrap_or(u64::MAX);
        self.last_resume_delay_milliseconds
            .store(milliseconds, Ordering::Relaxed);
    }

    /// Render side: the playback position, as the track and the index of the next frame to
    /// render within it. The two stores are separate, so at a track change the UI can briefly
    /// read the new track with the old track's position (one UI tick at most); the next
    /// snapshot is consistent.
    pub(crate) fn record_position(&self, track_id: TrackId, next_frame_index: u64) {
        self.current_track_id.store(track_id.0, Ordering::Relaxed);
        self.frames_rendered_in_track
            .store(next_frame_index, Ordering::Relaxed);
    }

    /// Render side: one underrun that inserted `silent_frame_count` frames of silence.
    pub(crate) fn record_underrun(&self, silent_frame_count: u64) {
        self.underrun_events.fetch_add(1, Ordering::Relaxed);
        self.silent_frames_inserted
            .fetch_add(silent_frame_count, Ordering::Relaxed);
    }

    /// Render side: device bytes or a tap event could not be copied into the verification tap.
    pub(crate) fn record_verification_tap_overflow(&self) {
        self.verification_tap_overflow_events
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Render side: frames of an outdated flush generation dropped without being played.
    pub(crate) fn record_stale_frames_discarded(&self, frame_count: u64) {
        self.stale_frames_discarded
            .fetch_add(frame_count, Ordering::Relaxed);
    }

    /// Render side: the feeder stopped at a block of a format the open stream cannot carry.
    pub(crate) fn record_format_boundary_reached(&self) {
        self.format_boundaries_reached
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Render side: a backend broke the `RenderSource` contract (`render` before
    /// `begin_stream`, an unusable render format, a destination that is not whole frames) or
    /// the playback ring was found inconsistent. Never expected; counted so it is not silent.
    pub(crate) fn record_render_contract_violation(&self) {
        self.render_contract_violations
            .fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reflects_every_record() {
        let counters = PlaybackCounters::new();
        assert_eq!(counters.snapshot(), PlaybackCountersSnapshot::default());
        counters.record_position(TrackId(7), 1_234);
        counters.record_underrun(10);
        counters.record_underrun(5);
        counters.record_verification_tap_overflow();
        counters.record_stale_frames_discarded(42);
        counters.record_resume_delay(Duration::from_millis(85));
        counters.record_render_contract_violation();
        counters.record_format_boundary_reached();
        assert_eq!(
            counters.snapshot(),
            PlaybackCountersSnapshot {
                current_track_id: 7,
                frames_rendered_in_track: 1_234,
                underrun_events: 2,
                silent_frames_inserted: 15,
                verification_tap_overflow_events: 1,
                stale_frames_discarded: 42,
                last_resume_delay_milliseconds: 85,
                render_contract_violations: 1,
                format_boundaries_reached: 1,
            }
        );
    }
}
