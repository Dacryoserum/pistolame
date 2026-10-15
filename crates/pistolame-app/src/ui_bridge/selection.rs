//! Multiple selection in the track lists and the selection action bar.

use super::*;
use crate::track_selection::{SelectionGesture, SelectionList};
use slint::Model;

impl UiBridge {
    pub(super) fn connect_selection(self: &Rc<Self>, state: &AppState<'_>) {
        let handle = self.clone();
        state.on_select_track(move |target, is_toggle, is_range| {
            let gesture = if is_range {
                SelectionGesture::ExtendRange
            } else if is_toggle {
                SelectionGesture::Toggle
            } else {
                SelectionGesture::Replace
            };
            handle.select_track(&target, gesture);
        });
        let handle = self.clone();
        state.on_selection_action(move |action| handle.run_selection_action(action));
        let handle = self.clone();
        state.on_clear_selection(move || handle.clear_selection());
    }

    /// The list and position a row target belongs to; `None` for whole albums and playlists.
    fn selection_place(&self, target: &TrackTarget) -> Option<(SelectionList, usize)> {
        let list = match target.source {
            TrackSource::AlbumTrack => SelectionList::AlbumTracks {
                album_index: usize::try_from(target.album).ok()?,
            },
            TrackSource::PlaylistEntry => SelectionList::Playlist {
                playlist_index: self.current_playlist_index()?,
            },
            TrackSource::SearchResult => SelectionList::SearchResults,
            TrackSource::LibraryTrack => SelectionList::LibraryTracks,
            TrackSource::Album | TrackSource::Playlist => return None,
        };
        let position = if target.source == TrackSource::AlbumTrack {
            target.track
        } else {
            target.position
        };
        Some((list, usize::try_from(position).ok()?))
    }

    /// True when the row at `position` of `list` is selected (used when rows are built).
    pub(super) fn is_row_selected(&self, list: SelectionList, position: usize) -> bool {
        self.selection.borrow().contains(list, position)
    }

    fn select_track(&self, target: &TrackTarget, gesture: SelectionGesture) {
        let Some((list, position)) = self.selection_place(target) else {
            return;
        };
        let previous: Vec<usize> = self.selection.borrow().positions().collect();
        self.selection.borrow_mut().click(list, position, gesture);
        self.show_selection(&previous);
    }

    /// Forgets the selection and unmarks its rows (page left, list rebuilt, Escape).
    pub(super) fn clear_selection(&self) {
        if self.selection.borrow().len() == 0 {
            return;
        }
        let previous: Vec<usize> = self.selection.borrow().positions().collect();
        self.selection.borrow_mut().clear();
        self.show_selection(&previous);
    }

    /// Redraws the marks and the action bar. `previous` are the positions selected before, so
    /// the long lists only rewrite the rows that changed.
    fn show_selection(&self, previous: &[usize]) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let selection = self.selection.borrow();
        let changed: Vec<usize> = previous
            .iter()
            .copied()
            .chain(selection.positions())
            .collect();
        state.set_selection_count(selection.len() as i32);
        state.set_can_remove_selection(matches!(
            selection.list(),
            Some(SelectionList::Playlist { .. })
        ));
        drop(selection);

