//! "Les plus écoutés": the ranking behind the automatic playlist. Pure logic; the bridge feeds it
//! the library and the settings.

use std::path::{Path, PathBuf};

/// The playlist's name, in the sidebar and on its page.
pub(crate) const TOP_TRACKS_TITLE: &str = "Les plus écoutés";
/// The ranking's size until the user picks one.
pub(crate) const DEFAULT_TOP_TRACK_COUNT: u32 = 50;
/// Upper bound of the free choice: beyond it the ranking is simply the whole library.
pub(crate) const MAXIMUM_TOP_TRACK_COUNT: u32 = 10_000;
/// Length of the recent period, rolling so the ranking is not empty on the 1st of each month.
pub(crate) const RECENT_PERIOD_SECONDS: i64 = 30 * 24 * 60 * 60;

/// Which plays the ranking counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum TopTracksPeriod {
    /// Every play ever counted.
    #[default]
    AllTime,
    /// The plays of the last [`RECENT_PERIOD_SECONDS`].
    Recent,
}

/// A track's standing in the period being ranked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RankedTrack<'path> {
    pub(crate) path: &'path Path,
    pub(crate) play_count: u32,
    pub(crate) last_played_seconds: Option<i64>,
}

/// The `limit` most played tracks, most played first; on equal counts, the one played last wins,
/// then the path, so the order never changes between two identical calls. Unplayed tracks are
/// not ranked.
pub(crate) fn rank_top_tracks(
    mut tracks: Vec<RankedTrack<'_>>,
    limit: usize,
) -> Vec<(PathBuf, u32)> {
    tracks.retain(|track| track.play_count > 0);
    tracks.sort_by(|left, right| {
        (right.play_count, right.last_played_seconds)
            .cmp(&(left.play_count, left.last_played_seconds))
            .then_with(|| left.path.cmp(right.path))
    });
    tracks
        .into_iter()
        .take(limit)
        .map(|track| (track.path.to_owned(), track.play_count))
        .collect()
}

/// The size typed in Réglages, kept within bounds; `None` while the field holds no number.
pub(crate) fn parse_top_track_count(text: &str) -> Option<u32> {
    let count = text.trim().parse::<u32>().ok()?;
    Some(count.clamp(1, MAXIMUM_TOP_TRACK_COUNT))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(path: &str, play_count: u32, last_played_seconds: i64) -> RankedTrack<'_> {
        RankedTrack {
            path: Path::new(path),
            play_count,
            last_played_seconds: Some(last_played_seconds),
        }
    }

    #[test]
    fn the_most_played_tracks_come_first_and_unplayed_ones_never() {
        let ranked = rank_top_tracks(
            vec![track("a", 1, 9), track("b", 5, 1), track("c", 0, 0)],
            10,
        );
        assert_eq!(ranked, vec![("b".into(), 5), ("a".into(), 1)]);
    }

    #[test]
    fn equal_counts_favor_the_track_played_last() {
        let ranked = rank_top_tracks(vec![track("old", 2, 10), track("new", 2, 20)], 10);
        assert_eq!(ranked, vec![("new".into(), 2), ("old".into(), 2)]);
    }

    #[test]
    fn the_ranking_stops_at_the_chosen_size() {
        let ranked = rank_top_tracks(
            vec![track("a", 3, 0), track("b", 2, 0), track("c", 1, 0)],
            2,
        );
        assert_eq!(ranked.len(), 2);
    }

    #[test]
    fn the_size_is_any_number_within_bounds() {
        assert_eq!(parse_top_track_count(" 25 "), Some(25));
        assert_eq!(parse_top_track_count("0"), Some(1));
        assert_eq!(
            parse_top_track_count("999999"),
            Some(MAXIMUM_TOP_TRACK_COUNT)
        );
        assert_eq!(parse_top_track_count(""), None);
        assert_eq!(parse_top_track_count("abc"), None);
    }
}
