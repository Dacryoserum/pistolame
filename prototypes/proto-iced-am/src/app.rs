//! Application state, messages, update loop and subscriptions.

use std::time::Instant;

use common::catalog::{AuthenticityFinding, CatalogAlbum, catalog_albums};
use common::playback::PlaybackState;
use common::spectrogram::{SPECTROGRAM_HEIGHT_PIXELS, SPECTROGRAM_WIDTH_PIXELS, spectrogram_rgba};
use iced::keyboard::{self, Key, key};
use iced::widget::{Id, image, operation};
use iced::{Size, Subscription, Task, Theme, theme, window};

use crate::assets::{Icons, load_artworks};
use crate::style::{color, tokens};

pub const INITIAL_WINDOW_WIDTH: f32 = 1280.0;
pub const INITIAL_WINDOW_HEIGHT: f32 = 820.0;
/// Sidebar item that shows the albums grid.
pub const ALBUMS_SIDEBAR_ITEM: usize = 2;
/// Frame gaps above this (play resumed after a pause) are ignored, not integrated.
const MAX_FRAME_DELTA_SECONDS: f32 = 0.1;

pub fn grid_scroll_id() -> Id {
    Id::new("grid-scroll")
}

pub fn detail_scroll_id() -> Id {
    Id::new("detail-scroll")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Grid,
    Detail(usize),
    Placeholder(usize),
}

#[derive(Debug, Clone)]
pub enum Message {
    Frame(Instant),
    WindowResized(Size),
    SidebarSelected(usize),
    SearchChanged(String),
    AlbumOpened(usize),
    AlbumPlayRequested(usize),
    BackPressed,
    TrackPressed(usize, usize),
    ShuffleAlbumPressed(usize),
    TogglePlay,
    PreviousPressed,
    NextPressed,
    ToggleShuffle,
    ToggleRepeat,
    Seek(f32),
    VolumeChanged(u8),
    PopoverToggled,
    PopoverClosed,
    ThemeToggled,
    GridScrolled(f32),
    LcdHoverChanged(bool),
    EscapePressed,
    SpacePressed,
}

pub struct App {
    pub albums: Vec<CatalogAlbum>,
    pub icons: Icons,
    pub artworks: Vec<image::Handle>,
    pub is_dark: bool,
    pub selected_sidebar_item: usize,
    pub view: View,
    pub query: String,
    pub visible: Vec<usize>,
    pub playback: PlaybackState,
    pub is_popover_open: bool,
    pub is_shuffle_on: bool,
    pub is_repeat_on: bool,
    pub window_size: Size,
    pub grid_scroll_offset: f32,
    pub detail_spectrogram: Option<image::Handle>,
    pub is_lcd_hovered: bool,
    pub equalizer_time_seconds: f32,
    last_frame: Option<Instant>,
}

impl App {
    pub fn new() -> Self {
        let albums = catalog_albums();
        let visible = (0..albums.len()).collect();
        Self {
            albums,
            icons: Icons::load(),
            artworks: load_artworks(),
            is_dark: true,
            selected_sidebar_item: ALBUMS_SIDEBAR_ITEM,
            view: View::Grid,
            query: String::new(),
            visible,
            playback: PlaybackState::default(),
            is_popover_open: false,
            is_shuffle_on: false,
            is_repeat_on: false,
            window_size: Size::new(INITIAL_WINDOW_WIDTH, INITIAL_WINDOW_HEIGHT),
            grid_scroll_offset: 0.0,
            detail_spectrogram: None,
            is_lcd_hovered: false,
            equalizer_time_seconds: 0.0,
            last_frame: None,
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Frame(now) => self.advance_frame(now),
            Message::WindowResized(size) => self.window_size = size,
            Message::SidebarSelected(item) => {
                self.selected_sidebar_item = item;
                self.is_popover_open = false;
                self.view = if item == ALBUMS_SIDEBAR_ITEM {
                    View::Grid
                } else {
                    View::Placeholder(item)
                };
            }
            Message::SearchChanged(query) => return self.apply_search(query),
            Message::AlbumOpened(album_index) => return self.open_album(album_index),
            Message::AlbumPlayRequested(album_index) => {
                self.playback.play_track(album_index, 0);
            }
            Message::BackPressed => return self.go_back_to_grid(),
            Message::TrackPressed(album_index, track_index) => {
                self.playback.play_track(album_index, track_index);
            }
            Message::ShuffleAlbumPressed(album_index) => {
                self.is_shuffle_on = true;
                let middle_track = self.albums[album_index].tracks.len() / 2;
                self.playback.play_track(album_index, middle_track);
            }
            Message::TogglePlay | Message::SpacePressed => self.playback.toggle_play(),
            Message::PreviousPressed => self.playback.previous_track(&self.albums),
            Message::NextPressed => self.playback.next_track(&self.albums),
            Message::ToggleShuffle => self.is_shuffle_on = !self.is_shuffle_on,
            Message::ToggleRepeat => self.is_repeat_on = !self.is_repeat_on,
            Message::Seek(fraction) => self.playback.seek_to_fraction(fraction, &self.albums),
            Message::VolumeChanged(volume_percent) => {
                self.playback.volume_percent = volume_percent;
            }
            Message::PopoverToggled => self.is_popover_open = !self.is_popover_open,
            Message::PopoverClosed => self.is_popover_open = false,
            Message::ThemeToggled => self.is_dark = !self.is_dark,
            Message::GridScrolled(offset) => self.grid_scroll_offset = offset,
            Message::LcdHoverChanged(is_hovered) => self.is_lcd_hovered = is_hovered,
            Message::EscapePressed => {
                if self.is_popover_open {
                    self.is_popover_open = false;
                } else if matches!(self.view, View::Detail(_)) {
                    return self.go_back_to_grid();
                }
            }
        }
        Task::none()
    }

