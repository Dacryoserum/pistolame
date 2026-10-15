//! Playlists: page, editor, export and the "Ajouter à une playlist" sheet.

use super::*;

impl UiBridge {
    pub(super) fn connect_playlists(self: &Rc<Self>, state: &AppState<'_>) {
        let handle = self.clone();
        state.on_create_playlist(move || handle.create_playlist(Vec::new(), true));
        let handle = self.clone();
        state.on_play_playlist_row(move |row| {
            if let Ok(row) = usize::try_from(row) {
                handle.play_playlist_row(row);
            }
        });
        let handle = self.clone();
        state.on_remove_playlist_entry(move |position| {
            if let Ok(position) = usize::try_from(position) {
                handle.remove_playlist_entry(position);
            }
        });
        let handle = self.clone();
        state.on_begin_playlist_edit(move || handle.begin_playlist_edit());
        let handle = self.clone();
        state.on_save_playlist_edit(move || handle.save_playlist_edit());
        let handle = self.clone();
        state.on_playlist_editor_choose_artwork(move || {
            if let Some((bytes, image)) = handle.pick_cover_image() {
                if let Some(window) = handle.window.upgrade() {
                    window
                        .global::<AppState>()
                        .set_playlist_editor_artwork(image.clone());
                }
                *handle.pending_playlist_artwork.borrow_mut() =
                    PendingArtwork::Chosen { bytes, image };
            }
        });
        let handle = self.clone();
        state.on_playlist_editor_remove_artwork(move || {
            if let Some(window) = handle.window.upgrade() {
                window
                    .global::<AppState>()
                    .set_playlist_editor_artwork(slint::Image::default());
            }
            *handle.pending_playlist_artwork.borrow_mut() = PendingArtwork::Restored;
        });
        let handle = self.clone();
        state.on_delete_current_playlist(move || handle.delete_current_playlist());
        let handle = self.clone();
        state.on_export_current_playlist(move || handle.export_current_playlist());
        let handle = self.clone();
        state.on_add_pending_to_playlist(move |index| {
            if let Ok(index) = usize::try_from(index) {
                handle.add_pending_to_playlist(index);
            }
        });
        let handle = self.clone();
        state.on_add_pending_to_new_playlist(move || {
            let tracks = std::mem::take(&mut *handle.pending_playlist_tracks.borrow_mut());
            handle.close_add_to_playlist();
            handle.create_playlist(tracks, false);
        });
    }

    pub(super) fn show_playlists(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let items: Vec<PlaylistItem> = self
            .playlists
            .borrow()
            .iter()
            .map(|playlist| PlaylistItem {
                name: playlist.name.as_str().into(),
                track_count: playlist.track_paths.len() as i32,
                artwork: decode_artwork(playlist.artwork.as_ref()),
            })
            .collect();
        window
            .global::<AppState>()
            .set_playlists(ModelRc::new(VecModel::from(items)));
        self.refresh_playlist_view();
    }

    /// The playlist open in the playlist view, if any.
    pub(super) fn current_playlist_index(&self) -> Option<usize> {
        let window = self.window.upgrade()?;
        let index = usize::try_from(window.global::<AppState>().get_selected_playlist()).ok()?;
        (index < self.playlists.borrow().len()).then_some(index)
    }

    /// The entries of playlist `index` still in the library, with their position among all
    /// entries (for removal) and their file.
    pub(super) fn playable_entries(&self, index: usize) -> Vec<(usize, PathBuf, (usize, usize))> {
        let Some(paths) = self
            .playlists
            .borrow()
            .get(index)
            .map(|playlist| playlist.track_paths.clone())
        else {
            return Vec::new();
        };
        paths
            .into_iter()
            .enumerate()
            .filter_map(|(position, path)| {
                self.track_location(&path)
                    .map(|location| (position, path, location))
            })
            .collect()
    }

