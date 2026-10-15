//! Session restore and save (queue and position).

use super::*;

impl UiBridge {
    /// Reopens the last session: the queue, and its current track shown paused at the stored
    /// position. Nothing plays until the user presses Play.
    pub(super) fn restore_session(&self) {
        let Some(session) = self
            .session_path()
            .and_then(|path| crate::session::Session::load_from(&path))
        else {
            return;
        };
        let queue = crate::play_queue::PlayQueue::from_snapshot(session.queue.clone());
        let location = queue.current().and_then(|path| self.track_location(path));
        *self.play_queue.borrow_mut() = queue;
        *self.last_saved_session.borrow_mut() = Some(session.clone());
        let (Some((album_index, track_index)), Some(window)) = (location, self.window.upgrade())
        else {
            return;
        };
        let state = window.global::<AppState>();
        let (sample_rate_hz, duration_seconds) = {
            let albums = self.albums.borrow();
            let Some(track) = albums
                .get(album_index)
                .and_then(|album| album.tracks.get(track_index))
            else {
                return;
            };
            (track.sample_rate_hz, track.duration_seconds)
        };
        self.selected_track.set(Some((album_index, track_index)));
        self.sample_rate_hz.set(sample_rate_hz);
        self.total_frames
            .set(duration_seconds * u64::from(sample_rate_hz));
        self.resume_position_seconds
            .set(Some(session.position_seconds.min(duration_seconds)));
        self.show_current_track(&state, album_index, track_index);
        self.refresh_queue_view();
    }

    /// The session file, beside the library database (so a preview never touches the user's).
    pub(super) fn session_path(&self) -> Option<PathBuf> {
        self.library_database
            .as_deref()
            .map(crate::session::Session::path_beside)
    }

    /// Writes the session when it changed. Never before the stored one was read.
    pub(crate) fn save_session(&self) {
        if !self.is_session_restored.get() {
            return;
        }
        let rate = u64::from(self.sample_rate_hz.get().max(1));
        let position_seconds = match self.resume_position_seconds.get() {
            Some(seconds) => seconds,
            None if self.state.get() == PlaybackState::Stopped => 0,
            None => self.engine.counters().frames_rendered_in_track / rate,
        };
        let session = crate::session::Session {
            queue: self.play_queue.borrow().snapshot(),
            position_seconds,
        };
        if self.last_saved_session.borrow().as_ref() == Some(&session) {
            return;
        }
        let Some(path) = self.session_path() else {
            return;
        };
        // A failed write only loses the resume point, so it stays silent.
        if session.save_to(&path).is_ok() {
            *self.last_saved_session.borrow_mut() = Some(session);
        }
    }
}
