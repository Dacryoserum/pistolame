//! Pure navigation history: pages, sidebar sections and Back/Forward stacks.
//!
//! This module knows nothing about the UI toolkit. The UI bridge asks it where to go and renders
//! the resulting [`Route`]; see `docs/plans/navigation.md` for the design.

use pistolame_library::AlbumKey;

/// Oldest entries are dropped beyond this; 50 covers any realistic browsing chain.
pub(crate) const MAX_HISTORY_ROUTES: usize = 50;

/// A page the content area can show, identified by stable keys (never by list index, which
/// changes with sorting and rescans).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Page {
    /// Albums most recently added to the library.
    RecentlyAdded,
    /// The list of artists.
    Artists,
    /// The album grid; `artist_filter` set = one artist's albums, `query` non-empty = search results.
    Albums {
        artist_filter: Option<String>,
        query: String,
    },
    /// Every track of the library.
    Tracks,
    /// Application settings.
    Settings,
    /// The detail page of one album.
    AlbumDetail { album: AlbumKey },
    /// One playlist, identified by its database id.
    Playlist { playlist_id: i64 },
    /// "Les plus écoutés", the automatic ranking.
    TopTracks,
    /// The page of the track being played.
    NowPlaying,
}

/// The sidebar entry highlighted for a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SidebarSection {
    RecentlyAdded,
    Artists,
    Albums,
    Tracks,
    Settings,
    Playlist(i64),
    TopTracks,
}

/// One history entry: the page, its sidebar section and where it was scrolled when left.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Route {
    /// The page to show.
    pub page: Page,
    /// The sidebar entry highlighted while the page is shown.
    pub section: SidebarSection,
    /// Scroll offset of the page content, as stored and restored verbatim by the UI.
    pub content_y_logical_pixels: f32,
}

/// Back and forward stacks around the current route.
#[derive(Debug)]
pub(crate) struct NavigationHistory {
    current: Route,
    back: Vec<Route>,
    forward: Vec<Route>,
}

impl Page {
    /// The top-level page a sidebar section opens.
    pub(crate) fn top_level_for(section: SidebarSection) -> Page {
        match section {
            SidebarSection::RecentlyAdded => Page::RecentlyAdded,
            SidebarSection::Artists => Page::Artists,
            SidebarSection::Albums => Page::Albums {
                artist_filter: None,
                query: String::new(),
            },
            SidebarSection::Tracks => Page::Tracks,
            SidebarSection::Settings => Page::Settings,
            SidebarSection::Playlist(playlist_id) => Page::Playlist { playlist_id },
            SidebarSection::TopTracks => Page::TopTracks,
        }
    }

    /// True for pages reached from another page: Escape goes back only from these.
    pub(crate) fn is_sub_page(&self) -> bool {
        match self {
            Page::AlbumDetail { .. } | Page::NowPlaying => true,
            Page::Albums {
                artist_filter,
                query,
            } => artist_filter.is_some() || !query.is_empty(),
            _ => false,
        }
    }
}

impl SidebarSection {
    /// Key stored in the settings file; only the four library sections are remembered.
    pub(crate) fn settings_key(self) -> Option<&'static str> {
        match self {
            SidebarSection::RecentlyAdded => Some("recently_added"),
            SidebarSection::Artists => Some("artists"),
            SidebarSection::Albums => Some("albums"),
            SidebarSection::Tracks => Some("tracks"),
            SidebarSection::Settings | SidebarSection::Playlist(_) | SidebarSection::TopTracks => {
                None
            }
        }
    }

    /// Inverse of [`SidebarSection::settings_key`]; unknown keys give `None`.
    pub(crate) fn from_settings_key(key: &str) -> Option<SidebarSection> {
        match key {
            "recently_added" => Some(SidebarSection::RecentlyAdded),
            "artists" => Some(SidebarSection::Artists),
            "albums" => Some(SidebarSection::Albums),
            "tracks" => Some(SidebarSection::Tracks),
            _ => None,
        }
    }

    /// Old sidebar numbering kept for PISTOLAME_DEBUG_START_ITEM / PISTOLAME_PREVIEW_ITEM:
    /// 0 recent, 1 artists, 2 albums, 3 tracks, 6 settings; anything else None.
    pub(crate) fn from_legacy_debug_index(index: i32) -> Option<SidebarSection> {
        match index {
            0 => Some(SidebarSection::RecentlyAdded),
            1 => Some(SidebarSection::Artists),
            2 => Some(SidebarSection::Albums),
            3 => Some(SidebarSection::Tracks),
            6 => Some(SidebarSection::Settings),
            _ => None,
        }
    }
}

