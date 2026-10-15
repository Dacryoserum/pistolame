//! Navigation wiring of the UI bridge: user intents become history moves
//! ([`crate::navigation::NavigationHistory`]) and the current route is rendered into `AppState`.
//!
//! Rust owns navigation; Slint only shows `view`, `section` and the per-page scroll offsets and
//! forwards intents through callbacks. See `docs/plans/navigation.md` §3.3 and §3.4.

use super::UiBridge;
use crate::navigation::{Page, Route, SidebarSection};
use crate::settings::AppSettings;
use crate::{AppState, ViewKind};
use slint::ComponentHandle;
use std::rc::Rc;
use std::time::Duration;

/// Delay before a restored scroll offset is written a second time. After a long jump, a Slint
/// 1.18 `ListView` lays its rows out from the start of the row under the offset ("random seek"
/// in `i-slint-core` `model/repeater.rs`), which moves the offset by up to one row; once that
/// frame is laid out, writing the offset again is a short move it keeps exactly. Measured on the
/// album grid: a zero-delay timer fires before the frame, two frames at 60 Hz are enough.
const SCROLL_RESTORE_SETTLE_DELAY: Duration = Duration::from_millis(32);

/// Title of the current-track page, as its header shows it.
const NOW_PLAYING_TITLE: &str = "À l'écoute";

/// The plain album grid: no artist filter, no search.
fn all_albums_page() -> Page {
    Page::top_level_for(SidebarSection::Albums)
}

/// True for an album grid showing search results (and not one artist's albums).
fn is_search_page(page: &Page) -> bool {
    matches!(page, Page::Albums { artist_filter: None, query } if !query.is_empty())
}

/// The page the app opens on: the remembered library section, Albums when none (or unknown).
pub(super) fn start_page(settings: &AppSettings) -> Page {
    Page::top_level_for(stored_start_section(settings))
}

/// The library section stored in the settings, Albums when none is stored or it is unknown.
fn stored_start_section(settings: &AppSettings) -> SidebarSection {
    settings
        .start_section
        .as_deref()
        .and_then(SidebarSection::from_settings_key)
        .unwrap_or(SidebarSection::Albums)
}

/// The Slint enum value highlighting `section` in the sidebar.
pub(crate) fn slint_sidebar_section(section: SidebarSection) -> crate::SidebarSection {
    match section {
        SidebarSection::RecentlyAdded => crate::SidebarSection::RecentlyAdded,
        SidebarSection::Artists => crate::SidebarSection::Artists,
        SidebarSection::Albums => crate::SidebarSection::Albums,
        SidebarSection::Tracks => crate::SidebarSection::Tracks,
        SidebarSection::Settings => crate::SidebarSection::Settings,
        SidebarSection::Playlist(_) => crate::SidebarSection::Playlist,
    }
}

/// The section a Slint sidebar click asks for; `None` for `playlist`, which carries no id
/// (playlist items go through `open-playlist`).
fn section_from_slint(section: crate::SidebarSection) -> Option<SidebarSection> {
    match section {
        crate::SidebarSection::RecentlyAdded => Some(SidebarSection::RecentlyAdded),
        crate::SidebarSection::Artists => Some(SidebarSection::Artists),
        crate::SidebarSection::Albums => Some(SidebarSection::Albums),
        crate::SidebarSection::Tracks => Some(SidebarSection::Tracks),
        crate::SidebarSection::Settings => Some(SidebarSection::Settings),
        crate::SidebarSection::Playlist => None,
    }
}

/// The view that renders `page`.
fn view_of(page: &Page) -> ViewKind {
    match page {
        Page::RecentlyAdded => ViewKind::RecentlyAdded,
        Page::Artists => ViewKind::Artists,
        Page::Albums { .. } => ViewKind::Grid,
        Page::Tracks => ViewKind::Tracks,
        Page::Settings => ViewKind::Settings,
        Page::AlbumDetail { .. } => ViewKind::Detail,
        Page::Playlist { .. } => ViewKind::Playlist,
        Page::NowPlaying => ViewKind::NowPlaying,
    }
}

