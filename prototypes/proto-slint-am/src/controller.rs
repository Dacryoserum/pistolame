//! Fake transport and catalog state, and the glue between it and the Slint `AppState` global.

use crate::models;
use crate::theme::apply_tokens;
use crate::{AppState, AppWindow};
use common::catalog::{CatalogAlbum, no_results_message, signal_path, signal_summary};
use common::playback::{PAUSED_EQUALIZER_LEVELS, PlaybackState, equalizer_levels};
use common::tokens::{DARK, LIGHT};
use slint::{ComponentHandle, Image, Model, ModelRc, VecModel, Weak};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

/// Owns the playback state and the catalog; every Slint callback ends up here.
pub struct Controller {
    window: Weak<AppWindow>,
    albums: Vec<CatalogAlbum>,
    artworks: Vec<Image>,
    playback: RefCell<PlaybackState>,
    detail_album_index: Cell<usize>,
    is_dark: Cell<bool>,
    equalizer_model: Rc<VecModel<f32>>,
    started_at: Instant,
    last_frame_at: Cell<Instant>,
}

impl Controller {
    pub fn new(window: &AppWindow) -> Rc<Self> {
        let now = Instant::now();
        Rc::new(Self {
            window: window.as_weak(),
            albums: common::catalog::catalog_albums(),
            artworks: models::build_artworks(),
            playback: RefCell::new(PlaybackState::default()),
            detail_album_index: Cell::new(0),
            is_dark: Cell::new(true),
            equalizer_model: Rc::new(VecModel::from(PAUSED_EQUALIZER_LEVELS.to_vec())),
            started_at: now,
            last_frame_at: Cell::new(now),
        })
    }

    fn with_window(&self, action: impl FnOnce(&AppWindow)) {
        if let Some(window) = self.window.upgrade() {
            action(&window);
        }
    }

    /// Pushes the initial state; call once before showing the window.
    pub fn initialize(&self) {
        self.with_window(|window| {
            apply_tokens(window, &DARK);
            let app_state = window.global::<AppState>();
            app_state.set_equalizer_levels(ModelRc::from(self.equalizer_model.clone()));
            self.show_albums(&app_state, "");
            self.open_album_on(&app_state, 0);
            self.refresh_track_view(window);
            self.refresh_progress(&app_state);
        });
    }

    fn show_albums(&self, app_state: &AppState, query: &str) {
        let visible = models::visible_album_indices(&self.albums, query);
        let cards = models::album_cards(&self.albums, &visible, &self.artworks);
        app_state.set_albums(ModelRc::new(VecModel::from(cards)));
        app_state.set_no_results_text(no_results_message(query).into());
    }

    /// Refreshes everything that depends on the current track, album or volume.
    fn refresh_track_view(&self, window: &AppWindow) {
        let app_state = window.global::<AppState>();
        let playback = self.playback.borrow();
        let album = &self.albums[playback.album_index];
        let track = playback.current_track(&self.albums);
        let summary = signal_summary(track.format, playback.volume_percent);
        app_state.set_lcd_title(track.title.as_str().into());
        app_state.set_lcd_subtitle(album.lcd_subtitle().into());
        app_state.set_lcd_artwork(self.artworks[album.artwork_variant].clone());
        app_state.set_is_playing(playback.is_playing);
        app_state.set_badge_label(summary.badge_label.into());
        app_state.set_is_bit_perfect(summary.is_bit_perfect);
        app_state.set_signal_title(summary.title.into());
        app_state.set_signal_description(summary.description.into());
        let steps = signal_path(track.format, playback.volume_percent);
        app_state.set_path_steps(ModelRc::new(VecModel::from(models::path_steps(steps))));
        app_state.set_playing_album_index(playback.album_index as i32);
        app_state.set_playing_track_index(playback.track_index as i32);
        self.refresh_equalizer(playback.is_playing);
        self.refresh_detail_signal(&app_state, playback.volume_percent);
    }

    /// The detail page shows the signal of its own album at the current volume.
    fn refresh_detail_signal(&self, app_state: &AppState, volume_percent: u8) {
        let album = &self.albums[self.detail_album_index.get()];
        let summary = signal_summary(album.format, volume_percent);
        app_state.set_detail_badge_label(summary.badge_label.into());
        app_state.set_detail_is_bit_perfect(summary.is_bit_perfect);
        let steps = signal_path(album.format, volume_percent);
        app_state.set_detail_path_steps(ModelRc::new(VecModel::from(models::path_steps(steps))));
    }

