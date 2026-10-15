//! The UI owns this bridge; only commands, events and atomic counters cross into audio threads.
use crate::{
    AlbumCard, AppState, AppWindow, ArtistCard, DetailInfo, LibraryTrackRow, PathStep,
    PlaylistItem, PlaylistTrackRow, QueueRow, TrackAction, TrackRowData, TrackSource, TrackTarget,
};
use crate::{presentation, settings::AppSettings};
use crossbeam_channel::{Receiver, Sender, unbounded};
use pistolame_core::*;
use pistolame_library::{
    AlbumKey, AlbumTextOverride, FolderWatcher, LibrarySnapshot, LibraryStore, MusicFile, Playlist,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

mod album_editor;
mod browsing;
mod library_sync;
mod media_session;
mod navigation_glue;
mod now_playing;
mod playback;
mod playlists;
mod queue;
mod selection;
mod session_glue;
mod sleep_timer_glue;
mod track_actions;
mod undo;
mod updates;
pub(crate) use navigation_glue::slint_sidebar_section;

struct Album {
    key: AlbumKey,
    /// What the app shows: the user's edit, or the tags.
    title: String,
    /// The album's artists as one line ("A, B & C").
    artist: String,
    /// Each artist of the album: the album shows under every one of them in "Artistes".
    artists: Vec<String>,
    /// What the tags say, to tell an edit from the original.
    tag_title: String,
    tag_artist: String,
    tracks: Vec<MusicFile>,
    /// What the grid's sort looks at (sort tags, year, added and played dates).
    sort_keys: crate::album_sort::AlbumSortKeys,
    artwork: slint::Image,
    /// Whether the user chose this cover (stored in the library, files untouched).
    has_custom_artwork: bool,
}

/// For each album of the catalog, what tells it apart from a namesake (see
/// `presentation::distinguishing_labels`).
fn distinguishing_labels(albums: &[Album]) -> Vec<Option<String>> {
    let identities: Vec<presentation::AlbumIdentity<'_>> = albums
        .iter()
        .map(|album| presentation::AlbumIdentity {
            title: &album.title,
            artist: &album.artist,
            format: album
                .tracks
                .first()
                .map(|track| {
                    presentation::compact_format(
                        track.bits_per_sample,
                        track.sample_rate_hz,
                        track.is_lossy.then_some(track.codec.as_str()),
                    )
                })
                .unwrap_or_default(),
            track_count: album.tracks.len(),
        })
        .collect();
    presentation::distinguishing_labels(&identities)
}

/// The grid tile of catalog album `index`, with what tells it apart from a namesake.
fn album_card(index: usize, album: &Album, distinguishing_label: Option<&str>) -> AlbumCard {
    AlbumCard {
        catalog_index: index as i32,
        title: album.title.as_str().into(),
        artist: presentation::album_tile_subtitle(&album.artist, distinguishing_label).into(),
        artwork: album.artwork.clone(),
    }
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
    artists: Vec<String>,
}

/// What the update worker sends back to the UI thread.
enum UpdateMessage {
    Checked(Option<crate::updater::AvailableUpdate>),
    CheckFailed,
    Installed(crate::updater::InstalledUpdate),
    InstallFailed(String),
}

/// Most tracks listed under a search; enough to find anything without a huge list.
const MAXIMUM_SEARCH_TRACKS: usize = 200;
/// How long a confirmation ("ajouté aux favoris") stays on screen: long enough to read two
/// short lines, short enough not to cover the content.
const NOTICE_DISPLAY_DURATION: Duration = Duration::from_secs(4);

/// First update check after start-up, then once a day.
const FIRST_UPDATE_CHECK_DELAY: Duration = Duration::from_secs(5);
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

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
    /// What plays now and next (album or playlist, plus the tracks queued by hand).
    play_queue: RefCell<crate::play_queue::PlayQueue>,
    /// Cover change waiting for "Enregistrer" in the playlist editor.
    pending_playlist_artwork: RefCell<PendingArtwork>,
    /// The tracks found by the current search, in display order.
    search_results: RefCell<Vec<PathBuf>>,
    /// Tracks waiting in the "Ajouter à une playlist" sheet.
    pending_playlist_tracks: RefCell<Vec<PathBuf>>,
    /// Self-update: messages from the worker, the update found, and when to check next.
    update_messages: Receiver<UpdateMessage>,
    update_sender: Sender<UpdateMessage>,
    available_update: RefCell<Option<crate::updater::AvailableUpdate>>,
    next_update_check: Cell<std::time::Instant>,
    is_update_worker_busy: Cell<bool>,
    /// The latest signal-path assessment, to redraw the path when the volume changes.
    last_assessment: RefCell<Option<SignalPathAssessment>>,
    state: Cell<PlaybackState>,
    track_id: Cell<Option<TrackId>>,
    /// False for debug and preview runs, which must not overwrite the user's saved start section.
    should_remember_start_section: Cell<bool>,
    sample_rate_hz: Cell<u32>,
    total_frames: Cell<u64>,
    underrun_baseline: Cell<u64>,
    selected_track: Cell<Option<(usize, usize)>>,
    detail_album: Cell<usize>,
    stream_end_baseline: Cell<u64>,
    should_advance: Cell<bool>,
    shuffle_seed: Cell<u64>,
    /// Position (seconds) of a restored track not opened yet: Play opens it there.
    resume_position_seconds: Cell<Option<u64>>,
    /// Files of the "Morceaux" table, in its current order.
    library_tracks: RefCell<Vec<PathBuf>>,
    track_sort: Cell<(crate::track_sort::TrackColumn, bool)>,
    media_controls: RefCell<crate::media_controls::SystemMediaControls>,
    /// Files marked as favorites (stored in the library by path).
    favorites: RefCell<std::collections::HashSet<PathBuf>>,
    /// Watches the library folders; its signals arrive on `folder_changes`.
    folder_watcher: RefCell<Option<(Vec<PathBuf>, FolderWatcher)>>,
    folder_change_sender: crossbeam_channel::Sender<()>,
    folder_changes: crossbeam_channel::Receiver<()>,
    /// When the last change was seen; the library is synchronized once changes settle.
    last_folder_change: Cell<Option<std::time::Instant>>,
    /// Files and folders dropped on the window, imported together at the next tick.
    dropped_paths: crate::file_drop::DroppedPaths,
    /// The archive being downloaded and its announced size, to show the progress.
    update_download: RefCell<Option<(PathBuf, u64)>>,
    /// Nyquist's analysis of the current track, off the UI thread.
    analysis_worker: Option<crate::analysis_worker::AnalysisWorker>,
    /// The latest analyses, newest last, so going back to a track shows it at once.
    analyses: RefCell<std::collections::VecDeque<crate::analysis_worker::AnalysisOutcome>>,
    /// Looks artist photos up online; `None` without a library database.
    artist_photo_worker: Option<crate::artist_photos::ArtistPhotoWorker>,
    /// Decoded artist photos, by artist name.
    artist_photos: RefCell<std::collections::HashMap<String, slint::Image>>,
    /// Whether the stored session was read; nothing is saved before, so an early save cannot
    /// replace it with an empty one.
    is_session_restored: Cell<bool>,
    last_saved_session: RefCell<Option<crate::session::Session>>,
    next_session_save: Cell<std::time::Instant>,
    /// Back/Forward history of the pages shown (see `navigation_glue`).
    navigation: RefCell<crate::navigation::NavigationHistory>,
    /// Counts the routes shown, so a delayed scroll restore never lands on a newer route.
    route_generation: Rc<Cell<u64>>,
    /// The rows selected in the track list shown (Ctrl/Cmd+click, Shift+click).
    selection: RefCell<crate::track_selection::TrackSelection>,
    /// What the "Annuler" of the confirmation shown puts back.
    pending_undo: RefCell<Option<undo::UndoableChange>>,
    /// Minuteur de veille (not stored between runs).
    sleep_timer: Cell<crate::sleep_timer::SleepTimer>,
}

