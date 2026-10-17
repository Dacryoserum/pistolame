//! Library loading, folder watching and artist photos.

use super::*;

impl UiBridge {
    /// Asks for the photos of the artists that have none yet (unless the user turned it off).
    pub(super) fn look_up_artist_photos(&self) {
        let (Some(worker), Some(database)) = (&self.artist_photo_worker, &self.library_database)
        else {
            return;
        };
        if self.settings.borrow().are_artist_photos_off {
            return;
        }
        let mut artists: Vec<String> = Vec::new();
        for artist in self.albums.borrow().iter().flat_map(|album| &album.artists) {
            if !artists.contains(artist) {
                artists.push(artist.clone());
            }
        }
        if let Ok(due) = LibraryStore::open(database).and_then(|store| {
            store.artists_to_look_up(&artists, crate::artist_photos::RETRY_AFTER_SECONDS)
        }) {
            worker.request(due);
        }
    }

    /// Shows the photos found since the last tick.
    pub(super) fn collect_artist_photos(&self) {
        let Some(worker) = &self.artist_photo_worker else {
            return;
        };
        let mut has_new_photo = false;
        for photo in worker.take_results() {
            // The worker stored the photo in the library: its id is all the cache needs.
            if let Some(bytes) = photo.image {
                self.artist_photos
                    .borrow_mut()
                    .insert(photo.artist, ArtworkId::of_bytes(&bytes));
                has_new_photo = true;
            }
        }
        if has_new_photo {
            self.show_artists();
            // An artist's page shown meanwhile takes its photo too.
            if let Some(window) = self.window.upgrade() {
                let state = window.global::<AppState>();
                let artist = state.get_artist_filter();
                if !artist.is_empty() {
                    self.show_artist_header(&state, &artist);
                }
            }
        }
    }

    /// Watches the library folders, restarting the watcher only when the folders changed.
    pub(super) fn watch_library_folders(&self) {
        let Some(database) = &self.library_database else {
            return;
        };
        let Ok(roots) = LibraryStore::open(database).and_then(|store| store.roots()) else {
            return;
        };
        if self
            .folder_watcher
            .borrow()
            .as_ref()
            .is_some_and(|(watched, _)| *watched == roots)
        {
            return;
        }
        let sender = self.folder_change_sender.clone();
        // Without a watcher the library still updates at start-up and on "Rescanner".
        let watcher = FolderWatcher::watch(&roots, move |change| {
            let _ = sender.send(change);
        })
        .ok();
        *self.folder_watcher.borrow_mut() = watcher.map(|watcher| (roots, watcher));
    }

    /// Synchronizes the changed files and folders once folder changes have settled (a copy in
    /// progress sends many events; one pass after the last one is enough).
    pub(super) fn synchronize_after_folder_changes(&self) {
        /// Quiet time after the last change before synchronizing.
        const FOLDER_SETTLE_DELAY: Duration = Duration::from_secs(2);
        /// Changed paths beyond which everything is synchronized: notify does not report the
        /// events Windows drops when its buffer overflows, which happens in bursts this large
        /// (retagging a whole collection), and a full pass is then about as cheap.
        const MAXIMUM_PATHS_SYNCHRONIZED_ALONE: usize = 500;
        let now = std::time::Instant::now();
        let mut has_new_change = false;
        for change in self.folder_changes.try_iter() {
            has_new_change = true;
            let mut pending = self.pending_folder_changes.borrow_mut();
            match change {
                FolderChange::Paths(paths) => pending.paths.extend(paths),
                FolderChange::Unknown => pending.needs_everything = true,
            }
        }
        if has_new_change {
            self.last_folder_change.set(Some(now));
        }
        if let Some(last_change) = self.last_folder_change.get()
            && now.duration_since(last_change) >= FOLDER_SETTLE_DELAY
            && !self.is_importing.get()
        {
            self.last_folder_change.set(None);
            let pending = std::mem::take(&mut *self.pending_folder_changes.borrow_mut());
            if pending.needs_everything || pending.paths.len() > MAXIMUM_PATHS_SYNCHRONIZED_ALONE {
                self.synchronize_library(Vec::new());
            } else if !pending.paths.is_empty() {
                self.synchronize_changed_paths(outermost_paths(pending.paths));
            }
        }
    }

    /// Shows the remembered library folders on the settings page.
    pub(super) fn show_library_folders(&self) {
        let (Some(window), Some(database)) = (self.window.upgrade(), &self.library_database) else {
            return;
        };
        let Ok(store) = LibraryStore::open(database) else {
            return;
        };
        let folders: Vec<SharedString> = store
            .roots()
            .unwrap_or_default()
            .iter()
            .map(|root| root.display().to_string().into())
            .collect();
        window
            .global::<AppState>()
            .set_library_folders(ModelRc::new(VecModel::from(folders)));
    }

    /// Forgets a library folder (its files stay on disk), then reloads the library.
    pub(super) fn remove_library_folder(&self, index: usize) {
        let Some(database) = &self.library_database else {
            return;
        };
        let Some(root) = LibraryStore::open(database)
            .ok()
            .and_then(|store| store.roots().ok())
            .and_then(|roots| roots.get(index).cloned())
        else {
            return;
        };
        if self
            .with_library(|store| store.remove_root(&root))
            .is_some()
        {
            self.synchronize_library(Vec::new());
        }
    }

