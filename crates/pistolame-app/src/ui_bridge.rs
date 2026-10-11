//! The UI owns this bridge; only commands, events and atomic counters cross into audio threads.
use crate::{AlbumCard, AppState, AppWindow, DetailInfo, PathStep, TrackRowData};
use crate::{presentation, settings::AppSettings};
use crossbeam_channel::{Receiver, Sender, unbounded};
use pistolame_core::*;
use pistolame_library::MusicFile;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

struct Album {
    title: String,
    artist: String,
    directory: PathBuf,
    tracks: Vec<MusicFile>,
    artwork: slint::Image,
}
struct Imported {
    files: Vec<MusicFile>,
    errors: Vec<String>,
}

pub(crate) struct UiBridge {
    window: slint::Weak<AppWindow>,
    engine: EngineHandle,
    settings: RefCell<AppSettings>,
    devices: RefCell<Vec<OutputDeviceInfo>>,
    albums: RefCell<Vec<Album>>,
    imports: Receiver<Imported>,
    import_sender: Sender<Imported>,
    is_importing: Cell<bool>,
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
            devices: RefCell::new(Vec::new()),
            albums: RefCell::new(Vec::new()),
            imports: receiver,
            import_sender: sender,
            is_importing: Cell::new(false),
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
        let state = window.global::<AppState>();
        let handle = self.clone();
        state.on_import_files(move || {
            if !handle.is_importing.get()
                && let Some(paths) = rfd::FileDialog::new()
                    .add_filter("FLAC", &["flac"])
                    .pick_files()
            {
                handle.import(paths);
            }
        });
        let handle = self.clone();
        state.on_import_folder(move || {
            if !handle.is_importing.get()
                && let Some(path) = rfd::FileDialog::new().pick_folder()
            {
                handle.import(vec![path]);
            }
        });
        let handle = self.clone();
        state.on_search_changed(move |query| handle.update_grid(query.as_str()));
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

    pub(crate) fn import(&self, paths: Vec<PathBuf>) {
        self.is_importing.set(true);
        if let Some(window) = self.window.upgrade() {
            window
                .global::<AppState>()
                .set_import_status("Lecture des fichiers…".into());
        }
        let sender = self.import_sender.clone();
        let result = std::thread::Builder::new()
            .name("pistolame-import".into())
            .spawn(move || {
                let (paths, mut errors) = pistolame_library::collect_flac_paths(&paths);
                let mut files = Vec::new();
                let mut covers = std::collections::HashMap::new();
                for path in paths {
                    match pistolame_library::read_music_file(&path) {
                        Ok(mut file) => {
                            // Albums share one encoded cover, rather than retaining a copy per track.
                            let key = (path.parent().map(ToOwned::to_owned), file.album.clone());
                            if let Some(cover) = covers.get(&key) {
                                file.artwork = Some(std::sync::Arc::clone(cover));
                            } else if let Some(cover) = &file.artwork {
                                covers.insert(key, cover.clone());
                            }
                            files.push(file);
                        }
                        Err(error) => errors.push(format!("{} : {error}", path.display())),
                    }
                }
                let _ = sender.send(Imported { files, errors });
            });
        if let Err(error) = result {
            self.is_importing.set(false);
            self.message(format!("Impossible de démarrer l’import : {error}"));
        }
    }