/// Section highlighted for `page` when it is opened from `origin`.
///
/// Top-level pages own their section; an artist-filtered grid belongs to Artists; album detail
/// and now playing inherit the section of the page they were opened from.
fn section_for(page: &Page, origin: &Route) -> SidebarSection {
    match page {
        Page::RecentlyAdded => SidebarSection::RecentlyAdded,
        Page::Artists => SidebarSection::Artists,
        Page::Albums {
            artist_filter: Some(_),
            ..
        } => SidebarSection::Artists,
        Page::Albums { .. } => SidebarSection::Albums,
        Page::Tracks => SidebarSection::Tracks,
        Page::Settings => SidebarSection::Settings,
        Page::Playlist { playlist_id } => SidebarSection::Playlist(*playlist_id),
        Page::TopTracks => SidebarSection::TopTracks,
        Page::AlbumDetail { .. } | Page::NowPlaying => origin.section,
    }
}

/// Appends `route` to `stack`, dropping the oldest entry beyond [`MAX_HISTORY_ROUTES`].
fn push_capped(stack: &mut Vec<Route>, route: Route) {
    stack.push(route);
    if stack.len() > MAX_HISTORY_ROUTES {
        stack.remove(0);
    }
}

/// Pops entries from `stack` until one passes `is_available`; unavailable ones are discarded.
fn pop_available(stack: &mut Vec<Route>, is_available: &impl Fn(&Page) -> bool) -> Option<Route> {
    while let Some(route) = stack.pop() {
        if is_available(&route.page) {
            return Some(route);
        }
    }
    None
}

impl NavigationHistory {
    /// Starts a history on `initial_page` with empty stacks.
    pub(crate) fn new(initial_page: Page) -> NavigationHistory {
        let placeholder_origin = Route {
            page: Page::Artists,
            section: SidebarSection::Albums,
            content_y_logical_pixels: 0.0,
        };
        let section = section_for(&initial_page, &placeholder_origin);
        NavigationHistory {
            current: Route {
                page: initial_page,
                section,
                content_y_logical_pixels: 0.0,
            },
            back: Vec::new(),
            forward: Vec::new(),
        }
    }

    /// The route being shown.
    pub(crate) fn current(&self) -> &Route {
        &self.current
    }

    /// Opens `page`; false when it is already the current page (nothing changed).
    pub(crate) fn push(&mut self, page: Page, leaving_content_y_logical_pixels: f32) -> bool {
        if page == self.current.page {
            return false;
        }
        let section = section_for(&page, &self.current);
        let new_route = Route {
            page,
            section,
            content_y_logical_pixels: 0.0,
        };
        let mut leaving_route = std::mem::replace(&mut self.current, new_route);
        leaving_route.content_y_logical_pixels = leaving_content_y_logical_pixels;
        push_capped(&mut self.back, leaving_route);
        self.forward.clear();
        true
    }

    /// Changes the current page in place (search typing): scroll back to 0, forward cleared.
    pub(crate) fn replace_current(&mut self, page: Page) {
        self.current.section = section_for(&page, &self.current);
        self.current.page = page;
        self.current.content_y_logical_pixels = 0.0;
        self.forward.clear();
        self.collapse_duplicate_entries();
    }

