//! How the album grid is ordered (audit 2026-10-13, YM Pro comparison §4.1). Pure logic.
//!
//! Titles compare *naturally*: case-insensitive, numbers by value ("Season 2" before
//! "Season 10"), a leading "The " ignored, and the files' sort tags (ALBUMSORT,
//! ALBUMARTISTSORT) used when present.

use std::cmp::Ordering;

/// The orders offered in the grid's sort menu, in menu order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AlbumSort {
    /// Album title, A to Z.
    Title,
    /// Album artist, then year (oldest first), then title: an artist's albums in release order.
    #[default]
    ArtistThenYear,
    /// Release year, newest first.
    Year,
    /// Most recently added to the library first.
    RecentlyAdded,
    /// Most recently played first; never-played albums last, by title.
    RecentlyPlayed,
}

impl AlbumSort {
    /// Every order, in menu order.
    pub(crate) const ALL: [AlbumSort; 5] = [
        AlbumSort::Title,
        AlbumSort::ArtistThenYear,
        AlbumSort::Year,
        AlbumSort::RecentlyAdded,
        AlbumSort::RecentlyPlayed,
    ];

    /// Label in the sort menu.
    pub(crate) fn label(self) -> &'static str {
        match self {
            AlbumSort::Title => "Titre",
            AlbumSort::ArtistThenYear => "Artiste",
            AlbumSort::Year => "Année",
            AlbumSort::RecentlyAdded => "Ajouts récents",
            AlbumSort::RecentlyPlayed => "Écoutés récemment",
        }
    }

    /// Stable name stored in the settings file.
    pub(crate) fn key(self) -> &'static str {
        match self {
            AlbumSort::Title => "title",
            AlbumSort::ArtistThenYear => "artist",
            AlbumSort::Year => "year",
            AlbumSort::RecentlyAdded => "added",
            AlbumSort::RecentlyPlayed => "played",
        }
    }

    /// Inverse of `key`.
    pub(crate) fn from_key(key: &str) -> Option<AlbumSort> {
        AlbumSort::ALL.into_iter().find(|sort| sort.key() == key)
    }
}

/// What the sort looks at for one album.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AlbumSortKeys {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) year: Option<u32>,
    pub(crate) added_seconds: i64,
    pub(crate) last_played_seconds: Option<i64>,
}

/// Orders two albums for `sort`.
pub(crate) fn compare_albums(
    sort: AlbumSort,
    left: &AlbumSortKeys,
    right: &AlbumSortKeys,
) -> Ordering {
    let by_title = || natural_compare(&left.title, &right.title);
    let by_year_oldest = || compare_years_oldest_first(left.year, right.year);
    match sort {
        AlbumSort::Title => by_title(),
        AlbumSort::ArtistThenYear => natural_compare(&left.artist, &right.artist)
            .then_with(by_year_oldest)
            .then_with(by_title),
        AlbumSort::Year => match (left.year, right.year) {
            (Some(left_year), Some(right_year)) => right_year.cmp(&left_year).then_with(by_title),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => by_title(),
        },
        AlbumSort::RecentlyAdded => right
            .added_seconds
            .cmp(&left.added_seconds)
            .then_with(by_title),
        AlbumSort::RecentlyPlayed => match (left.last_played_seconds, right.last_played_seconds) {
            (Some(left_played), Some(right_played)) => {
                right_played.cmp(&left_played).then_with(by_title)
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => by_title(),
        },
    }
}

/// Albums without a year go after the dated ones.
fn compare_years_oldest_first(left: Option<u32>, right: Option<u32>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Case-insensitive comparison where runs of digits compare by value and a leading "The " is
/// ignored.
pub(crate) fn natural_compare(left: &str, right: &str) -> Ordering {
    let left = sortable(left);
    let right = sortable(right);
    let mut left_chars = left.chars().peekable();
    let mut right_chars = right.chars().peekable();
    loop {
        match (left_chars.peek().copied(), right_chars.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left_char), Some(right_char))
                if left_char.is_ascii_digit() && right_char.is_ascii_digit() =>
            {
                let left_number = take_number(&mut left_chars);
                let right_number = take_number(&mut right_chars);
                let order = compare_digit_runs(&left_number, &right_number);
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(left_char), Some(right_char)) => {
                let order = left_char.cmp(&right_char);
                if order != Ordering::Equal {
                    return order;
                }
                left_chars.next();
                right_chars.next();
            }
        }
    }
}

