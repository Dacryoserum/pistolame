//! Fake transport shared by both prototypes, so play/pause, next/previous, seeking and
//! auto-advance behave identically.

use crate::catalog::{CatalogAlbum, CatalogTrack, format_clock, format_remaining_clock};

/// "Previous" restarts the current track when more than this has been played
/// (the usual convention of desktop players).
pub const PREVIOUS_RESTARTS_AFTER_SECONDS: f32 = 3.0;
/// Bar heights shown in the track list while paused (fractions of the max height).
pub const PAUSED_EQUALIZER_LEVELS: [f32; 3] = [0.35, 0.6, 0.25];

#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackState {
    /// Index into `catalog_albums()` (not into a filtered list).
    pub album_index: usize,
    pub track_index: usize,
    pub elapsed_seconds: f32,
    pub is_playing: bool,
    pub volume_percent: u8,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            album_index: 0,
            track_index: 0,
            elapsed_seconds: 0.0,
            is_playing: true,
            volume_percent: 100,
        }
    }
}

impl PlaybackState {
    pub fn current_track<'a>(&self, albums: &'a [CatalogAlbum]) -> &'a CatalogTrack {
        &albums[self.album_index].tracks[self.track_index]
    }

    pub fn play_track(&mut self, album_index: usize, track_index: usize) {
        self.album_index = album_index;
        self.track_index = track_index;
        self.elapsed_seconds = 0.0;
        self.is_playing = true;
    }

    pub fn toggle_play(&mut self) {
        self.is_playing = !self.is_playing;
    }

    /// Moves time forward while playing; at the end of a track, continues with the next one.
    /// Returns true when the track changed.
    pub fn advance(&mut self, delta_seconds: f32, albums: &[CatalogAlbum]) -> bool {
        if !self.is_playing {
            return false;
        }
        self.elapsed_seconds += delta_seconds;
        if self.elapsed_seconds >= self.current_track(albums).duration_seconds as f32 {
            self.next_track(albums);
            return true;
        }
        false
    }

    /// Next track of the album, wrapping to the first one.
    pub fn next_track(&mut self, albums: &[CatalogAlbum]) {
        let track_count = albums[self.album_index].tracks.len();
        self.track_index = (self.track_index + 1) % track_count;
        self.elapsed_seconds = 0.0;
    }

    /// Restarts the track after 3 s of play, otherwise goes to the previous one (wrapping).
    pub fn previous_track(&mut self, albums: &[CatalogAlbum]) {
        if self.elapsed_seconds > PREVIOUS_RESTARTS_AFTER_SECONDS {
            self.elapsed_seconds = 0.0;
            return;
        }
        let track_count = albums[self.album_index].tracks.len();
        self.track_index = (self.track_index + track_count - 1) % track_count;
        self.elapsed_seconds = 0.0;
    }

    pub fn seek_to_fraction(&mut self, fraction: f32, albums: &[CatalogAlbum]) {
        let duration = self.current_track(albums).duration_seconds as f32;
        self.elapsed_seconds = fraction.clamp(0.0, 1.0) * duration;
    }

    pub fn progress_fraction(&self, albums: &[CatalogAlbum]) -> f32 {
        let duration = self.current_track(albums).duration_seconds as f32;
        (self.elapsed_seconds / duration).clamp(0.0, 1.0)
    }

    /// "1:23".
    pub fn elapsed_label(&self) -> String {
        format_clock(self.elapsed_seconds as u32)
    }

    /// "−2:41".
    pub fn remaining_label(&self, albums: &[CatalogAlbum]) -> String {
        let duration = self.current_track(albums).duration_seconds;
        format_remaining_clock(duration.saturating_sub(self.elapsed_seconds as u32))
    }

    /// Live MD5 of the decoded stream, compared with the FLAC STREAMINFO MD5 at track end.
    /// "En cours · 37 %".
    pub fn md5_progress_label(&self, albums: &[CatalogAlbum]) -> String {
        let percent = (self.progress_fraction(albums) * 100.0).floor() as u32;
        format!("En cours · {percent}\u{a0}%")
    }
}

/// Three bar levels (0..1) for the now-playing indicator, from the fake spectrum.
pub fn equalizer_levels(time_seconds: f32) -> [f32; 3] {
    let spectrum = crate::spectrum(time_seconds);
    [spectrum[3], spectrum[12], spectrum[24]]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::catalog_albums;

    #[test]
    fn previous_restarts_then_wraps() {
        let albums = catalog_albums();
        let mut state = PlaybackState {
            elapsed_seconds: 10.0,
            ..PlaybackState::default()
        };
        state.previous_track(&albums);
        assert_eq!((state.track_index, state.elapsed_seconds), (0, 0.0));
        state.previous_track(&albums);
        assert_eq!(state.track_index, albums[0].tracks.len() - 1);
    }

    #[test]
    fn advance_moves_to_next_track_at_end() {
        let albums = catalog_albums();
        let mut state = PlaybackState::default();
        let duration = albums[0].tracks[0].duration_seconds as f32;
        assert!(state.advance(duration + 0.1, &albums));
        assert_eq!(state.track_index, 1);
    }
}
