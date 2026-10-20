//! Persistent library behavior (docs/plans/persistent-library.md, "Tests").
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use pistolame_core::testing::{CD_STEREO, generate_fixture};
use pistolame_library::{AlbumKey, AlbumTextOverride, ArtworkId, LibraryStore};

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
fn synchronizing_changed_paths_adds_updates_and_removes_only_them() {
    let library = TestLibrary::new("changed-paths", 2);
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    store.synchronize().unwrap();
    let disc = library.music.join("Disc 2");
    std::fs::create_dir_all(&disc).unwrap();
    std::fs::copy(library.track(1), disc.join("01.flac")).unwrap();
    std::fs::copy(library.track(1), disc.join("02.flac")).unwrap();
    std::fs::copy(library.track(1), library.track(3)).unwrap();
    // A new folder, a new file, and a path outside every root (ignored).
    let report = store
        .synchronize_paths(&[
            disc.clone(),
            library.track(3),
            disc.join("01.flac"),
            library.folder.join("outside.flac"),
        ])
        .unwrap();
    assert!(report.has_changed);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(paths(&store).len(), 5);
    // Unchanged paths: nothing to do.
    assert!(
        !store
            .synchronize_paths(std::slice::from_ref(&disc))
            .unwrap()
            .has_changed
    );
    // A removed folder takes its tracks along, and only them; a removed file too.
    std::fs::remove_dir_all(&disc).unwrap();
    std::fs::remove_file(library.track(2)).unwrap();
    let report = store
        .synchronize_paths(&[disc.clone(), library.track(2)])
        .unwrap();
    assert!(report.has_changed);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(paths(&store), vec![library.track(1), library.track(3)]);
}

#[test]
fn a_rename_that_only_changes_case_keeps_one_copy_of_each_track() {
    let library = TestLibrary::new("case-rename", 1);
    let disc = library.music.join("disc 2");
    std::fs::create_dir_all(&disc).unwrap();
    std::fs::copy(library.track(1), disc.join("01.flac")).unwrap();
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    store.synchronize().unwrap();
    let renamed = library.music.join("Disc 2");
    std::fs::rename(&disc, &renamed).unwrap();
    // A watcher reports both spellings.
    store
        .synchronize_paths(&[disc.clone(), renamed.clone()])
        .unwrap();
    assert_eq!(
        paths(&store),
        vec![library.track(1), renamed.join("01.flac")]
    );
}

#[test]
fn a_folder_sharing_a_prefix_is_not_taken_for_a_subfolder() {
    let library = TestLibrary::new("prefix", 1);
    let sibling = library.folder.join("Album 2");
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::copy(library.track(1), sibling.join("01.flac")).unwrap();
    let mut store = library.open();
    store
        .add_roots(&[library.music.clone(), sibling.clone()])
        .unwrap();
    store.synchronize().unwrap();
    // "Album" is gone, but "Album 2" (same prefix) must keep its track.
    std::fs::remove_file(library.track(1)).unwrap();
    store
        .synchronize_paths(std::slice::from_ref(&library.music))
        .unwrap();
    assert_eq!(paths(&store), vec![sibling.join("01.flac")]);
}

#[test]
fn changed_paths_under_a_missing_root_remove_nothing() {
    let library = TestLibrary::new("unplugged-paths", 2);
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    store.synchronize().unwrap();
    std::fs::rename(&library.music, library.folder.join("elsewhere")).unwrap();
    let report = store
        .synchronize_paths(&[library.music.clone(), library.track(1)])
        .unwrap();
    assert!(!report.has_changed);
    assert_eq!(paths(&store).len(), 2);
}

