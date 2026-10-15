//! Minuteur de veille wiring: the menu of the queue panel, the timed pause and the stop at the
//! end of the track.

use super::*;
use crate::sleep_timer::SleepTimer;
use std::time::Instant;

impl UiBridge {
    pub(super) fn connect_sleep_timer(self: &Rc<Self>, state: &AppState<'_>) {
        let handle = self.clone();
        state.on_set_sleep_timer(move |choice| {
            let timer = SleepTimer::from_choice(choice, Instant::now());
            handle.sleep_timer.set(timer);
            handle.show_sleep_timer();
            handle.notice(match timer {
                SleepTimer::Off => "Minuteur de veille désactivé".to_owned(),
                SleepTimer::EndOfTrack => "La lecture s’arrêtera à la fin du morceau".to_owned(),
                SleepTimer::At(_) => {
                    format!("La lecture s’arrêtera dans {}", timer.label(Instant::now()))
                }
            });
        });
    }

    /// At each tick: pauses when the timer is due, and keeps the minutes left up to date.
    pub(super) fn check_sleep_timer(&self) {
        if self.sleep_timer.get().is_due(Instant::now()) {
            self.sleep_timer.set(SleepTimer::Off);
            self.engine.send(EngineCommand::Pause);
            self.notice("Minuteur de veille : lecture mise en pause".to_owned());
        }
        self.show_sleep_timer();
    }

    /// At the end of a track: true when the timer stops playback here (the next track must not
    /// start).
    pub(super) fn should_sleep_after_track(&self) -> bool {
        if self.sleep_timer.get() != SleepTimer::EndOfTrack {
            return false;
        }
        self.sleep_timer.set(SleepTimer::Off);
        self.show_sleep_timer();
        self.notice("Minuteur de veille : lecture arrêtée à la fin du morceau".to_owned());
        true
    }

    /// Writes the label only when it changed, so an idle tick redraws nothing.
    fn show_sleep_timer(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let label = self.sleep_timer.get().label(Instant::now());
        if state.get_sleep_timer_label() != label.as_str() {
            state.set_sleep_timer_label(label.into());
        }
    }
}
