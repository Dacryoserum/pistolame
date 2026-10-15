//! Multiple selection in a track list: plain click, Ctrl/Cmd+click (toggle) and Shift+click
//! (range from the anchor), as in Apple Music and the Finder.
//!
//! Pure model, independent of Slint: the bridge maps rows to positions and redraws the marks.

use std::collections::BTreeSet;

/// The list a selection belongs to. Selecting in another list starts a new selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionList {
    /// The tracks of one album page (positions are track indexes in the album).
    AlbumTracks { album_index: usize },
    /// The entries of one playlist (positions among all its entries).
    Playlist { playlist_index: usize },
    /// The tracks listed under the search results.
    SearchResults,
    /// The "Morceaux" table, in its current order.
    LibraryTracks,
}

/// How a click changes the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionGesture {
    /// Plain click: only this row.
    Replace,
    /// Ctrl/Cmd+click: adds or removes this row.
    Toggle,
    /// Shift+click: every row from the anchor to this one.
    ExtendRange,
}

/// The selected rows of one list.
#[derive(Debug, Default)]
pub(crate) struct TrackSelection {
    list: Option<SelectionList>,
    positions: BTreeSet<usize>,
    /// The row a Shift+click range starts from: the last row clicked without Shift.
    anchor: Option<usize>,
}

impl TrackSelection {
    /// Applies a click on `position` in `list`.
    pub(crate) fn click(
        &mut self,
        list: SelectionList,
        position: usize,
        gesture: SelectionGesture,
    ) {
        if self.list != Some(list) {
            self.clear();
            self.list = Some(list);
        }
        match gesture {
            SelectionGesture::Replace => {
                self.positions.clear();
                self.positions.insert(position);
                self.anchor = Some(position);
            }
            SelectionGesture::Toggle => {
                if !self.positions.remove(&position) {
                    self.positions.insert(position);
                }
                self.anchor = Some(position);
            }
            SelectionGesture::ExtendRange => {
                let anchor = self.anchor.unwrap_or(position);
                self.positions.clear();
                self.positions
                    .extend(anchor.min(position)..=anchor.max(position));
                self.anchor = Some(anchor);
            }
        }
        if self.positions.is_empty() {
            self.list = None;
            self.anchor = None;
        }
    }

    /// Keeps only the selected positions `is_shown` accepts: a range must not take in rows the
    /// list does not show (playlist entries whose file is gone).
    pub(crate) fn retain_shown(&mut self, is_shown: impl Fn(usize) -> bool) {
        self.positions.retain(|&position| is_shown(position));
        if self.positions.is_empty() {
            self.clear();
        }
    }

    /// Forgets the selection (page left, list rebuilt, Escape).
    pub(crate) fn clear(&mut self) {
        self.list = None;
        self.positions.clear();
        self.anchor = None;
    }

    /// The list the selection is in, if any row is selected.
    pub(crate) fn list(&self) -> Option<SelectionList> {
        self.list
    }

    /// True when `position` of `list` is selected.
    pub(crate) fn contains(&self, list: SelectionList, position: usize) -> bool {
        self.list == Some(list) && self.positions.contains(&position)
    }

    /// The selected positions, in list order.
    pub(crate) fn positions(&self) -> impl DoubleEndedIterator<Item = usize> + '_ {
        self.positions.iter().copied()
    }

    /// How many rows are selected.
    pub(crate) fn len(&self) -> usize {
        self.positions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARY: SelectionList = SelectionList::LibraryTracks;

    fn selected(selection: &TrackSelection) -> Vec<usize> {
        selection.positions().collect()
    }

    #[test]
    fn a_plain_click_selects_only_that_row() {
        let mut selection = TrackSelection::default();
        selection.click(LIBRARY, 3, SelectionGesture::Replace);
        selection.click(LIBRARY, 5, SelectionGesture::Replace);
        assert_eq!(selected(&selection), [5]);
    }

    #[test]
    fn toggle_adds_then_removes_a_row() {
        let mut selection = TrackSelection::default();
        selection.click(LIBRARY, 1, SelectionGesture::Replace);
        selection.click(LIBRARY, 4, SelectionGesture::Toggle);
        assert_eq!(selected(&selection), [1, 4]);
        selection.click(LIBRARY, 1, SelectionGesture::Toggle);
        assert_eq!(selected(&selection), [4]);
    }

    #[test]
    fn toggling_the_last_row_off_empties_the_selection() {
        let mut selection = TrackSelection::default();
        selection.click(LIBRARY, 2, SelectionGesture::Toggle);
        selection.click(LIBRARY, 2, SelectionGesture::Toggle);
        assert_eq!(selection.len(), 0);
        assert_eq!(selection.list(), None);
    }

    #[test]
    fn a_range_runs_from_the_anchor_in_either_direction() {
        let mut selection = TrackSelection::default();
        selection.click(LIBRARY, 5, SelectionGesture::Replace);
        selection.click(LIBRARY, 2, SelectionGesture::ExtendRange);
        assert_eq!(selected(&selection), [2, 3, 4, 5]);
        // The anchor stays: a second Shift+click redraws the range from it.
        selection.click(LIBRARY, 7, SelectionGesture::ExtendRange);
        assert_eq!(selected(&selection), [5, 6, 7]);
    }

    #[test]
    fn a_range_without_anchor_selects_the_clicked_row() {
        let mut selection = TrackSelection::default();
        selection.click(LIBRARY, 4, SelectionGesture::ExtendRange);
        assert_eq!(selected(&selection), [4]);
    }

    #[test]
    fn hidden_rows_leave_a_range() {
        let mut selection = TrackSelection::default();
        selection.click(LIBRARY, 1, SelectionGesture::Replace);
        selection.click(LIBRARY, 5, SelectionGesture::ExtendRange);
        selection.retain_shown(|position| position != 3);
        assert_eq!(selected(&selection), [1, 2, 4, 5]);
        selection.retain_shown(|_| false);
        assert_eq!(selection.list(), None);
    }

    #[test]
    fn clicking_in_another_list_starts_over() {
        let mut selection = TrackSelection::default();
        let album = SelectionList::AlbumTracks { album_index: 0 };
        selection.click(album, 1, SelectionGesture::Replace);
        selection.click(LIBRARY, 3, SelectionGesture::Toggle);
        assert_eq!(selection.list(), Some(LIBRARY));
        assert_eq!(selected(&selection), [3]);
        assert!(!selection.contains(album, 1));
    }
}
