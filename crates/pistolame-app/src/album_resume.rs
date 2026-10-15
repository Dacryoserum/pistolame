//! "Reprendre" on an album page: where listening to each album stopped (audit Psysonic §8),
//! useful for soundtracks and long complete works. One point per album, the most recent first.
//!
//! Pure model, stored with the session. Albums are recognized by their tracks, so a rescan that
//! renumbers albums keeps the points.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Albums remembered at most; the oldest point is dropped beyond.
const MAXIMUM_RESUME_POINTS: usize = 50;
/// Below this position in an album's first track, there is nothing worth resuming.
const MINIMUM_FIRST_TRACK_POSITION_SECONDS: u64 = 30;

/// Where listening to one album stopped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ResumePoint {
    pub(crate) track_path: PathBuf,
    pub(crate) position_seconds: u64,
}

/// Records that listening to the album made of `album_tracks` is at `point` (the track's
/// index in the album is `track_index`). The start of the first track forgets the album.
pub(crate) fn remember(
    points: &mut Vec<ResumePoint>,
    point: ResumePoint,
    track_index: usize,
    album_tracks: &[PathBuf],
) {
    forget_album(points, album_tracks);
    if track_index == 0 && point.position_seconds < MINIMUM_FIRST_TRACK_POSITION_SECONDS {
        return;
    }
    points.insert(0, point);
    points.truncate(MAXIMUM_RESUME_POINTS);
}

/// Forgets the album made of `album_tracks` (listened to the end, or restarted).
pub(crate) fn forget_album(points: &mut Vec<ResumePoint>, album_tracks: &[PathBuf]) {
    points.retain(|point| !contains(album_tracks, &point.track_path));
}

/// The point where the album made of `album_tracks` stopped, if any.
pub(crate) fn find<'a>(
    points: &'a [ResumePoint],
    album_tracks: &[PathBuf],
) -> Option<&'a ResumePoint> {
    points
        .iter()
        .find(|point| contains(album_tracks, &point.track_path))
}

fn contains(album_tracks: &[PathBuf], path: &Path) -> bool {
    album_tracks.iter().any(|track| track == path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(prefix: &str) -> Vec<PathBuf> {
        (1..=4)
            .map(|n| PathBuf::from(format!("{prefix}{n}")))
            .collect()
    }

    fn at(path: &str, position_seconds: u64) -> ResumePoint {
        ResumePoint {
            track_path: path.into(),
            position_seconds,
        }
    }

    #[test]
    fn one_point_per_album_the_latest_kept() {
        let (first, second) = (album("a"), album("b"));
        let mut points = Vec::new();
        remember(&mut points, at("a2", 40), 1, &first);
        remember(&mut points, at("b3", 10), 2, &second);
        remember(&mut points, at("a3", 5), 2, &first);
        assert_eq!(find(&points, &first), Some(&at("a3", 5)));
        assert_eq!(find(&points, &second), Some(&at("b3", 10)));
        assert_eq!(points.len(), 2);
        assert_eq!(points[0], at("a3", 5));
    }

    #[test]
    fn the_very_start_of_an_album_is_not_worth_resuming() {
        let tracks = album("a");
        let mut points = Vec::new();
        remember(&mut points, at("a3", 50), 2, &tracks);
        remember(&mut points, at("a1", 12), 0, &tracks);
        assert_eq!(find(&points, &tracks), None);
        remember(&mut points, at("a1", 31), 0, &tracks);
        assert_eq!(find(&points, &tracks), Some(&at("a1", 31)));
    }

    #[test]
    fn a_finished_album_is_forgotten() {
        let tracks = album("a");
        let mut points = vec![at("a4", 100), at("z1", 3)];
        forget_album(&mut points, &tracks);
        assert_eq!(points, vec![at("z1", 3)]);
    }

    #[test]
    fn the_oldest_points_are_dropped() {
        let mut points = Vec::new();
        for index in 0..MAXIMUM_RESUME_POINTS + 3 {
            let tracks = album(&format!("album{index}-"));
            remember(&mut points, at(&format!("album{index}-2"), 60), 1, &tracks);
        }
        assert_eq!(points.len(), MAXIMUM_RESUME_POINTS);
        assert_eq!(find(&points, &album("album0-")), None);
    }
}