#[test]
fn imports_larger_than_one_batch_are_complete() {
    // More files than one write batch (256), all of them stored.
    let library = TestLibrary::new("batches", 300);
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    let report = store.synchronize().unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(paths(&store).len(), 300);
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
    let chosen = &snapshot.artwork_overrides[&album];
    assert_eq!(*chosen, ArtworkId::of_bytes(b"second cover"));
    assert_eq!(
        store.artwork_bytes(chosen).unwrap().as_deref(),
        Some(&b"second cover"[..])
    );
    // The replaced image is no longer stored.
    assert_eq!(
        store
            .artwork_bytes(&ArtworkId::of_bytes(b"first cover"))
            .unwrap(),
        None
    );
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
    assert_eq!(covers[0], ArtworkId::of_bytes(b"jpeg bytes"));
    assert_eq!(covers[0], covers[2]);
    // One stored image, not one copy per track.
    assert_eq!(
        store
            .stored_artwork_ids()
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        vec![covers[0].clone()]
    );
    assert_eq!(
        store.artwork_bytes(&covers[0]).unwrap().as_deref(),
        Some(&b"jpeg bytes"[..])
    );
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

#[test]
fn playlists_keep_order_and_survive_reopening() {
    let library = TestLibrary::new("playlists", 3);
    let mut store = library.open();
    let id = store.create_playlist("Écoute critique").unwrap();
    store
        .append_to_playlist(id, &[library.track(3), library.track(1)])
        .unwrap();
    store.append_to_playlist(id, &[library.track(2)]).unwrap();
    drop(store);
    let mut store = library.open();
    let playlists = store.snapshot().unwrap().playlists;
    assert_eq!(playlists.len(), 1);
    assert_eq!(playlists[0].name, "Écoute critique");
    assert_eq!(
        playlists[0].track_paths,
        vec![library.track(3), library.track(1), library.track(2)]
    );

    store.remove_from_playlist(id, 1).unwrap();
    store.rename_playlist(id, "Favoris").unwrap();
    let playlist = &store.snapshot().unwrap().playlists[0];
    assert_eq!(playlist.name, "Favoris");
    assert_eq!(
        playlist.track_paths,
        vec![library.track(3), library.track(2)]
    );

    store.delete_playlist(id).unwrap();
    assert!(store.snapshot().unwrap().playlists.is_empty());
}

#[test]
fn playlist_entries_can_be_replaced_at_once() {
    let library = TestLibrary::new("playlist-replace", 3);
    let mut store = library.open();
    let id = store.create_playlist("Essai").unwrap();
    store
        .append_to_playlist(id, &[library.track(1), library.track(2)])
        .unwrap();
    let entries = vec![library.track(3), library.track(1), library.track(3)];
    store.replace_playlist_entries(id, &entries).unwrap();
    assert_eq!(store.snapshot().unwrap().playlists[0].track_paths, entries);
    store.replace_playlist_entries(id, &[]).unwrap();
    assert!(
        store.snapshot().unwrap().playlists[0]
            .track_paths
            .is_empty()
    );
}

#[test]
fn a_playlist_cover_is_stored_and_cleared() {
    let library = TestLibrary::new("playlist-cover", 1);
    let mut store = library.open();
    let id = store.create_playlist("Vinyles").unwrap();
    store.set_playlist_artwork(id, b"png bytes").unwrap();
    let playlist = &store.snapshot().unwrap().playlists[0];
    assert_eq!(playlist.artwork, Some(ArtworkId::of_bytes(b"png bytes")));
    store.clear_playlist_artwork(id).unwrap();
    assert!(store.snapshot().unwrap().playlists[0].artwork.is_none());
}

#[test]
fn a_version_3_database_gains_the_playlist_cover_column() {
    let library = TestLibrary::new("migration-v3", 1);
    {
        let connection = rusqlite::Connection::open(&library.database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE playlists (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);
                 INSERT INTO playlists (name) VALUES ('Ancienne');",
            )
            .unwrap();
    }
    let store = library.open();
    let playlists = store.snapshot().unwrap().playlists;
    assert_eq!(playlists[0].name, "Ancienne");
    assert!(playlists[0].artwork.is_none());
}

#[test]
fn plays_are_counted_and_added_dates_recorded() {
    let library = TestLibrary::new("plays", 1);
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    store.synchronize().unwrap();
    let before = store.snapshot().unwrap().tracks[0].clone();
    assert!(before.added_seconds > 0);
    assert_eq!((before.play_count, before.last_played_seconds), (0, None));
    store.record_play(&library.track(1)).unwrap();
    store.record_play(&library.track(1)).unwrap();
    let after = &store.snapshot().unwrap().tracks[0];
    assert_eq!(after.play_count, 2);
    assert!(after.last_played_seconds.is_some());
    // A rescan of a changed file keeps the statistics.
    touch_with_extra_byte(&library.track(1));
    store.synchronize().unwrap();
    let rescanned = &store.snapshot().unwrap().tracks[0];
    assert_eq!(rescanned.play_count, 2);
    assert_eq!(rescanned.added_seconds, before.added_seconds);
}

#[test]
fn removing_a_root_forgets_its_tracks_only() {
    let library = TestLibrary::new("remove-root", 2);
    let other = library.folder.join("Other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::copy(library.track(1), other.join("x.flac")).unwrap();
    let mut store = library.open();
    store
        .add_roots(&[library.music.clone(), other.clone()])
        .unwrap();
    store.synchronize().unwrap();
    assert_eq!(paths(&store).len(), 3);
    store.remove_root(&library.music).unwrap();
    assert_eq!(store.roots().unwrap(), vec![other.clone()]);
    assert_eq!(paths(&store), vec![other.join("x.flac")]);
    assert!(library.track(1).exists(), "files are never deleted");
}

#[test]
fn a_root_inside_another_joins_it_and_its_deleted_tracks_leave() {
    // Regression: an album folder dropped from a folder already in the library became a root
    // of its own; once deleted, it counted as an unplugged drive and its tracks stayed.
    let library = TestLibrary::new("nested-root", 2);
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    store.synchronize().unwrap();
    store
        .add_roots(&[library.folder.clone(), library.track(1)])
        .unwrap();
    assert_eq!(store.roots().unwrap(), vec![library.folder.clone()]);
    std::fs::remove_dir_all(&library.music).unwrap();
    assert!(store.synchronize().unwrap().has_changed);
    assert!(paths(&store).is_empty());
}

#[test]
fn a_deleted_folder_inside_a_root_leaves_with_the_watcher_paths() {
    let library = TestLibrary::new("nested-root-paths", 2);
    let mut store = library.open();
    store
        .add_roots(&[library.music.clone(), library.folder.clone()])
        .unwrap();
    store.synchronize().unwrap();
    std::fs::remove_dir_all(&library.music).unwrap();
    store
        .synchronize_paths(std::slice::from_ref(&library.music))
        .unwrap();
    assert!(paths(&store).is_empty());
}

#[test]
fn removed_tracks_stay_out_until_restored_or_their_folder_is_added_again() {
    let library = TestLibrary::new("removed-tracks", 2);
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    store.synchronize().unwrap();
    store.remove_tracks(&[library.track(1)]).unwrap();
    assert_eq!(paths(&store), vec![library.track(2)]);
    store.synchronize().unwrap();
    store.synchronize_paths(&[library.track(1)]).unwrap();
    assert_eq!(paths(&store), vec![library.track(2)]);
    assert!(library.track(1).exists(), "files are never deleted");

    // "Annuler" gives the track back as it was, play count included.
    store.record_play(&library.track(1)).unwrap();
    store.restore_tracks(&[library.track(1)]).unwrap();
    let restored = store.snapshot().unwrap().tracks;
    assert_eq!(restored.len(), 2);
    assert_eq!(restored[0].play_count, 1);

    store.remove_tracks(&[library.track(1)]).unwrap();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    store.synchronize().unwrap();
    assert_eq!(paths(&store).len(), 2);
}

#[test]
fn a_removed_track_whose_file_is_gone_leaves_for_good() {
    let library = TestLibrary::new("removed-gone", 2);
    let mut store = library.open();
    store
        .add_roots(std::slice::from_ref(&library.music))
        .unwrap();
    store.synchronize().unwrap();
    store.remove_tracks(&[library.track(1)]).unwrap();
    std::fs::remove_file(library.track(1)).unwrap();
    store.synchronize().unwrap();
    store.restore_tracks(&[library.track(1)]).unwrap();
    assert_eq!(paths(&store), vec![library.track(2)]);
}

#[test]
fn favorites_survive_reopening_and_can_be_removed() {
    let library = TestLibrary::new("favorites", 2);
    let mut store = library.open();
    store.set_favorite(&library.track(1), true).unwrap();
    store.set_favorite(&library.track(2), true).unwrap();
    // Marking twice changes nothing.
    store.set_favorite(&library.track(1), true).unwrap();
    drop(store);
    let mut reopened = library.open();
    assert_eq!(
        reopened.favorites().unwrap(),
        vec![library.track(2), library.track(1)]
    );
    reopened.set_favorite(&library.track(2), false).unwrap();
    assert_eq!(reopened.favorites().unwrap(), vec![library.track(1)]);
}

#[test]
fn a_late_automatic_lookup_never_replaces_a_chosen_photo() {
    let library = TestLibrary::new("artist-image-chosen", 1);
    let mut store = library.open();
    store.set_artist_image("Maes", Some(&[1, 2, 3])).unwrap();
    assert!(!store.record_looked_up_artist_image("Maes", None).unwrap());
    assert!(
        !store
            .record_looked_up_artist_image("Maes", Some(&[9, 9]))
            .unwrap()
    );
    assert!(store.record_looked_up_artist_image("Other", None).unwrap());
    let images = store.snapshot().unwrap().artist_images;
    assert_eq!(images["Maes"], ArtworkId::of_bytes(&[1, 2, 3]));
    assert_eq!(
        store.artwork_bytes(&ArtworkId::of_bytes(&[9, 9])).unwrap(),
        None
    );
}

#[test]
fn artist_photos_are_kept_and_lookups_are_not_repeated() {
    let library = TestLibrary::new("artist-images", 1);
    let mut store = library.open();
    let artists = vec!["Howard Shore".to_owned(), "Unknown".to_owned()];
    assert_eq!(store.artists_to_look_up(&artists, 3600).unwrap(), artists);
    store
        .set_artist_image("Howard Shore", Some(&[1, 2, 3]))
        .unwrap();
    store.set_artist_image("Unknown", None).unwrap();
    // Found: never again. Not found: only once the retry delay has passed.
    assert!(store.artists_to_look_up(&artists, 3600).unwrap().is_empty());
    assert_eq!(
        store.artists_to_look_up(&artists, 0).unwrap(),
        vec!["Unknown".to_owned()]
    );
    drop(store);
    let reopened = library.open();
    let images = reopened.snapshot().unwrap().artist_images;
    assert_eq!(images.len(), 1);
    assert_eq!(images["Howard Shore"], ArtworkId::of_bytes(&[1, 2, 3]));
    assert_eq!(
        reopened.artwork_bytes(&images["Howard Shore"]).unwrap(),
        Some(vec![1, 2, 3])
    );
}