    fn finish_import(&self, imported: Imported) {
        self.is_importing.set(false);
        let selected_path = self.selected_track.get().and_then(|(album, track)| {
            self.albums
                .borrow()
                .get(album)
                .and_then(|album| album.tracks.get(track))
                .map(|track| track.path.clone())
        });
        let mut albums = self.albums.borrow_mut();
        for file in imported.files {
            if albums
                .iter()
                .any(|album| album.tracks.iter().any(|track| track.path == file.path))
            {
                continue;
            }
            let directory = file
                .path
                .parent()
                .unwrap_or_else(|| std::path::Path::new(""))
                .to_owned();
            let existing = albums
                .iter()
                .position(|album| album.directory == directory && album.title == file.album);
            let index = existing.unwrap_or_else(|| {
                let artwork = file
                    .artwork
                    .as_ref()
                    .and_then(|bytes| slint::Image::load_from_data(bytes, None).ok())
                    .unwrap_or_default();
                albums.push(Album {
                    title: file.album.clone(),
                    artist: file.artist.clone(),
                    directory,
                    tracks: Vec::new(),
                    artwork,
                });
                albums.len() - 1
            });
            albums[index].tracks.push(file);
        }
        for album in albums.iter_mut() {
            album.tracks.sort_by(|left, right| {
                (left.disc_number, left.track_number, &left.path).cmp(&(
                    right.disc_number,
                    right.track_number,
                    &right.path,
                ))
            });
        }
        if let Some(path) = selected_path {
            self.selected_track
                .set(albums.iter().enumerate().find_map(|(album, entry)| {
                    entry
                        .tracks
                        .iter()
                        .position(|track| track.path == path)
                        .map(|track| (album, track))
                }));
        }
        let count: usize = albums.iter().map(|album| album.tracks.len()).sum();
        drop(albums);
        if let Some(window) = self.window.upgrade() {
            let state = window.global::<AppState>();
            state.set_import_status(
                format!(
                    "{count} morceau{} · {} album{}",
                    if count == 1 { "" } else { "x" },
                    self.albums.borrow().len(),
                    if self.albums.borrow().len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                )
                .into(),
            );
            self.update_grid(state.get_query().as_str());
            self.show_album(self.detail_album.get());
            if let Some((album, track)) = self.selected_track.get() {
                state.set_playing_album_index(album as i32);
                state.set_playing_track_index(track as i32);
            }
        }
        if let Some(first) = imported.errors.first() {
            self.message(format!(
                "{} problème(s) d’import. {first}",
                imported.errors.len()
            ));
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
                format_label: format!(
                    "{}/{}",
                    track.bits_per_sample,
                    presentation::frequency(track.sample_rate_hz)
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

    fn play_track(&self, album: usize, track: usize) {
        let albums = self.albums.borrow();
        let Some(file) = albums.get(album).and_then(|album| album.tracks.get(track)) else {
            return;
        };
        self.selected_track.set(Some((album, track)));
        self.should_advance.set(true);
        self.stream_end_baseline
            .set(self.engine.counters().stream_ends_reached);
        self.message(String::new());
        self.engine.send(EngineCommand::OpenFileAndPlay {
            path: file.path.clone(),
        });
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
        let Some((album, track)) = self.selected_track.get() else {
            return;
        };
        let count = self
            .albums
            .borrow()
            .get(album)
            .map_or(0, |entry| entry.tracks.len());
        if count == 0 {
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
        let shuffle = self
            .window
            .upgrade()
            .is_some_and(|window| window.global::<AppState>().get_is_shuffle_on());
        let target = if next && shuffle && count > 1 {
            (track + 1 + self.random_track(count - 1)) % count
        } else if next {
            (track + 1) % count
        } else {
            (track + count - 1) % count
        };
        self.play_track(album, target);
    }

    fn advance_after_end(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let Some((album, track)) = self.selected_track.get() else {
            return;
        };
        let count = self
            .albums
            .borrow()
            .get(album)
            .map_or(0, |entry| entry.tracks.len());
        if crate::transport::can_advance(
            track,
            count,
            state.get_is_repeat_on(),
            state.get_is_shuffle_on(),
        ) {
            self.adjacent_track(true);
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
        for imported in self.imports.try_iter() {
            self.finish_import(imported);
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
                            "FLAC {} bits / {} kHz",
                            track.bits_per_sample,
                            presentation::frequency(track.sample_rate_hz)
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
                let steps = presentation::signal_steps(&value)
                    .into_iter()
                    .map(|(label, value, is_transparent)| PathStep {
                        label: label.into(),
                        value: value.into(),
                        is_transparent,
                    })
                    .collect::<Vec<_>>();
                state.set_path_steps(ModelRc::new(VecModel::from(steps)));
                self.update_detail_signal();
            }
            EngineEvent::SignalPathChanged { assessment: None } => {
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
