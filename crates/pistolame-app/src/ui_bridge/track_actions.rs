//! The "…" menu actions and favorites.

use super::*;

impl UiBridge {
    /// Runs a "…" menu action on a track, an album or a playlist.
    pub(super) fn run_track_action(&self, target: TrackTarget, action: TrackAction) {
        let selected = self.selected_tracks_including(&target);
        if selected.is_some() {
            // The selection did its job; its rows are rebuilt by some actions anyway.
            if action == TrackAction::RemoveFromPlaylist {
                self.run_selection_action(action);
                return;
            }
            self.clear_selection();
        }
        if action == TrackAction::RemoveFromPlaylist {
            if target.source == TrackSource::PlaylistEntry
                && let Ok(position) = usize::try_from(target.position)
            {
                self.remove_playlist_entry(position);
            }
            return;
        }
        let Some((paths, subject)) = selected.or_else(|| self.target_tracks(&target)) else {
            return;
        };
        match action {
            TrackAction::PlayNext | TrackAction::AddToQueue => {
                {
                    let mut queue = self.play_queue.borrow_mut();
                    if action == TrackAction::PlayNext {
                        queue.play_next(paths);
                    } else {
                        queue.add_to_queue(paths);
                    }
                }
                if self.selected_track.get().is_none() {
                    // Nothing was playing: start with the first queued track.
                    self.play_next_in_queue(false);
                } else {
                    self.refresh_queue_view();
                }
                self.notice(format!(
                    "{subject} : {}",
                    if action == TrackAction::PlayNext {
                        "lu ensuite"
                    } else {
                        "ajouté à la file d’attente"
                    }
                ));
            }
            TrackAction::AddToPlaylist => {
                *self.pending_playlist_tracks.borrow_mut() = paths;
                self.open_add_to_playlist(subject);
            }
            TrackAction::RevealInFileManager => {
                if let Some(path) = paths.first() {
                    reveal_in_file_manager(path);
                }
            }
            TrackAction::ToggleFavorite => self.toggle_favorite(&paths, &subject),
            TrackAction::GoToAlbum | TrackAction::GoToArtist => {
                let Some((album_index, _)) =
                    paths.first().and_then(|path| self.track_location(path))
                else {
                    return;
                };
                if action == TrackAction::GoToAlbum {
                    self.open_album_at(album_index);
                } else {
                    let artist = self
                        .albums
                        .borrow()
                        .get(album_index)
                        .and_then(|album| album.artists.first().cloned());
                    if let Some(artist) = artist {
                        self.open_artist_named(&artist);
                    }
                }
            }
            TrackAction::RemoveFromLibrary => self.remove_from_library(paths, &subject),
            TrackAction::RemoveFromPlaylist => {}
        }
    }

    /// Forgets `paths` (the files stay on disk) and keeps them out of later rescans; "Annuler"
    /// lets them in again.
    fn remove_from_library(&self, paths: Vec<PathBuf>, subject: &str) {
        let Some(snapshot) = self.with_library(|store| {
            store.remove_tracks(&paths)?;
            store.snapshot()
        }) else {
            return;
        };
        self.replace_albums(snapshot);
        self.notice_with_undo(
            format!("{subject} : retiré de la bibliothèque"),
            super::undo::UndoableChange::LibraryRemoval { paths },
        );
    }

    /// Reads the favorites from the library. A read failure keeps the ones in memory.
    pub(super) fn load_favorites(&self) {
        if let Some(database) = &self.library_database
            && let Ok(store) = LibraryStore::open(database)
            && let Ok(favorites) = store.favorites()
        {
            *self.favorites.borrow_mut() = favorites.into_iter().collect();
        }
    }

    /// Adds `paths` to the favorites, or removes them when they all are already.
    pub(super) fn toggle_favorite(&self, paths: &[PathBuf], subject: &str) {
        let Some(database) = &self.library_database else {
            return;
        };
        let previous_favorites: Vec<PathBuf> = paths
            .iter()
            .filter(|path| self.favorites.borrow().contains(*path))
            .cloned()
            .collect();
        let should_add = previous_favorites.len() < paths.len();
        let saved = LibraryStore::open(database).and_then(|mut store| {
            paths
                .iter()
                .try_for_each(|path| store.set_favorite(path, should_add))
        });
        if let Err(error) = saved {
            self.message(format!("Impossible d’enregistrer le favori : {error}"));
            return;
        }
        {
            let mut favorites = self.favorites.borrow_mut();
            for path in paths {
                if should_add {
                    favorites.insert(path.clone());
                } else {
                    favorites.remove(path);
                }
            }
        }
        self.refresh_favorite_marks();
        self.notice_with_undo(
            format!(
                "{subject} : {}",
                if should_add {
                    "ajouté aux favoris"
                } else {
                    "retiré des favoris"
                }
            ),
            super::undo::UndoableChange::Favorites {
                paths: paths.to_vec(),
                previous_favorites,
            },
        );
    }

    /// Redraws every list that shows a heart.
    pub(super) fn refresh_favorite_marks(&self) {
        self.show_album(self.detail_album.get());
        self.refresh_playlist_view();
        self.show_top_tracks();
        self.show_library_tracks();
        if let Some(window) = self.window.upgrade() {
            self.update_grid(window.global::<AppState>().get_query().as_str());
        }
    }

    /// The tracks a menu action applies to, and how to name them.
    pub(super) fn target_tracks(&self, target: &TrackTarget) -> Option<(Vec<PathBuf>, String)> {
        let albums = self.albums.borrow();
        match target.source {
            TrackSource::AlbumTrack => {
                let track = albums
                    .get(usize::try_from(target.album).ok()?)?
                    .tracks
                    .get(usize::try_from(target.track).ok()?)?;
                Some((vec![track.path.clone()], format!("« {} »", track.title)))
            }
            TrackSource::Album => {
                let album = albums.get(usize::try_from(target.album).ok()?)?;
                Some((
                    album
                        .tracks
                        .iter()
                        .map(|track| track.path.clone())
                        .collect(),
                    format!("L’album « {} »", album.title),
                ))
            }
            TrackSource::PlaylistEntry => {
                let index = self.current_playlist_index()?;
                let path = self
                    .playlists
                    .borrow()
                    .get(index)?
                    .track_paths
                    .get(usize::try_from(target.position).ok()?)?
                    .clone();
                let title = albums
                    .iter()
                    .flat_map(|album| &album.tracks)
                    .find(|track| track.path == path)
                    .map(|track| track.title.clone())
                    .unwrap_or_default();
                Some((vec![path], format!("« {title} »")))
            }
            TrackSource::LibraryTrack | TrackSource::SearchResult | TrackSource::TopTrack => {
                let list = match target.source {
                    TrackSource::LibraryTrack => &self.library_tracks,
                    TrackSource::TopTrack => &self.top_tracks,
                    _ => &self.search_results,
                };
                let path = list
                    .borrow()
                    .get(usize::try_from(target.position).ok()?)?
                    .clone();
                let title = albums
                    .iter()
                    .flat_map(|album| &album.tracks)
                    .find(|track| track.path == path)
                    .map(|track| track.title.clone())
                    .unwrap_or_default();
                Some((vec![path], format!("« {title} »")))
            }
            TrackSource::Playlist => {
                let index = self.current_playlist_index()?;
                drop(albums);
                let paths: Vec<PathBuf> = self
                    .playable_entries(index)
                    .into_iter()
                    .map(|(_, path, _)| path)
                    .collect();
                let name = self.playlists.borrow().get(index)?.name.clone();
                Some((paths, format!("La playlist « {name} »")))
            }
        }
    }
}