impl UiBridge {
    /// Connects every navigation callback of `AppState` to the history.
    pub(super) fn connect_navigation(self: &Rc<Self>, state: &AppState<'_>) {
        let handle = self.clone();
        state.on_navigate_to_section(move |section| {
            if let Some(section) = section_from_slint(section) {
                handle.navigate_to(Page::top_level_for(section));
            }
        });
        let handle = self.clone();
        state.on_navigate_back(move || handle.navigate_back());
        let handle = self.clone();
        state.on_navigate_forward(move || handle.navigate_forward());
        let handle = self.clone();
        state.on_show_now_playing(move || handle.show_now_playing());
        let handle = self.clone();
        state.on_search_changed(move |text| handle.search_changed(text.as_str()));
        let handle = self.clone();
        state.on_open_album(move |index| {
            if let Ok(index) = usize::try_from(index) {
                handle.open_album_at(index);
            }
        });
        let handle = self.clone();
        state.on_open_artist(move |index| handle.open_artist(index));
        let handle = self.clone();
        state.on_open_current_album(move || {
            if let Some((album_index, _)) = handle.selected_track.get() {
                handle.open_album_at(album_index);
            }
        });
        let handle = self.clone();
        state.on_open_current_artist(move || {
            let artist = handle.selected_track.get().and_then(|(album_index, _)| {
                handle
                    .albums
                    .borrow()
                    .get(album_index)
                    .and_then(|album| album.artists.first().cloned())
            });
            if let Some(artist) = artist {
                handle.open_artist_named(&artist);
            }
        });
        let handle = self.clone();
        state.on_open_artist_named(move |artist| handle.open_artist_named(&artist));
        let handle = self.clone();
        state.on_open_playlist(move |index| {
            if let Ok(index) = usize::try_from(index) {
                handle.open_playlist(index);
            }
        });
    }