fn sortable(text: &str) -> String {
    let lower = text.trim().to_lowercase();
    lower
        .strip_prefix("the ")
        .map(str::to_owned)
        .unwrap_or(lower)
}

fn take_number(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut digits = String::new();
    while let Some(digit) = chars.peek().copied().filter(char::is_ascii_digit) {
        digits.push(digit);
        chars.next();
    }
    digits
}

/// Compares two digit strings by numeric value, without overflow on very long runs.
fn compare_digit_runs(left: &str, right: &str) -> Ordering {
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(title: &str, artist: &str, year: Option<u32>) -> AlbumSortKeys {
        AlbumSortKeys {
            title: title.into(),
            artist: artist.into(),
            year,
            ..AlbumSortKeys::default()
        }
    }

    fn sorted_titles(sort: AlbumSort, mut albums: Vec<AlbumSortKeys>) -> Vec<String> {
        albums.sort_by(|left, right| compare_albums(sort, left, right));
        albums.into_iter().map(|album| album.title).collect()
    }

    #[test]
    fn numbers_sort_by_value() {
        assert_eq!(natural_compare("Season 2", "Season 10"), Ordering::Less);
        assert_eq!(natural_compare("season 10", "Season 9"), Ordering::Greater);
        assert_eq!(natural_compare("Track 007", "Track 7"), Ordering::Equal);
        assert_eq!(
            natural_compare("The Wall", "Wish You Were Here"),
            Ordering::Less
        );
    }

    #[test]
    fn game_of_thrones_soundtracks_follow_each_other() {
        let mut albums: Vec<AlbumSortKeys> = (1..=8)
            .rev()
            .map(|season| {
                album(
                    &format!("Game of Thrones: Season {season}"),
                    "Ramin Djawadi",
                    Some(2010 + season),
                )
            })
            .collect();
        albums.push(album("Dune: Part Two", "Hans Zimmer", Some(2024)));
        albums.push(album("Pacific Rim", "Ramin Djawadi", Some(2013)));
        let expected_by_title = vec![
            "Dune: Part Two",
            "Game of Thrones: Season 1",
            "Game of Thrones: Season 2",
            "Game of Thrones: Season 3",
            "Game of Thrones: Season 4",
            "Game of Thrones: Season 5",
            "Game of Thrones: Season 6",
            "Game of Thrones: Season 7",
            "Game of Thrones: Season 8",
            "Pacific Rim",
        ];
        assert_eq!(
            sorted_titles(AlbumSort::Title, albums.clone()),
            expected_by_title
        );
        // By artist: Hans Zimmer first, then Ramin Djawadi's albums in release order.
        let by_artist = sorted_titles(AlbumSort::ArtistThenYear, albums);
        assert_eq!(by_artist[0], "Dune: Part Two");
        assert_eq!(by_artist[1], "Game of Thrones: Season 1");
        assert_eq!(by_artist[2], "Game of Thrones: Season 2");
        assert_eq!(by_artist[3], "Game of Thrones: Season 3");
        assert_eq!(by_artist[4], "Pacific Rim");
        assert_eq!(by_artist[9], "Game of Thrones: Season 8");
    }

    #[test]
    fn year_puts_the_newest_first_and_undated_last() {
        let albums = vec![
            album("Old", "A", Some(1999)),
            album("Undated", "A", None),
            album("New", "A", Some(2024)),
        ];
        assert_eq!(
            sorted_titles(AlbumSort::Year, albums),
            vec!["New", "Old", "Undated"]
        );
    }

    #[test]
    fn recently_played_puts_never_played_albums_last() {
        let mut played = album("Played", "A", None);
        played.last_played_seconds = Some(100);
        let mut latest = album("Latest", "A", None);
        latest.last_played_seconds = Some(200);
        let never = album("Never", "A", None);
        assert_eq!(
            sorted_titles(AlbumSort::RecentlyPlayed, vec![never, played, latest]),
            vec!["Latest", "Played", "Never"]
        );
    }

    #[test]
    fn keys_round_trip() {
        for sort in AlbumSort::ALL {
            assert_eq!(AlbumSort::from_key(sort.key()), Some(sort));
        }
    }
}
