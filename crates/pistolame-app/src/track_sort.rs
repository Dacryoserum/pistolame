//! How the "Morceaux" table is ordered (audit 2026-10-13, §4 item 4). Pure logic.
//!
//! Every column sorts naturally (see `album_sort::natural_compare`), with fixed tie-breakers so
//! an album's tracks always stay in disc and track order.

use std::cmp::Ordering;

use crate::album_sort::natural_compare;

/// The table's sortable columns, in display order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum TrackColumn {
    #[default]
    Title,
    Artist,
    Album,
    Year,
    Format,
    Duration,
}

impl TrackColumn {
    /// Every column, in display order (the UI refers to them by this index).
    pub(crate) const ALL: [TrackColumn; 6] = [
        TrackColumn::Title,
        TrackColumn::Artist,
        TrackColumn::Album,
        TrackColumn::Year,
        TrackColumn::Format,
        TrackColumn::Duration,
    ];
}

/// What the sort looks at for one track.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TrackSortKeys {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    pub(crate) year: Option<u32>,
    pub(crate) sample_rate_hz: u32,
    pub(crate) bits_per_sample: u32,
    pub(crate) duration_seconds: u64,
    pub(crate) disc_number: u64,
    pub(crate) track_number: u64,
}

/// Orders two tracks by `column`, reversed when `is_descending`. Ties fall back to artist,
/// album, disc and track number, always ascending, so albums never get shuffled.
pub(crate) fn compare_tracks(
    column: TrackColumn,
    is_descending: bool,
    left: &TrackSortKeys,
    right: &TrackSortKeys,
) -> Ordering {
    let primary = match column {
        TrackColumn::Title => natural_compare(&left.title, &right.title),
        TrackColumn::Artist => natural_compare(&left.artist, &right.artist),
        TrackColumn::Album => natural_compare(&left.album, &right.album),
        TrackColumn::Year => compare_years(left.year, right.year),
        TrackColumn::Format => (left.bits_per_sample, left.sample_rate_hz)
            .cmp(&(right.bits_per_sample, right.sample_rate_hz)),
        TrackColumn::Duration => left.duration_seconds.cmp(&right.duration_seconds),
    };
    let primary = if is_descending {
        primary.reverse()
    } else {
        primary
    };
    primary
        .then_with(|| natural_compare(&left.artist, &right.artist))
        .then_with(|| natural_compare(&left.album, &right.album))
        .then_with(|| left.disc_number.cmp(&right.disc_number))
        .then_with(|| left.track_number.cmp(&right.track_number))
        .then_with(|| natural_compare(&left.title, &right.title))
}

/// Undated tracks go after the dated ones.
fn compare_years(left: Option<u32>, right: Option<u32>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, album: &str, number: u64) -> TrackSortKeys {
        TrackSortKeys {
            title: title.into(),
            artist: "Ramin Djawadi".into(),
            album: album.into(),
            track_number: number,
            disc_number: 1,
            ..TrackSortKeys::default()
        }
    }

    fn sorted(
        column: TrackColumn,
        is_descending: bool,
        mut tracks: Vec<TrackSortKeys>,
    ) -> Vec<String> {
        tracks.sort_by(|left, right| compare_tracks(column, is_descending, left, right));
        tracks.into_iter().map(|track| track.title).collect()
    }

    #[test]
    fn sorting_by_album_keeps_track_order_inside_each_album() {
        let tracks = vec![
            track("Finale", "Season 10", 2),
            track("Main Title", "Season 2", 1),
            track("Opening", "Season 10", 1),
            track("Goodbye", "Season 2", 2),
        ];
        assert_eq!(
            sorted(TrackColumn::Album, false, tracks.clone()),
            vec!["Main Title", "Goodbye", "Opening", "Finale"]
        );
        // Descending reverses the albums, never the tracks inside one.
        assert_eq!(
            sorted(TrackColumn::Album, true, tracks),
            vec!["Opening", "Finale", "Main Title", "Goodbye"]
        );
    }

    #[test]
    fn titles_sort_naturally() {
        let tracks = vec![
            track("Part 10", "A", 1),
            track("part 2", "A", 2),
            track("The End", "A", 3),
        ];
        // "The " is ignored, so "The End" sorts as "End".
        assert_eq!(
            sorted(TrackColumn::Title, false, tracks),
            vec!["The End", "part 2", "Part 10"]
        );
    }

    #[test]
    fn undated_tracks_come_last_and_formats_order_by_resolution() {
        let mut dated = track("Dated", "A", 1);
        dated.year = Some(2011);
        let undated = track("Undated", "A", 2);
        assert_eq!(
            sorted(
                TrackColumn::Year,
                false,
                vec![undated.clone(), dated.clone()]
            ),
            vec!["Dated", "Undated"]
        );
        let mut hi_res = track("Hi-Res", "A", 1);
        hi_res.bits_per_sample = 24;
        hi_res.sample_rate_hz = 96_000;
        let mut cd = track("CD", "A", 2);
        cd.bits_per_sample = 16;
        cd.sample_rate_hz = 44_100;
        assert_eq!(
            sorted(TrackColumn::Format, true, vec![cd, hi_res]),
            vec!["Hi-Res", "CD"]
        );
    }
}
