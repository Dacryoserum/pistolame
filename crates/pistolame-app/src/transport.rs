//! Album sequencing policy, independent of the engine and widgets.

use std::time::{Duration, Instant};

pub(crate) fn can_advance(track: usize, count: usize, repeat: bool, shuffle: bool) -> bool {
    count > 0 && (track + 1 < count || repeat || (shuffle && count > 1))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn album_end_stops_unless_a_playback_mode_continues_it() {
        assert!(!can_advance(2, 3, false, false));
        assert!(can_advance(1, 3, false, false));
        assert!(can_advance(2, 3, true, false));
        assert!(can_advance(2, 3, false, true));
        assert!(!can_advance(0, 0, true, true));
        assert!(!can_advance(0, 1, false, true));
    }
}

/// How long a seek is shown without the engine confirming it, in case it never lands (the track
/// ended, the file failed): the bar must not stay there for good.
const SEEK_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(3);
/// How far past the target a position still proves the seek: the blocks buffered before it was
/// heard cannot be this long.
const SEEK_LANDING_WINDOW_SECONDS: u64 = 2;

/// A seek sent to the engine. The position it reports is the old one until the first block of the
/// new position plays (and stays so while paused): showing it would make the bar jump back.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingSeek {
    pub(crate) target_frame_index: u64,
    sent_at: Instant,
}

impl PendingSeek {
    pub(crate) fn new(target_frame_index: u64) -> Self {
        Self {
            target_frame_index,
            sent_at: Instant::now(),
        }
    }

    /// While playing: whether `played_frames` is at the target, or waiting is over.
    pub(crate) fn has_landed(&self, played_frames: u64, sample_rate_hz: u64) -> bool {
        let window = SEEK_LANDING_WINDOW_SECONDS * sample_rate_hz;
        (self.target_frame_index..=self.target_frame_index + window).contains(&played_frames)
            || self.sent_at.elapsed() >= SEEK_CONFIRMATION_TIMEOUT
    }
}

#[cfg(test)]
mod seek_tests {
    use super::*;

    const RATE: u64 = 44_100;

    #[test]
    fn a_seek_lands_when_the_engine_reaches_the_target() {
        let seek = PendingSeek::new(10 * RATE);
        assert!(!seek.has_landed(RATE, RATE), "still the old position");
        assert!(seek.has_landed(10 * RATE, RATE));
        assert!(seek.has_landed(11 * RATE, RATE));
    }

    #[test]
    fn a_seek_backward_is_not_confirmed_by_the_old_position() {
        let seek = PendingSeek::new(RATE);
        assert!(!seek.has_landed(100 * RATE, RATE));
    }

    #[test]
    fn a_seek_that_never_lands_is_given_up() {
        let seek = PendingSeek {
            target_frame_index: RATE,
            sent_at: Instant::now() - SEEK_CONFIRMATION_TIMEOUT,
        };
        assert!(seek.has_landed(100 * RATE, RATE));
    }
}
