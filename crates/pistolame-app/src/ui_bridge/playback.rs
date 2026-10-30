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
        if !self.make_track_current(&path) {
            return;
        }
        self.engine.send(EngineCommand::OpenFileAndPlay { path });
        self.refresh_playlist_view();
        self.mark_current_top_track();
        self.refresh_queue_view();
    }

    /// Everything a track the queue just made current needs besides being opened: `false`
    /// when it is not in the library. The engine forgets the next track when a file opens or
    /// follows, so the next one is planned and announced again.
    fn make_track_current(&self, path: &std::path::Path) -> bool {
        self.resume_position_seconds.set(None);
        self.pending_seek.set(None);
        let Some(location) = self.track_location(path) else {
            self.message(format!("Morceau introuvable : {}", path.display()));
            return false;
        };
        self.selected_track.set(Some(location));
        self.should_advance.set(true);
        self.stream_end_baseline
            .set(self.engine.counters().stream_ends_reached);
        self.planned_shuffle_position.set(None);
        self.announced_next_track.borrow_mut().take();
        self.message(String::new());
        self.record_play(path, location);
        true
    }

    /// The engine went on with the announced track without a gap: the queue moves to it the
    /// way `advance_after_end` would have. If the queue changed in between, what it says now
    /// plays instead. `stream_ends_baseline` comes from the engine: a short track may already
    /// have ended when this runs, and its end must still advance the queue.
    pub(super) fn follow_gapless_advance(&self, path: PathBuf, stream_ends_baseline: u64) {
        self.forget_album_when_finished();
        let (is_shuffle_on, is_repeat_on) = self.playback_modes();
        let shuffled_position = self.shuffled_position_for_next(is_shuffle_on);
        let queued = self
            .play_queue
            .borrow_mut()
            .advance(shuffled_position, is_repeat_on);
        if queued.as_ref() != Some(&path) {
            match queued {
                Some(queued) => self.start_playback(queued),
                None => {
                    self.should_advance.set(false);
                    self.engine.send(EngineCommand::Stop);
                }
            }
            return;
        }
        if !self.make_track_current(&path) {
            return;
        }
        self.stream_end_baseline.set(stream_ends_baseline);
        // Set during the last moments of the previous track, after the engine had queued this
        // one: it still stops right away.
        if self.should_sleep_after_track() {
            self.engine.send(EngineCommand::Pause);
        }
        self.refresh_playlist_view();
        self.mark_current_top_track();
        self.refresh_queue_view();
    }

    /// At each tick: tells the engine which track follows the current one, when it changed, so
    /// it can prepare it (and play it without a gap). Nothing follows when playback is to stop
    /// at the end of this track.
    pub(super) fn announce_next_track(&self) {
        let next_track = self.next_track_after_end();
        if *self.announced_next_track.borrow() != next_track {
            self.engine.send(EngineCommand::SetNextTrack {
                path: next_track.clone(),
            });
            *self.announced_next_track.borrow_mut() = next_track;
            self.show_adjacent_tracks();
        }
    }

    /// The track `advance_after_end` would start, without moving the queue.
    fn next_track_after_end(&self) -> Option<PathBuf> {
        if !self.should_advance.get()
            || self.sleep_timer.get() == crate::sleep_timer::SleepTimer::EndOfTrack
        {
            return None;
        }
        self.next_track_in_queue()
    }

    /// The track that follows the current one in the queue, whether or not playback will get
    /// there by itself (a session restored in pause has not started yet).
    pub(super) fn next_track_in_queue(&self) -> Option<PathBuf> {
        let (is_shuffle_on, is_repeat_on) = self.playback_modes();
        if !self
            .play_queue
            .borrow()
            .has_next(is_repeat_on, is_shuffle_on)
        {
            return None;
        }
        let shuffled_position = self.shuffled_position_for_next(is_shuffle_on);
        self.play_queue
            .borrow()
            .peek_next(shuffled_position, is_repeat_on)
            .cloned()
    }

    /// (shuffle, repeat), as the transport buttons show them.
    fn playback_modes(&self) -> (bool, bool) {
        self.window
            .upgrade()
            .map(|window| {
                let state = window.global::<AppState>();
                (state.get_is_shuffle_on(), state.get_is_repeat_on())
            })
            .unwrap_or_default()
    }

    /// With shuffle on, the random context position that follows the current track, other than
    /// the current one. Drawn once per track, so the announced next track and the one played
    /// are the same.
    fn shuffled_position_for_next(&self, is_shuffle_on: bool) -> Option<usize> {
        let (count, position) = {
            let queue = self.play_queue.borrow();
            (queue.context_len(), queue.context_position())
        };
        if !is_shuffle_on || count < 2 {
            return None;
        }
        if let Some(planned) = self.planned_shuffle_position.get()
            && planned < count
            && planned != position
        {
            return Some(planned);
        }
        let shuffled_position = (position + 1 + self.random_track(count - 1)) % count;
        self.planned_shuffle_position.set(Some(shuffled_position));
        Some(shuffled_position)
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
                if let Some(window) = self.window.upgrade() {
                    window.global::<AppState>().set_current_play_count_label(
                        presentation::play_count_label(file.play_count).into(),
                    );
                }
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
            self.seek_to(Duration::ZERO);
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
        let (is_shuffle_on, is_repeat_on) = self.playback_modes();
        let shuffled_position = self.shuffled_position_for_next(is_shuffle_on);
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
