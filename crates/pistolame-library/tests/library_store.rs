//! Persistent library behavior (docs/plans/persistent-library.md, "Tests").
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use pistolame_core::testing::{CD_STEREO, generate_fixture};
use pistolame_library::{AlbumKey, AlbumTextOverride, LibraryStore};

/// A fresh folder with a music subfolder holding `track_count` copies of a generated FLAC,
/// and the database path beside it.
struct TestLibrary {
    folder: PathBuf,
    music: PathBuf,
    database: PathBuf,
}

impl TestLibrary {
    fn new(name: &str, track_count: usize) -> TestLibrary {
        let folder = std::env::temp_dir().join(format!("pistolame-library-test-{name}"));
        let _ = std::fs::remove_dir_all(&folder);
        let music = folder.join("Album");
        std::fs::create_dir_all(&music).unwrap();
        let fixture = generate_fixture(&CD_STEREO);
        for index in 1..=track_count {
            std::fs::copy(&fixture.path, music.join(format!("{index:02}.flac"))).unwrap();
        }
        TestLibrary {
            database: folder.join("library.sqlite3"),
            folder,
            music,
        }
    }

    fn open(&self) -> LibraryStore {
        LibraryStore::open(&self.database).unwrap()
    }

    fn track(&self, index: usize) -> PathBuf {
        self.music.join(format!("{index:02}.flac"))
    }
}

impl Drop for TestLibrary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

fn album_of(store: &LibraryStore) -> AlbumKey {
    AlbumKey::of(&store.snapshot().unwrap().tracks[0])
}

fn paths(store: &LibraryStore) -> Vec<PathBuf> {
    store
        .snapshot()
        .unwrap()
        .tracks
        .into_iter()
        .map(|track| track.path)
        .collect()
}

fn touch_with_extra_byte(path: &Path) {
    let mut bytes = std::fs::read(path).unwrap();
    bytes.push(0);
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn imported_tracks_survive_reopening_the_database() {
    let library = TestLibrary::new("survive", 2);
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    let report = store.synchronize().unwrap();
    assert!(report.has_changed);
    assert!(report.errors.is_empty());
    drop(store);
    let reopened = library.open();
    assert_eq!(paths(&reopened), vec![library.track(1), library.track(2)]);
    assert_eq!(reopened.roots().unwrap(), vec![library.music.clone()]);
    let track = &reopened.snapshot().unwrap().tracks[0];
    assert_eq!((track.sample_rate_hz, track.bits_per_sample), (44_100, 16));
}

#[test]
fn unchanged_files_are_not_read_again_and_changed_ones_are() {
    let library = TestLibrary::new("stamps", 1);
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    store.synchronize().unwrap();
    assert!(!store.synchronize().unwrap().has_changed);
    touch_with_extra_byte(&library.track(1));
    assert!(store.synchronize().unwrap().has_changed);
}

#[test]
fn new_files_are_added_and_deleted_files_removed() {
    let library = TestLibrary::new("add-remove", 2);
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    store.synchronize().unwrap();
    std::fs::copy(library.track(1), library.track(3)).unwrap();
    std::fs::remove_file(library.track(2)).unwrap();
    assert!(store.synchronize().unwrap().has_changed);
    assert_eq!(paths(&store), vec![library.track(1), library.track(3)]);
}

#[test]
fn tracks_of_a_missing_root_are_kept() {
    let library = TestLibrary::new("unplugged", 2);
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    store.synchronize().unwrap();
    // Simulates an unplugged drive: the whole root disappears.
    std::fs::rename(&library.music, library.folder.join("elsewhere")).unwrap();
    assert!(!store.synchronize().unwrap().has_changed);
    assert_eq!(paths(&store).len(), 2);
}

#[test]
fn a_chosen_cover_replaces_and_restores_the_album_cover() {
    let library = TestLibrary::new("cover", 2);
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    store.synchronize().unwrap();
    let album = album_of(&store);
    assert!(store.snapshot().unwrap().artwork_overrides.is_empty());

    store.set_album_artwork(&album, b"first cover").unwrap();
    store.set_album_artwork(&album, b"second cover").unwrap();
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.artwork_overrides.len(), 1);
    assert_eq!(&*snapshot.artwork_overrides[&album], b"second cover");
    // The replaced image is no longer stored.
    assert_eq!(
        store.album_artwork_override(&album).unwrap().as_deref(),
        Some(&b"second cover"[..])
    );

    store.clear_album_artwork(&album).unwrap();
    assert!(store.snapshot().unwrap().artwork_overrides.is_empty());
    assert_eq!(store.album_artwork_override(&album).unwrap(), None);
}

#[test]
fn a_cover_shared_by_an_album_is_stored_once() {
    let library = TestLibrary::new("dedup", 3);
    std::fs::write(library.music.join("cover.jpg"), b"jpeg bytes").unwrap();
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    store.synchronize().unwrap();
    let tracks = store.snapshot().unwrap().tracks;
    let covers: Vec<_> = tracks
        .iter()
        .map(|track| track.artwork.clone().unwrap())
        .collect();
    assert_eq!(&*covers[0], b"jpeg bytes");
    // One shared allocation, not one copy per track.
    assert!(std::sync::Arc::ptr_eq(&covers[0], &covers[2]));
}

#[test]
fn a_too_large_cover_is_refused() {
    let library = TestLibrary::new("large-cover", 1);
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    store.synchronize().unwrap();
    let album = album_of(&store);
    let oversized = vec![0u8; pistolame_library::MAX_COVER_BYTES as usize + 1];
    assert!(store.set_album_artwork(&album, &oversized).is_err());
}

#[test]
fn edited_album_text_survives_reopening_and_can_be_cleared() {
    let library = TestLibrary::new("album-text", 1);
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    store.synchronize().unwrap();
    let album = album_of(&store);
    let edited = AlbumTextOverride {
        title: Some("Live à Paris".into()),
        artist: None,
    };
    store.set_album_text(&album, &edited).unwrap();
    drop(store);
    let mut store = library.open();
    assert_eq!(store.snapshot().unwrap().text_overrides[&album], edited);
    store
        .set_album_text(&album, &AlbumTextOverride::default())
        .unwrap();
    assert!(store.snapshot().unwrap().text_overrides.is_empty());
}
