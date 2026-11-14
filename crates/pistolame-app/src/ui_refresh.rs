//! The pace of the UI refresh timer (audit 2026-11-12, adaptive UI loop).
//!
//! The timer drives `UiBridge::tick`: playback position, engine events, worker results. It
//! runs at 30 Hz only while a track plays in a window the user can see, or for a moment after
//! the user did something; otherwise it slows to 4 Hz so the processor can sleep between
//! frames. Slint handles clicks and keys at once, without this timer; the only thing that waits
//! for it is what `tick` collects, and input or a media key brings it back to 30 Hz at once.
//!
//! Engine events and worker results do not count as activity: the engine reports during
//! playback, so counting them would keep a hidden window at 30 Hz.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use pistolame_core::timing::UI_REFRESH_INTERVAL;

/// Period when nothing moves on screen: worker results and device changes still show within
/// a quarter of a second, and the processor is woken 4 times a second instead of 30.
pub(crate) const IDLE_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
/// How long the timer stays at 30 Hz after an input: covers the engine's answer to a click
/// (opening, seeking) before playback itself keeps the pace.
pub(crate) const ACTIVITY_GRACE: Duration = Duration::from_secs(1);

thread_local! {
    /// The pace of the UI thread, for the window hook and the workers' wake-up
    /// (`invoke_from_event_loop` takes a `Send` closure, and the pace is not `Send`).
    static UI_THREAD_PACE: RefCell<Weak<RefreshPace>> = const { RefCell::new(Weak::new()) };
}

/// The refresh period for this state of the app.
pub(crate) fn refresh_interval(
    is_playing: bool,
    is_window_hidden: bool,
    since_last_activity: Duration,
) -> Duration {
    if since_last_activity < ACTIVITY_GRACE || (is_playing && !is_window_hidden) {
        UI_REFRESH_INTERVAL
    } else {
        IDLE_REFRESH_INTERVAL
    }
}

/// The UI refresh timer and what sets its pace. Lives on the UI thread.
pub(crate) struct RefreshPace {
    timer: slint::Timer,
    last_activity: Cell<Instant>,
    /// The system reports the window fully covered (macOS, Windows; never on some backends).
    is_occluded: Cell<bool>,
}

impl RefreshPace {
    /// Starts the timer. `tick` refreshes the UI and returns whether a track plays in a window
    /// that is not minimized. Keep the returned value alive: dropping it stops the timer.
    pub(crate) fn start(mut tick: impl FnMut() -> bool + 'static) -> Rc<RefreshPace> {
        let pace = Rc::new_cyclic(|weak: &Weak<RefreshPace>| {
            let weak = weak.clone();
            let timer = slint::Timer::default();
            timer.start(slint::TimerMode::Repeated, UI_REFRESH_INTERVAL, move || {
                let is_playing_visibly = tick();
                if let Some(pace) = weak.upgrade() {
                    pace.adapt(is_playing_visibly);
                }
            });
            RefreshPace {
                timer,
                last_activity: Cell::new(Instant::now()),
                is_occluded: Cell::new(false),
            }
        });
        UI_THREAD_PACE.with(|registered| *registered.borrow_mut() = Rc::downgrade(&pace));
        pace
    }

    fn adapt(&self, is_playing_visibly: bool) {
        self.apply(refresh_interval(
            is_playing_visibly,
            self.is_occluded.get(),
            self.last_activity.get().elapsed(),
        ));
    }

    fn note_activity(&self) {
        self.last_activity.set(Instant::now());
        self.apply(UI_REFRESH_INTERVAL);
    }

    /// Changes the period only when it differs: `set_interval` reschedules the next tick, so
    /// calling it on every input would postpone the ticks while the user types.
    fn apply(&self, interval: Duration) {
        if self.timer.interval() != interval {
            self.timer.set_interval(interval);
        }
    }
}

/// The user acted (click, key, drop): back to 30 Hz for a moment. UI thread only; does nothing
/// before the timer starts.
pub(crate) fn note_activity() {
    UI_THREAD_PACE.with(|registered| {
        if let Some(pace) = registered.borrow().upgrade() {
            pace.note_activity();
        }
    });
}

/// The system covers or uncovers the whole window. UI thread only.
pub(crate) fn set_occluded(is_occluded: bool) {
    UI_THREAD_PACE.with(|registered| {
        if let Some(pace) = registered.borrow().upgrade() {
            pace.is_occluded.set(is_occluded);
        }
    });
}

/// Like `note_activity`, from any thread (media keys arrive on system threads).
pub(crate) fn note_activity_from_any_thread() {
    // Fails only once the event loop is gone: nothing is refreshed then.
    let _ = slint::invoke_from_event_loop(note_activity);
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG_AGO: Duration = Duration::from_secs(60);

    #[test]
    fn playing_in_a_visible_window_refreshes_at_full_rate() {
        assert_eq!(refresh_interval(true, false, LONG_AGO), UI_REFRESH_INTERVAL);
    }

    #[test]
    fn idle_or_hidden_playback_slows_down() {
        assert_eq!(
            refresh_interval(false, false, LONG_AGO),
            IDLE_REFRESH_INTERVAL
        );
        assert_eq!(
            refresh_interval(true, true, LONG_AGO),
            IDLE_REFRESH_INTERVAL
        );
    }

    #[test]
    fn recent_input_keeps_full_rate_until_the_grace_ends() {
        assert_eq!(
            refresh_interval(false, true, Duration::from_millis(10)),
            UI_REFRESH_INTERVAL
        );
        assert_eq!(
            refresh_interval(false, false, ACTIVITY_GRACE),
            IDLE_REFRESH_INTERVAL
        );
    }
}
