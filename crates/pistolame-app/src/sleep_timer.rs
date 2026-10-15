//! Minuteur de veille: stops playback at the end of the current track or at a set time.
//!
//! Pure model: the bridge asks it, at each tick and at each track end, whether to stop. There
//! is no fade, so the samples stay bit-perfect up to the stop.

use std::time::{Duration, Instant};

const SECONDS_PER_MINUTE: u64 = 60;

/// When playback should stop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SleepTimer {
    #[default]
    Off,
    /// At the end of the track playing now (the next one does not start).
    EndOfTrack,
    /// At this moment (playback pauses).
    At(Instant),
}

impl SleepTimer {
    /// The timer for a menu choice: -1 the end of the track, 0 off, else minutes from `now`.
    pub(crate) fn from_choice(choice: i32, now: Instant) -> SleepTimer {
        match choice {
            -1 => SleepTimer::EndOfTrack,
            minutes if minutes > 0 => {
                SleepTimer::At(now + Duration::from_secs(minutes as u64 * SECONDS_PER_MINUTE))
            }
            _ => SleepTimer::Off,
        }
    }

    /// True when the timer is due at `now` (only a timed one can be).
    pub(crate) fn is_due(&self, now: Instant) -> bool {
        matches!(self, SleepTimer::At(deadline) if now >= *deadline)
    }

    /// What the queue panel shows: empty when off, "Fin du morceau", or the minutes left
    /// rounded up ("1 min" until the very end).
    pub(crate) fn label(&self, now: Instant) -> String {
        match self {
            SleepTimer::Off => String::new(),
            SleepTimer::EndOfTrack => "Fin du morceau".to_owned(),
            SleepTimer::At(deadline) => {
                let seconds_left = deadline.saturating_duration_since(now).as_secs();
                let minutes_left = seconds_left.div_ceil(SECONDS_PER_MINUTE).max(1);
                format!("{minutes_left} min")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_choices_map_to_timers() {
        let now = Instant::now();
        assert_eq!(SleepTimer::from_choice(0, now), SleepTimer::Off);
        assert_eq!(SleepTimer::from_choice(-1, now), SleepTimer::EndOfTrack);
        assert_eq!(
            SleepTimer::from_choice(15, now),
            SleepTimer::At(now + Duration::from_secs(900))
        );
        assert_eq!(SleepTimer::from_choice(-7, now), SleepTimer::Off);
    }

    #[test]
    fn only_a_timed_stop_becomes_due() {
        let now = Instant::now();
        let timer = SleepTimer::from_choice(15, now);
        assert!(!timer.is_due(now));
        assert!(timer.is_due(now + Duration::from_secs(900)));
        assert!(!SleepTimer::EndOfTrack.is_due(now + Duration::from_secs(100_000)));
    }

    #[test]
    fn the_label_counts_whole_minutes_left() {
        let now = Instant::now();
        let timer = SleepTimer::from_choice(30, now);
        assert_eq!(timer.label(now), "30 min");
        assert_eq!(timer.label(now + Duration::from_secs(61)), "29 min");
        assert_eq!(timer.label(now + Duration::from_secs(1799)), "1 min");
        assert_eq!(timer.label(now + Duration::from_secs(5000)), "1 min");
        assert_eq!(SleepTimer::Off.label(now), "");
        assert_eq!(SleepTimer::EndOfTrack.label(now), "Fin du morceau");
    }
}
