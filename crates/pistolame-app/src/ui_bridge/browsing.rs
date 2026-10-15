//! Album grid, artists, the "Morceaux" table and the album page.

use super::*;

impl UiBridge {
    /// Runs one quick library change on the UI thread; `None` (and a message) on failure.
    pub(super) fn with_library<T>(
        &self,
        change: impl FnOnce(&mut LibraryStore) -> Result<T, pistolame_library::LibraryError>,
    ) -> Option<T> {
        let result = self
            .library_database
            .clone()
            .ok_or_else(|| "aucun dossier pour la bibliothèque".to_owned())
            .and_then(|database| LibraryStore::open(&database).map_err(|error| error.to_string()))
            .and_then(|mut store| change(&mut store).map_err(|error| error.to_string()));
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.message(format!("Bibliothèque non enregistrée : {error}"));
                None
            }
        }
    }

    /// Redraws everything that shows album `index`: grid, album page, LCD and current track.
    pub(super) fn refresh_album(&self, index: usize) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        self.update_grid(state.get_query().as_str());
        if self.detail_album.get() == index {
            self.show_album(index);
        }
        let albums = self.albums.borrow();
        if let (Some((album_index, _)), Some(album)) =
            (self.selected_track.get(), albums.get(index))
            && album_index == index
        {
            state.set_lcd_artwork(album.artwork.clone());
            state.set_current_album(album.title.as_str().into());
            state.set_current_artist(album.artist.as_str().into());
        }
    }

    /// Shows the albums matching `query` (all of them when it is empty) and, while searching,
    /// the matching tracks under them.
    pub(super) fn update_grid(&self, query: &str) {
        // The rows are rebuilt: their positions may change.
        self.clear_selection();
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let words = crate::search::query_words(query);
        let artist_filter = state.get_artist_filter().to_string();
        let is_artist_page = !artist_filter.is_empty() && words.is_empty();
        let albums = self.albums.borrow();
        let labels = distinguishing_labels(&albums);
        let mut cards: Vec<(Option<u32>, AlbumCard)> = albums
            .iter()
            .enumerate()
            .filter(|(_, album)| artist_filter.is_empty() || album.artists.contains(&artist_filter))
            .filter(|(_, album)| {
                let year = album_year(album)
                    .map(|year| year.to_string())
                    .unwrap_or_default();
                let genre = album
                    .tracks
                    .iter()
                    .find_map(|track| track.genre.clone())
                    .unwrap_or_default();
                crate::search::matches(&words, &[&album.title, &album.artist, &year, &genre])
            })
            .map(|(index, album)| {
                (
                    album_year(album),
                    album_card(index, album, labels.get(index).and_then(Option::as_deref)),
                )
            })
            .collect();
        if is_artist_page {
            // An artist's page lists the newest albums first, undated ones last.
            cards.sort_by_key(|(year, _)| std::cmp::Reverse(*year));
        }
        let cards: Vec<AlbumCard> = cards.into_iter().map(|(_, card)| card).collect();
        let current_path = self.current_path();
        let mut results = Vec::new();
        let mut rows = Vec::new();
        if is_artist_page {
            // "Les plus écoutés": the artist's tracks played most often (local play counts).
            let mut played: Vec<(&MusicFile, &Album)> = albums
                .iter()
                .filter(|album| album.artists.contains(&artist_filter))
                .flat_map(|album| album.tracks.iter().map(move |track| (track, album)))
                .filter(|(track, _)| track.play_count > 0)
                .collect();
            played.sort_by(|left, right| {
                (right.0.play_count, right.0.last_played_seconds)
                    .cmp(&(left.0.play_count, left.0.last_played_seconds))
            });
            for (file, album) in played.into_iter().take(ARTIST_TOP_TRACK_COUNT) {
                rows.push(self.search_row(rows.len(), file, album, current_path.as_ref()));
                results.push(file.path.clone());
            }
        }
        if !words.is_empty() {
            for album in albums.iter() {
                for file in &album.tracks {
                    if rows.len() >= MAXIMUM_SEARCH_TRACKS {
                        break;
                    }
                    if crate::search::matches(&words, &[&file.title, &album.artist, &album.title]) {
                        rows.push(self.search_row(rows.len(), file, album, current_path.as_ref()));
                        results.push(file.path.clone());
                    }
                }
            }
        }
        drop(albums);
        *self.search_results.borrow_mut() = results;
        state.set_no_results_text(if words.is_empty() {
            "Ajoutez un dossier ou des fichiers audio pour retrouver vos albums.".into()
        } else {
            format!("Rien trouvé pour « {query} ».").into()
        });
        state.set_search_tracks(ModelRc::new(VecModel::from(rows)));
        state.set_albums(ModelRc::new(VecModel::from(cards)));
        if is_artist_page {
            self.show_artist_header(&state, &artist_filter);
        }
    }

    /// One row of the tracks listed under the grid (search results, "Les plus écoutés").
    fn search_row(
        &self,
        row: usize,
        file: &MusicFile,
        album: &Album,
        current_path: Option<&PathBuf>,
    ) -> PlaylistTrackRow {
        PlaylistTrackRow {
            position: row as i32,
            number: row as i32 + 1,
            title: file.title.as_str().into(),
            subtitle: format!("{} — {}", album.artist, album.title).into(),
            format_label: presentation::audio_format(file.bits_per_sample, file.sample_rate_hz)
                .into(),
            duration_label: presentation::clock(file.duration_seconds).into(),
            is_current: current_path == Some(&file.path),
            is_favorite: self.favorites.borrow().contains(&file.path),
            is_selected: false,
        }
    }

    /// The header of an artist's page: photo (or newest cover) and counts.
    pub(super) fn show_artist_header(&self, state: &AppState<'_>, artist: &str) {
        let albums = self.albums.borrow();
        let artist_albums: Vec<&Album> = albums
            .iter()
            .filter(|album| album.artists.iter().any(|name| name == artist))
            .collect();
        let track_count: usize = artist_albums.iter().map(|album| album.tracks.len()).sum();
        let photo = self
            .artist_photos
            .borrow()
            .get(artist)
            .cloned()
            .or_else(|| {
                artist_albums
                    .iter()
                    .max_by_key(|album| album_year(album))
                    .map(|album| album.artwork.clone())
            });
        state.set_artist_page_photo(photo.unwrap_or_default());
        state.set_artist_page_detail(
            format!(
                "{} · {}",
                presentation::count_label(artist_albums.len(), "album", "albums"),
                presentation::count_label(track_count, "morceau", "morceaux"),
            )
            .into(),
        );
    }

    /// "Tout lire" / "Aléatoire" on an artist's page: every track of the artist's albums, the
    /// albums from the newest, as on the page.
    pub(super) fn play_artist(&self, is_shuffled: bool) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let artist = state.get_artist_filter().to_string();
        let paths: Vec<PathBuf> = {
            let albums = self.albums.borrow();
            let mut artist_albums: Vec<&Album> = albums
                .iter()
                .filter(|album| album.artists.contains(&artist))
                .collect();
            artist_albums.sort_by_key(|album| std::cmp::Reverse(album_year(album)));
            artist_albums
                .iter()
                .flat_map(|album| album.tracks.iter().map(|track| track.path.clone()))
                .collect()
        };
        if paths.is_empty() {
            return;
        }
        state.set_is_shuffle_on(is_shuffled);
        let start = if is_shuffled {
            self.random_track(paths.len())
        } else {
            0
        };
        self.play_context(paths, start, artist);
    }

    /// One card per album artist, in natural order, with their newest album's cover.
    pub(super) fn show_artists(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let cards = artist_cards(&self.albums.borrow(), &self.artist_photos.borrow());
        window
            .global::<AppState>()
            .set_artists(ModelRc::new(VecModel::from(cards)));
    }

    /// Fills the "Morceaux" table with every track, in the chosen column order.
    pub(super) fn show_library_tracks(&self) {
        // The rows are rebuilt: their positions may change.
        self.clear_selection();
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let (column, is_descending) = self.track_sort.get();
        let is_favorites_only = window.global::<AppState>().get_is_favorites_only();
        let favorites = self.favorites.borrow();
        let albums = self.albums.borrow();
        let mut tracks: Vec<(crate::track_sort::TrackSortKeys, &Album, &MusicFile)> = albums
            .iter()
            .flat_map(|album| album.tracks.iter().map(move |track| (album, track)))
            .filter(|(_, track)| !is_favorites_only || favorites.contains(&track.path))
            .map(|(album, track)| {
                let keys = crate::track_sort::TrackSortKeys {
                    title: track.title.clone(),
                    artist: track.artist.clone(),
                    album: album.title.clone(),
                    year: track.year,
                    sample_rate_hz: track.sample_rate_hz,
                    bits_per_sample: track.bits_per_sample,
                    duration_seconds: track.duration_seconds,
                    disc_number: track.disc_number,
                    track_number: track.track_number,
                };
                (keys, album, track)
            })
            .collect();
        tracks.sort_by(|left, right| {
            crate::track_sort::compare_tracks(column, is_descending, &left.0, &right.0)
        });
        let rows: Vec<LibraryTrackRow> = tracks
            .iter()
            .map(|(keys, _, track)| LibraryTrackRow {
                title: keys.title.as_str().into(),
                artist: keys.artist.as_str().into(),
                album: keys.album.as_str().into(),
                year: track
                    .year
                    .map(|year| year.to_string())
                    .unwrap_or_default()
                    .into(),
                format_label: presentation::audio_format(
                    track.bits_per_sample,
                    track.sample_rate_hz,
                )
                .into(),
                duration_label: presentation::clock(track.duration_seconds).into(),
                path: track.path.to_string_lossy().as_ref().into(),
                is_favorite: favorites.contains(&track.path),
                is_selected: false,
            })
            .collect();
        *self.library_tracks.borrow_mut() = tracks
            .iter()
            .map(|(_, _, track)| track.path.clone())
            .collect();
        let state = window.global::<AppState>();
        state.set_track_sort_column(
            crate::track_sort::TrackColumn::ALL
                .iter()
                .position(|candidate| *candidate == column)
                .unwrap_or(0) as i32,
        );
        state.set_is_track_sort_descending(is_descending);
        state.set_library_tracks(ModelRc::new(VecModel::from(rows)));
    }

    /// Shows the albums of artist `index` (in the artists list) in the grid.
    pub(super) fn open_artist(&self, index: i32) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let Some(artist) = usize::try_from(index)
            .ok()
            .and_then(|index| slint::Model::row_data(&state.get_artists(), index))
        else {
            return;
        };
        self.open_artist_named(&artist.name);
    }

    /// The file playing now, if any.
    pub(super) fn current_path(&self) -> Option<PathBuf> {
        self.selected_track.get().and_then(|(album, track)| {
            self.albums
                .borrow()
                .get(album)
                .and_then(|album| album.tracks.get(track))
                .map(|track| track.path.clone())
        })
    }

    /// The grid order chosen in the settings.
    pub(super) fn album_sort(&self) -> crate::album_sort::AlbumSort {
        self.settings
            .borrow()
            .album_sort
            .as_deref()
            .and_then(crate::album_sort::AlbumSort::from_key)
            .unwrap_or_default()
    }

    /// Changes the grid order, remembers it and re-sorts the albums in place.
    pub(super) fn set_album_sort(&self, sort: crate::album_sort::AlbumSort) {
        self.settings.borrow_mut().album_sort = Some(sort.key().to_owned());
        self.save_settings();
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
        self.albums.borrow_mut().sort_by(|left, right| {
            crate::album_sort::compare_albums(sort, &left.sort_keys, &right.sort_keys)
        });
        if let Some(path) = selected_path {
            self.selected_track.set(self.track_location(&path));
        }
        if let Some(index) = detail_key.and_then(|key| {
            self.albums
                .borrow()
                .iter()
                .position(|album| album.key == key)
        }) {
            self.detail_album.set(index);
        }
        if let Some(window) = self.window.upgrade() {
            let state = window.global::<AppState>();
            state.set_album_sort_index(
                crate::album_sort::AlbumSort::ALL
                    .iter()
                    .position(|candidate| *candidate == sort)
                    .unwrap_or(0) as i32,
            );
            state.set_grid_scroll_y(0.0);
            self.update_grid(state.get_query().as_str());
            let (album, track) = self
                .selected_track
                .get()
                .map_or((-1, -1), |(album, track)| (album as i32, track as i32));
            state.set_playing_album_index(album);
            state.set_playing_track_index(track);
            state.set_detail_album_index(self.detail_album.get() as i32);
        }
    }

    pub(super) fn show_album(&self, index: usize) {
        // The rows are rebuilt: their positions may change.
        self.clear_selection();
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let albums = self.albums.borrow();
        let Some(album) = albums.get(index) else {
            return;
        };
        self.detail_album.set(index);
        let state = window.global::<AppState>();
        state.set_detail_album_index(index as i32);
        let artist_links: Vec<SharedString> = if album.artists.is_empty() {
            vec![album.artist.as_str().into()]
        } else {
            album
                .artists
                .iter()
                .map(|artist| artist.as_str().into())
                .collect()
        };
        state.set_detail_artists(ModelRc::new(VecModel::from(artist_links)));
        state.set_detail_resume_label(self.album_resume_label(index).into());
        state.set_detail(DetailInfo {
            title: album.title.as_str().into(),
            artist: album.artist.as_str().into(),
            artwork: album.artwork.clone(),
            meta_line: match album_year(album) {
                Some(year) => format!("{year} · {} morceaux · {}", album.tracks.len(), album_codecs(album)),
                None => format!("{} morceaux · {}", album.tracks.len(), album_codecs(album)),
            }
            .into(),
            footer_line: format!("{} morceaux · {}", album.tracks.len(), presentation::clock(album.tracks.iter().map(|track| track.duration_seconds).sum())).into(),
            verdict_label: "Non analysé".into(),
            summary: "L’analyse d’authenticité n’a pas été effectuée. Aucun verdict sur la résolution réelle de cet album.".into(),
            cells: ModelRc::new(VecModel::from(["Résolution effective", "Coupure spectrale", "Plage dynamique", "Écrêtage", "Contrôle MD5"].map(|label| crate::DataCell { label: label.into(), value: "Non analysé".into() }).to_vec())),
            ..Default::default()
        });
        let is_multi_disc = album
            .tracks
            .iter()
            .any(|track| track.disc_number != album.tracks[0].disc_number);
        let mut rows: Vec<TrackRowData> = Vec::new();
        for (index, track) in album.tracks.iter().enumerate() {
            let starts_disc =
                index == 0 || album.tracks[index - 1].disc_number != track.disc_number;
            if is_multi_disc && starts_disc {
                rows.push(TrackRowData {
                    title: format!("Disque {}", track.disc_number.max(1)).into(),
                    is_disc_header: true,
                    track_index: -1,
                    ..Default::default()
                });
            }
            rows.push(TrackRowData {
                number: if track.track_number > 0 {
                    track.track_number as i32
                } else {
                    index as i32 + 1
                },
                title: track.title.as_str().into(),
                format_label: presentation::audio_format(
                    track.bits_per_sample,
                    track.sample_rate_hz,
                )
                .into(),
                duration_label: presentation::clock(track.duration_seconds).into(),
                is_disc_header: false,
                track_index: index as i32,
                is_favorite: self.favorites.borrow().contains(&track.path),
                is_selected: false,
            });
        }
        state.set_detail_tracks(ModelRc::new(VecModel::from(rows)));
        drop(albums);
        self.update_detail_signal();
    }

    pub(super) fn inactive_path() -> ModelRc<PathStep> {
        ModelRc::new(VecModel::from(
            ["Source", "Décodage", "Volume", "Sortie", "DAC"]
                .map(|label| PathStep {
                    label: label.into(),
                    value: "Aucune lecture".into(),
                    is_transparent: false,
                })
                .to_vec(),
        ))
    }

    pub(super) fn update_detail_signal(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let is_current = self
            .selected_track
            .get()
            .is_some_and(|(album, _)| album == self.detail_album.get());
        state.set_detail_is_bit_perfect(is_current && state.get_is_bit_perfect());
        state.set_detail_badge_label(if is_current {
            state.get_badge_label()
        } else {
            "Aucune lecture".into()
        });
        state.set_detail_path_steps(if is_current {
            state.get_path_steps()
        } else {
            Self::inactive_path()
        });
    }
}
