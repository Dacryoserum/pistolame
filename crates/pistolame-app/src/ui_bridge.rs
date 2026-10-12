//! The UI owns this bridge; only commands, events and atomic counters cross into audio threads.
use crate::{
    AlbumCard, AppState, AppWindow, DetailInfo, PathStep, PlaylistItem, PlaylistTrackRow,
    TrackRowData, ViewKind,
};
use crate::{presentation, settings::AppSettings};
use crossbeam_channel::{Receiver, Sender, unbounded};
use pistolame_core::*;
use pistolame_library::{
    AlbumKey, AlbumTextOverride, LibrarySnapshot, LibraryStore, MusicFile, Playlist,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

struct Album {
    key: AlbumKey,
    /// What the app shows: the user's edit, or the tags.
    title: String,
    artist: String,
    /// What the tags say, to tell an edit from the original.
    tag_title: String,
    tag_artist: String,
    tracks: Vec<MusicFile>,
    artwork: slint::Image,
    /// Whether the user chose this cover (stored in the library, files untouched).
    has_custom_artwork: bool,
}

/// A cover change waiting for "Enregistrer" in the album editor.
enum PendingArtwork {
    Unchanged,
    Chosen { bytes: Vec<u8>, image: slint::Image },
    Restored,
}

/// The album being edited in the "Modifier l'album" sheet.
struct AlbumEditDraft {
    index: usize,
    pending_artwork: PendingArtwork,
}

/// What the library worker sends back to the UI thread.
struct LibraryUpdate {
    /// The whole library, when it changed; `None` when nothing needs to be redrawn.
    snapshot: Option<LibrarySnapshot>,
    /// Files or folders that could not be read, as display text.
    errors: Vec<String>,
    /// The last message of a synchronization: the worker is done.
    is_final: bool,
}

pub(crate) struct UiBridge {
    window: slint::Weak<AppWindow>,
    engine: EngineHandle,
    settings: RefCell<AppSettings>,
    /// The SQLite file of the persistent library; `None` when no app folder exists.
    library_database: Option<PathBuf>,
    devices: RefCell<Vec<OutputDeviceInfo>>,
    albums: RefCell<Vec<Album>>,
    imports: Receiver<LibraryUpdate>,
    import_sender: Sender<LibraryUpdate>,
    is_importing: Cell<bool>,
    album_edit: RefCell<Option<AlbumEditDraft>>,
    /// The user's playlists, as stored in the library.
    playlists: RefCell<Vec<Playlist>>,
    /// What plays next: the tracks of an album or a playlist, by path so a library rescan
    /// cannot shift them, and the position of the current one.
    queue: RefCell<Vec<PathBuf>>,
    queue_position: Cell<usize>,
    /// Tracks waiting in the "Ajouter à une playlist" sheet.
    pending_playlist_tracks: RefCell<Vec<PathBuf>>,
    /// The latest signal-path assessment, to redraw the path when the volume changes.
    last_assessment: RefCell<Option<SignalPathAssessment>>,
    state: Cell<PlaybackState>,
    track_id: Cell<Option<TrackId>>,
    sample_rate_hz: Cell<u32>,
    total_frames: Cell<u64>,
    underrun_baseline: Cell<u64>,
    selected_track: Cell<Option<(usize, usize)>>,
    detail_album: Cell<usize>,
    animation_started: std::time::Instant,
    stream_end_baseline: Cell<u64>,
    should_advance: Cell<bool>,
    shuffle_seed: Cell<u64>,
}

impl UiBridge {
    pub(crate) fn new(window: &AppWindow) -> anyhow::Result<Rc<Self>> {
        Self::with_library_database(window, crate::settings::library_database_path())
    }

    /// Like `new`, with an explicit library database (`None`: no persistence available).
    pub(crate) fn with_library_database(
        window: &AppWindow,
        library_database: Option<PathBuf>,
    ) -> anyhow::Result<Rc<Self>> {
        let settings = AppSettings::load();
        let engine = EngineHandle::start(
            pistolame_output::platform_default_output(),
            DecoderRegistry::default(),
            settings.engine_start_options(),
        )?;
        let (sender, receiver) = unbounded();
        let bridge = Rc::new(Self {
            window: window.as_weak(),
            engine,
            settings: RefCell::new(settings),
            library_database,
            devices: RefCell::new(Vec::new()),
            albums: RefCell::new(Vec::new()),
            imports: receiver,
            import_sender: sender,
            is_importing: Cell::new(false),
            album_edit: RefCell::new(None),
            last_assessment: RefCell::new(None),
            playlists: RefCell::new(Vec::new()),
            queue: RefCell::new(Vec::new()),
            queue_position: Cell::new(0),
            pending_playlist_tracks: RefCell::new(Vec::new()),
            state: Cell::new(PlaybackState::Stopped),
            track_id: Cell::new(None),
            sample_rate_hz: Cell::new(0),
            total_frames: Cell::new(0),
            underrun_baseline: Cell::new(0),
            selected_track: Cell::new(None),
            detail_album: Cell::new(0),
            animation_started: std::time::Instant::now(),
            stream_end_baseline: Cell::new(0),
            should_advance: Cell::new(false),
            shuffle_seed: Cell::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(1, |duration| duration.as_nanos() as u64),
            ),
        });
        let state = window.global::<AppState>();
        state.set_signal_title("Aucune lecture".into());
        state.set_signal_description(
            "Choisissez un morceau pour inspecter le chemin du signal.".into(),
        );
        state.set_equalizer_levels(ModelRc::new(VecModel::from(vec![0.3, 0.6, 0.4])));
        state.set_path_steps(Self::inactive_path());
        state.set_lcd_title("Pistolame".into());
        state.set_lcd_subtitle("Choisissez un morceau".into());
        state.set_badge_label("À l’arrêt".into());
        state.set_elapsed_label("0:00".into());
        state.set_remaining_label("0:00".into());
        state.set_playing_album_index(-1);
        state.set_playing_track_index(-1);
        state.set_no_results_text(
            "Ajoutez un dossier ou des fichiers FLAC pour retrouver vos albums.".into(),
        );
        state.set_transmission_text("Transmission : aucune lecture".into());
        state.set_integrity_text("Fichier : aucune lecture".into());
        state
            .set_exclusive(bridge.settings.borrow().output_mode == OutputModePreference::Exclusive);
        Ok(bridge)
    }

    pub(crate) fn connect(self: &Rc<Self>, window: &AppWindow) {
        self.show_volume();
        let state = window.global::<AppState>();
        let handle = self.clone();
        state.on_import_files(move || {
            if !handle.is_importing.get()
                && let Some(paths) = rfd::FileDialog::new()
                    .add_filter("FLAC", &["flac"])
                    .pick_files()
            {
                handle.synchronize_library(paths);
            }
        });
        let handle = self.clone();
        state.on_import_folder(move || {
            if !handle.is_importing.get()
                && let Some(path) = rfd::FileDialog::new().pick_folder()
            {
                handle.synchronize_library(vec![path]);
            }
        });
        let handle = self.clone();
        state.on_search_changed(move |query| handle.update_grid(query.as_str()));
        let handle = self.clone();
        state.on_volume_changed(move |decibels, is_muted| {
            handle.set_shared_mode_volume(SharedModeVolume {
                is_muted,
                ..SharedModeVolume::at_decibels(decibels)
            });
        });
        self.connect_playlists(&state);
        let handle = self.clone();
        state.on_begin_album_edit(move |index| {
            if let Ok(index) = usize::try_from(index) {
                handle.begin_album_edit(index);
            }
        });
        let handle = self.clone();
        state.on_editor_choose_artwork(move || handle.editor_choose_artwork());
        let handle = self.clone();
        state.on_editor_reset_artwork(move || handle.editor_reset_artwork());
        let handle = self.clone();
        state.on_save_album_edit(move || handle.save_album_edit());
        let handle = self.clone();
        state.on_cancel_album_edit(move || handle.close_album_editor());
        let handle = self.clone();
        state.on_open_current_album(move || {
            if let Some((album, _)) = handle.selected_track.get() {
                handle.show_album(album);
                if let Some(window) = handle.window.upgrade() {
                    let state = window.global::<AppState>();
                    state.set_detail_scroll_y(0.0);
                    state.set_view(crate::ViewKind::Detail);
                }
            }
        });
        let handle = self.clone();
        state.on_open_album(move |album| {
            if let Ok(album) = usize::try_from(album) {
                handle.show_album(album);
            }
        });
        let handle = self.clone();
        state.on_play_track(move |album, track| {
            if let (Ok(album), Ok(track)) = (usize::try_from(album), usize::try_from(track)) {
                handle.play_track(album, track);
            }
        });
        let handle = self.clone();
        state.on_play_detail_album(move || handle.play_track(handle.detail_album.get(), 0));
        let handle = self.clone();
        state.on_toggle_play(move || handle.toggle_play());
        let handle = self.clone();
        state.on_stop(move || {
            handle.should_advance.set(false);
            handle.engine.send(EngineCommand::Stop);
        });
        let weak = window.as_weak();
        state.on_toggle_theme(move || {
            if let Some(window) = weak.upgrade() {
                let state = window.global::<AppState>();
                state.set_is_dark(!state.get_is_dark());
            }
        });
        let handle = self.clone();
        state.on_shuffle_detail_album(move || {
            if let Some(window) = handle.window.upgrade() {
                window.global::<AppState>().set_is_shuffle_on(true);
            }
            let album = handle.detail_album.get();
            let count = handle
                .albums
                .borrow()
                .get(album)
                .map_or(0, |entry| entry.tracks.len());
            if count > 0 {
                handle.play_track(album, handle.random_track(count));
            }
        });
        let handle = self.clone();
        state.on_previous_track(move || handle.adjacent_track(false));
        let handle = self.clone();
        state.on_next_track(move || handle.adjacent_track(true));
        let handle = self.clone();
        state.on_seek(move |fraction| {
            if fraction.is_finite() && handle.sample_rate_hz.get() > 0 {
                let seconds =
                    handle.total_frames.get() as f64 / f64::from(handle.sample_rate_hz.get());
                handle.engine.send(EngineCommand::SeekTo {
                    position_in_track: Duration::from_secs_f64(
                        seconds * f64::from(fraction.clamp(0.0, 1.0)),
                    ),
                });
            }
        });
        let handle = self.clone();
        state.on_refresh_devices(move || handle.engine.send(EngineCommand::RefreshDeviceList));
        let handle = self.clone();
        state.on_select_device(move |index| handle.select_device(index));
        let handle = self.clone();
        state.on_set_exclusive(move |enabled| handle.set_exclusive(enabled));
        let handle = self.clone();
        state.on_dismiss_notice(move || {
            handle.settings.borrow_mut().exclusive_mode_notice_seen = true;
            handle.save_settings();
            if let Some(window) = handle.window.upgrade() {
                window.global::<AppState>().set_show_exclusive_notice(false);
            }
        });
    }

    fn save_settings(&self) {
        if let Err(error) = self.settings.borrow().save() {
            self.message(format!("Impossible d’enregistrer les réglages : {error}"));
        }
    }

    /// Applies, remembers and shows the shared-mode volume.
    fn set_shared_mode_volume(&self, volume: SharedModeVolume) {
        self.engine
            .send(EngineCommand::SetSharedModeVolume { volume });
        self.settings.borrow_mut().set_shared_mode_volume(volume);
        self.save_settings();
        self.show_volume();
    }

    /// Shows the remembered volume on the slider, the output page and the signal path.
    fn show_volume(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let volume = self.settings.borrow().shared_mode_volume();
        state.set_volume_decibels(volume.decibels);
        state.set_is_volume_muted(volume.is_muted);
        state.set_volume_label(presentation::volume_label(volume).into());
        let is_exclusive_output = self
            .last_assessment
            .borrow()
            .as_ref()
            .map_or(state.get_exclusive(), |assessment| {
                assessment.negotiated.access_mode == DeviceAccessMode::Exclusive
            });
        state.set_volume_description(
            presentation::volume_description(volume, is_exclusive_output).into(),
        );
        if let Some(assessment) = self.last_assessment.borrow().as_ref() {
            state.set_path_steps(Self::path_steps(assessment, volume));
            self.update_detail_signal();
        }
    }

    fn path_steps(
        assessment: &SignalPathAssessment,
        volume: SharedModeVolume,
    ) -> ModelRc<PathStep> {
        ModelRc::new(VecModel::from(
            presentation::signal_steps(assessment, volume)
                .into_iter()
                .map(|(label, value, is_transparent)| PathStep {
                    label: label.into(),
                    value: value.into(),
                    is_transparent,
                })
                .collect::<Vec<_>>(),
        ))
    }

    fn set_exclusive(&self, enabled: bool) {
        let preference = if enabled {
            OutputModePreference::Exclusive
        } else {
            OutputModePreference::Shared
        };
        self.settings.borrow_mut().output_mode = preference;
        self.engine
            .send(EngineCommand::SetOutputModePreference { preference });
        if let Some(window) = self.window.upgrade() {
            let state = window.global::<AppState>();
            state.set_exclusive(enabled);
            let show_notice = enabled && !self.settings.borrow().exclusive_mode_notice_seen;
            state.set_show_exclusive_notice(show_notice);
            if show_notice {
                state.set_is_popover_open(true);
            }
        }
        self.save_settings();
    }

    fn select_device(&self, index: i32) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        if let Some(device) = self.devices.borrow().get(index) {
            self.engine.send(EngineCommand::SelectOutputDevice {
                device_id: device.id.clone(),
            });
            let mut settings = self.settings.borrow_mut();
            settings.output_device_id = Some(device.id.clone());
            settings.output_device_name = Some(device.display_name.clone());
        }
        self.save_settings();
    }

    fn message(&self, text: String) {
        if let Some(window) = self.window.upgrade() {
            window.global::<AppState>().set_message(text.into());
        }
    }

    /// Remembers `new_roots` (none at start-up), shows the stored library at once, then
    /// rescans every root in the background (docs/plans/persistent-library.md).
    pub(crate) fn synchronize_library(&self, new_roots: Vec<PathBuf>) {
        let Some(database) = self.library_database.clone() else {
            self.message("Aucun dossier pour enregistrer la bibliothèque.".into());
            return;
        };
        self.is_importing.set(true);
        if let Some(window) = self.window.upgrade() {
            window.global::<AppState>().set_import_status(
                if new_roots.is_empty() {
                    "Mise à jour de la bibliothèque…"
                } else {
                    "Lecture des fichiers…"
                }
                .into(),
            );
        }
        let sender = self.import_sender.clone();
        let result = std::thread::Builder::new()
            .name("pistolame-library".into())
            .spawn(move || {
                let outcome = synchronize_in_background(&database, &new_roots, &sender);
                if let Err(error) = outcome {
                    let _ = sender.send(LibraryUpdate {
                        snapshot: None,
                        errors: vec![error.to_string()],
                        is_final: true,
                    });
                }
            });
        if let Err(error) = result {
            self.is_importing.set(false);
            self.message(format!("Impossible de démarrer l’import : {error}"));
        }
    }

    fn apply_library_update(&self, update: LibraryUpdate) {
        if update.is_final {
            self.is_importing.set(false);
        }
        if let Some(snapshot) = update.snapshot {
            self.replace_albums(snapshot);
        }
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
        if let Some(first) = update.errors.first() {
            self.message(format!(
                "{} problème(s) d’import. {first}",
                update.errors.len()
            ));
        }
    }

    /// Rebuilds the albums from a library snapshot, keeping the selected track and the open
    /// album when they still exist.
    fn replace_albums(&self, snapshot: LibrarySnapshot) {
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
        let albums = albums_from_snapshot(snapshot);
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
        self.show_playlists();
    }

    /// Opens the "Modifier l'album" sheet for album `index`.
    fn begin_album_edit(&self, index: usize) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let albums = self.albums.borrow();
        let Some(album) = albums.get(index) else {
            return;
        };
        let state = window.global::<AppState>();
        state.set_editor_title(album.title.as_str().into());
        state.set_editor_artist(album.artist.as_str().into());
        state.set_editor_artwork(album.artwork.clone());
        state.set_editor_has_custom_artwork(album.has_custom_artwork);
        state.set_is_album_editor_open(true);
        *self.album_edit.borrow_mut() = Some(AlbumEditDraft {
            index,
            pending_artwork: PendingArtwork::Unchanged,
        });
    }

    /// Lets the user pick a JPEG or PNG; applied only on "Enregistrer".
    fn editor_choose_artwork(&self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Image", &["jpg", "jpeg", "png"])
            .pick_file()
        else {
            return;
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.message(format!("Impossible de lire l’image : {error}"));
                return;
            }
        };
        if bytes.len() as u64 > pistolame_library::MAX_COVER_BYTES {
            self.message("Cette image dépasse 16 Mo.".into());
            return;
        }
        let Ok(image) = slint::Image::load_from_data(&bytes, None) else {
            self.message("Cette image n’est ni un JPEG ni un PNG lisible.".into());
            return;
        };
        if let Some(window) = self.window.upgrade() {
            let state = window.global::<AppState>();
            state.set_editor_artwork(image.clone());
            state.set_editor_has_custom_artwork(true);
        }
        if let Some(draft) = self.album_edit.borrow_mut().as_mut() {
            draft.pending_artwork = PendingArtwork::Chosen { bytes, image };
        }
    }

    /// Shows the files' own cover again; applied only on "Enregistrer".
    fn editor_reset_artwork(&self) {
        let Some(index) = self.album_edit.borrow().as_ref().map(|draft| draft.index) else {
            return;
        };
        let original = self
            .albums
            .borrow()
            .get(index)
            .map(|album| decode_artwork(original_artwork(&album.tracks)))
            .unwrap_or_default();
        if let Some(window) = self.window.upgrade() {
            let state = window.global::<AppState>();
            state.set_editor_artwork(original);
            state.set_editor_has_custom_artwork(false);
        }
        if let Some(draft) = self.album_edit.borrow_mut().as_mut() {
            draft.pending_artwork = PendingArtwork::Restored;
        }
    }

    /// Stores the edits in the library, then shows them everywhere.
    fn save_album_edit(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let Some(draft) = self.album_edit.borrow_mut().take() else {
            self.close_album_editor();
            return;
        };
        let state = window.global::<AppState>();
        let Some((key, tag_title, tag_artist)) =
            self.albums.borrow().get(draft.index).map(|album| {
                (
                    album.key.clone(),
                    album.tag_title.clone(),
                    album.tag_artist.clone(),
                )
            })
        else {
            self.close_album_editor();
            return;
        };
        let text = AlbumTextOverride {
            title: edited_value(state.get_editor_title().as_str(), &tag_title),
            artist: edited_value(state.get_editor_artist().as_str(), &tag_artist),
        };
        let saved = self.with_library(|store| {
            store.set_album_text(&key, &text)?;
            match &draft.pending_artwork {
                PendingArtwork::Unchanged => Ok(()),
                PendingArtwork::Chosen { bytes, .. } => store.set_album_artwork(&key, bytes),
                PendingArtwork::Restored => store.clear_album_artwork(&key),
            }
        });
        if saved.is_none() {
            return;
        }
        if let Some(album) = self.albums.borrow_mut().get_mut(draft.index) {
            album.title = text.title.clone().unwrap_or(tag_title);
            album.artist = text.artist.clone().unwrap_or(tag_artist);
            match draft.pending_artwork {
                PendingArtwork::Unchanged => {}
                PendingArtwork::Chosen { image, .. } => {
                    album.artwork = image;
                    album.has_custom_artwork = true;
                }
                PendingArtwork::Restored => {
                    album.artwork = decode_artwork(original_artwork(&album.tracks));
                    album.has_custom_artwork = false;
                }
            }
        }
        self.close_album_editor();
        self.refresh_album(draft.index);
    }

    fn close_album_editor(&self) {
        self.album_edit.borrow_mut().take();
        if let Some(window) = self.window.upgrade() {
            window.global::<AppState>().set_is_album_editor_open(false);
        }
    }

    /// Runs one quick library change on the UI thread; `None` (and a message) on failure.
    fn with_library<T>(
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
    fn refresh_album(&self, index: usize) {
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

    fn update_grid(&self, query: &str) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let query = query.to_lowercase();
        let cards: Vec<AlbumCard> = self
            .albums
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, album)| {
                format!("{} {}", album.title, album.artist)
                    .to_lowercase()
                    .contains(&query)
            })
            .map(|(index, album)| AlbumCard {
                catalog_index: index as i32,
                title: album.title.as_str().into(),
                artist: album.artist.as_str().into(),
                artwork: album.artwork.clone(),
            })
            .collect();
        window
            .global::<AppState>()
            .set_no_results_text(if query.is_empty() {
                "Ajoutez un dossier ou des fichiers FLAC pour retrouver vos albums.".into()
            } else {
                format!("Aucun album trouvé pour « {query} ».").into()
            });
        window
            .global::<AppState>()
            .set_albums(ModelRc::new(VecModel::from(cards)));
    }

    fn show_album(&self, index: usize) {
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
        state.set_detail(DetailInfo {
            title: album.title.as_str().into(),
            artist: album.artist.as_str().into(),
            artwork: album.artwork.clone(),
            meta_line: format!("{} morceaux · FLAC", album.tracks.len()).into(),
            footer_line: format!("{} morceaux · {}", album.tracks.len(), presentation::clock(album.tracks.iter().map(|track| track.duration_seconds).sum())).into(),
            verdict_label: "Non analysé".into(),
            summary: "L’analyse d’authenticité n’a pas été effectuée. Aucun verdict sur la résolution réelle de cet album.".into(),
            cells: ModelRc::new(VecModel::from(["Résolution effective", "Coupure spectrale", "Plage dynamique", "Écrêtage", "Contrôle MD5"].map(|label| crate::DataCell { label: label.into(), value: "Non analysé".into() }).to_vec())),
            ..Default::default()
        });
        let rows: Vec<TrackRowData> = album
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| TrackRowData {
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
            })
            .collect();
        state.set_detail_tracks(ModelRc::new(VecModel::from(rows)));
        drop(albums);
        self.update_detail_signal();
    }

    fn inactive_path() -> ModelRc<PathStep> {
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

    fn update_detail_signal(&self) {
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

    /// Plays track `track` of album `album`; the album becomes the queue.
    fn play_track(&self, album: usize, track: usize) {
        let paths: Vec<PathBuf> = match self.albums.borrow().get(album) {
            Some(album) => album
                .tracks
                .iter()
                .map(|track| track.path.clone())
                .collect(),
            None => return,
        };
        self.play_queue(paths, track);
    }

    /// Makes `paths` the queue and plays the one at `position`.
    fn play_queue(&self, paths: Vec<PathBuf>, position: usize) {
        if position >= paths.len() {
            return;
        }
        *self.queue.borrow_mut() = paths;
        self.play_queue_position(position);
    }

    fn play_queue_position(&self, position: usize) {
        let Some(path) = self.queue.borrow().get(position).cloned() else {
            return;
        };
        let Some(location) = self.track_location(&path) else {
            self.message(format!("Morceau introuvable : {}", path.display()));
            return;
        };
        self.queue_position.set(position);
        self.selected_track.set(Some(location));
        self.should_advance.set(true);
        self.stream_end_baseline
            .set(self.engine.counters().stream_ends_reached);
        self.message(String::new());
        self.engine.send(EngineCommand::OpenFileAndPlay { path });
        self.refresh_playlist_view();
    }

    /// The (album, track) indices of a file in the current album list.
    fn track_location(&self, path: &std::path::Path) -> Option<(usize, usize)> {
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

    fn random_track(&self, count: usize) -> usize {
        let seed = self
            .shuffle_seed
            .get()
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1);
        self.shuffle_seed.set(seed);
        (seed >> 32) as usize % count.max(1)
    }

    fn adjacent_track(&self, next: bool) {
        let count = self.queue.borrow().len();
        if count == 0 || self.selected_track.get().is_none() {
            return;
        }
        if !next
            && self.engine.counters().frames_rendered_in_track
                > u64::from(self.sample_rate_hz.get()) * 3
        {
            self.engine.send(EngineCommand::SeekTo {
                position_in_track: Duration::ZERO,
            });
            return;
        }
        let position = self.queue_position.get();
        let shuffle = self
            .window
            .upgrade()
            .is_some_and(|window| window.global::<AppState>().get_is_shuffle_on());
        let target = if next && shuffle && count > 1 {
            (position + 1 + self.random_track(count - 1)) % count
        } else if next {
            (position + 1) % count
        } else {
            (position + count - 1) % count
        };
        self.play_queue_position(target);
    }

    fn advance_after_end(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        if crate::transport::can_advance(
            self.queue_position.get(),
            self.queue.borrow().len(),
            state.get_is_repeat_on(),
            state.get_is_shuffle_on(),
        ) {
            self.adjacent_track(true);
        }
    }

    fn connect_playlists(self: &Rc<Self>, state: &AppState<'_>) {
        let handle = self.clone();
        state.on_create_playlist(move || handle.create_playlist(Vec::new(), true));
        let handle = self.clone();
        state.on_open_playlist(move |index| {
            if let Ok(index) = usize::try_from(index) {
                handle.open_playlist(index);
            }
        });
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
        state.on_delete_current_playlist(move || handle.delete_current_playlist());
        let handle = self.clone();
        state.on_begin_add_album_to_playlist(move || handle.begin_add_album_to_playlist());
        let handle = self.clone();
        state.on_begin_add_track_to_playlist(move |track| {
            if let Ok(track) = usize::try_from(track) {
                handle.begin_add_track_to_playlist(track);
            }
        });
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

    fn show_playlists(&self) {
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
            })
            .collect();
        window
            .global::<AppState>()
            .set_playlists(ModelRc::new(VecModel::from(items)));
        self.refresh_playlist_view();
    }

    /// The playlist open in the playlist view, if any.
    fn current_playlist_index(&self) -> Option<usize> {
        let window = self.window.upgrade()?;
        let index = usize::try_from(window.global::<AppState>().get_selected_playlist()).ok()?;
        (index < self.playlists.borrow().len()).then_some(index)
    }

    fn open_playlist(&self, index: usize) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        state.set_selected_playlist(index as i32);
        state.set_is_popover_open(false);
        state.set_view(ViewKind::Playlist);
        self.refresh_playlist_view();
    }

    /// The entries of playlist `index` still in the library, with their position among all
    /// entries (for removal) and their file.
    fn playable_entries(&self, index: usize) -> Vec<(usize, PathBuf, (usize, usize))> {
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

    fn refresh_playlist_view(&self) {
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
                })
            })
            .collect();
        let count = rows.len();
        if let Some(playlist) = self.playlists.borrow().get(index) {
            state.set_playlist_title(playlist.name.as_str().into());
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

    fn play_playlist_row(&self, row: usize) {
        let Some(index) = self.current_playlist_index() else {
            return;
        };
        let paths: Vec<PathBuf> = self
            .playable_entries(index)
            .into_iter()
            .map(|(_, path, _)| path)
            .collect();
        self.play_queue(paths, row);
    }

    /// Creates a playlist named "Nouvelle playlist" (numbered if taken) holding `tracks`. When
    /// `should_open` is set, it opens it with the rename sheet, as Apple Music does.
    fn create_playlist(&self, tracks: Vec<PathBuf>, should_open: bool) {
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
        });
        self.show_playlists();
        if should_open {
            self.open_playlist(self.playlists.borrow().len() - 1);
            self.begin_playlist_edit();
        }
    }

    fn remove_playlist_entry(&self, position: usize) {
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

    fn begin_playlist_edit(&self) {
        let (Some(window), Some(index)) = (self.window.upgrade(), self.current_playlist_index())
        else {
            return;
        };
        let state = window.global::<AppState>();
        if let Some(playlist) = self.playlists.borrow().get(index) {
            state.set_playlist_editor_name(playlist.name.as_str().into());
        }
        state.set_is_playlist_editor_open(true);
    }

    fn save_playlist_edit(&self) {
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
        if !name.is_empty()
            && self
                .with_library(|store| store.rename_playlist(id, &name))
                .is_some()
        {
            if let Some(playlist) = self.playlists.borrow_mut().get_mut(index) {
                playlist.name = name;
            }
            self.show_playlists();
        }
        state.set_is_playlist_editor_open(false);
    }

    fn delete_current_playlist(&self) {
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
        state.set_selected_playlist(-1);
        state.invoke_select_item(2);
        self.show_playlists();
    }

    fn begin_add_album_to_playlist(&self) {
        let albums = self.albums.borrow();
        let Some(album) = albums.get(self.detail_album.get()) else {
            return;
        };
        *self.pending_playlist_tracks.borrow_mut() = album
            .tracks
            .iter()
            .map(|track| track.path.clone())
            .collect();
        let subject = format!(
            "L’album « {} » ({} morceaux)",
            album.title,
            album.tracks.len()
        );
        drop(albums);
        self.open_add_to_playlist(subject);
    }

    fn begin_add_track_to_playlist(&self, track: usize) {
        let albums = self.albums.borrow();
        let Some(file) = albums
            .get(self.detail_album.get())
            .and_then(|album| album.tracks.get(track))
        else {
            return;
        };
        *self.pending_playlist_tracks.borrow_mut() = vec![file.path.clone()];
        let subject = format!("« {} »", file.title);
        drop(albums);
        self.open_add_to_playlist(subject);
    }

    fn open_add_to_playlist(&self, subject: String) {
        if let Some(window) = self.window.upgrade() {
            let state = window.global::<AppState>();
            state.set_add_to_playlist_subject(subject.into());
            state.set_is_add_to_playlist_open(true);
        }
    }

    fn close_add_to_playlist(&self) {
        if let Some(window) = self.window.upgrade() {
            window
                .global::<AppState>()
                .set_is_add_to_playlist_open(false);
        }
    }

    fn add_pending_to_playlist(&self, index: usize) {
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
        if let Some(window) = self.window.upgrade() {
            window.global::<AppState>().set_import_status(
                format!(
                    "{count} morceau{} ajouté{} à « {name} »",
                    if count == 1 { "" } else { "x" },
                    if count == 1 { "" } else { "s" }
                )
                .into(),
            );
        }
    }

    fn toggle_play(&self) {
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

    pub(crate) fn tick(&self) {
        for update in self.imports.try_iter() {
            self.apply_library_update(update);
        }
        for event in self.engine.try_receive_events() {
            self.handle_event(event);
        }
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let counters = self.engine.counters();
        if self.state.get() == PlaybackState::Stopped
            && self.should_advance.get()
            && counters.stream_ends_reached > self.stream_end_baseline.get()
        {
            self.should_advance.set(false);
            self.advance_after_end();
        }
        let time = self.animation_started.elapsed().as_secs_f32();
        let levels = if state.get_is_playing() {
            vec![
                0.5 + 0.5 * (time * 7.0).sin(),
                0.5 + 0.5 * (time * 9.0 + 1.0).sin(),
                0.5 + 0.5 * (time * 6.0 + 2.0).sin(),
            ]
        } else {
            vec![0.3, 0.6, 0.4]
        };
        state.set_equalizer_levels(ModelRc::new(VecModel::from(levels)));
        let rate = u64::from(self.sample_rate_hz.get().max(1));
        state.set_elapsed_label(
            presentation::clock(counters.frames_rendered_in_track / rate).into(),
        );
        let remaining_seconds = self
            .total_frames
            .get()
            .saturating_sub(counters.frames_rendered_in_track)
            / rate;
        state.set_remaining_label(presentation::remaining_clock(remaining_seconds).into());
        state.set_progress(if self.total_frames.get() == 0 {
            0.0
        } else {
            (counters.frames_rendered_in_track as f64 / self.total_frames.get() as f64)
                .clamp(0.0, 1.0) as f32
        });
        state.set_underrun_label(
            format!(
                "Coupures : {}",
                counters
                    .underrun_events
                    .saturating_sub(self.underrun_baseline.get())
            )
            .into(),
        );
    }

    fn handle_event(&self, event: EngineEvent) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        match event {
            EngineEvent::DeviceListChanged { devices, selected } => {
                let index = devices
                    .iter()
                    .position(|device| Some(&device.id) == selected.as_ref());
                let names: Vec<SharedString> = devices
                    .iter()
                    .map(|device| device.display_name.as_str().into())
                    .collect();
                state.set_device_names(ModelRc::new(VecModel::from(names)));
                state.set_selected_device_index(index.map_or(-1, |index| index as i32));
                *self.devices.borrow_mut() = devices;
            }
            EngineEvent::PreferredDeviceUnavailable { .. } => {
                self.message(
                    "La sortie mémorisée est introuvable ; la sortie par défaut est utilisée."
                        .into(),
                );
            }
            EngineEvent::TrackLoaded {
                track_id,
                source,
                path,
            } => {
                self.track_id.set(Some(track_id));
                self.sample_rate_hz.set(source.format.sample_rate_hz);
                self.total_frames.set(source.total_frames.unwrap_or(0));
                self.underrun_baseline
                    .set(self.engine.counters().underrun_events);
                state.set_transmission_ok(false);
                state.set_integrity_ok(false);
                state.set_transmission_text("Transmission : vérification en cours…".into());
                state.set_integrity_text("Fichier : vérification en cours…".into());
                state.set_transmission_digest("".into());
                state.set_integrity_digest("".into());
                state.set_md5_label("En cours".into());
                state.set_has_current_track(false);
                state.set_lcd_artwork(slint::Image::default());
                state.set_lcd_subtitle("".into());
                state.set_playing_album_index(-1);
                state.set_playing_track_index(-1);
                state.set_lcd_title(
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .as_ref()
                        .into(),
                );
                let selection =
                    self.albums
                        .borrow()
                        .iter()
                        .enumerate()
                        .find_map(|(album, entry)| {
                            entry
                                .tracks
                                .iter()
                                .position(|track| track.path == path)
                                .map(|track| (album, track))
                        });
                self.selected_track.set(selection);
                if let Some((album_index, track_index)) = selection
                    && let Some(album) = self.albums.borrow().get(album_index)
                    && let Some(track) = album.tracks.get(track_index)
                {
                    state.set_has_current_track(true);
                    state.set_current_artist(track.artist.as_str().into());
                    state.set_current_album(album.title.as_str().into());
                    state.set_current_format(
                        format!(
                            "FLAC {}",
                            presentation::audio_format(track.bits_per_sample, track.sample_rate_hz)
                        )
                        .into(),
                    );
                    state.set_current_duration(presentation::clock(track.duration_seconds).into());
                    state.set_current_filename(
                        track
                            .path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .as_ref()
                            .into(),
                    );
                    state.set_lcd_title(track.title.as_str().into());
                    state.set_lcd_subtitle(format!("{} — {}", album.artist, album.title).into());
                    state.set_lcd_artwork(album.artwork.clone());
                    state.set_playing_album_index(album_index as i32);
                    state.set_playing_track_index(track_index as i32);
                }
            }
            EngineEvent::PlaybackStateChanged { state: playback } => {
                self.state.set(playback);
                state.set_is_playing(playback == PlaybackState::Playing);
            }
            EngineEvent::SignalPathChanged {
                assessment: Some(value),
            } => {
                let (badge, description, perfect) = presentation::assessment(&value);
                state.set_badge_label(badge.clone().into());
                state.set_signal_title(badge.into());
                state.set_signal_description(description.into());
                state.set_is_bit_perfect(perfect);
                state.set_is_shared(
                    value.negotiated.access_mode == DeviceAccessMode::SharedThroughSystemMixer,
                );
                *self.last_assessment.borrow_mut() = Some(value);
                self.show_volume();
                self.update_detail_signal();
            }
            EngineEvent::SignalPathChanged { assessment: None } => {
                self.last_assessment.borrow_mut().take();
                state.set_is_shared(false);
                state.set_is_bit_perfect(false);
                state.set_badge_label("Sortie libérée".into());
                state.set_signal_title("Sortie libérée".into());
                state.set_signal_description(
                    "Aucun flux audio actif ; le périphérique est disponible.".into(),
                );
                state.set_path_steps(Self::inactive_path());
                self.update_detail_signal();
            }
            EngineEvent::VerificationChanged { track_id, status }
                if self.track_id.get() == Some(track_id) =>
            {
                let transmission =
                    presentation::transmission(&status.transmission, self.sample_rate_hz.get());
                let integrity = presentation::integrity(&status.file_integrity);
                state.set_transmission_text(transmission.text.into());
                state.set_transmission_digest(transmission.evidence.into());
                state.set_transmission_ok(transmission.is_verified);
                state.set_integrity_text(integrity.text.into());
                state.set_integrity_digest(integrity.evidence.into());
                state.set_integrity_ok(integrity.is_verified);
                state.set_md5_label(
                    if integrity.is_verified {
                        "Identique"
                    } else {
                        "Voir les détails"
                    }
                    .into(),
                );
            }
            EngineEvent::VerificationChanged { .. } => {}
            EngineEvent::PlaybackFailed { kind, detail: _ } => {
                self.should_advance.set(false);
                self.message(presentation::failure(kind).into());
            }
        }
    }
}