    /// The route to show after Back, or None when there is nowhere to go.
    pub(crate) fn go_back(
        &mut self,
        leaving_content_y_logical_pixels: f32,
        is_available: impl Fn(&Page) -> bool,
    ) -> Option<&Route> {
        self.current.content_y_logical_pixels = leaving_content_y_logical_pixels;
        let destination = match pop_available(&mut self.back, &is_available) {
            Some(route) => route,
            None if self.current.page.is_sub_page() => Route {
                page: Page::top_level_for(self.current.section),
                section: self.current.section,
                content_y_logical_pixels: 0.0,
            },
            None => return None,
        };
        let leaving_route = std::mem::replace(&mut self.current, destination);
        push_capped(&mut self.forward, leaving_route);
        Some(&self.current)
    }

    /// Mirror of [`NavigationHistory::go_back`]: reapplies a page that was just left.
    pub(crate) fn go_forward(
        &mut self,
        leaving_content_y_logical_pixels: f32,
        is_available: impl Fn(&Page) -> bool,
    ) -> Option<&Route> {
        self.current.content_y_logical_pixels = leaving_content_y_logical_pixels;
        let destination = pop_available(&mut self.forward, &is_available)?;
        let leaving_route = std::mem::replace(&mut self.current, destination);
        push_capped(&mut self.back, leaving_route);
        Some(&self.current)
    }

    /// True when Back would do something.
    pub(crate) fn can_go_back(&self) -> bool {
        !self.back.is_empty() || self.current.page.is_sub_page()
    }

    /// True when Forward would do something.
    pub(crate) fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// The page Back would show (for the back button label), ignoring availability.
    pub(crate) fn back_destination(&self) -> Option<Page> {
        if let Some(route) = self.back.last() {
            Some(route.page.clone())
        } else if self.current.page.is_sub_page() {
            Some(Page::top_level_for(self.current.section))
        } else {
            None
        }
    }

    /// Drops routes whose page fails `is_available` from both stacks (playlist deleted), moves
    /// routes opened from a deleted playlist to the Albums section, and removes the duplicate
    /// entries the pruning may leave.
    pub(crate) fn remove_unavailable(&mut self, is_available: impl Fn(&Page) -> bool) {
        self.back.retain(|route| is_available(&route.page));
        self.forward.retain(|route| is_available(&route.page));
        self.remap_sections_of_deleted_playlists(&is_available);
        self.collapse_duplicate_entries();
    }

    /// Album and now-playing routes keep the section of the playlist they were opened from;
    /// once that playlist is gone nothing would be highlighted, so they fall back to Albums.
    fn remap_sections_of_deleted_playlists(&mut self, is_available: &impl Fn(&Page) -> bool) {
        let routes = std::iter::once(&mut self.current)
            .chain(self.back.iter_mut())
            .chain(self.forward.iter_mut());
        for route in routes {
            if let SidebarSection::Playlist(playlist_id) = route.section
                && !is_available(&Page::Playlist { playlist_id })
            {
                route.section = SidebarSection::Albums;
            }
        }
    }