    fn refresh_equalizer(&self, is_playing: bool) {
        let levels = if is_playing {
            equalizer_levels(self.started_at.elapsed().as_secs_f32())
        } else {
            PAUSED_EQUALIZER_LEVELS
        };
        for (row, level) in levels.into_iter().enumerate() {
            self.equalizer_model.set_row_data(row, level);
        }
    }

    fn refresh_progress(&self, app_state: &AppState) {
        let playback = self.playback.borrow();
        app_state.set_progress(playback.progress_fraction(&self.albums));
        app_state.set_elapsed_label(playback.elapsed_label().into());
        app_state.set_remaining_label(playback.remaining_label(&self.albums).into());
        app_state.set_md5_label(playback.md5_progress_label(&self.albums).into());
    }

    /// Applies a change of the transport, then refreshes the full playback view.
    fn change_playback(&self, change: impl FnOnce(&mut PlaybackState, &[CatalogAlbum])) {
        change(&mut self.playback.borrow_mut(), &self.albums);
        self.last_frame_at.set(Instant::now());
        self.with_window(|window| {
            self.refresh_track_view(window);
            self.refresh_progress(&window.global::<AppState>());
        });
    }

    /// Called every ~16 ms by the timer; does nothing while paused.
    pub fn tick(&self) {
        let now = Instant::now();
        let delta_seconds = now
            .duration_since(self.last_frame_at.replace(now))
            .as_secs_f32();
        let did_track_change = {
            let mut playback = self.playback.borrow_mut();
            if !playback.is_playing {
                return;
            }
            playback.advance(delta_seconds, &self.albums)
        };
        self.with_window(|window| {
            if did_track_change {
                self.refresh_track_view(window);
            } else {
                self.refresh_equalizer(true);
            }
            self.refresh_progress(&window.global::<AppState>());
        });
    }

    pub fn search_changed(&self, query: &str) {
        self.with_window(|window| self.show_albums(&window.global::<AppState>(), query));
    }

    pub fn open_album(&self, album_index: usize) {
        self.with_window(|window| self.open_album_on(&window.global::<AppState>(), album_index));
    }

    fn open_album_on(&self, app_state: &AppState, album_index: usize) {
        self.detail_album_index.set(album_index);
        let album = &self.albums[album_index];
        app_state.set_detail_album_index(album_index as i32);
        app_state.set_detail(models::detail_info(album_index, album, &self.artworks));
        app_state.set_detail_tracks(ModelRc::new(VecModel::from(models::track_rows(album))));
        self.refresh_detail_signal(app_state, self.playback.borrow().volume_percent);
    }

    pub fn play_track(&self, album_index: usize, track_index: usize) {
        self.change_playback(|playback, _| playback.play_track(album_index, track_index));
    }

    pub fn play_detail_album(&self) {
        self.play_track(self.detail_album_index.get(), 0);
    }

    pub fn shuffle_detail_album(&self) {
        let album_index = self.detail_album_index.get();
        self.play_track(album_index, self.albums[album_index].tracks.len() / 2);
    }

    pub fn toggle_play(&self) {
        self.change_playback(|playback, _| playback.toggle_play());
    }

    pub fn previous_track(&self) {
        self.change_playback(|playback, albums| playback.previous_track(albums));
    }

    pub fn next_track(&self) {
        self.change_playback(|playback, albums| playback.next_track(albums));
    }

    pub fn seek(&self, fraction: f32) {
        self.playback
            .borrow_mut()
            .seek_to_fraction(fraction, &self.albums);
        self.with_window(|window| self.refresh_progress(&window.global::<AppState>()));
    }

    pub fn set_volume(&self, volume_percent: u8) {
        self.change_playback(|playback, _| playback.volume_percent = volume_percent);
    }

    pub fn toggle_theme(&self) {
        let is_dark = !self.is_dark.get();
        self.is_dark.set(is_dark);
        self.with_window(|window| {
            apply_tokens(window, if is_dark { &DARK } else { &LIGHT });
            window.global::<AppState>().set_is_dark(is_dark);
        });
    }
}