    pub(super) fn apply_library_update(&self, update: LibraryUpdate) {
        let is_final = update.is_final;
        if is_final {
            self.is_importing.set(false);
            self.watch_library_folders();
        }
        if let Some(snapshot) = update.snapshot {
            self.replace_albums(snapshot);
            if !self.is_session_restored.get() {
                self.is_session_restored.set(true);
                self.restore_session();
            }
        }
        self.show_library_folders();
        if let Some(window) = self.window.upgrade() {
            let albums = self.albums.borrow();
            let count: usize = albums.iter().map(|album| album.tracks.len()).sum();
            window.global::<AppState>().set_import_status(
                format!(
                    "{count} morceau{} · {} album{}",
                    if count == 1 { "" } else { "x" },
                    albums.len(),
                    if albums.len() == 1 { "" } else { "s" }
                )
                .into(),
            );
        }
        if is_final {
            self.look_up_artist_photos();
        }
        if let Some(first) = update.errors.first() {
            self.message(format!(
                "{} problème(s) d’import. {first}",
                update.errors.len()
            ));
        }
    }

    /// Rebuilds the albums from a library snapshot, keeping the selected track and the open
    /// album when they still exist.
    pub(super) fn replace_albums(&self, mut snapshot: LibrarySnapshot) {
        *self.artist_photos.borrow_mut() = std::mem::take(&mut snapshot.artist_images);
        let selected_path = self.selected_track.get().and_then(|(album, track)| {
            self.albums
                .borrow()
                .get(album)
                .and_then(|album| album.tracks.get(track))
                .map(|track| track.path.clone())
        });
        let detail_key = self
            .albums
            .borrow()
            .get(self.detail_album.get())
            .map(|album| album.key.clone());
        let mut snapshot = snapshot;
        *self.playlists.borrow_mut() = std::mem::take(&mut snapshot.playlists);
        let mut albums = albums_from_snapshot(snapshot);
        let sort = self.album_sort();
        albums.sort_by(|left, right| {
            crate::album_sort::compare_albums(sort, &left.sort_keys, &right.sort_keys)
        });
        self.selected_track.set(selected_path.and_then(|path| {
            albums.iter().enumerate().find_map(|(album, entry)| {
                entry
                    .tracks
                    .iter()
                    .position(|track| track.path == path)
                    .map(|track| (album, track))
            })
        }));
        if let Some(index) =
            detail_key.and_then(|key| albums.iter().position(|album| album.key == key))
        {
            self.detail_album.set(index);
        }
        *self.albums.borrow_mut() = albums;
        self.load_favorites();
        if let Some(window) = self.window.upgrade() {
            let state = window.global::<AppState>();
            self.update_grid(state.get_query().as_str());
            self.show_album(self.detail_album.get());
            let (album, track) = self
                .selected_track
                .get()
                .map_or((-1, -1), |(album, track)| (album as i32, track as i32));
            state.set_playing_album_index(album);
            state.set_playing_track_index(track);
        }
        // Before the playlists are redrawn: the open playlist's index may have moved.
        self.synchronize_navigation_with_library();
        self.show_playlists();
        self.show_artists();
        self.show_library_tracks();
        self.show_recent_albums();
    }

    /// Fills "Ajouts récents": the newest albums, then the ones played last.
    pub(super) fn show_recent_albums(&self) {
        /// More than two rows of the widest grid never show.
        const MAXIMUM_SHELF_ALBUMS: usize = 16;
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let albums = self.albums.borrow();
        let labels = distinguishing_labels(&albums);
        let card = |(index, album): (usize, &Album)| {
            album_card(index, album, labels.get(index).and_then(Option::as_deref))
        };
        let mut added: Vec<(usize, &Album)> = albums.iter().enumerate().collect();
        added.sort_by_key(|(_, album)| std::cmp::Reverse(album.sort_keys.added_seconds));
        let mut played: Vec<(usize, &Album, i64)> = albums
            .iter()
            .enumerate()
            .filter_map(|(index, album)| {
                album
                    .sort_keys
                    .last_played_seconds
                    .map(|seconds| (index, album, seconds))
            })
            .collect();
        played.sort_by_key(|(_, _, seconds)| std::cmp::Reverse(*seconds));
        let added: Vec<_> = added
            .into_iter()
            .take(MAXIMUM_SHELF_ALBUMS)
            .map(card)
            .collect();
        let played: Vec<_> = played
            .into_iter()
            .take(MAXIMUM_SHELF_ALBUMS)
            .map(|(index, album, _)| card((index, album)))
            .collect();
        let state = window.global::<AppState>();
        state.set_recently_added_albums(album_card_model(&self.artwork, added));
        state.set_recently_played_albums(album_card_model(&self.artwork, played));
    }
}

/// `paths` without those inside another of them: synchronizing a folder covers its content.
fn outermost_paths(paths: std::collections::BTreeSet<PathBuf>) -> Vec<PathBuf> {
    // Sorted, a folder comes right before what it holds.
    let mut outermost: Vec<PathBuf> = Vec::new();
    for path in paths {
        if !outermost
            .last()
            .is_some_and(|folder| path.starts_with(folder))
        {
            outermost.push(path);
        }
    }
    outermost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_inside_another_changed_path_are_left_out() {
        let paths = [
            "/m/A/01.flac",
            "/m/A",
            "/m/A 2/01.flac",
            "/m/B/x/02.flac",
            "/m/B/x",
        ]
        .map(PathBuf::from)
        .into_iter()
        .collect();
        assert_eq!(
            outermost_paths(paths),
            ["/m/A", "/m/A 2/01.flac", "/m/B/x"].map(PathBuf::from)
        );
    }
}
