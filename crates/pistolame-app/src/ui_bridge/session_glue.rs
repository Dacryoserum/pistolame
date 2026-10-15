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
        *self.album_resume_points.borrow_mut() = session.album_resume_points.clone();
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
        if self.state.get() != PlaybackState::Stopped
            && self.resume_position_seconds.get().is_none()
        {
            self.remember_album_position(position_seconds);
        }
        let session = crate::session::Session {
            queue: self.play_queue.borrow().snapshot(),
            position_seconds,
            album_resume_points: self.album_resume_points.borrow().clone(),
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

    /// The tracks of album `album_index`, in order.
    fn album_track_paths(&self, album_index: usize) -> Vec<PathBuf> {
        self.albums
            .borrow()
            .get(album_index)
            .map(|album| {
                album
                    .tracks
                    .iter()
                    .map(|track| track.path.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Records where the album of the current track is ("Reprendre").
    fn remember_album_position(&self, position_seconds: u64) {
        let Some((album_index, track_index)) = self.selected_track.get() else {
            return;
        };
        let album_tracks = self.album_track_paths(album_index);
        let Some(track_path) = album_tracks.get(track_index).cloned() else {
            return;
        };
        crate::album_resume::remember(
            &mut self.album_resume_points.borrow_mut(),
            crate::album_resume::ResumePoint {
                track_path,
                position_seconds,
            },
            track_index,
            &album_tracks,
        );
    }

    /// At the natural end of a track: an album listened to its last track is forgotten.
    pub(super) fn forget_album_when_finished(&self) {
        let Some((album_index, track_index)) = self.selected_track.get() else {
            return;
        };
        let album_tracks = self.album_track_paths(album_index);
        if track_index + 1 == album_tracks.len() {
            crate::album_resume::forget_album(
                &mut self.album_resume_points.borrow_mut(),
                &album_tracks,
            );
        }
    }

    /// The "Reprendre" label of album `album_index` ("" when there is nothing to resume).
    pub(super) fn album_resume_label(&self, album_index: usize) -> String {
        let album_tracks = self.album_track_paths(album_index);
        let points = self.album_resume_points.borrow();
        let Some(point) = crate::album_resume::find(&points, &album_tracks) else {
            return String::new();
        };
        let track_number = album_tracks
            .iter()
            .position(|path| *path == point.track_path)
            .map_or(1, |index| index + 1);
        format!(
            "Reprendre · piste {track_number}, {}",
            presentation::clock(point.position_seconds)
        )
    }

    /// "Reprendre" on the album page: plays the album from where it stopped.
    pub(super) fn resume_detail_album(&self) {
        let album_index = self.detail_album.get();
        let album_tracks = self.album_track_paths(album_index);
        let point =
            crate::album_resume::find(&self.album_resume_points.borrow(), &album_tracks).cloned();
        let Some(point) = point else {
            return;
        };
        let Some(track_index) = album_tracks
            .iter()
            .position(|path| *path == point.track_path)
        else {
            return;
        };
        self.play_track(album_index, track_index);
        if point.position_seconds > 0 {
            self.engine.send(EngineCommand::SeekTo {
                position_in_track: Duration::from_secs(point.position_seconds),
            });
        }
    }
}