/// Smallest progress change (fraction of the track) worth pushing to the UI; below this the
/// progress bar does not move by a visible pixel, so the write only costs an accessibility rebuild.
/// The ends (0 and 1) are always written, so the bar never stops just short of them.
const PROGRESS_MINIMUM_VISIBLE_CHANGE: f32 = 0.0005;

/// How often the session (queue, position) is written while the app runs.
const SESSION_SAVE_INTERVAL: Duration = Duration::from_secs(5);

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
        let start_page = navigation_glue::start_page(&settings);
        let engine = EngineHandle::start(
            pistolame_output::platform_default_output(),
            DecoderRegistry::default(),
            settings.engine_start_options(),
        )?;
        let (sender, receiver) = unbounded();
        let (update_sender, update_messages) = unbounded();
        let (folder_change_sender, folder_changes) = unbounded();
        let bridge = Rc::new(Self {
            window: window.as_weak(),
            engine,
            settings: RefCell::new(settings),
            library_database: library_database.clone(),
            devices: RefCell::new(Vec::new()),
            albums: RefCell::new(Vec::new()),
            imports: receiver,
            import_sender: sender,
            is_importing: Cell::new(false),
            album_edit: RefCell::new(None),
            last_assessment: RefCell::new(None),
            update_messages,
            update_sender,
            available_update: RefCell::new(None),
            next_update_check: Cell::new(std::time::Instant::now() + FIRST_UPDATE_CHECK_DELAY),
            is_update_worker_busy: Cell::new(false),
            playlists: RefCell::new(Vec::new()),
            play_queue: RefCell::new(crate::play_queue::PlayQueue::default()),
            pending_playlist_tracks: RefCell::new(Vec::new()),
            search_results: RefCell::new(Vec::new()),
            pending_playlist_artwork: RefCell::new(PendingArtwork::Unchanged),
            state: Cell::new(PlaybackState::Stopped),
            track_id: Cell::new(None),
            should_remember_start_section: Cell::new(true),
            sample_rate_hz: Cell::new(0),
            total_frames: Cell::new(0),
            underrun_baseline: Cell::new(0),
            selected_track: Cell::new(None),
            detail_album: Cell::new(0),
            stream_end_baseline: Cell::new(0),
            should_advance: Cell::new(false),
            shuffle_seed: Cell::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(1, |duration| duration.as_nanos() as u64),
            ),
            resume_position_seconds: Cell::new(None),
            library_tracks: RefCell::new(Vec::new()),
            track_sort: Cell::new((crate::track_sort::TrackColumn::default(), false)),
            media_controls: RefCell::new(crate::media_controls::SystemMediaControls::new()),
            favorites: RefCell::new(std::collections::HashSet::new()),
            folder_watcher: RefCell::new(None),
            folder_change_sender,
            folder_changes,
            last_folder_change: Cell::new(None),
            dropped_paths: Rc::default(),
            update_download: RefCell::new(None),
            analysis_worker: crate::analysis_worker::AnalysisWorker::start(),
            analyses: RefCell::new(std::collections::VecDeque::new()),
            artist_photo_worker: library_database
                .clone()
                .and_then(crate::artist_photos::ArtistPhotoWorker::start),
            artist_photos: RefCell::new(std::collections::HashMap::new()),
            is_session_restored: Cell::new(false),
            last_saved_session: RefCell::new(None),
            next_session_save: Cell::new(std::time::Instant::now()),
            navigation: RefCell::new(crate::navigation::NavigationHistory::new(start_page)),
            route_generation: Rc::new(Cell::new(0)),
            selection: RefCell::default(),
            pending_undo: RefCell::default(),
            sleep_timer: Cell::default(),
        });
        let state = window.global::<AppState>();
        state.set_signal_title("Aucune lecture".into());
        state.set_signal_description(
            "Choisissez un morceau pour inspecter le chemin du signal.".into(),
        );
        state.set_path_steps(Self::inactive_path());
        state.set_lcd_title("Pistolame".into());
        state.set_lcd_subtitle("Choisissez un morceau".into());
        state.set_badge_label("À l’arrêt".into());
        state.set_elapsed_label("0:00".into());
        state.set_remaining_label("0:00".into());
        state.set_playing_album_index(-1);
        state.set_playing_track_index(-1);
        state.set_no_results_text(
            "Ajoutez un dossier ou des fichiers audio pour retrouver vos albums.".into(),
        );
        state.set_transmission_text("Transmission : aucune lecture".into());
        state.set_integrity_text("Fichier : aucune lecture".into());
        state
            .set_exclusive(bridge.settings.borrow().output_mode == OutputModePreference::Exclusive);
        Ok(bridge)
    }

    /// The window position and size saved at the last exit.
    pub(crate) fn saved_window_geometry(&self) -> Option<crate::settings::WindowGeometry> {
        self.settings.borrow().window_geometry
    }

    /// Saves the window position and size, to reopen it the same way.
    pub(crate) fn remember_window_geometry(&self, geometry: crate::settings::WindowGeometry) {
        if self.settings.borrow().window_geometry == Some(geometry) {
            return;
        }
        self.settings.borrow_mut().window_geometry = Some(geometry);
        self.save_settings();
    }

    /// Stops navigation from saving the start section (debug and preview runs).
    pub(crate) fn disable_remembering_start_section(&self) {
        self.should_remember_start_section.set(false);
    }

    pub(crate) fn connect(self: &Rc<Self>, window: &AppWindow) {
        self.show_volume();
        crate::window_events::install(window, self.dropped_paths.clone());
        let state = window.global::<AppState>();
        let handle = self.clone();
        state.on_import_files(move || {
            if !handle.is_importing.get()
                && let Some(paths) = rfd::FileDialog::new()
                    .add_filter("Audio", pistolame_core::SUPPORTED_AUDIO_FILE_EXTENSIONS)
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
        state.on_volume_changed(move |decibels, is_muted| {
            handle.set_shared_mode_volume(SharedModeVolume {
                is_muted,
                ..SharedModeVolume::at_decibels(decibels)
            });
        });
        self.connect_playlists(&state);
        self.connect_navigation(&state);
        state.set_album_sort_labels(ModelRc::new(VecModel::from(
            crate::album_sort::AlbumSort::ALL
                .iter()
                .map(|sort| SharedString::from(sort.label()))
                .collect::<Vec<_>>(),
        )));
        state.set_album_sort_index(
            crate::album_sort::AlbumSort::ALL
                .iter()
                .position(|sort| *sort == self.album_sort())
                .unwrap_or(0) as i32,
        );
        // The "Morceaux" table reopens with its last order and filter.
        let column_at = |index: i32| {
            usize::try_from(index)
                .ok()
                .and_then(|index| crate::track_sort::TrackColumn::ALL.get(index).copied())
        };
        if let Some((index, is_descending)) = self.settings.borrow().track_sort
            && let Some(column) = column_at(index)
        {
            self.track_sort.set((column, is_descending));
        }
        state.set_is_favorites_only(self.settings.borrow().is_tracks_favorites_only);
        let handle = self.clone();
        state.on_sort_tracks(move |index| {
            let Some(column) = column_at(index) else {
                return;
            };
            // A second click on the sorted column flips the direction.
            let (current, is_descending) = handle.track_sort.get();
            let is_now_descending = current == column && !is_descending;
            handle.track_sort.set((column, is_now_descending));
            handle.settings.borrow_mut().track_sort = Some((index, is_now_descending));
            handle.save_settings();
            handle.show_library_tracks();
        });
        let handle = self.clone();
        state.on_reveal_current_track(move || {
            if let Some(path) = handle.current_path() {
                reveal_in_file_manager(&path);
            }
        });
        state.on_open_nyquist(|| open_in_browser(crate::authenticity::NYQUIST_URL));
        state.set_is_single_click_play(self.settings.borrow().is_single_click_play);
        let handle = self.clone();
        state.on_set_single_click_play(move |is_on| {
            handle.settings.borrow_mut().is_single_click_play = is_on;
            handle.save_settings();
            if let Some(window) = handle.window.upgrade() {
                window.global::<AppState>().set_is_single_click_play(is_on);
            }
        });
        state.set_are_artist_photos_on(!self.settings.borrow().are_artist_photos_off);
        let handle = self.clone();
        state.on_set_artist_photos(move |is_on| {
            handle.settings.borrow_mut().are_artist_photos_off = !is_on;
            // A failed save only loses the choice at the next launch.
            let _ = handle.settings.borrow().save();
            if let Some(window) = handle.window.upgrade() {
                window.global::<AppState>().set_are_artist_photos_on(is_on);
            }
            if is_on {
                handle.look_up_artist_photos();
            }
        });
        let handle = self.clone();
        state.on_toggle_favorites_only(move || {
            if let Some(window) = handle.window.upgrade() {
                let state = window.global::<AppState>();
                let is_favorites_only = !state.get_is_favorites_only();
                state.set_is_favorites_only(is_favorites_only);
                handle.settings.borrow_mut().is_tracks_favorites_only = is_favorites_only;
                handle.save_settings();
            }
            handle.show_library_tracks();
        });
        let handle = self.clone();
        state.on_play_library_track(move |index| {
            if let Ok(index) = usize::try_from(index) {
                let paths = handle.library_tracks.borrow().clone();
                handle.play_context(paths, index, "Morceaux".into());
            }
        });
        let handle = self.clone();
        state.on_play_search_result(move |index| {
            let paths = handle.search_results.borrow().clone();
            if let Ok(index) = usize::try_from(index) {
                let name = handle
                    .window
                    .upgrade()
                    .map(|window| {
                        format!("Recherche « {} »", window.global::<AppState>().get_query())
                    })
                    .unwrap_or_default();
                handle.play_context(paths, index, name);
            }
        });
        let handle = self.clone();
        state.on_set_album_sort(move |index| {
            if let Some(sort) = usize::try_from(index)
                .ok()
                .and_then(|index| crate::album_sort::AlbumSort::ALL.get(index))
            {
                handle.set_album_sort(*sort);
            }
        });
        state.set_current_version(crate::updater::CURRENT_VERSION.into());
        state.set_reveal_label(reveal_label().into());
        state.set_shortcut_modifier(
            if std::env::consts::OS == "macos" {
                "⌘"
            } else {
                "Ctrl+"
            }
            .into(),
        );
        let handle = self.clone();
        state.on_start_update(move || handle.start_update());
        let handle = self.clone();
        state.on_check_for_updates(move || {
            if !handle.is_update_worker_busy.get() {
                if let Some(window) = handle.window.upgrade() {
                    window
                        .global::<AppState>()
                        .set_update_check_status("Recherche d’une mise à jour…".into());
                }
                handle.check_for_update();
            }
        });
        let handle = self.clone();
        state.on_rescan_library(move || {
            if !handle.is_importing.get() {
                handle.synchronize_library(Vec::new());
            }
        });
        let handle = self.clone();
        state.on_remove_library_folder(move |index| {
            if let Ok(index) = usize::try_from(index) {
                handle.remove_library_folder(index);
            }
        });
        self.connect_queue(&state);
        self.connect_selection(&state);
        self.connect_undo(&state);
        self.connect_sleep_timer(&state);
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
        state.on_editor_add_artist(move || {
            if let Some(window) = handle.window.upgrade() {
                let state = window.global::<AppState>();
                let name = state.get_editor_new_artist().to_string();
                state.set_editor_new_artist("".into());
                handle.editor_add_artist(&name);
            }
        });
        let handle = self.clone();
        state.on_editor_add_suggested_artist(move |index| {
            let name = handle.window.upgrade().and_then(|window| {
                usize::try_from(index).ok().and_then(|index| {
                    slint::Model::row_data(
                        &window.global::<AppState>().get_editor_suggested_artists(),
                        index,
                    )
                })
            });
            if let Some(name) = name {
                handle.editor_add_artist(&name);
            }
        });
        let handle = self.clone();
        state.on_editor_remove_artist(move |index| {
            if let Ok(index) = usize::try_from(index) {
                handle.editor_remove_artist(index);
            }
        });
        let handle = self.clone();
        state.on_cancel_album_edit(move || handle.close_album_editor());
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
                let position = seconds * f64::from(fraction.clamp(0.0, 1.0));
                if handle.resume_position_seconds.get().is_some() {
                    // The restored track is not open yet: Play will start it there.
                    handle.resume_position_seconds.set(Some(position as u64));
                    return;
                }
                handle.seek_to(Duration::from_secs_f64(position));
            }
        });
        let handle = self.clone();
        state.on_refresh_devices(move || handle.engine.send(EngineCommand::RefreshDeviceList));
        let handle = self.clone();
        state.on_select_device(move |index| handle.select_device(index));
        let handle = self.clone();
        state.on_select_system_output(move || handle.select_system_output());
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
        // Opens on the remembered library section (`start_section` in the settings).
        self.apply_current_route();
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
            let selection = OutputDeviceSelection::Specific(device.id.clone());
            self.engine.send(EngineCommand::SelectOutputDevice {
                selection: selection.clone(),
            });
            let mut settings = self.settings.borrow_mut();
            settings.output_device = selection;
            settings.output_device_name = Some(device.display_name.clone());
        }
        self.save_settings();
    }

    /// "Sortie du système": follow the system default output from now on.
    fn select_system_output(&self) {
        self.engine.send(EngineCommand::SelectOutputDevice {
            selection: OutputDeviceSelection::SystemDefault,
        });
        {
            let mut settings = self.settings.borrow_mut();
            settings.output_device = OutputDeviceSelection::SystemDefault;
            settings.output_device_name = None;
        }
        self.save_settings();
    }

    /// Shows an error; it stays until the user closes it or a new track starts. An empty text
    /// hides the message.
    fn message(&self, text: String) {
        self.forget_pending_undo();
        if let Some(window) = self.window.upgrade() {
            let state = window.global::<AppState>();
            state.set_is_message_error(true);
            state.set_message(text.into());
        }
    }

    /// Shows a confirmation or an information that leaves by itself after
    /// `NOTICE_DISPLAY_DURATION`, unless another message replaced it meanwhile.
    fn notice(&self, text: String) {
        self.notice_for(text, NOTICE_DISPLAY_DURATION);
    }

    /// Like [`UiBridge::notice`], shown for `duration`.
    fn notice_for(&self, text: String, duration: Duration) {
        self.forget_pending_undo();
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        state.set_is_message_error(false);
        state.set_message(text.as_str().into());
        let weak = window.as_weak();
        slint::Timer::single_shot(duration, move || {
            if let Some(window) = weak.upgrade() {
                let state = window.global::<AppState>();
                if !state.get_is_message_error() && state.get_message() == text.as_str() {
                    state.set_message("".into());
                    state.set_message_action_label("".into());
                }
            }
        });
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

    pub(crate) fn tick(&self) {
        self.check_sleep_timer();
        for update in self.imports.try_iter() {
            self.apply_library_update(update);
        }
        for message in self.update_messages.try_iter() {
            self.apply_update_message(message);
        }
        self.show_update_progress();
        if !self.is_update_worker_busy.get()
            && std::time::Instant::now() >= self.next_update_check.get()
        {
            self.check_for_update();
        }
        for event in self.engine.try_receive_events() {
            self.handle_event(event);
        }
        self.handle_media_commands();
        self.synchronize_after_folder_changes();
        self.collect_analyses();
        self.collect_artist_photos();
        // Dropped files wait for an import in progress to finish.
        if !self.is_importing.get() && !self.dropped_paths.borrow().is_empty() {
            let paths = std::mem::take(&mut *self.dropped_paths.borrow_mut());
            self.synchronize_library(paths);
        }
        if std::time::Instant::now() >= self.next_session_save.get() {
            self.next_session_save
                .set(std::time::Instant::now() + SESSION_SAVE_INTERVAL);
            self.save_session();
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
        let rate = u64::from(self.sample_rate_hz.get().max(1));
        let frames_in_track = self
            .resume_position_seconds
            .get()
            .map_or(counters.frames_rendered_in_track, |seconds| seconds * rate);
        // Every setter below invalidates bindings, and Slint rebuilds the window's
        // accessibility tree after each change: only write values that really changed.
        let elapsed_label: SharedString = presentation::clock(frames_in_track / rate).into();
        if state.get_elapsed_label() != elapsed_label {
            state.set_elapsed_label(elapsed_label);
        }
        let remaining_seconds = self.total_frames.get().saturating_sub(frames_in_track) / rate;
        let remaining_label: SharedString = presentation::remaining_clock(remaining_seconds).into();
        if state.get_remaining_label() != remaining_label {
            state.set_remaining_label(remaining_label);
        }
        let progress = if self.total_frames.get() == 0 {
            0.0
        } else {
            (frames_in_track as f64 / self.total_frames.get() as f64).clamp(0.0, 1.0) as f32
        };
        if should_write_progress(state.get_progress(), progress) {
            state.set_progress(progress);
        }
        let underrun_label: SharedString = format!(
            "Coupures : {}",
            counters
                .underrun_events
                .saturating_sub(self.underrun_baseline.get())
        )
        .into();
        if state.get_underrun_label() != underrun_label {
            state.set_underrun_label(underrun_label);
        }
    }

    fn handle_event(&self, event: EngineEvent) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        match event {
            EngineEvent::DeviceListChanged {
                devices,
                selection,
                selected,
            } => {
                let view = presentation::device_list_view(&devices, &selection, selected.as_ref());
                let names: Vec<SharedString> = view
                    .device_names
                    .iter()
                    .map(|name| name.as_str().into())
                    .collect();
                state.set_device_names(ModelRc::new(VecModel::from(names)));
                state.set_selected_device_index(view.selected_device_index);
                state.set_is_system_output_selected(view.is_system_output_selected);
                state.set_system_output_device_name(view.system_output_device_name.into());
                *self.devices.borrow_mut() = devices;
            }
            EngineEvent::PreferredDeviceUnavailable { .. } => {
                self.notice(
                    "Sortie mémorisée absente : lecture sur la sortie par défaut du système."
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
                if let Some((album_index, track_index)) = selection {
                    self.show_current_track(&state, album_index, track_index);
                }
            }
            EngineEvent::PlaybackStateChanged { state: playback } => {
                self.state.set(playback);
                state.set_is_playing(playback == PlaybackState::Playing);
                self.publish_media_status();
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
                state.set_is_output_active(true);
                *self.last_assessment.borrow_mut() = Some(value);
                self.show_volume();
                self.update_detail_signal();
            }
            EngineEvent::SignalPathChanged { assessment: None } => {
                self.last_assessment.borrow_mut().take();
                state.set_is_output_active(false);
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
            let artists = crate::album_artists::split(&track.artist);
            albums.push(Album {
                title: track.album.clone(),
                artist: crate::album_artists::display(&artists),
                artists,
                tag_title: track.album.clone(),
                tag_artist: track.artist.clone(),
                key,
                tracks: Vec::new(),
                sort_keys: crate::album_sort::AlbumSortKeys::default(),
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
                album.artists = crate::album_artists::split(artist);
                album.artist = crate::album_artists::display(&album.artists);
            }
        }
        let custom = snapshot.artwork_overrides.get(&album.key);
        album.has_custom_artwork = custom.is_some();
        album.artwork = decode_artwork(custom.or_else(|| original_artwork(&album.tracks)));
        album.sort_keys = sort_keys_of(album);
    }
    albums
}

/// What the grid's sort looks at: the files' sort tags when present (unless the user renamed
/// the album), the earliest year, the latest added and played dates.
fn sort_keys_of(album: &Album) -> crate::album_sort::AlbumSortKeys {
    let tag = |pick: fn(&MusicFile) -> Option<&String>| album.tracks.iter().find_map(pick).cloned();
    let is_title_edited = album.title != album.tag_title;
    let is_artist_edited = album.artists != crate::album_artists::split(&album.tag_artist);
    crate::album_sort::AlbumSortKeys {
        title: tag(|track| track.sort_album.as_ref())
            .filter(|_| !is_title_edited)
            .unwrap_or_else(|| album.title.clone()),
        artist: tag(|track| track.sort_album_artist.as_ref())
            .filter(|_| !is_artist_edited)
            .unwrap_or_else(|| album.artist.clone()),
        year: album.tracks.iter().filter_map(|track| track.year).min(),
        added_seconds: album
            .tracks
            .iter()
            .map(|track| track.added_seconds)
            .max()
            .unwrap_or(0),
        last_played_seconds: album
            .tracks
            .iter()
            .filter_map(|track| track.last_played_seconds)
            .max(),
    }
}

/// Whether the progress bar must be updated: a visible change, or reaching either end.
fn should_write_progress(shown: f32, new: f32) -> bool {
    let is_at_an_end = new == 0.0 || new == 1.0;
    (is_at_an_end && shown != new) || (shown - new).abs() >= PROGRESS_MINIMUM_VISIBLE_CHANGE
}

/// The album's year: the earliest year of its tracks.
fn album_year(album: &Album) -> Option<u32> {
    album.sort_keys.year
}

/// The codecs of an album's tracks, in track order without repeats ("FLAC", "MP3 / AAC").
fn album_codecs(album: &Album) -> String {
    let mut codecs: Vec<&str> = Vec::new();
    for track in &album.tracks {
        if !codecs.contains(&track.codec.as_str()) {
            codecs.push(&track.codec);
        }
    }
    codecs.join(" / ")
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

/// The file manager's name for "show this file", in each system's own words.
fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Afficher dans le Finder"
    } else if cfg!(windows) {
        "Afficher dans l’Explorateur"
    } else {
        "Ouvrir le dossier"
    }
}

/// Opens the system file manager with `path` selected (its folder on Linux, where file managers
/// have no common "select" option). Best effort: nothing to report if it cannot start.
fn reveal_in_file_manager(path: &std::path::Path) {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("open");
        command.arg("-R").arg(path);
        command
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("explorer");
        let mut argument = std::ffi::OsString::from("/select,");
        argument.push(path);
        command.arg(argument);
        command
    } else {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(path.parent().unwrap_or(path));
        command
    };
    let _ = command.spawn();
}

/// Opens `url` in the default browser. Best effort, like revealing a file.
fn open_in_browser(url: &str) {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else {
        std::process::Command::new("xdg-open")
    };
    let _ = command.arg(url).spawn();
}

/// Artist cards from the albums: one per album artist (as displayed), natural order, with the
/// cover of their newest album that has one.
fn artist_cards(
    albums: &[Album],
    photos: &std::collections::HashMap<String, slint::Image>,
) -> Vec<ArtistCard> {
    struct ArtistEntry {
        album_count: usize,
        newest_year: Option<u32>,
        artwork: slint::Image,
    }
    let mut by_artist: std::collections::HashMap<&str, ArtistEntry> =
        std::collections::HashMap::new();
    for (album, artist) in albums
        .iter()
        .flat_map(|album| album.artists.iter().map(move |artist| (album, artist)))
    {
        let entry = by_artist
            .entry(artist.as_str())
            .or_insert_with(|| ArtistEntry {
                album_count: 0,
                newest_year: None,
                artwork: slint::Image::default(),
            });
        entry.album_count += 1;
        let has_artwork = album.artwork.size().width > 0;
        let has_no_artwork_yet = entry.artwork.size().width == 0;
        let year = album_year(album);
        if has_artwork && (has_no_artwork_yet || year > entry.newest_year) {
            entry.artwork = album.artwork.clone();
            entry.newest_year = year;
        }
    }
    let mut artists: Vec<(&str, ArtistEntry)> = by_artist.into_iter().collect();
    artists.sort_by(|left, right| crate::album_sort::natural_compare(left.0, right.0));
    artists
        .into_iter()
        .map(|(name, entry)| ArtistCard {
            name: name.into(),
            detail: format!(
                "{} album{}",
                entry.album_count,
                if entry.album_count == 1 { "" } else { "s" }
            )
            .into(),
            artwork: photos.get(name).cloned().unwrap_or(entry.artwork),
        })
        .collect()
}

#[cfg(test)]
mod progress_tests {
    use super::should_write_progress;

    #[test]
    fn a_tiny_change_is_skipped() {
        assert!(!should_write_progress(0.5, 0.5001));
        assert!(should_write_progress(0.5, 0.501));
    }

    #[test]
    fn the_ends_are_always_written() {
        assert!(should_write_progress(0.9999, 1.0));
        assert!(should_write_progress(0.0001, 0.0));
        assert!(!should_write_progress(1.0, 1.0));
        assert!(!should_write_progress(0.0, 0.0));
    }
}
