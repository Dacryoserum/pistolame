//! Criterion 1: fake track catalog and a custom `Model` that sorts and filters by index.
//!
//! Only `visible_indices` is rearranged on sort/filter; the 100 000 tracks stay in place and
//! `row_data` builds Slint strings only for the rows the `ListView` asks for.

use crate::TrackRow;
use slint::{Model, ModelNotify, ModelRc, ModelTracker, SharedString};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

const WORDS: [&str; 24] = [
    "Night", "River", "Glass", "Echo", "Golden", "Silent", "Hour", "Ocean", "Paper", "Violet",
    "Static", "Winter", "Harbor", "Signal", "Velvet", "Orbit", "Amber", "Lantern", "Drift",
    "Mirror", "Ember", "Hollow", "Neon", "Meadow",
];

/// One fake track. `search_key` is lowercase so filtering allocates nothing per row.
struct Track {
    title: String,
    artist: String,
    album: String,
    duration_seconds: u32,
    search_key: String,
}

/// Sortable columns, in the order of the header buttons in the UI.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SortColumn {
    Title,
    Artist,
    Album,
    Duration,
}

impl SortColumn {
    fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Artist,
            2 => Self::Album,
            3 => Self::Duration,
            _ => Self::Title,
        }
    }
}

/// Small deterministic generator: keeps the spike free of a `rand` dependency.
struct LinearCongruentialGenerator(u64);

impl LinearCongruentialGenerator {
    fn next_below(&mut self, upper_bound: usize) -> usize {
        // Constants from Knuth's MMIX.
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % upper_bound
    }
}

fn random_phrase(generator: &mut LinearCongruentialGenerator, word_count: usize) -> String {
    let words: Vec<&str> =
        (0..word_count).map(|_| WORDS[generator.next_below(WORDS.len())]).collect();
    words.join(" ")
}

fn generate_tracks(track_count: usize) -> Vec<Track> {
    let mut generator = LinearCongruentialGenerator(0x5eed);
    (0..track_count)
        .map(|index| {
            let title = format!("{} {}", random_phrase(&mut generator, 2), index);
            let artist = random_phrase(&mut generator, 2);
            let album = random_phrase(&mut generator, 3);
            let duration_seconds = 90 + generator.next_below(450) as u32;
            let search_key = format!("{title} {artist} {album}").to_lowercase();
            Track { title, artist, album, duration_seconds, search_key }
        })
        .collect()
}

fn format_duration(duration_seconds: u32) -> String {
    format!("{}:{:02}", duration_seconds / 60, duration_seconds % 60)
}

/// Model backing the `ListView`; it is also the sort/filter engine.
pub struct TrackModel {
    tracks: Vec<Track>,
    visible_indices: RefCell<Vec<u32>>,
    sort_column: Cell<SortColumn>,
    is_sort_ascending: Cell<bool>,
    filter_text_lowercase: RefCell<String>,
    notify: ModelNotify,
}

impl TrackModel {
    pub fn new(track_count: usize) -> Rc<Self> {
        let tracks = generate_tracks(track_count);
        let visible_indices = (0..tracks.len() as u32).collect();
        Rc::new(Self {
            tracks,
            visible_indices: RefCell::new(visible_indices),
            sort_column: Cell::new(SortColumn::Title),
            is_sort_ascending: Cell::new(false),
            filter_text_lowercase: RefCell::new(String::new()),
            notify: ModelNotify::default(),
        })
    }

    pub fn as_model_rc(self: &Rc<Self>) -> ModelRc<TrackRow> {
        ModelRc::from(self.clone())
    }

    pub fn total_count(&self) -> usize {
        self.tracks.len()
    }

    /// Re-filters and keeps the current order. Returns the time spent (model work only).
    pub fn apply_filter(&self, filter_text: &str) -> Duration {
        let started_at = Instant::now();
        *self.filter_text_lowercase.borrow_mut() = filter_text.to_lowercase();
        self.rebuild_visible_indices();
        started_at.elapsed()
    }

    /// Sorts by `column_index`; pressing the same column again flips the direction.
    pub fn sort_by_column(&self, column_index: i32) -> Duration {
        let started_at = Instant::now();
        let column = SortColumn::from_index(column_index);
        let is_ascending =
            if self.sort_column.get() == column { !self.is_sort_ascending.get() } else { true };
        self.sort_column.set(column);
        self.is_sort_ascending.set(is_ascending);
        self.sort_visible_indices();
        self.notify.reset();
        started_at.elapsed()
    }

    pub fn visible_count(&self) -> usize {
        self.visible_indices.borrow().len()
    }

    fn rebuild_visible_indices(&self) {
        let filter = self.filter_text_lowercase.borrow();
        let matching_indices = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| track.search_key.contains(filter.as_str()))
            .map(|(index, _)| index as u32)
            .collect();
        *self.visible_indices.borrow_mut() = matching_indices;
        self.sort_visible_indices();
        self.notify.reset();
    }

    fn sort_visible_indices(&self) {
        let column = self.sort_column.get();
        let is_ascending = self.is_sort_ascending.get();
        self.visible_indices.borrow_mut().sort_unstable_by(|left, right| {
            let (left, right) = (&self.tracks[*left as usize], &self.tracks[*right as usize]);
            let ordering = match column {
                SortColumn::Title => left.title.cmp(&right.title),
                SortColumn::Artist => left.artist.cmp(&right.artist),
                SortColumn::Album => left.album.cmp(&right.album),
                SortColumn::Duration => left.duration_seconds.cmp(&right.duration_seconds),
            };
            if is_ascending { ordering } else { ordering.reverse() }
        });
    }
}

impl Model for TrackModel {
    type Data = TrackRow;

    fn row_count(&self) -> usize {
        self.visible_indices.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<TrackRow> {
        let track_index = *self.visible_indices.borrow().get(row)? as usize;
        let track = &self.tracks[track_index];
        Some(TrackRow {
            title: SharedString::from(track.title.as_str()),
            artist: SharedString::from(track.artist.as_str()),
            album: SharedString::from(track.album.as_str()),
            duration: SharedString::from(format_duration(track.duration_seconds)),
        })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}
