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

mod navigation_glue;
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
            "Ajoutez un dossier ou des fichiers FLAC pour retrouver vos albums.".into(),
        );
        state.set_transmission_text("Transmission : aucune lecture".into());
        state.set_integrity_text("Fichier : aucune lecture".into());
        state
            .set_exclusive(bridge.settings.borrow().output_mode == OutputModePreference::Exclusive);
        Ok(bridge)
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
        let handle = self.clone();
        state.on_sort_tracks(move |index| {
            let Some(column) = usize::try_from(index)
                .ok()
                .and_then(|index| crate::track_sort::TrackColumn::ALL.get(index).copied())
            else {
                return;
            };
            // A second click on the sorted column flips the direction.
            let (current, is_descending) = handle.track_sort.get();
            handle
                .track_sort
                .set((column, current == column && !is_descending));
            handle.show_library_tracks();
        });
        let handle = self.clone();
        state.on_reveal_current_track(move || {
            if let Some(path) = handle.current_path() {
                reveal_in_file_manager(&path);
            }
        });
        state.on_open_nyquist(|| open_in_browser(crate::authenticity::NYQUIST_URL));
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
                state.set_is_favorites_only(!state.get_is_favorites_only());
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

    /// Asks for the photos of the artists that have none yet (unless the user turned it off).
    fn look_up_artist_photos(&self) {
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
    fn collect_artist_photos(&self) {
        let Some(worker) = &self.artist_photo_worker else {
            return;
        };
        let mut has_new_photo = false;
        for photo in worker.take_results() {
            if let Some(bytes) = photo.image {
                let image = decode_artwork(Some(&std::sync::Arc::from(bytes)));
                if image.size().width > 0 {
                    self.artist_photos.borrow_mut().insert(photo.artist, image);
                    has_new_photo = true;
                }
            }
        }
        if has_new_photo {
            self.show_artists();
        }
    }

    /// Watches the library folders, restarting the watcher only when the folders changed.
    fn watch_library_folders(&self) {
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
        let watcher = FolderWatcher::watch(&roots, move || {
            let _ = sender.send(());
        })
        .ok();
        *self.folder_watcher.borrow_mut() = watcher.map(|watcher| (roots, watcher));
    }

    /// Synchronizes the library once folder changes have settled (a copy in progress sends
    /// many events; one pass after the last one is enough).
    fn synchronize_after_folder_changes(&self) {
        /// Quiet time after the last change before synchronizing.
        const FOLDER_SETTLE_DELAY: Duration = Duration::from_secs(2);
        let now = std::time::Instant::now();
        if self.folder_changes.try_iter().count() > 0 {
            self.last_folder_change.set(Some(now));
        }
        if let Some(last_change) = self.last_folder_change.get()
            && now.duration_since(last_change) >= FOLDER_SETTLE_DELAY
            && !self.is_importing.get()
        {
            self.last_folder_change.set(None);
            self.synchronize_library(Vec::new());
        }
    }

    /// Shows the remembered library folders on the settings page.
    fn show_library_folders(&self) {
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
    fn remove_library_folder(&self, index: usize) {
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

    fn apply_library_update(&self, update: LibraryUpdate) {
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
    fn replace_albums(&self, mut snapshot: LibrarySnapshot) {
        *self.artist_photos.borrow_mut() = std::mem::take(&mut snapshot.artist_images)
            .into_iter()
            .map(|(artist, bytes)| (artist, decode_artwork(Some(&bytes))))
            .filter(|(_, image)| image.size().width > 0)
            .collect();
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
    fn show_recent_albums(&self) {
        /// More than two rows of the widest grid never show.
        const MAXIMUM_SHELF_ALBUMS: usize = 16;
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let albums = self.albums.borrow();
        let card = |(index, album): (usize, &Album)| AlbumCard {
            catalog_index: index as i32,
            title: album.title.as_str().into(),
            artist: album.artist.as_str().into(),
            artwork: album.artwork.clone(),
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
        let added: Vec<AlbumCard> = added
            .into_iter()
            .take(MAXIMUM_SHELF_ALBUMS)
            .map(card)
            .collect();
        let played: Vec<AlbumCard> = played
            .into_iter()
            .take(MAXIMUM_SHELF_ALBUMS)
            .map(|(index, album, _)| card((index, album)))
            .collect();
        let state = window.global::<AppState>();
        state.set_recently_added_albums(ModelRc::new(VecModel::from(added)));
        state.set_recently_played_albums(ModelRc::new(VecModel::from(played)));
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
        state.set_editor_new_artist("".into());
        state.set_editor_artwork(album.artwork.clone());
        state.set_editor_has_custom_artwork(album.has_custom_artwork);
        state.set_is_album_editor_open(true);
        *self.album_edit.borrow_mut() = Some(AlbumEditDraft {
            index,
            pending_artwork: PendingArtwork::Unchanged,
            artists: album.artists.clone(),
        });
        drop(albums);
        self.show_editor_artists();
    }

    /// Shows the draft's artists, and the track artists not yet among them.
    fn show_editor_artists(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let draft = self.album_edit.borrow();
        let Some(draft) = draft.as_ref() else {
            return;
        };
        let albums = self.albums.borrow();
        let suggestions = albums
            .get(draft.index)
            .map(|album| {
                crate::album_artists::suggestions(
                    album.tracks.iter().map(|track| track.artist.as_str()),
                    &draft.artists,
                )
            })
            .unwrap_or_default();
        let model = |names: &[String]| {
            ModelRc::new(VecModel::from(
                names
                    .iter()
                    .map(|name| SharedString::from(name.as_str()))
                    .collect::<Vec<_>>(),
            ))
        };
        let state = window.global::<AppState>();
        state.set_editor_artists(model(&draft.artists));
        state.set_editor_suggested_artists(model(&suggestions));
    }

    /// Adds the typed artist (or each one, when separated by `;`).
    fn editor_add_artist(&self, name: &str) {
        if let Some(draft) = self.album_edit.borrow_mut().as_mut() {
            for artist in crate::album_artists::split(name) {
                if !draft
                    .artists
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(&artist))
                {
                    draft.artists.push(artist);
                }
            }
        }
        self.show_editor_artists();
    }

    /// Removes artist `index` from the draft.
    fn editor_remove_artist(&self, index: usize) {
        if let Some(draft) = self.album_edit.borrow_mut().as_mut()
            && index < draft.artists.len()
        {
            draft.artists.remove(index);
        }
        self.show_editor_artists();
    }

    /// Lets the user pick a JPEG or PNG; applied only on "Enregistrer".
    fn editor_choose_artwork(&self) {
        let Some((bytes, image)) = self.pick_cover_image() else {
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

    /// Asks for a JPEG or PNG cover and checks it decodes and is not too large.
    fn pick_cover_image(&self) -> Option<(Vec<u8>, slint::Image)> {
        let path = rfd::FileDialog::new()
            .add_filter("Image", &["jpg", "jpeg", "png"])
            .pick_file()?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.message(format!("Impossible de lire l’image : {error}"));
                return None;
            }
        };
        if bytes.len() as u64 > pistolame_library::MAX_COVER_BYTES {
            self.message("Cette image dépasse 16 Mo.".into());
            return None;
        }
        let Ok(image) = slint::Image::load_from_data(&bytes, None) else {
            self.message("Cette image n’est ni un JPEG ni un PNG lisible.".into());
            return None;
        };
        Some((bytes, image))
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
        // A name still typed in the field counts, as if "Ajouter" had been pressed.
        let mut artists = draft.artists.clone();
        for typed in crate::album_artists::split(state.get_editor_new_artist().as_str()) {
            if !artists
                .iter()
                .any(|known| known.eq_ignore_ascii_case(&typed))
            {
                artists.push(typed);
            }
        }
        let text = AlbumTextOverride {
            title: edited_value(state.get_editor_title().as_str(), &tag_title),
            artist: edited_value(
                &crate::album_artists::join_for_storage(&artists),
                &crate::album_artists::join_for_storage(&crate::album_artists::split(&tag_artist)),
            ),
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
            album.artists =
                crate::album_artists::split(text.artist.as_deref().unwrap_or(&tag_artist));
            album.artist = crate::album_artists::display(&album.artists);
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

    /// Shows the albums matching `query` (all of them when it is empty) and, while searching,
    /// the matching tracks under them.
    fn update_grid(&self, query: &str) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let words = crate::search::query_words(query);
        let artist_filter = state.get_artist_filter().to_string();
        let albums = self.albums.borrow();
        let cards: Vec<AlbumCard> = albums
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
            .map(|(index, album)| AlbumCard {
                catalog_index: index as i32,
                title: album.title.as_str().into(),
                artist: album.artist.as_str().into(),
                artwork: album.artwork.clone(),
            })
            .collect();
        let current_path = self.current_path();
        let mut results = Vec::new();
        let mut rows = Vec::new();
        if !words.is_empty() {
            for album in albums.iter() {
                for file in &album.tracks {
                    if rows.len() >= MAXIMUM_SEARCH_TRACKS {
                        break;
                    }
                    if crate::search::matches(&words, &[&file.title, &album.artist, &album.title]) {
                        rows.push(PlaylistTrackRow {
                            position: rows.len() as i32,
                            number: rows.len() as i32 + 1,
                            title: file.title.as_str().into(),
                            subtitle: format!("{} — {}", album.artist, album.title).into(),
                            format_label: presentation::audio_format(
                                file.bits_per_sample,
                                file.sample_rate_hz,
                            )
                            .into(),
                            duration_label: presentation::clock(file.duration_seconds).into(),
                            is_current: current_path.as_ref() == Some(&file.path),
                            is_favorite: self.favorites.borrow().contains(&file.path),
                        });
                        results.push(file.path.clone());
                    }
                }
            }
        }
        drop(albums);
        *self.search_results.borrow_mut() = results;
        state.set_no_results_text(if words.is_empty() {
            "Ajoutez un dossier ou des fichiers FLAC pour retrouver vos albums.".into()
        } else {
            format!("Rien trouvé pour « {query} ».").into()
        });
        state.set_search_tracks(ModelRc::new(VecModel::from(rows)));
        state.set_albums(ModelRc::new(VecModel::from(cards)));
    }

    /// One card per album artist, in natural order, with their newest album's cover.
    fn show_artists(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let cards = artist_cards(&self.albums.borrow(), &self.artist_photos.borrow());
        window
            .global::<AppState>()
            .set_artists(ModelRc::new(VecModel::from(cards)));
    }

    /// Fills the "Morceaux" table with every track, in the chosen column order.
    fn show_library_tracks(&self) {
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
    fn open_artist(&self, index: i32) {
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
    fn current_path(&self) -> Option<PathBuf> {
        self.selected_track.get().and_then(|(album, track)| {
            self.albums
                .borrow()
                .get(album)
                .and_then(|album| album.tracks.get(track))
                .map(|track| track.path.clone())
        })
    }

    /// The grid order chosen in the settings.
    fn album_sort(&self) -> crate::album_sort::AlbumSort {
        self.settings
            .borrow()
            .album_sort
            .as_deref()
            .and_then(crate::album_sort::AlbumSort::from_key)
            .unwrap_or_default()
    }

    /// Changes the grid order, remembers it and re-sorts the albums in place.
    fn set_album_sort(&self, sort: crate::album_sort::AlbumSort) {
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
            meta_line: match album_year(album) {
                Some(year) => format!("{year} · {} morceaux · FLAC", album.tracks.len()),
                None => format!("{} morceaux · FLAC", album.tracks.len()),
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
            });
        }
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

    /// Plays track `track` of album `album`; the album becomes the playing context.
    fn play_track(&self, album: usize, track: usize) {
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
    fn play_context(&self, paths: Vec<PathBuf>, position: usize, name: String) {
        let path = self
            .play_queue
            .borrow_mut()
            .play_context(paths, position, name);
        if let Some(path) = path {
            self.start_playback(path);
        }
    }

    /// Opens and plays `path`, a track the queue just made current.
    fn start_playback(&self, path: PathBuf) {
        self.resume_position_seconds.set(None);
        let Some(location) = self.track_location(&path) else {
            self.message(format!("Morceau introuvable : {}", path.display()));
            return;
        };
        self.selected_track.set(Some(location));
        self.should_advance.set(true);
        self.stream_end_baseline
            .set(self.engine.counters().stream_ends_reached);
        self.message(String::new());
        self.record_play(&path, location);
        self.engine.send(EngineCommand::OpenFileAndPlay { path });
        self.refresh_playlist_view();
        self.refresh_queue_view();
    }

    /// Counts a play in the library and in memory ("Écoutés récemment"). A failed write only
    /// loses the statistic, so it stays silent.
    fn record_play(&self, path: &std::path::Path, (album, track): (usize, usize)) {
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
            }
            album.sort_keys.last_played_seconds = Some(now);
        }
        self.show_recent_albums();
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

    /// The previous/next buttons. "Previous" restarts the track after 3 s, like every player.
    fn adjacent_track(&self, next: bool) {
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
            self.engine.send(EngineCommand::SeekTo {
                position_in_track: Duration::ZERO,
            });
            return;
        }
        let path = self.play_queue.borrow_mut().previous();
        if let Some(path) = path {
            self.start_playback(path);
        }
    }

    /// Plays what comes next. The "next" button wraps around the album or playlist; the end of
    /// a track wraps only when repeat is on.
    fn play_next_in_queue(&self, is_manual: bool) {
        let (is_shuffle_on, is_repeat_on) = self
            .window
            .upgrade()
            .map(|window| {
                let state = window.global::<AppState>();
                (state.get_is_shuffle_on(), state.get_is_repeat_on())
            })
            .unwrap_or_default();
        let (count, position) = {
            let queue = self.play_queue.borrow();
            (queue.context_len(), queue.context_position())
        };
        let shuffled_position = (is_shuffle_on && count > 1)
            .then(|| (position + 1 + self.random_track(count - 1)) % count);
        let path = self
            .play_queue
            .borrow_mut()
            .advance(shuffled_position, is_manual || is_repeat_on);
        if let Some(path) = path {
            self.start_playback(path);
        }
    }

    fn advance_after_end(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        if self
            .play_queue
            .borrow()
            .has_next(state.get_is_repeat_on(), state.get_is_shuffle_on())
        {
            self.play_next_in_queue(false);
        }
    }

    /// Runs a "…" menu action on a track, an album or a playlist.
    fn run_track_action(&self, target: TrackTarget, action: TrackAction) {
        if action == TrackAction::RemoveFromPlaylist {
            if target.source == TrackSource::PlaylistEntry
                && let Ok(position) = usize::try_from(target.position)
            {
                self.remove_playlist_entry(position);
            }
            return;
        }
        let Some((paths, subject)) = self.target_tracks(&target) else {
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
                self.status(format!(
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
            TrackAction::RemoveFromPlaylist => {}
        }
    }

    /// Reads the favorites from the library. A read failure keeps the ones in memory.
    fn load_favorites(&self) {
        if let Some(database) = &self.library_database
            && let Ok(store) = LibraryStore::open(database)
            && let Ok(favorites) = store.favorites()
        {
            *self.favorites.borrow_mut() = favorites.into_iter().collect();
        }
    }

    /// Adds `paths` to the favorites, or removes them when they all are already.
    fn toggle_favorite(&self, paths: &[PathBuf], subject: &str) {
        let Some(database) = &self.library_database else {
            return;
        };
        let should_add = !paths
            .iter()
            .all(|path| self.favorites.borrow().contains(path));
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
        self.status(format!(
            "{subject} : {}",
            if should_add {
                "ajouté aux favoris"
            } else {
                "retiré des favoris"
            }
        ));
    }

    /// Redraws every list that shows a heart.
    fn refresh_favorite_marks(&self) {
        self.show_album(self.detail_album.get());
        self.refresh_playlist_view();
        self.show_library_tracks();
        if let Some(window) = self.window.upgrade() {
            self.update_grid(window.global::<AppState>().get_query().as_str());
        }
    }

    /// The tracks a menu action applies to, and how to name them.
    fn target_tracks(&self, target: &TrackTarget) -> Option<(Vec<PathBuf>, String)> {
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
            TrackSource::LibraryTrack | TrackSource::SearchResult => {
                let list = if target.source == TrackSource::LibraryTrack {
                    &self.library_tracks
                } else {
                    &self.search_results
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

    /// Asks GitHub for a newer release, in the background.
    fn check_for_update(&self) {
        self.next_update_check
            .set(std::time::Instant::now() + UPDATE_CHECK_INTERVAL);
        self.is_update_worker_busy.set(true);
        let sender = self.update_sender.clone();
        let spawned = std::thread::Builder::new()
            .name("pistolame-update-check".into())
            .spawn(move || {
                let _ = sender.send(match crate::updater::check_for_update() {
                    Ok(update) => UpdateMessage::Checked(update),
                    Err(_) => UpdateMessage::CheckFailed,
                });
            });
        if spawned.is_err() {
            self.is_update_worker_busy.set(false);
        }
    }

    /// "Mettre à jour": download, verify and install in the background, then relaunch.
    fn start_update(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        if let Some(reason) = crate::updater::installation_blocker() {
            state.set_update_status(reason.into());
            return;
        }
        let Some(update) = self.available_update.borrow().clone() else {
            return;
        };
        state.set_is_updating(true);
        state.set_update_status("Connexion…".into());
        self.is_update_worker_busy.set(true);
        let work_folder = std::env::temp_dir().join("pistolame-update");
        *self.update_download.borrow_mut() = crate::updater::download_path(&update, &work_folder)
            .map(|path| (path, update.asset.size));
        let sender = self.update_sender.clone();
        let spawned = std::thread::Builder::new()
            .name("pistolame-update-install".into())
            .spawn(move || {
                let result = crate::updater::download_and_verify(&update, &work_folder)
                    .and_then(|archive| crate::updater::install(&archive, &work_folder));
                let _ = sender.send(match result {
                    Ok(installed) => UpdateMessage::Installed(installed),
                    Err(error) => UpdateMessage::InstallFailed(format!("{error:#}")),
                });
            });
        if spawned.is_err() {
            self.is_update_worker_busy.set(false);
            state.set_is_updating(false);
        }
    }

    /// "Téléchargement… 12,3 / 22,4 Mo" while the archive grows, then the verification step.
    fn show_update_progress(&self) {
        let Some((path, size)) = self.update_download.borrow().clone() else {
            return;
        };
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let received = std::fs::metadata(&path).map_or(0, |metadata| metadata.len());
        if received == 0 {
            return;
        }
        let megabytes = |bytes: u64| format!("{:.1}", bytes as f64 / 1_000_000.0).replace('.', ",");
        let text = if size > 0 && received >= size {
            "Vérification de la signature et installation…".to_owned()
        } else if size > 0 {
            format!(
                "Téléchargement… {} / {} Mo",
                megabytes(received),
                megabytes(size)
            )
        } else {
            format!("Téléchargement… {} Mo", megabytes(received))
        };
        let state = window.global::<AppState>();
        let status: SharedString = text.into();
        if state.get_update_status() != status {
            state.set_update_status(status);
        }
    }

    fn apply_update_message(&self, message: UpdateMessage) {
        if matches!(
            message,
            UpdateMessage::Installed(_) | UpdateMessage::InstallFailed(_)
        ) {
            *self.update_download.borrow_mut() = None;
        }
        self.is_update_worker_busy.set(false);
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        match message {
            UpdateMessage::Checked(Some(update)) => {
                state.set_update_check_status(
                    format!(
                        "La version {} est disponible : cliquez sur la pastille verte.",
                        update.version
                    )
                    .into(),
                );
                state.set_update_version(update.version.as_str().into());
                state.set_update_notes(update.notes.as_str().into());
                state.set_update_status("".into());
                state.set_is_update_available(true);
                *self.available_update.borrow_mut() = Some(update);
            }
            // No newer release, or no network: the settings page says so, nothing else does.
            UpdateMessage::Checked(None) => {
                state.set_update_check_status("Pistolame est à jour.".into());
            }
            UpdateMessage::CheckFailed => {
                state.set_update_check_status(
                    "Impossible de joindre GitHub. Nouvel essai plus tard.".into(),
                );
            }
            UpdateMessage::Installed(installed) => {
                state.set_update_status("Redémarrage…".into());
                self.engine.send(EngineCommand::Stop);
                match crate::updater::relaunch(&installed) {
                    Ok(()) => {
                        let _ = slint::quit_event_loop();
                    }
                    Err(error) => {
                        state.set_is_updating(false);
                        state.set_update_status(
                            format!("Mise à jour installée, mais {error:#}. Relancez Pistolame.")
                                .into(),
                        );
                    }
                }
            }
            UpdateMessage::InstallFailed(error) => {
                state.set_is_updating(false);
                state.set_update_status(format!("Échec de la mise à jour : {error}").into());
            }
        }
    }

    /// A short confirmation in the status line under the sidebar.
    fn status(&self, text: String) {
        if let Some(window) = self.window.upgrade() {
            window.global::<AppState>().set_import_status(text.into());
        }
    }

    /// Redraws the "À suivre" panel.
    fn refresh_queue_view(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let queue = self.play_queue.borrow();
        let row = |path: &PathBuf| self.queue_row(path);
        let current = queue.current().and_then(row);
        state.set_has_queue_current(current.is_some());
        state.set_queue_current(current.unwrap_or_default());
        state.set_queue_up_next(ModelRc::new(VecModel::from(
            queue.up_next().filter_map(row).collect::<Vec<_>>(),
        )));
        state.set_queue_upcoming(ModelRc::new(VecModel::from(
            queue.upcoming().iter().filter_map(row).collect::<Vec<_>>(),
        )));
        state.set_queue_context_name(queue.context_name().into());
    }

    fn queue_row(&self, path: &std::path::Path) -> Option<QueueRow> {
        let (album, track) = self.track_location(path)?;
        let albums = self.albums.borrow();
        let album = albums.get(album)?;
        let track = album.tracks.get(track)?;
        Some(QueueRow {
            title: track.title.as_str().into(),
            subtitle: format!("{} — {}", album.artist, album.title).into(),
            duration_label: presentation::clock(track.duration_seconds).into(),
            artwork: album.artwork.clone(),
        })
    }

    fn connect_queue(self: &Rc<Self>, state: &AppState<'_>) {
        let handle = self.clone();
        state.on_track_action(move |target, action| handle.run_track_action(target, action));
        let handle = self.clone();
        state.on_queue_remove(move |index| {
            if let Ok(index) = usize::try_from(index) {
                handle.play_queue.borrow_mut().remove_up_next(index);
                handle.refresh_queue_view();
            }
        });
        let handle = self.clone();
        state.on_queue_clear(move || {
            handle.play_queue.borrow_mut().clear_up_next();
            handle.refresh_queue_view();
        });
        let handle = self.clone();
        state.on_queue_play_up_next(move |index| {
            let path = usize::try_from(index)
                .ok()
                .and_then(|index| handle.play_queue.borrow_mut().jump_to_up_next(index));
            if let Some(path) = path {
                handle.start_playback(path);
            }
        });
        let handle = self.clone();
        state.on_queue_play_upcoming(move |index| {
            let path = usize::try_from(index)
                .ok()
                .and_then(|index| handle.play_queue.borrow_mut().jump_to_upcoming(index));
            if let Some(path) = path {
                handle.start_playback(path);
            }
        });
    }

    fn connect_playlists(self: &Rc<Self>, state: &AppState<'_>) {
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
                artwork: decode_artwork(playlist.artwork.as_ref()),
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

    fn play_playlist_row(&self, row: usize) {
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
            artwork: None,
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
            state.set_playlist_editor_artwork(decode_artwork(playlist.artwork.as_ref()));
        }
        *self.pending_playlist_artwork.borrow_mut() = PendingArtwork::Unchanged;
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
    fn export_current_playlist(&self) {
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
            Ok(()) => self.status(format!("Playlist exportée : {}", path.display())),
            Err(error) => self.message(format!("Export impossible : {error}")),
        }
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
        // Leaves the deleted playlist (Back, else the album grid) and forgets it in the history.
        self.synchronize_navigation_with_library();
        self.show_playlists();
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

    /// Shows track `track_index` of album `album_index` on the LCD and in the details.
    fn show_current_track(&self, state: &AppState<'_>, album_index: usize, track_index: usize) {
        let albums = self.albums.borrow();
        let Some(album) = albums.get(album_index) else {
            return;
        };
        let Some(track) = album.tracks.get(track_index) else {
            return;
        };
        state.set_has_current_track(true);
        state.set_current_track_path(track.path.to_string_lossy().as_ref().into());
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
        let path = track.path.clone();
        let cells = crate::presentation::track_info_cells(
            track,
            std::fs::metadata(&path).ok().map(|metadata| metadata.len()),
        );
        drop(albums);
        state.set_current_path(path.to_string_lossy().as_ref().into());
        self.show_track_info(state, cells, None);
        self.show_authenticity_for(&path);
        self.publish_media_track();
    }

    /// Fills "Informations sur le fichier"; `analysis` adds what only the decode knows.
    fn show_track_info(
        &self,
        state: &AppState<'_>,
        cells: Vec<(String, String)>,
        analysis: Option<&crate::authenticity::AuthenticityView>,
    ) {
        let mut cells: Vec<crate::DataCell> = cells
            .into_iter()
            .map(|(label, value)| crate::DataCell {
                label: label.into(),
                value: value.into(),
            })
            .collect();
        if let Some(view) = analysis {
            cells.extend(view.file_facts.iter().map(|fact| crate::DataCell {
                label: fact.label.as_str().into(),
                value: fact.text.as_str().into(),
            }));
        }
        state.set_track_info_cells(ModelRc::new(VecModel::from(cells)));
    }

    /// Shows the stored analysis of `path`, or starts one.
    fn show_authenticity_for(&self, path: &std::path::Path) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        state.set_nyquist_note(crate::authenticity::NYQUIST_NOTE.into());
        state.set_authenticity_disclaimer(crate::authenticity::DISCLAIMER.into());
        let known = self
            .analyses
            .borrow()
            .iter()
            .find(|(analyzed, _)| analyzed == path)
            .map(|(_, outcome)| outcome.clone());
        match (known, &self.analysis_worker) {
            (Some(outcome), _) => self.show_authenticity(&state, &outcome),
            (None, Some(worker)) => {
                state.set_authenticity_state(1);
                state.set_has_spectrogram(false);
                worker.request(path.to_path_buf());
            }
            (None, None) => {
                state.set_authenticity_state(3);
                state.set_authenticity_error("le module d’analyse n’a pas pu démarrer.".into());
                state.set_has_spectrogram(false);
            }
        }
    }

    /// Stores finished analyses and shows the one of the current track.
    fn collect_analyses(&self) {
        /// Analyses kept in memory (a spectrogram is about a megabyte).
        const KEPT_ANALYSES: usize = 6;
        let Some(worker) = &self.analysis_worker else {
            return;
        };
        let outcomes = worker.take_results();
        if outcomes.is_empty() {
            return;
        }
        let current = self.current_path();
        for (path, outcome) in outcomes {
            if current.as_ref() == Some(&path)
                && let Some(window) = self.window.upgrade()
            {
                let state = window.global::<AppState>();
                self.show_authenticity(&state, &outcome);
                if let Ok(view) = &outcome {
                    let size = std::fs::metadata(&path).ok().map(|metadata| metadata.len());
                    let cells = self
                        .selected_track
                        .get()
                        .and_then(|(album, track)| {
                            self.albums
                                .borrow()
                                .get(album)?
                                .tracks
                                .get(track)
                                .map(|file| crate::presentation::track_info_cells(file, size))
                        })
                        .unwrap_or_default();
                    self.show_track_info(&state, cells, Some(view));
                }
            }
            let mut analyses = self.analyses.borrow_mut();
            analyses.retain(|(analyzed, _)| *analyzed != path);
            analyses.push_back((path, outcome));
            while analyses.len() > KEPT_ANALYSES {
                analyses.pop_front();
            }
        }
    }

    /// Puts one analysis on the current-track page.
    fn show_authenticity(
        &self,
        state: &AppState<'_>,
        outcome: &Result<crate::authenticity::AuthenticityView, String>,
    ) {
        use crate::authenticity::VerdictTone;
        let view = match outcome {
            Ok(view) => view,
            Err(error) => {
                state.set_authenticity_state(3);
                state.set_authenticity_error(error.as_str().into());
                state.set_has_spectrogram(false);
                return;
            }
        };
        let cells = |items: &[crate::authenticity::LabeledText]| {
            ModelRc::new(VecModel::from(
                items
                    .iter()
                    .map(|item| crate::DataCell {
                        label: item.label.as_str().into(),
                        value: item.text.as_str().into(),
                    })
                    .collect::<Vec<_>>(),
            ))
        };
        state.set_authenticity_verdict(view.verdict.as_str().into());
        state.set_authenticity_tone(match view.tone {
            VerdictTone::Neutral => 0,
            VerdictTone::Good => 1,
            VerdictTone::Warning => 2,
        });
        state.set_authenticity_blurb(view.blurb.as_str().into());
        state.set_authenticity_strength(view.strength.as_str().into());
        state.set_authenticity_findings(cells(&view.findings));
        state.set_authenticity_measures(cells(&view.measures));
        state.set_authenticity_indicators(ModelRc::new(VecModel::from(
            view.indicators
                .iter()
                .map(|indicator| SharedString::from(indicator.as_str()))
                .collect::<Vec<_>>(),
        )));
        match &view.spectrogram {
            Some(pixels) => {
                let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                    &pixels.rgba,
                    pixels.width,
                    pixels.height,
                );
                state.set_spectrogram(slint::Image::from_rgba8(buffer));
                state.set_spectrogram_top_label(
                    format!(
                        "{} kHz",
                        crate::authenticity::number(view.spectrogram_max_hz / 1000.0, 0)
                    )
                    .into(),
                );
                state.set_spectrogram_middle_label(
                    format!(
                        "{} kHz",
                        crate::authenticity::number(view.spectrogram_max_hz / 2000.0, 0)
                    )
                    .into(),
                );
                state.set_spectrogram_end_label(
                    presentation::clock(view.duration_seconds.round() as u64).into(),
                );
                state.set_has_spectrogram(true);
            }
            None => state.set_has_spectrogram(false),
        }
        state.set_authenticity_state(2);
    }

    /// Shows the selected track in the system media panel.
    fn publish_media_track(&self) {
        let Some((album_index, track_index)) = self.selected_track.get() else {
            return;
        };
        let albums = self.albums.borrow();
        if let Some(album) = albums.get(album_index)
            && let Some(track) = album.tracks.get(track_index)
        {
            self.media_controls.borrow_mut().publish_track(
                &track.title,
                &track.artist,
                &album.title,
                Duration::from_secs(track.duration_seconds),
            );
        }
    }

    /// Position in the current track: the restored one, or what the engine has played.
    fn position_in_track(&self) -> Duration {
        if let Some(seconds) = self.resume_position_seconds.get() {
            return Duration::from_secs(seconds);
        }
        let rate = u64::from(self.sample_rate_hz.get().max(1));
        Duration::from_secs_f64(
            self.engine.counters().frames_rendered_in_track as f64 / rate as f64,
        )
    }

    /// Jumps in the current track (a restored track not opened yet starts there on Play).
    fn seek_to(&self, position: Duration) {
        if self.resume_position_seconds.get().is_some() {
            self.resume_position_seconds.set(Some(position.as_secs()));
        } else {
            self.engine.send(EngineCommand::SeekTo {
                position_in_track: position,
            });
        }
        self.publish_media_status_at(position);
    }

    /// Tells the system media panel whether the app plays, and where.
    fn publish_media_status(&self) {
        self.publish_media_status_at(self.position_in_track());
    }

    fn publish_media_status_at(&self, position: Duration) {
        use crate::media_controls::TransportStatus;
        let status = match self.state.get() {
            PlaybackState::Playing | PlaybackState::Opening => TransportStatus::Playing,
            PlaybackState::Paused => TransportStatus::Paused,
            PlaybackState::Stopped if self.selected_track.get().is_some() => {
                TransportStatus::Paused
            }
            PlaybackState::Stopped => TransportStatus::Stopped,
        };
        self.media_controls
            .borrow_mut()
            .publish_status(status, position);
    }

    /// Registers the media keys once the window exists, then applies what they ask.
    fn handle_media_commands(&self) {
        use crate::media_controls::MediaCommand;
        if let Some(window) = self.window.upgrade() {
            let is_new = self.media_controls.borrow_mut().install_once(&window);
            if is_new {
                self.publish_media_track();
                self.publish_media_status();
            }
        }
        let commands = self.media_controls.borrow().take_commands();
        for command in commands {
            let is_active = matches!(
                self.state.get(),
                PlaybackState::Playing | PlaybackState::Opening
            );
            match command {
                MediaCommand::Play if !is_active => self.toggle_play(),
                MediaCommand::Pause if is_active => self.toggle_play(),
                MediaCommand::Play | MediaCommand::Pause => {}
                MediaCommand::Toggle => self.toggle_play(),
                MediaCommand::Next => self.adjacent_track(true),
                MediaCommand::Previous => self.adjacent_track(false),
                MediaCommand::Stop => self.engine.send(EngineCommand::Stop),
                MediaCommand::SeekTo(position) => self.seek_to(position),
                MediaCommand::SeekBy { is_forward, amount } => {
                    let current = self.position_in_track();
                    self.seek_to(if is_forward {
                        current + amount
                    } else {
                        current.saturating_sub(amount)
                    });
                }
            }
        }
    }

    /// Reopens the last session: the queue, and its current track shown paused at the stored
    /// position. Nothing plays until the user presses Play.
    fn restore_session(&self) {
        let Some(session) = self
            .session_path()
            .and_then(|path| crate::session::Session::load_from(&path))
        else {
            return;
        };
        let queue = crate::play_queue::PlayQueue::from_snapshot(session.queue.clone());
        let location = queue.current().and_then(|path| self.track_location(path));
        *self.play_queue.borrow_mut() = queue;
        *self.last_saved_session.borrow_mut() = Some(session.clone());
        let (Some((album_index, track_index)), Some(window)) = (location, self.window.upgrade())
        else {
            return;
        };
        let state = window.global::<AppState>();
        let (sample_rate_hz, duration_seconds) = {
            let albums = self.albums.borrow();
            let Some(track) = albums
                .get(album_index)
                .and_then(|album| album.tracks.get(track_index))
            else {
                return;
            };
            (track.sample_rate_hz, track.duration_seconds)
        };
        self.selected_track.set(Some((album_index, track_index)));
        self.sample_rate_hz.set(sample_rate_hz);
        self.total_frames
            .set(duration_seconds * u64::from(sample_rate_hz));
        self.resume_position_seconds
            .set(Some(session.position_seconds.min(duration_seconds)));
        self.show_current_track(&state, album_index, track_index);
        self.refresh_queue_view();
    }

    /// The session file, beside the library database (so a preview never touches the user's).
    fn session_path(&self) -> Option<PathBuf> {
        self.library_database
            .as_deref()
            .map(crate::session::Session::path_beside)
    }

    /// Writes the session when it changed. Never before the stored one was read.
    pub(crate) fn save_session(&self) {
        if !self.is_session_restored.get() {
            return;
        }
        let rate = u64::from(self.sample_rate_hz.get().max(1));
        let position_seconds = match self.resume_position_seconds.get() {
            Some(seconds) => seconds,
            None if self.state.get() == PlaybackState::Stopped => 0,
            None => self.engine.counters().frames_rendered_in_track / rate,
        };
        let session = crate::session::Session {
            queue: self.play_queue.borrow().snapshot(),
            position_seconds,
        };
        if self.last_saved_session.borrow().as_ref() == Some(&session) {
            return;
        }
        let Some(path) = self.session_path() else {
            return;
        };
        // A failed write only loses the resume point, so it stays silent.
        if session.save_to(&path).is_ok() {
            *self.last_saved_session.borrow_mut() = Some(session);
        }
    }

    fn toggle_play(&self) {
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

    pub(crate) fn tick(&self) {
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