        let album_list = SelectionList::AlbumTracks {
            album_index: self.detail_album.get(),
        };
        mark_rows(
            &state.get_detail_tracks(),
            0..usize::MAX,
            |_, row: &TrackRowData| {
                !row.is_disc_header
                    && usize::try_from(row.track_index)
                        .is_ok_and(|track| self.is_row_selected(album_list, track))
            },
            |row| &mut row.is_selected,
        );
        if let Some(playlist_index) = self.current_playlist_index() {
            let playlist_list = SelectionList::Playlist { playlist_index };
            mark_rows(
                &state.get_playlist_tracks(),
                0..usize::MAX,
                |_, row: &PlaylistTrackRow| {
                    usize::try_from(row.position)
                        .is_ok_and(|position| self.is_row_selected(playlist_list, position))
                },
                |row| &mut row.is_selected,
            );
        }
        for &position in &changed {
            mark_rows(
                &state.get_search_tracks(),
                position..position + 1,
                |row, _: &PlaylistTrackRow| self.is_row_selected(SelectionList::SearchResults, row),
                |row| &mut row.is_selected,
            );
            mark_rows(
                &state.get_library_tracks(),
                position..position + 1,
                |row, _: &LibraryTrackRow| self.is_row_selected(SelectionList::LibraryTracks, row),
                |row| &mut row.is_selected,
            );
        }
    }

    /// The selected tracks when `target` is one of several selected rows: a menu action on a
    /// selected row applies to the whole selection.
    pub(super) fn selected_tracks_including(
        &self,
        target: &TrackTarget,
    ) -> Option<(Vec<PathBuf>, String)> {
        let (list, position) = self.selection_place(target)?;
        let selection = self.selection.borrow();
        if selection.len() < 2 || !selection.contains(list, position) {
            return None;
        }
        let paths: Vec<PathBuf> = selection
            .positions()
            .filter_map(|position| self.path_at(list, position))
            .collect();
        let subject = format!("{} morceaux", paths.len());
        Some((paths, subject))
    }

    /// The file at `position` of `list`.
    fn path_at(&self, list: SelectionList, position: usize) -> Option<PathBuf> {
        match list {
            SelectionList::AlbumTracks { album_index } => self
                .albums
                .borrow()
                .get(album_index)?
                .tracks
                .get(position)
                .map(|track| track.path.clone()),
            SelectionList::Playlist { playlist_index } => self
                .playlists
                .borrow()
                .get(playlist_index)?
                .track_paths
                .get(position)
                .cloned(),
            SelectionList::SearchResults => self.search_results.borrow().get(position).cloned(),
            SelectionList::LibraryTracks => self.library_tracks.borrow().get(position).cloned(),
        }
    }

    /// Runs an action of the selection bar on every selected track.
    pub(super) fn run_selection_action(&self, action: TrackAction) {
        let (list, positions) = {
            let selection = self.selection.borrow();
            let Some(list) = selection.list() else {
                return;
            };
            (list, selection.positions().collect::<Vec<_>>())
        };
        if action == TrackAction::RemoveFromPlaylist {
            if matches!(list, SelectionList::Playlist { .. }) {
                self.clear_selection();
                // From the end, so the positions still to remove do not move.
                for position in positions.into_iter().rev() {
                    self.remove_playlist_entry(position);
                }
            }
            return;
        }
        let Some(&first) = positions.first() else {
            return;
        };
        let target = match list {
            SelectionList::AlbumTracks { album_index } => TrackTarget {
                source: TrackSource::AlbumTrack,
                album: album_index as i32,
                track: first as i32,
                position: 0,
            },
            SelectionList::Playlist { .. } => TrackTarget {
                source: TrackSource::PlaylistEntry,
                position: first as i32,
                ..Default::default()
            },
            SelectionList::SearchResults => TrackTarget {
                source: TrackSource::SearchResult,
                position: first as i32,
                ..Default::default()
            },
            SelectionList::LibraryTracks => TrackTarget {
                source: TrackSource::LibraryTrack,
                position: first as i32,
                ..Default::default()
            },
        };
        self.run_track_action(target, action);
    }
}

/// Rewrites the `is_selected` flag of the rows of `model` in `rows` whose mark changed.
fn mark_rows<Row: Clone + 'static>(
    model: &ModelRc<Row>,
    rows: std::ops::Range<usize>,
    should_be_selected: impl Fn(usize, &Row) -> bool,
    flag: impl Fn(&mut Row) -> &mut bool,
) {
    let end = rows.end.min(model.row_count());
    for index in rows.start..end {
        let Some(mut row) = model.row_data(index) else {
            continue;
        };
        let is_selected = should_be_selected(index, &row);
        if *flag(&mut row) != is_selected {
            *flag(&mut row) = is_selected;
            model.set_row_data(index, row);
        }
    }
}