    pub(super) fn refresh_playlist_view(&self) {
        let (Some(window), Some(index)) = (self.window.upgrade(), self.current_playlist_index())
        else {
            return;
        };
        let state = window.global::<AppState>();
        let current_path = self.selected_track.get().and_then(|(album, track)| {
            self.albums
                .borrow()
                .get(album)
                .and_then(|album| album.tracks.get(track))
                .map(|track| track.path.clone())
        });
        let albums = self.albums.borrow();
        let mut total_seconds = 0;
        let rows: Vec<PlaylistTrackRow> = self
            .playable_entries(index)
            .into_iter()
            .enumerate()
            .filter_map(|(row, (position, path, (album, track)))| {
                let album = albums.get(album)?;
                let file = album.tracks.get(track)?;
                total_seconds += file.duration_seconds;
                Some(PlaylistTrackRow {
                    position: position as i32,
                    number: row as i32 + 1,
                    title: file.title.as_str().into(),
                    subtitle: format!("{} — {}", album.artist, album.title).into(),
                    format_label: presentation::audio_format(
                        file.bits_per_sample,
                        file.sample_rate_hz,
                    )
                    .into(),
                    duration_label: presentation::clock(file.duration_seconds).into(),
                    is_current: current_path.as_ref() == Some(&path),
                    is_favorite: self.favorites.borrow().contains(&path),
                })
            })
            .collect();
        let count = rows.len();
        if let Some(playlist) = self.playlists.borrow().get(index) {
            state.set_playlist_title(playlist.name.as_str().into());
            state.set_playlist_artwork(decode_artwork(playlist.artwork.as_ref()));
        }
        state.set_playlist_meta(
            format!(
                "{count} morceau{} · {}",
                if count == 1 { "" } else { "x" },
                presentation::clock(total_seconds)
            )
            .into(),
        );
        state.set_playlist_tracks(ModelRc::new(VecModel::from(rows)));
    }

    pub(super) fn play_playlist_row(&self, row: usize) {
        let Some(index) = self.current_playlist_index() else {
            return;
        };
        let paths: Vec<PathBuf> = self
            .playable_entries(index)
            .into_iter()
            .map(|(_, path, _)| path)
            .collect();
        let name = self
            .playlists
            .borrow()
            .get(index)
            .map(|playlist| playlist.name.clone())
            .unwrap_or_default();
        self.play_context(paths, row, name);
    }

    /// Creates a playlist named "Nouvelle playlist" (numbered if taken) holding `tracks`. When
    /// `should_open` is set, it opens it with the rename sheet, as Apple Music does.
    pub(super) fn create_playlist(&self, tracks: Vec<PathBuf>, should_open: bool) {
        let name = {
            let playlists = self.playlists.borrow();
            let is_taken = |name: &str| playlists.iter().any(|playlist| playlist.name == name);
            let mut name = "Nouvelle playlist".to_owned();
            let mut number = 2;
            while is_taken(&name) {
                name = format!("Nouvelle playlist {number}");
                number += 1;
            }
            name
        };
        let Some(id) = self.with_library(|store| {
            let id = store.create_playlist(&name)?;
            store.append_to_playlist(id, &tracks)?;
            Ok(id)
        }) else {
            return;
        };
        self.playlists.borrow_mut().push(Playlist {
            id,
            name,
            track_paths: tracks,
            artwork: None,
        });
        self.show_playlists();
        if should_open {
            self.open_playlist(self.playlists.borrow().len() - 1);
            self.begin_playlist_edit();
        }
    }

    pub(super) fn remove_playlist_entry(&self, position: usize) {
        let Some(index) = self.current_playlist_index() else {
            return;
        };
        let Some(id) = self
            .playlists
            .borrow()
            .get(index)
            .map(|playlist| playlist.id)
        else {
            return;
        };
        if self
            .with_library(|store| store.remove_from_playlist(id, position))
            .is_none()
        {
            return;
        }
        if let Some(playlist) = self.playlists.borrow_mut().get_mut(index)
            && position < playlist.track_paths.len()
        {
            playlist.track_paths.remove(position);
        }
        self.show_playlists();
    }

    pub(super) fn begin_playlist_edit(&self) {
        let (Some(window), Some(index)) = (self.window.upgrade(), self.current_playlist_index())
        else {
            return;
        };
        let state = window.global::<AppState>();
        if let Some(playlist) = self.playlists.borrow().get(index) {
            state.set_playlist_editor_name(playlist.name.as_str().into());
            state.set_playlist_editor_artwork(decode_artwork(playlist.artwork.as_ref()));
        }
        *self.pending_playlist_artwork.borrow_mut() = PendingArtwork::Unchanged;
        state.set_is_playlist_editor_open(true);
    }

