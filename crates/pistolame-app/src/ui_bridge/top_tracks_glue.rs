//! The "Les plus écoutés" playlist: page, Réglages and playback.

use super::*;
use crate::navigation::Page;
use crate::top_tracks::{
    DEFAULT_TOP_TRACK_COUNT, RECENT_PERIOD_SECONDS, RankedTrack, TOP_TRACKS_TITLE, TopTracksPeriod,
    parse_top_track_count, rank_top_tracks,
};
use crate::track_selection::SelectionList;
use slint::Model;

impl UiBridge {
    pub(super) fn connect_top_tracks(self: &Rc<Self>, state: &AppState<'_>) {
        {
            let settings = self.settings.borrow();
            state.set_are_top_tracks_on(!settings.are_top_tracks_off);
            state.set_top_track_count_text(
                settings
                    .top_track_count
                    .unwrap_or(DEFAULT_TOP_TRACK_COUNT)
                    .to_string()
                    .into(),
            );
            state.set_is_top_tracks_recent(settings.top_tracks_period == TopTracksPeriod::Recent);
        }
        let handle = self.clone();
        state.on_set_top_tracks_on(move |is_on| {
            handle.settings.borrow_mut().are_top_tracks_off = !is_on;
            handle.save_settings();
            if let Some(window) = handle.window.upgrade() {
                window.global::<AppState>().set_are_top_tracks_on(is_on);
            }
        });
        let handle = self.clone();
        state.on_set_top_track_count(move |text| {
            if let Some(count) = parse_top_track_count(&text) {
                handle.settings.borrow_mut().top_track_count = Some(count);
                handle.save_settings();
                handle.show_top_tracks();
            }
        });
        let handle = self.clone();
        state.on_set_top_tracks_recent(move |is_recent| {
            handle.settings.borrow_mut().top_tracks_period = if is_recent {
                TopTracksPeriod::Recent
            } else {
                TopTracksPeriod::AllTime
            };
            handle.save_settings();
            if let Some(window) = handle.window.upgrade() {
                window
                    .global::<AppState>()
                    .set_is_top_tracks_recent(is_recent);
            }
            handle.show_top_tracks();
        });
        let handle = self.clone();
        state.on_play_top_track(move |row| {
            if let Ok(row) = usize::try_from(row) {
                let paths = handle.top_tracks.borrow().clone();
                handle.play_context(paths, row, TOP_TRACKS_TITLE.to_owned());
            }
        });
    }

    /// The ranking under the current settings. A failed read of the recent plays gives an empty
    /// list: the page says so, and all time still works.
    fn ranked_top_tracks(&self) -> Vec<(PathBuf, u32)> {
        let (period, limit) = {
            let settings = self.settings.borrow();
            (
                settings.top_tracks_period,
                settings.top_track_count.unwrap_or(DEFAULT_TOP_TRACK_COUNT) as usize,
            )
        };
        let recent_counts = (period == TopTracksPeriod::Recent).then(|| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs() as i64);
            self.library_database
                .as_ref()
                .and_then(|database| LibraryStore::open(database).ok())
                .and_then(|store| store.play_counts_since(now - RECENT_PERIOD_SECONDS).ok())
                .unwrap_or_default()
        });
        let albums = self.albums.borrow();
        let tracks = albums
            .iter()
            .flat_map(|album| &album.tracks)
            .map(|track| RankedTrack {
                path: &track.path,
                play_count: match &recent_counts {
                    Some(counts) => counts.get(&track.path).copied().unwrap_or(0),
                    None => track.play_count,
                },
                last_played_seconds: track.last_played_seconds,
            })
            .collect();
        rank_top_tracks(tracks, limit)
    }

    /// Fills the page, only while it is shown: the ranking is read each time it opens.
    pub(super) fn show_top_tracks(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        if self.navigation.borrow().current().page != Page::TopTracks {
            return;
        }
        let ranked = self.ranked_top_tracks();
        let current_path = self.current_path();
        let albums = self.albums.borrow();
        let locations: std::collections::HashMap<&std::path::Path, (&Album, &MusicFile)> = albums
            .iter()
            .flat_map(|album| {
                album
                    .tracks
                    .iter()
                    .map(move |track| (track.path.as_path(), (album, track)))
            })
            .collect();
        let mut total_seconds = 0;
        let rows: Vec<PlaylistTrackRow> = ranked
            .iter()
            .enumerate()
            .filter_map(|(position, (path, play_count))| {
                let (album, file) = locations.get(path.as_path())?;
                total_seconds += file.duration_seconds;
                Some(PlaylistTrackRow {
                    position: position as i32,
                    number: position as i32 + 1,
                    title: file.title.as_str().into(),
                    subtitle: format!("{} — {}", album.artist, album.title).into(),
                    format_label: presentation::play_count_label(*play_count).into(),
                    duration_label: presentation::clock(file.duration_seconds).into(),
                    is_current: current_path.as_ref() == Some(path),
                    is_favorite: self.favorites.borrow().contains(path),
                    is_selected: self.is_row_selected(SelectionList::TopTracks, position),
                })
            })
            .collect();
        drop(locations);
        drop(albums);
        let count = rows.len();
        state.set_top_tracks_meta(
            format!(
                "{count} morceau{} · {}",
                if count == 1 { "" } else { "x" },
                presentation::clock(total_seconds)
            )
            .into(),
        );
        state.set_top_tracks(ModelRc::new(VecModel::from(rows)));
        *self.top_tracks.borrow_mut() = ranked.into_iter().map(|(path, _)| path).collect();
    }

    /// Moves the "playing" mark without ranking again: rows must not jump under the pointer
    /// each time a track starts.
    pub(super) fn mark_current_top_track(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let model = window.global::<AppState>().get_top_tracks();
        let current_path = self.current_path();
        let paths = self.top_tracks.borrow();
        for (index, path) in paths.iter().enumerate().take(model.row_count()) {
            let is_current = current_path.as_ref() == Some(path);
            if let Some(mut row) = model.row_data(index)
                && row.is_current != is_current
            {
                row.is_current = is_current;
                model.set_row_data(index, row);
            }
        }
    }
}