    /// Collapses adjacent entries showing the same page in each stack, and drops the back and
    /// forward tops that show the current page: Back/Forward must always change what is shown.
    /// Pages are compared, not scroll offsets.
    fn collapse_duplicate_entries(&mut self) {
        self.back
            .dedup_by(|later, earlier| later.page == earlier.page);
        self.forward
            .dedup_by(|later, earlier| later.page == earlier.page);
        while self
            .back
            .last()
            .is_some_and(|route| route.page == self.current.page)
        {
            self.back.pop();
        }
        while self
            .forward
            .last()
            .is_some_and(|route| route.page == self.current.page)
        {
            self.forward.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn albums_grid() -> Page {
        Page::Albums {
            artist_filter: None,
            query: String::new(),
        }
    }

    fn artist_albums(name: &str) -> Page {
        Page::Albums {
            artist_filter: Some(name.to_owned()),
            query: String::new(),
        }
    }

    fn search_results(text: &str) -> Page {
        Page::Albums {
            artist_filter: None,
            query: text.to_owned(),
        }
    }

    fn album_detail(title: &str) -> Page {
        Page::AlbumDetail {
            album: AlbumKey {
                directory: "/music".into(),
                title: title.to_owned(),
            },
        }
    }

    fn always_available(_page: &Page) -> bool {
        true
    }

    #[test]
    fn push_then_back_restores_previous_page_and_its_scroll() {
        let mut history = NavigationHistory::new(Page::Tracks);
        assert!(history.push(album_detail("A"), -120.0));
        let route = history.go_back(-30.0, always_available).expect("route");
        assert_eq!(route.page, Page::Tracks);
        assert_eq!(route.section, SidebarSection::Tracks);
        assert_eq!(route.content_y_logical_pixels, -120.0);
    }

    #[test]
    fn back_then_forward_reapplies_left_page_with_its_scroll() {
        let mut history = NavigationHistory::new(Page::Tracks);
        history.push(album_detail("A"), -120.0);
        history.go_back(-30.0, always_available);
        assert!(history.can_go_forward());
        let route = history.go_forward(-120.0, always_available).expect("route");
        assert_eq!(route.page, album_detail("A"));
        assert_eq!(route.content_y_logical_pixels, -30.0);
    }

    #[test]
    fn push_clears_forward_stack() {
        let mut history = NavigationHistory::new(Page::Tracks);
        history.push(album_detail("A"), 0.0);
        history.go_back(0.0, always_available);
        assert!(history.can_go_forward());
        history.push(Page::Artists, 0.0);
        assert!(!history.can_go_forward());
    }

    #[test]
    fn pushing_current_page_is_a_no_op() {
        let mut history = NavigationHistory::new(Page::Tracks);
        assert!(!history.push(Page::Tracks, -50.0));
        assert!(!history.can_go_back());
        assert_eq!(history.current().content_y_logical_pixels, 0.0);
    }

    #[test]
    fn album_opened_from_artists_keeps_artists_section() {
        let mut history = NavigationHistory::new(Page::Artists);
        history.push(album_detail("A"), 0.0);
        assert_eq!(history.current().section, SidebarSection::Artists);
    }

    #[test]
    fn filtered_grid_belongs_to_artists() {
        let mut history = NavigationHistory::new(Page::Tracks);
        history.push(artist_albums("Bjork"), 0.0);
        assert_eq!(history.current().section, SidebarSection::Artists);
    }

    #[test]
    fn plain_grid_belongs_to_albums() {
        let mut history = NavigationHistory::new(Page::Tracks);
        history.push(albums_grid(), 0.0);
        assert_eq!(history.current().section, SidebarSection::Albums);
        history.push(search_results("x"), 0.0);
        assert_eq!(history.current().section, SidebarSection::Albums);
    }

    #[test]
    fn now_playing_inherits_origin_section() {
        let mut history = NavigationHistory::new(Page::Tracks);
        history.push(Page::NowPlaying, 0.0);
        assert_eq!(history.current().section, SidebarSection::Tracks);
        history.push(Page::Playlist { playlist_id: 7 }, 0.0);
        history.push(Page::NowPlaying, 0.0);
        assert_eq!(history.current().section, SidebarSection::Playlist(7));
    }

    #[test]
    fn chain_album_artist_album_unwinds_step_by_step() {
        let mut history = NavigationHistory::new(Page::Artists);
        history.push(artist_albums("Bjork"), -10.0);
        history.push(album_detail("Homogenic"), -20.0);
        history.push(album_detail("Vespertine"), -5.0);

        let route = history.go_back(0.0, always_available).expect("route");
        assert_eq!(route.page, album_detail("Homogenic"));
        assert_eq!(route.content_y_logical_pixels, -5.0);
        let route = history.go_back(0.0, always_available).expect("route");
        assert_eq!(route.page, artist_albums("Bjork"));
        assert_eq!(route.content_y_logical_pixels, -20.0);
        let route = history.go_back(0.0, always_available).expect("route");
        assert_eq!(route.page, Page::Artists);
        assert_eq!(route.section, SidebarSection::Artists);
        assert_eq!(route.content_y_logical_pixels, -10.0);
        assert!(history.go_back(0.0, always_available).is_none());
    }

    #[test]
    fn back_skips_unavailable_routes() {
        let mut history = NavigationHistory::new(Page::Tracks);
        history.push(album_detail("Gone"), 0.0);
        history.push(Page::Artists, 0.0);
        let route = history
            .go_back(0.0, |page| *page != album_detail("Gone"))
            .expect("route");
        assert_eq!(route.page, Page::Tracks);
    }

    #[test]
    fn remove_unavailable_prunes_both_stacks() {
        let mut history = NavigationHistory::new(Page::Tracks);
        history.push(Page::Playlist { playlist_id: 1 }, 0.0);
        history.push(Page::Artists, 0.0);
        history.push(Page::Playlist { playlist_id: 1 }, 0.0);
        history.go_back(0.0, always_available);
        history.go_back(0.0, always_available);
        // back: [Tracks], current: Playlist 1, forward: [Playlist 1, Artists]
        history.remove_unavailable(|page| *page != Page::Playlist { playlist_id: 1 });
        assert_eq!(history.back.len(), 1);
        assert_eq!(history.forward.len(), 1);
        assert_eq!(history.forward[0].page, Page::Artists);
    }

    #[test]
    fn back_with_empty_stack_from_sub_page_goes_to_section_page() {
        let mut history = NavigationHistory::new(artist_albums("Bjork"));
        assert!(history.can_go_back());
        assert_eq!(history.back_destination(), Some(Page::Artists));
        let route = history.go_back(0.0, always_available).expect("route");
        assert_eq!(route.page, Page::Artists);
        assert_eq!(route.section, SidebarSection::Artists);
        assert!(history.can_go_forward());
    }

    #[test]
    fn back_from_top_level_with_empty_stack_is_none() {
        let mut history = NavigationHistory::new(Page::Tracks);
        assert!(!history.can_go_back());
        assert_eq!(history.back_destination(), None);
        assert!(history.go_back(0.0, always_available).is_none());
    }

    #[test]
    fn history_is_capped_at_max_routes() {
        let mut history = NavigationHistory::new(Page::Tracks);
        for playlist_id in 0..(MAX_HISTORY_ROUTES as i64 + 20) {
            history.push(Page::Playlist { playlist_id }, 0.0);
        }
        assert_eq!(history.back.len(), MAX_HISTORY_ROUTES);
        for _ in 0..MAX_HISTORY_ROUTES {
            history.go_back(0.0, always_available);
        }
        assert_eq!(history.forward.len(), MAX_HISTORY_ROUTES);
        // The oldest routes (Tracks, playlists 0..=18) were dropped.
        assert_eq!(history.current().page, Page::Playlist { playlist_id: 19 });
    }

    #[test]
    fn replace_current_keeps_back_stack_and_clears_forward() {
        let mut history = NavigationHistory::new(Page::Tracks);
        history.push(search_results("a"), 0.0);
        history.push(album_detail("A"), 0.0);
        history.go_back(0.0, always_available);
        assert!(history.can_go_forward());
        history.replace_current(search_results("ab"));
        assert!(!history.can_go_forward());
        assert_eq!(history.current().page, search_results("ab"));
        assert_eq!(history.current().content_y_logical_pixels, 0.0);
        assert_eq!(history.back.len(), 1);
        assert_eq!(history.back_destination(), Some(Page::Tracks));
    }

    #[test]
    fn clearing_a_search_does_not_leave_a_duplicate_back_entry() {
        let mut history = NavigationHistory::new(albums_grid());
        history.push(search_results("x"), -40.0);
        history.replace_current(albums_grid());
        assert_eq!(history.current().page, albums_grid());
        assert!(!history.can_go_back());
        assert!(history.go_back(0.0, always_available).is_none());
    }

    #[test]
    fn pruning_collapses_adjacent_duplicates() {
        let mut history = NavigationHistory::new(Page::Tracks);
        history.push(Page::Playlist { playlist_id: 1 }, 0.0);
        history.push(Page::Artists, 0.0);
        history.push(Page::Playlist { playlist_id: 1 }, 0.0);
        history.push(Page::Artists, 0.0);
        history.push(Page::Tracks, 0.0);
        // back: [Tracks, P1, Artists, P1, Artists], current: Tracks
        history.remove_unavailable(|page| *page != Page::Playlist { playlist_id: 1 });
        // back: [Tracks, Artists, Artists] collapses to [Tracks, Artists], and the top is not
        // the current page.
        let back_pages: Vec<Page> = history
            .back
            .iter()
            .map(|route| route.page.clone())
            .collect();
        assert_eq!(back_pages, vec![Page::Tracks, Page::Artists]);

        let mut history = NavigationHistory::new(Page::Tracks);
        history.push(Page::Playlist { playlist_id: 1 }, 0.0);
        history.push(Page::Tracks, 0.0);
        history.remove_unavailable(|page| *page != Page::Playlist { playlist_id: 1 });
        // back: [Tracks], current: Tracks -> the equal top is dropped.
        assert!(history.back.is_empty());
    }

    #[test]
    fn routes_opened_from_a_deleted_playlist_fall_back_to_albums_section() {
        let mut history = NavigationHistory::new(Page::Playlist { playlist_id: 7 });
        history.push(album_detail("A"), 0.0);
        history.push(Page::NowPlaying, 0.0);
        history.remove_unavailable(|page| *page != Page::Playlist { playlist_id: 7 });
        assert_eq!(history.current().section, SidebarSection::Albums);
        assert!(
            history
                .back
                .iter()
                .all(|route| route.section != SidebarSection::Playlist(7))
        );
        assert_eq!(
            history.back.last().map(|r| r.section),
            Some(SidebarSection::Albums)
        );
    }

    #[test]
    fn settings_key_round_trips_for_library_sections_only() {
        for section in [
            SidebarSection::RecentlyAdded,
            SidebarSection::Artists,
            SidebarSection::Albums,
            SidebarSection::Tracks,
        ] {
            let key = section.settings_key().expect("library section has a key");
            assert_eq!(SidebarSection::from_settings_key(key), Some(section));
        }
        assert_eq!(SidebarSection::Settings.settings_key(), None);
        assert_eq!(SidebarSection::Playlist(3).settings_key(), None);
        assert_eq!(SidebarSection::from_settings_key("settings"), None);
        assert_eq!(SidebarSection::from_settings_key(""), None);
    }

    #[test]
    fn legacy_debug_index_maps_0_1_2_3_6() {
        assert_eq!(
            SidebarSection::from_legacy_debug_index(0),
            Some(SidebarSection::RecentlyAdded)
        );
        assert_eq!(
            SidebarSection::from_legacy_debug_index(1),
            Some(SidebarSection::Artists)
        );
        assert_eq!(
            SidebarSection::from_legacy_debug_index(2),
            Some(SidebarSection::Albums)
        );
        assert_eq!(
            SidebarSection::from_legacy_debug_index(3),
            Some(SidebarSection::Tracks)
        );
        assert_eq!(
            SidebarSection::from_legacy_debug_index(6),
            Some(SidebarSection::Settings)
        );
        for unmapped in [-2, -1, 4, 5, 7, 99] {
            assert_eq!(SidebarSection::from_legacy_debug_index(unmapped), None);
        }
    }
}