    pub(super) fn save_playlist_edit(&self) {
        let (Some(window), Some(index)) = (self.window.upgrade(), self.current_playlist_index())
        else {
            return;
        };
        let state = window.global::<AppState>();
        let name = state.get_playlist_editor_name().trim().to_owned();
        let Some(id) = self
            .playlists
            .borrow()
            .get(index)
            .map(|playlist| playlist.id)
        else {
            return;
        };
        let artwork = std::mem::replace(
            &mut *self.pending_playlist_artwork.borrow_mut(),
            PendingArtwork::Unchanged,
        );
        let saved = self.with_library(|store| {
            if !name.is_empty() {
                store.rename_playlist(id, &name)?;
            }
            match &artwork {
                PendingArtwork::Unchanged => Ok(()),
                PendingArtwork::Chosen { bytes, .. } => store.set_playlist_artwork(id, bytes),
                PendingArtwork::Restored => store.clear_playlist_artwork(id),
            }
        });
        if saved.is_some() {
            if let Some(playlist) = self.playlists.borrow_mut().get_mut(index) {
                if !name.is_empty() {
                    playlist.name = name;
                }
                match artwork {
                    PendingArtwork::Unchanged => {}
                    PendingArtwork::Chosen { bytes, .. } => playlist.artwork = Some(bytes.into()),
                    PendingArtwork::Restored => playlist.artwork = None,
                }
            }
            self.show_playlists();
        }
        state.set_is_playlist_editor_open(false);
    }

    /// Saves the open playlist as an extended M3U (UTF-8), readable by other players.
    pub(super) fn export_current_playlist(&self) {
        let Some(index) = self.current_playlist_index() else {
            return;
        };
        let Some(name) = self
            .playlists
            .borrow()
            .get(index)
            .map(|playlist| playlist.name.clone())
        else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{name}.m3u8"))
            .add_filter("Playlist M3U", &["m3u8", "m3u"])
            .save_file()
        else {
            return;
        };
        let albums = self.albums.borrow();
        let entries: Vec<crate::m3u::M3uEntry> = self
            .playable_entries(index)
            .into_iter()
            .filter_map(|(_, path, (album, track))| {
                let album = albums.get(album)?;
                let file = album.tracks.get(track)?;
                Some(crate::m3u::M3uEntry {
                    path,
                    duration_seconds: file.duration_seconds,
                    artist: album.artist.clone(),
                    title: file.title.clone(),
                })
            })
            .collect();
        drop(albums);
        match std::fs::write(&path, crate::m3u::write_m3u(&entries)) {
            Ok(()) => self.notice(format!("Playlist exportée : {}", path.display())),
            Err(error) => self.message(format!("Export impossible : {error}")),
        }
    }

    pub(super) fn delete_current_playlist(&self) {
        let (Some(window), Some(index)) = (self.window.upgrade(), self.current_playlist_index())
        else {
            return;
        };
        let state = window.global::<AppState>();
        let Some(id) = self
            .playlists
            .borrow()
            .get(index)
            .map(|playlist| playlist.id)
        else {
            return;
        };
        if self
            .with_library(|store| store.delete_playlist(id))
            .is_none()
        {
            return;
        }
        self.playlists.borrow_mut().remove(index);
        state.set_is_playlist_editor_open(false);
        // Leaves the deleted playlist (Back, else the album grid) and forgets it in the history.
        self.synchronize_navigation_with_library();
        self.show_playlists();
    }

    pub(super) fn open_add_to_playlist(&self, subject: String) {
        if let Some(window) = self.window.upgrade() {
            let state = window.global::<AppState>();
            state.set_add_to_playlist_subject(subject.into());
            state.set_is_add_to_playlist_open(true);
        }
    }

    pub(super) fn close_add_to_playlist(&self) {
        if let Some(window) = self.window.upgrade() {
            window
                .global::<AppState>()
                .set_is_add_to_playlist_open(false);
        }
    }

    pub(super) fn add_pending_to_playlist(&self, index: usize) {
        let tracks = std::mem::take(&mut *self.pending_playlist_tracks.borrow_mut());
        self.close_add_to_playlist();
        let Some((id, name)) = self
            .playlists
            .borrow()
            .get(index)
            .map(|playlist| (playlist.id, playlist.name.clone()))
        else {
            return;
        };
        if self
            .with_library(|store| store.append_to_playlist(id, &tracks))
            .is_none()
        {
            return;
        }
        let count = tracks.len();
        if let Some(playlist) = self.playlists.borrow_mut().get_mut(index) {
            playlist.track_paths.extend(tracks);
        }
        self.show_playlists();
        self.notice(format!(
            "{count} morceau{} ajouté{} à « {name} »",
            if count == 1 { "" } else { "x" },
            if count == 1 { "" } else { "s" }
        ));
    }
}