    /// Opens `page` as a new history entry (no-op when already shown).
    pub(super) fn navigate_to(&self, page: Page) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let leaving_content_y =
            Self::read_content_y(&state, &self.navigation.borrow().current().page);
        let has_changed = self.navigation.borrow_mut().push(page, leaving_content_y);
        if has_changed {
            self.apply_current_route();
        } else {
            state.set_is_popover_open(false);
        }
    }

    /// True while a modal sheet (album editor, playlist editor, add-to-playlist) is open: Back
    /// and Forward from keys or mouse buttons must not change the page behind it.
    fn is_modal_sheet_open(state: &AppState<'_>) -> bool {
        state.get_is_album_editor_open()
            || state.get_is_playlist_editor_open()
            || state.get_is_add_to_playlist_open()
    }

    /// Returns to the previous available page, restoring its state and scroll.
    pub(super) fn navigate_back(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        if Self::is_modal_sheet_open(&state) {
            return;
        }
        let leaving_content_y =
            Self::read_content_y(&state, &self.navigation.borrow().current().page);
        let has_moved = self
            .navigation
            .borrow_mut()
            .go_back(leaving_content_y, |page| self.is_page_available(page))
            .is_some();
        if has_moved {
            self.replace_current_if_unavailable();
            self.apply_current_route();
        }
    }

    /// Re-applies the page just left by Back, with its state and scroll.
    pub(super) fn navigate_forward(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        if Self::is_modal_sheet_open(&state) {
            return;
        }
        let leaving_content_y =
            Self::read_content_y(&state, &self.navigation.borrow().current().page);
        let has_moved = self
            .navigation
            .borrow_mut()
            .go_forward(leaving_content_y, |page| self.is_page_available(page))
            .is_some();
        if has_moved {
            self.replace_current_if_unavailable();
            self.apply_current_route();
        }
    }

    /// Back's fallback when the stack is empty is the section page, which availability does not
    /// check: a deleted playlist's section would show a dead page, so the plain grid replaces it.
    fn replace_current_if_unavailable(&self) {
        let current_page = self.navigation.borrow().current().page.clone();
        if !self.is_page_available(&current_page) {
            self.navigation
                .borrow_mut()
                .replace_current(all_albums_page());
        }
    }

    /// Shows the current route: view, section, filter, query, models, then scroll; updates
    /// can-go-back/forward, is-sub-page, back-label; remembers the start section.
    pub(super) fn apply_current_route(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let route = self.navigation.borrow().current().clone();
        let generation = self.route_generation.get().wrapping_add(1);
        self.route_generation.set(generation);
        state.set_is_popover_open(false);
        // A selection belongs to the page it was made on.
        self.clear_selection();
        // Data before scroll, so the restored offset is reachable.
        self.fill_page(&state, &route.page);
        state.set_view(view_of(&route.page));
        self.show_route_section(&state, &route);
        Self::write_content_y(&state, &route.page, route.content_y_logical_pixels);
        if route.content_y_logical_pixels != 0.0 {
            self.write_content_y_after_layout(
                route.page.clone(),
                route.content_y_logical_pixels,
                generation,
            );
        }
        self.show_history_flags(&state);
        self.remember_start_section(&route);
    }

    /// Writes the restored offset again once the page has been laid out (see
    /// [`SCROLL_RESTORE_SETTLE_DELAY`]), unless another route was shown in the meantime.
    fn write_content_y_after_layout(
        &self,
        page: Page,
        content_y_logical_pixels: f32,
        generation: u64,
    ) {
        let window = self.window.clone();
        let route_generation = self.route_generation.clone();
        slint::Timer::single_shot(SCROLL_RESTORE_SETTLE_DELAY, move || {
            if route_generation.get() != generation {
                return;
            }
            if let Some(window) = window.upgrade() {
                Self::write_content_y(
                    &window.global::<AppState>(),
                    &page,
                    content_y_logical_pixels,
                );
            }
        });
    }

    /// Sets the filter and the query the page shows, and fills the models it needs.
    fn fill_page(&self, state: &AppState<'_>, page: &Page) {
        match page {
            Page::Albums {
                artist_filter,
                query,
            } => {
                state.set_artist_filter(artist_filter.as_deref().unwrap_or_default().into());
                state.set_query(query.as_str().into());
                self.update_grid(query);
            }
            Page::AlbumDetail { album } => {
                let index = self
                    .albums
                    .borrow()
                    .iter()
                    .position(|candidate| candidate.key == *album);
                if let Some(index) = index {
                    self.show_album(index);
                }
            }
            // Over the page it was opened from: that page's filter and query stay as they are.
            // The analysis waits for this page (it decodes the whole file on several cores).
            Page::NowPlaying => {
                if let Some(path) = self.current_path() {
                    self.show_authenticity_for(&path, true);
                }
            }
            Page::Playlist { playlist_id } => {
                Self::clear_grid_filters(state);
                state.set_selected_playlist(self.playlist_index_of(*playlist_id));
                self.refresh_playlist_view();
            }
            Page::RecentlyAdded | Page::Artists | Page::Tracks | Page::Settings => {
                Self::clear_grid_filters(state);
            }
        }
    }

    /// Empties the search field and the artist filter (top-level pages other than the grid).
    fn clear_grid_filters(state: &AppState<'_>) {
        state.set_query("".into());
        state.set_artist_filter("".into());
    }

    /// Re-shows the highlight and the history flags after the library or the playlists changed
    /// under the same page. The page's data were refilled by the caller; its scroll is kept.
    fn refresh_current_route(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let route = self.navigation.borrow().current().clone();
        self.show_route_section(&state, &route);
        self.show_history_flags(&state);
    }

    /// Highlights the route's sidebar entry (and playlist, by its current index).
    fn show_route_section(&self, state: &AppState<'_>, route: &Route) {
        state.set_section(slint_sidebar_section(route.section));
        let playlist_index = match route.section {
            SidebarSection::Playlist(playlist_id) => self.playlist_index_of(playlist_id),
            _ => -1,
        };
        state.set_selected_playlist(playlist_index);
    }

    /// Back/Forward availability, sub-page flag and the back button's label.
    fn show_history_flags(&self, state: &AppState<'_>) {
        let navigation = self.navigation.borrow();
        state.set_can_go_back(navigation.can_go_back());
        state.set_can_go_forward(navigation.can_go_forward());
        state.set_is_sub_page(navigation.current().page.is_sub_page());
        let back_label = navigation
            .back_destination()
            .map(|page| self.page_title(&page))
            .unwrap_or_default();
        state.set_back_label(back_label.into());
    }

    /// Stores the library section shown, so the next launch opens on it. Settings, playlists
    /// and sub-pages are never stored: opening on them would surprise.
    fn remember_start_section(&self, route: &Route) {
        if !self.should_remember_start_section.get() {
            return;
        }
        let is_library_section_page = matches!(
            route.page,
            Page::RecentlyAdded | Page::Artists | Page::Tracks
        ) || route.page == all_albums_page();
        let Some(key) = route.section.settings_key() else {
            return;
        };
        if !is_library_section_page
            || stored_start_section(&self.settings.borrow()) == route.section
        {
            return;
        }
        self.settings.borrow_mut().start_section = Some(key.to_owned());
        self.save_settings();
    }

    /// Whether `page` can still be shown: its album or playlist exists, or a track is current.
    pub(super) fn is_page_available(&self, page: &Page) -> bool {
        match page {
            Page::AlbumDetail { album } => self
                .albums
                .borrow()
                .iter()
                .any(|candidate| candidate.key == *album),
            Page::Playlist { playlist_id } => self.playlist_index_of(*playlist_id) >= 0,
            // Bridge state, not the `has-current-track` UI flag, which flickers to false at each
            // TrackLoaded.
            Page::NowPlaying => {
                self.selected_track.get().is_some() || self.track_id.get().is_some()
            }
            Page::RecentlyAdded
            | Page::Artists
            | Page::Albums { .. }
            | Page::Tracks
            | Page::Settings => true,
        }
    }

    /// The scroll offset of the component showing `page`.
    fn read_content_y(state: &AppState<'_>, page: &Page) -> f32 {
        match page {
            Page::RecentlyAdded => state.get_recent_scroll_y(),
            Page::Artists => state.get_artists_scroll_y(),
            Page::Albums { .. } => state.get_grid_scroll_y(),
            Page::Tracks => state.get_tracks_scroll_y(),
            Page::Settings => state.get_settings_scroll_y(),
            Page::AlbumDetail { .. } => state.get_detail_scroll_y(),
            Page::Playlist { .. } => state.get_playlist_scroll_y(),
            Page::NowPlaying => state.get_now_playing_scroll_y(),
        }
    }

    /// Scrolls the component showing `page` to `content_y_logical_pixels` (stored verbatim).
    fn write_content_y(state: &AppState<'_>, page: &Page, content_y_logical_pixels: f32) {
        match page {
            Page::RecentlyAdded => state.set_recent_scroll_y(content_y_logical_pixels),
            Page::Artists => state.set_artists_scroll_y(content_y_logical_pixels),
            Page::Albums { .. } => state.set_grid_scroll_y(content_y_logical_pixels),
            Page::Tracks => state.set_tracks_scroll_y(content_y_logical_pixels),
            Page::Settings => state.set_settings_scroll_y(content_y_logical_pixels),
            Page::AlbumDetail { .. } => state.set_detail_scroll_y(content_y_logical_pixels),
            Page::Playlist { .. } => state.set_playlist_scroll_y(content_y_logical_pixels),
            Page::NowPlaying => state.set_now_playing_scroll_y(content_y_logical_pixels),
        }
    }

    /// The name of `page` for the back button ("‹ Albums", "‹ Artist name"…).
    fn page_title(&self, page: &Page) -> String {
        match page {
            Page::RecentlyAdded => "Ajouts récents".to_owned(),
            Page::Artists => "Artistes".to_owned(),
            Page::Albums {
                artist_filter: Some(artist),
                ..
            } => artist.clone(),
            Page::Albums { query, .. } if !query.is_empty() => "Recherche".to_owned(),
            Page::Albums { .. } => "Albums".to_owned(),
            Page::Tracks => "Morceaux".to_owned(),
            Page::Settings => "Réglages".to_owned(),
            Page::AlbumDetail { album } => self
                .albums
                .borrow()
                .iter()
                .find(|candidate| candidate.key == *album)
                .map_or_else(|| album.title.clone(), |found| found.title.clone()),
            Page::Playlist { playlist_id } => self
                .playlists
                .borrow()
                .iter()
                .find(|playlist| playlist.id == *playlist_id)
                .map_or_else(|| "Playlist".to_owned(), |playlist| playlist.name.clone()),
            Page::NowPlaying => NOW_PLAYING_TITLE.to_owned(),
        }
    }

    /// Index of playlist `playlist_id` in the sidebar list, -1 when it no longer exists.
    fn playlist_index_of(&self, playlist_id: i64) -> i32 {
        self.playlists
            .borrow()
            .iter()
            .position(|playlist| playlist.id == playlist_id)
            .map_or(-1, |index| index as i32)
    }

    /// Opens the page of album `index` (in the catalog order).
    pub(super) fn open_album_at(&self, index: usize) {
        let key = self
            .albums
            .borrow()
            .get(index)
            .map(|album| album.key.clone());
        if let Some(album) = key {
            self.navigate_to(Page::AlbumDetail { album });
        }
    }

    /// Opens the grid of `artist`'s albums (an album artist, as displayed).
    pub(super) fn open_artist_named(&self, artist: &str) {
        self.navigate_to(Page::Albums {
            artist_filter: Some(artist.to_owned()),
            query: String::new(),
        });
    }

    /// Opens playlist `index` (in the sidebar order).
    pub(super) fn open_playlist(&self, index: usize) {
        let playlist_id = self
            .playlists
            .borrow()
            .get(index)
            .map(|playlist| playlist.id);
        if let Some(playlist_id) = playlist_id {
            self.navigate_to(Page::Playlist { playlist_id });
        }
    }

    /// Opens the current-track page; nothing happens when no track is current.
    fn show_now_playing(&self) {
        if self.is_page_available(&Page::NowPlaying) {
            self.navigate_to(Page::NowPlaying);
        }
    }

    /// The search field changed. Typing on a search route refines it in place (one history
    /// entry per search, not per key); typing elsewhere opens the results; clearing the field on
    /// a search route shows the plain grid.
    fn search_changed(&self, text: &str) {
        let search_page = Page::Albums {
            artist_filter: None,
            query: text.to_owned(),
        };
        let is_on_search_page = is_search_page(&self.navigation.borrow().current().page);
        if is_on_search_page {
            self.navigation.borrow_mut().replace_current(search_page);
            self.apply_current_route();
        } else if !text.is_empty() {
            self.navigate_to(search_page);
        }
    }

    /// After the library or the playlists changed: forgets pages that no longer exist, leaves
    /// the current one if it vanished, and otherwise re-renders it without moving its scroll.
    pub(super) fn synchronize_navigation_with_library(&self) {
        self.navigation
            .borrow_mut()
            .remove_unavailable(|page| self.is_page_available(page));
        let current_page = self.navigation.borrow().current().page.clone();
        if self.is_page_available(&current_page) {
            self.refresh_current_route();
        } else {
            self.leave_unavailable_page();
        }
    }

    /// Goes back from a page that no longer exists; with nowhere to go back to, the plain grid
    /// replaces it (so the dead page never enters the back stack).
    fn leave_unavailable_page(&self) {
        let has_moved = self
            .navigation
            .borrow_mut()
            .go_back(0.0, |page| self.is_page_available(page))
            .is_some();
        if !has_moved {
            self.navigation
                .borrow_mut()
                .replace_current(all_albums_page());
        }
        self.replace_current_if_unavailable();
        // Back put the vanished page on the forward stack: Forward must not offer it.
        self.navigation
            .borrow_mut()
            .remove_unavailable(|page| self.is_page_available(page));
        self.apply_current_route();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_library_section_maps_to_its_slint_value_and_back() {
        for section in [
            SidebarSection::RecentlyAdded,
            SidebarSection::Artists,
            SidebarSection::Albums,
            SidebarSection::Tracks,
            SidebarSection::Settings,
        ] {
            assert_eq!(
                section_from_slint(slint_sidebar_section(section)),
                Some(section)
            );
        }
    }

    #[test]
    fn slint_playlist_section_opens_nothing() {
        assert_eq!(
            slint_sidebar_section(SidebarSection::Playlist(7)),
            crate::SidebarSection::Playlist
        );
        assert_eq!(section_from_slint(crate::SidebarSection::Playlist), None);
    }

    #[test]
    fn only_a_plain_query_grid_is_a_search_page() {
        assert!(is_search_page(&Page::Albums {
            artist_filter: None,
            query: "x".to_owned(),
        }));
        assert!(!is_search_page(&all_albums_page()));
        assert!(!is_search_page(&Page::Albums {
            artist_filter: Some("A".to_owned()),
            query: String::new(),
        }));
    }

    #[test]
    fn start_page_falls_back_to_albums() {
        let mut settings = AppSettings::default();
        assert_eq!(start_page(&settings), all_albums_page());
        settings.start_section = Some("artists".to_owned());
        assert_eq!(start_page(&settings), Page::Artists);
        settings.start_section = Some("settings".to_owned());
        assert_eq!(start_page(&settings), all_albums_page());
    }

    #[test]
    fn each_page_has_its_own_view() {
        assert_eq!(view_of(&all_albums_page()), ViewKind::Grid);
        assert_eq!(view_of(&Page::Settings), ViewKind::Settings);
        assert_eq!(view_of(&Page::NowPlaying), ViewKind::NowPlaying);
        assert_eq!(
            view_of(&Page::Playlist { playlist_id: 1 }),
            ViewKind::Playlist
        );
    }
}