    fn advance_frame(&mut self, now: Instant) {
        let delta_seconds = self
            .last_frame
            .map(|previous| (now - previous).as_secs_f32())
            .filter(|delta| *delta <= MAX_FRAME_DELTA_SECONDS)
            .unwrap_or(0.0);
        self.last_frame = Some(now);
        self.equalizer_time_seconds += delta_seconds;
        self.playback.advance(delta_seconds, &self.albums);
    }

    fn apply_search(&mut self, query: String) -> Task<Message> {
        self.visible = (0..self.albums.len())
            .filter(|index| self.albums[*index].matches_query(&query))
            .collect();
        self.query = query;
        self.selected_sidebar_item = ALBUMS_SIDEBAR_ITEM;
        self.view = View::Grid;
        self.grid_scroll_offset = 0.0;
        operation::snap_to(grid_scroll_id(), operation::RelativeOffset::START)
    }

    fn open_album(&mut self, album_index: usize) -> Task<Message> {
        let album = &self.albums[album_index];
        let pixels = spectrogram_rgba(
            album.authenticity.cutoff_fraction_of_nyquist(album.format),
            album.authenticity.finding == AuthenticityFinding::LossyTranscode,
            album_index as u64 + 1,
        );
        self.detail_spectrogram = Some(image::Handle::from_rgba(
            SPECTROGRAM_WIDTH_PIXELS,
            SPECTROGRAM_HEIGHT_PIXELS,
            pixels,
        ));
        self.view = View::Detail(album_index);
        operation::snap_to(detail_scroll_id(), operation::RelativeOffset::START)
    }

    fn go_back_to_grid(&mut self) -> Task<Message> {
        self.view = View::Grid;
        operation::scroll_to(
            grid_scroll_id(),
            operation::AbsoluteOffset {
                x: None,
                y: Some(self.grid_scroll_offset),
            },
        )
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let frames = if self.playback.is_playing {
            window::frames().map(Message::Frame)
        } else {
            Subscription::none()
        };
        Subscription::batch([
            frames,
            window::resize_events().map(|(_, size)| Message::WindowResized(size)),
            // Only yields events no widget captured: Space typed in the search field is safe.
            keyboard::listen().filter_map(key_to_message),
        ])
    }

    /// Window background and default text color follow the active token table.
    pub fn window_style(&self, _theme: &Theme) -> theme::Style {
        let tokens = tokens(self.is_dark);
        theme::Style {
            background_color: color(tokens.surface_content),
            text_color: color(tokens.text_primary),
        }
    }
}

fn key_to_message(event: keyboard::Event) -> Option<Message> {
    let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
        return None;
    };
    match key.as_ref() {
        Key::Named(key::Named::Escape) => Some(Message::EscapePressed),
        Key::Named(key::Named::Space) => Some(Message::SpacePressed),
        Key::Character("t") if modifiers.control() => Some(Message::ThemeToggled),
        _ => None,
    }
}
