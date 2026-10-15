//! Starting tracks, choosing the next one and play/pause.

use super::*;

impl UiBridge {
    /// Plays track `track` of album `album`; the album becomes the playing context.
    pub(super) fn play_track(&self, album: usize, track: usize) {
        let (paths, name) = match self.albums.borrow().get(album) {
            Some(album) => (
                album
                    .tracks
                    .iter()
                    .map(|track| track.path.clone())
                    .collect(),
                album.title.clone(),
            ),
            None => return,
        };
        self.play_context(paths, track, name);
    }

    /// Plays `paths` (an album or a playlist called `name`) from `position`.
    pub(super) fn play_context(&self, paths: Vec<PathBuf>, position: usize, name: String) {
        let path = self
            .play_queue
            .borrow_mut()
            .play_context(paths, position, name);
        if let Some(path) = path {
            self.start_playback(path);
        }
    }

    /// Opens and plays `path`, a track the queue just made current.
    pub(super) fn start_playback(&self, path: PathBuf) {
        self.resume_position_seconds.set(None);
        let Some(location) = self.track_location(&path) else {
            self.message(format!("Morceau introuvable : {}", path.display()));
            return;
        };
        self.selected_track.set(Some(location));
        self.should_advance.set(true);
        self.stream_end_baseline
            .set(self.engine.counters().stream_ends_reached);
        self.message(String::new());
        self.record_play(&path, location);
        self.engine.send(EngineCommand::OpenFileAndPlay { path });
        self.refresh_playlist_view();
        self.refresh_queue_view();
    }

    /// Counts a play in the library and in memory ("Écoutés récemment"). A failed write only
    /// loses the statistic, so it stays silent.
    pub(super) fn record_play(&self, path: &std::path::Path, (album, track): (usize, usize)) {
        if let Some(database) = &self.library_database
            && let Ok(mut store) = LibraryStore::open(database)
        {
            let _ = store.record_play(path);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        if let Some(album) = self.albums.borrow_mut().get_mut(album) {
            if let Some(file) = album.tracks.get_mut(track) {
                file.play_count += 1;
                file.last_played_seconds = Some(now);
            }
            album.sort_keys.last_played_seconds = Some(now);
        }
        self.show_recent_albums();
    }

    /// The (album, track) indices of a file in the current album list.
    pub(super) fn track_location(&self, path: &std::path::Path) -> Option<(usize, usize)> {
        self.albums
            .borrow()
            .iter()
            .enumerate()
            .find_map(|(album_index, album)| {
                album
                    .tracks
                    .iter()
                    .position(|track| track.path == path)
                    .map(|track_index| (album_index, track_index))
            })
    }

    pub(super) fn random_track(&self, count: usize) -> usize {
        let seed = self
            .shuffle_seed
            .get()
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1);
        self.shuffle_seed.set(seed);
        (seed >> 32) as usize % count.max(1)
    }

    /// The previous/next buttons. "Previous" restarts the track after 3 s, like every player.
    pub(super) fn adjacent_track(&self, next: bool) {
        if self.selected_track.get().is_none() {
            return;
        }
        if next {
            self.play_next_in_queue(true);
            return;
        }
        if self.engine.counters().frames_rendered_in_track
            > u64::from(self.sample_rate_hz.get()) * 3
        {
            self.engine.send(EngineCommand::SeekTo {
                position_in_track: Duration::ZERO,
            });
            return;
        }
        let path = self.play_queue.borrow_mut().previous();
        if let Some(path) = path {
            self.start_playback(path);
        }
    }

    /// Plays what comes next. The "next" button wraps around the album or playlist; the end of
    /// a track wraps only when repeat is on.
    pub(super) fn play_next_in_queue(&self, is_manual: bool) {
        let (is_shuffle_on, is_repeat_on) = self
            .window
            .upgrade()
            .map(|window| {
                let state = window.global::<AppState>();
                (state.get_is_shuffle_on(), state.get_is_repeat_on())
            })
            .unwrap_or_default();
        let (count, position) = {
            let queue = self.play_queue.borrow();
            (queue.context_len(), queue.context_position())
        };
        let shuffled_position = (is_shuffle_on && count > 1)
            .then(|| (position + 1 + self.random_track(count - 1)) % count);
        let path = self
            .play_queue
            .borrow_mut()
            .advance(shuffled_position, is_manual || is_repeat_on);
        if let Some(path) = path {
            self.start_playback(path);
        }
    }

    pub(super) fn advance_after_end(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        self.forget_album_when_finished();
        if self.should_sleep_after_track() {
            return;
        }
        if self
            .play_queue
            .borrow()
            .has_next(state.get_is_repeat_on(), state.get_is_shuffle_on())
        {
            self.play_next_in_queue(false);
        }
    }

    pub(super) fn toggle_play(&self) {
        if let Some(position_seconds) = self.resume_position_seconds.get()
            && self.state.get() == PlaybackState::Stopped
            && let Some(path) = self.play_queue.borrow().current().cloned()
        {
            self.start_playback(path);
            if position_seconds > 0 {
                self.engine.send(EngineCommand::SeekTo {
                    position_in_track: Duration::from_secs(position_seconds),
                });
            }
            return;
        }
        self.engine.send(
            if matches!(
                self.state.get(),
                PlaybackState::Playing | PlaybackState::Opening
            ) {
                EngineCommand::Pause
            } else {
                EngineCommand::Play
            },
        );
    }
}