/// The worker side of `synchronize_library`: the stored library first, then the rescan.
fn synchronize_in_background(
    database: &std::path::Path,
    new_roots: &[PathBuf],
    sender: &Sender<LibraryUpdate>,
) -> Result<(), pistolame_library::LibraryError> {
    let mut store = LibraryStore::open(database)?;
    store.add_roots(new_roots)?;
    let _ = sender.send(LibraryUpdate {
        snapshot: Some(store.snapshot()?),
        errors: Vec::new(),
        is_final: false,
    });
    let report = store.synchronize()?;
    let _ = sender.send(LibraryUpdate {
        snapshot: if report.has_changed {
            Some(store.snapshot()?)
        } else {
            None
        },
        errors: report.errors,
        is_final: true,
    });
    Ok(())
}

/// Groups tracks into albums (same folder, same album tag), tracks in disc/track order. The
/// chosen cover wins over the files' own cover.
fn albums_from_snapshot(snapshot: LibrarySnapshot) -> Vec<Album> {
    let mut albums: Vec<Album> = Vec::new();
    let mut positions: std::collections::HashMap<AlbumKey, usize> =
        std::collections::HashMap::new();
    for track in snapshot.tracks {
        let key = AlbumKey::of(&track);
        let index = *positions.entry(key.clone()).or_insert_with(|| {
            albums.push(Album {
                title: track.album.clone(),
                artist: track.artist.clone(),
                tag_title: track.album.clone(),
                tag_artist: track.artist.clone(),
                key,
                tracks: Vec::new(),
                artwork: slint::Image::default(),
                has_custom_artwork: false,
            });
            albums.len() - 1
        });
        albums[index].tracks.push(track);
    }
    for album in &mut albums {
        album.tracks.sort_by(|left, right| {
            (left.disc_number, left.track_number, &left.path).cmp(&(
                right.disc_number,
                right.track_number,
                &right.path,
            ))
        });
        if let Some(text) = snapshot.text_overrides.get(&album.key) {
            if let Some(title) = &text.title {
                album.title = title.clone();
            }
            if let Some(artist) = &text.artist {
                album.artist = artist.clone();
            }
        }
        let custom = snapshot.artwork_overrides.get(&album.key);
        album.has_custom_artwork = custom.is_some();
        album.artwork = decode_artwork(custom.or_else(|| original_artwork(&album.tracks)));
    }
    albums
}

/// The first cover found in an album's files (embedded or beside them).
fn original_artwork(tracks: &[MusicFile]) -> Option<&std::sync::Arc<[u8]>> {
    tracks.iter().find_map(|track| track.artwork.as_ref())
}

fn decode_artwork(bytes: Option<&std::sync::Arc<[u8]>>) -> slint::Image {
    bytes
        .and_then(|bytes| slint::Image::load_from_data(bytes, None).ok())
        .unwrap_or_default()
}

/// The edited value to store: `None` when the field is empty or equal to the tags, so the app
/// follows the files again.
fn edited_value(field: &str, tag_value: &str) -> Option<String> {
    let trimmed = field.trim();
    (!trimmed.is_empty() && trimmed != tag_value).then(|| trimmed.to_owned())
}
