//! The persistent library: imported roots, track metadata, covers and the covers the user
//! chose, in one SQLite file (plan docs/plans/persistent-library.md). Music files are only read.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use md5::{Digest, Md5};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{MAX_COVER_BYTES, MusicFile, collect_audio_paths, read_music_file};

/// Schema version stored in `PRAGMA user_version`; bump it with a migration when the schema
/// changes.
const SCHEMA_VERSION: i64 = 8;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS roots (
    path TEXT PRIMARY KEY NOT NULL
);
CREATE TABLE IF NOT EXISTS artworks (
    hash TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS tracks (
    path TEXT PRIMARY KEY NOT NULL,
    root TEXT NOT NULL,
    file_size INTEGER NOT NULL,
    modified_nanoseconds INTEGER NOT NULL,
    title TEXT NOT NULL,
    album TEXT NOT NULL,
    artist TEXT NOT NULL,
    disc_number INTEGER NOT NULL,
    track_number INTEGER NOT NULL,
    sample_rate_hz INTEGER NOT NULL,
    bits_per_sample INTEGER NOT NULL,
    codec TEXT NOT NULL DEFAULT 'FLAC',
    is_lossy INTEGER NOT NULL DEFAULT 0,
    duration_seconds INTEGER NOT NULL,
    artwork_hash TEXT REFERENCES artworks(hash),
    year INTEGER,
    sort_album TEXT,
    sort_album_artist TEXT,
    genre TEXT,
    added_seconds INTEGER NOT NULL DEFAULT 0,
    play_count INTEGER NOT NULL DEFAULT 0,
    last_played_seconds INTEGER
);
CREATE TABLE IF NOT EXISTS artist_images (
    artist TEXT PRIMARY KEY NOT NULL,
    artwork_hash TEXT REFERENCES artworks(hash),
    checked_seconds INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS favorites (
    track_path TEXT PRIMARY KEY NOT NULL,
    added_seconds INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS playlists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    artwork_hash TEXT REFERENCES artworks(hash)
);
CREATE TABLE IF NOT EXISTS playlist_entries (
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    track_path TEXT NOT NULL,
    PRIMARY KEY (playlist_id, position)
);
CREATE TABLE IF NOT EXISTS album_text_overrides (
    directory TEXT NOT NULL,
    album TEXT NOT NULL,
    title TEXT,
    artist TEXT,
    PRIMARY KEY (directory, album)
);
CREATE TABLE IF NOT EXISTS album_artwork_overrides (
    directory TEXT NOT NULL,
    album TEXT NOT NULL,
    artwork_hash TEXT NOT NULL REFERENCES artworks(hash),
    PRIMARY KEY (directory, album)
);
";

/// Identifies an album the way the player groups tracks: same folder, same album tag.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AlbumKey {
    /// Folder that holds the album's files.
    pub directory: PathBuf,
    /// Album title from the tags (or the folder name).
    pub title: String,
}

impl AlbumKey {
    /// The album a track belongs to.
    pub fn of(track: &MusicFile) -> AlbumKey {
        AlbumKey {
            directory: track.path.parent().unwrap_or(Path::new("")).to_owned(),
            title: track.album.clone(),
        }
    }
}

/// Everything the UI needs to show the library.
#[derive(Clone, Debug, Default)]
pub struct LibrarySnapshot {
    /// Every known track; `artwork` is the file's own cover (embedded or beside it).
    pub tracks: Vec<MusicFile>,
    /// Covers the user chose, which replace the files' own covers for these albums.
    pub artwork_overrides: HashMap<AlbumKey, Arc<[u8]>>,
    /// Album titles and artists the user edited, shown instead of the tags.
    pub text_overrides: HashMap<AlbumKey, AlbumTextOverride>,
    /// The user's playlists, in creation order.
    pub playlists: Vec<Playlist>,
    /// Artist photos found online, by artist name as the library shows it.
    pub artist_images: HashMap<String, Arc<[u8]>>,
}

/// A user playlist: an ordered list of track paths. A track may appear twice; an entry whose
/// file left the library stays in the list and is simply not shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Playlist {
    /// Stable identifier in the database.
    pub id: i64,
    /// Name shown in the sidebar.
    pub name: String,
    /// Tracks in playing order.
    pub track_paths: Vec<PathBuf>,
    /// Cover the user chose for the playlist, if any.
    pub artwork: Option<Arc<[u8]>>,
}

/// Album title and artist chosen by the user; `None` keeps what the tags say.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlbumTextOverride {
    /// Title shown instead of the album tag.
    pub title: Option<String>,
    /// Artist shown instead of the album artist tag.
    pub artist: Option<String>,
}

/// What a synchronization did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SynchronizationReport {
    /// Files or folders that could not be read, as display text.
    pub errors: Vec<String>,
    /// Whether any track was added, updated or removed.
    pub has_changed: bool,
}

/// Library errors.
#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    /// The database could not be opened, read or written.
    #[error("bibliothèque : {0}")]
    Database(#[from] rusqlite::Error),
    /// The database folder could not be created.
    #[error("bibliothèque : {0}")]
    Io(#[from] std::io::Error),
    /// The chosen cover is larger than the accepted maximum.
    #[error("l’image dépasse {} Mo", MAX_COVER_BYTES / (1024 * 1024))]
    CoverTooLarge,
}

/// The library database. Open one per thread; SQLite serializes concurrent writers.
pub struct LibraryStore {
    connection: Connection,
}

impl LibraryStore {
    /// Opens (or creates) the library at `path`, creating its folder and schema.
    ///
    /// # Errors
    /// Fails when the folder or the database cannot be created or read.
    pub fn open(path: &Path) -> Result<LibraryStore, LibraryError> {
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.execute_batch(SCHEMA)?;
        migrate(&connection)?;
        connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(LibraryStore { connection })
    }

    /// Remembers files or folders to keep in the library. Already known roots are ignored.
    ///
    /// # Errors
    /// Fails when the database cannot be written.
    pub fn add_roots(&mut self, roots: &[PathBuf]) -> Result<(), LibraryError> {
        let transaction = self.connection.transaction()?;
        for root in roots {
            transaction.execute(
                "INSERT OR IGNORE INTO roots (path) VALUES (?1)",
                params![path_text(root)],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Forgets a root and every track found under it (the files themselves stay on disk).
    ///
    /// # Errors
    /// Fails when the database cannot be written.
    pub fn remove_root(&mut self, root: &Path) -> Result<(), LibraryError> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM roots WHERE path = ?1",
            params![path_text(root)],
        )?;
        transaction.execute(
            "DELETE FROM tracks WHERE root = ?1",
            params![path_text(root)],
        )?;
        transaction.commit()?;
        self.delete_unused_artworks()
    }

    /// The remembered roots.
    ///
    /// # Errors
    /// Fails when the database cannot be read.
    pub fn roots(&self) -> Result<Vec<PathBuf>, LibraryError> {
        let mut statement = self
            .connection
            .prepare("SELECT path FROM roots ORDER BY path")?;
        let roots = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|path| path.map(PathBuf::from))
            .collect::<Result<_, _>>()?;
        Ok(roots)
    }

    /// Every track and every chosen cover. Each cover is loaded once and shared.
    ///
    /// # Errors
    /// Fails when the database cannot be read.
    pub fn snapshot(&self) -> Result<LibrarySnapshot, LibraryError> {
        let artworks = self.artworks()?;
        let cover = |hash: Option<String>| hash.and_then(|hash| artworks.get(&hash).cloned());
        let mut statement = self.connection.prepare(
            "SELECT path, title, album, artist, disc_number, track_number, sample_rate_hz,
                    bits_per_sample, duration_seconds, artwork_hash, year, sort_album,
                    sort_album_artist, genre, added_seconds, play_count, last_played_seconds,
                    codec, is_lossy
             FROM tracks ORDER BY path",
        )?;
        let tracks = statement
            .query_map([], |row| {
                Ok(MusicFile {
                    path: PathBuf::from(row.get::<_, String>(0)?),
                    title: row.get(1)?,
                    album: row.get(2)?,
                    artist: row.get(3)?,
                    disc_number: stored_count(row.get(4)?),
                    track_number: stored_count(row.get(5)?),
                    sample_rate_hz: row.get(6)?,
                    bits_per_sample: row.get(7)?,
                    duration_seconds: stored_count(row.get(8)?),
                    artwork: cover(row.get(9)?),
                    year: row.get(10)?,
                    sort_album: row.get(11)?,
                    sort_album_artist: row.get(12)?,
                    genre: row.get(13)?,
                    added_seconds: row.get(14)?,
                    play_count: row.get(15)?,
                    last_played_seconds: row.get(16)?,
                    codec: row.get(17)?,
                    is_lossy: row.get(18)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut statement = self
            .connection
            .prepare("SELECT directory, album, artwork_hash FROM album_artwork_overrides")?;
        let artwork_overrides = statement
            .query_map([], |row| {
                Ok((
                    AlbumKey {
                        directory: PathBuf::from(row.get::<_, String>(0)?),
                        title: row.get(1)?,
                    },
                    row.get::<_, String>(2)?,
                ))
            })?
            .filter_map(|entry| match entry {
                Ok((key, hash)) => artworks.get(&hash).cloned().map(|bytes| Ok((key, bytes))),
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(LibrarySnapshot {
            tracks,
            artwork_overrides,
            text_overrides: self.text_overrides()?,
            playlists: self.playlists()?,
            artist_images: self.artist_images(&artworks)?,
        })
    }

    /// Artist photos with their bytes (artists looked up without result are left out).
    fn artist_images(
        &self,
        artworks: &HashMap<String, Arc<[u8]>>,
    ) -> Result<HashMap<String, Arc<[u8]>>, LibraryError> {
        let mut statement = self.connection.prepare(
            "SELECT artist, artwork_hash FROM artist_images WHERE artwork_hash IS NOT NULL",
        )?;
        let images = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|row| match row {
                Ok((artist, hash)) => artworks.get(&hash).map(|bytes| Ok((artist, bytes.clone()))),
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(images)
    }

    /// Rescans every root: reads new and changed files, removes files that disappeared from a
    /// root that still exists, keeps everything under a root that is missing (an unplugged
    /// drive), then deletes covers nothing uses any more.
    ///
    /// # Errors
    /// Fails when the database cannot be read or written; unreadable files are reported in the
    /// returned errors instead.
    pub fn synchronize(&mut self) -> Result<SynchronizationReport, LibraryError> {
        let mut report = SynchronizationReport::default();
        let known = self.known_file_stamps()?;
        let mut seen = HashSet::new();
        for root in self.roots()? {
            if !root.exists() {
                // Unplugged drive or renamed folder: keep its tracks, never silently empty.
                seen.extend(known.keys().filter(|path| path.starts_with(&root)).cloned());
                continue;
            }
            let (paths, errors) = collect_audio_paths(std::slice::from_ref(&root));
            report.errors.extend(errors);
            for path in paths {
                seen.insert(path.clone());
                let Some(stamp) = FileStamp::of(&path) else {
                    report
                        .errors
                        .push(format!("{} : fichier illisible", path.display()));
                    continue;
                };
                if known.get(&path) == Some(&stamp) {
                    continue;
                }
                match read_music_file(&path) {
                    Ok(track) => {
                        self.upsert_track(&root, &track, stamp)?;
                        report.has_changed = true;
                    }
                    Err(error) => report.errors.push(format!("{} : {error}", path.display())),
                }
            }
        }
        for path in known.keys().filter(|path| !seen.contains(*path)) {
            self.connection.execute(
                "DELETE FROM tracks WHERE path = ?1",
                params![path_text(path)],
            )?;
            report.has_changed = true;
        }
        self.delete_unused_artworks()?;
        Ok(report)
    }

    /// Replaces the cover shown for `album` by `image_bytes` (the music files are untouched).
    ///
    /// # Errors
    /// Fails when the image is too large or the database cannot be written.
    pub fn set_album_artwork(
        &mut self,
        album: &AlbumKey,
        image_bytes: &[u8],
    ) -> Result<(), LibraryError> {
        if image_bytes.len() as u64 > MAX_COVER_BYTES {
            return Err(LibraryError::CoverTooLarge);
        }
        let hash = self.store_artwork(image_bytes)?;
        self.connection.execute(
            "INSERT INTO album_artwork_overrides (directory, album, artwork_hash)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (directory, album) DO UPDATE SET artwork_hash = excluded.artwork_hash",
            params![path_text(&album.directory), album.title, hash],
        )?;
        self.delete_unused_artworks()
    }

    /// Counts one more play of `path` and remembers when it happened.
    ///
    /// # Errors
    /// Fails when the database cannot be written.
    pub fn record_play(&mut self, path: &Path) -> Result<(), LibraryError> {
        self.connection.execute(
            "UPDATE tracks SET play_count = play_count + 1, last_played_seconds = ?2
             WHERE path = ?1",
            params![path_text(path), now_seconds()],
        )?;
        Ok(())
    }

    /// Shows `text` instead of the tags for `album`. An override with neither field set removes
    /// it. Music files are never modified.
    ///
    /// # Errors
    /// Fails when the database cannot be written.
    pub fn set_album_text(
        &mut self,
        album: &AlbumKey,
        text: &AlbumTextOverride,
    ) -> Result<(), LibraryError> {
        if text.title.is_none() && text.artist.is_none() {
            self.connection.execute(
                "DELETE FROM album_text_overrides WHERE directory = ?1 AND album = ?2",
                params![path_text(&album.directory), album.title],
            )?;
            return Ok(());
        }
        self.connection.execute(
            "INSERT INTO album_text_overrides (directory, album, title, artist)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (directory, album) DO UPDATE SET
                title = excluded.title, artist = excluded.artist",
            params![
                path_text(&album.directory),
                album.title,
                text.title,
                text.artist
            ],
        )?;
        Ok(())
    }

    /// Creates an empty playlist and returns its identifier.
    ///
    /// # Errors
    /// Fails when the database cannot be written.
    pub fn create_playlist(&mut self, name: &str) -> Result<i64, LibraryError> {
        self.connection
            .execute("INSERT INTO playlists (name) VALUES (?1)", params![name])?;
        Ok(self.connection.last_insert_rowid())
    }

    /// Renames playlist `id`.
    ///
    /// # Errors
    /// Fails when the database cannot be written.
    pub fn rename_playlist(&mut self, id: i64, name: &str) -> Result<(), LibraryError> {
        self.connection.execute(
            "UPDATE playlists SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(())
    }

    /// Marks `path` as a favorite, or not. Kept by path, like playlist entries, so a rescan or a
    /// folder removed then added again keeps it.
    ///
    /// # Errors
    /// Fails when the database cannot be written.
    pub fn set_favorite(&mut self, path: &Path, is_favorite: bool) -> Result<(), LibraryError> {
        if is_favorite {
            self.connection.execute(
                "INSERT OR IGNORE INTO favorites (track_path, added_seconds) VALUES (?1, ?2)",
                params![path_text(path), now_seconds()],
            )?;
        } else {
            self.connection.execute(
                "DELETE FROM favorites WHERE track_path = ?1",
                params![path_text(path)],
            )?;
        }
        Ok(())
    }

    /// The favorite files, most recently marked first.
    ///
    /// # Errors
    /// Fails when the database cannot be read.
    pub fn favorites(&self) -> Result<Vec<PathBuf>, LibraryError> {
        let mut statement = self
            .connection
            .prepare("SELECT track_path FROM favorites ORDER BY added_seconds DESC, rowid DESC")?;
        let paths = statement
            .query_map([], |row| Ok(PathBuf::from(row.get::<_, String>(0)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(paths)
    }

    /// Remembers the photo found for `artist`, or that none was found (`None`), so the lookup
    /// is not repeated until [`LibraryStore::artists_to_look_up`] says it is due again.
    ///
    /// # Errors
    /// Fails when the database cannot be written.
    pub fn set_artist_image(
        &mut self,
        artist: &str,
        image: Option<&[u8]>,
    ) -> Result<(), LibraryError> {
        let hash = match image {
            Some(bytes) => Some(self.store_artwork(bytes)?),
            None => None,
        };
        self.connection.execute(
            "INSERT INTO artist_images (artist, artwork_hash, checked_seconds) VALUES (?1, ?2, ?3)
             ON CONFLICT (artist) DO UPDATE SET artwork_hash = excluded.artwork_hash,
                checked_seconds = excluded.checked_seconds",
            params![artist, hash, now_seconds()],
        )?;
        self.delete_unused_artworks()
    }

    /// The artists of `artists` never looked up, or looked up without result more than
    /// `retry_after_seconds` ago (a photo may have been added online since).
    ///
    /// # Errors
    /// Fails when the database cannot be read.
    pub fn artists_to_look_up(
        &self,
        artists: &[String],
        retry_after_seconds: i64,
    ) -> Result<Vec<String>, LibraryError> {
        let mut statement = self.connection.prepare(
            "SELECT artwork_hash IS NOT NULL, checked_seconds FROM artist_images WHERE artist = ?1",
        )?;
        let now = now_seconds();
        let mut due = Vec::new();
        for artist in artists {
            let known: Option<(bool, i64)> = statement
                .query_row(params![artist], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;
            let is_due = match known {
                None => true,
                Some((has_photo, checked)) => {
                    !has_photo && now.saturating_sub(checked) >= retry_after_seconds
                }
            };
            if is_due {
                due.push(artist.clone());
            }
        }
        Ok(due)
    }

    /// Deletes playlist `id` and its entries (the tracks stay in the library).
    ///
    /// # Errors
    /// Fails when the database cannot be written.
    pub fn delete_playlist(&mut self, id: i64) -> Result<(), LibraryError> {
        self.connection
            .execute("DELETE FROM playlists WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Adds `paths` at the end of playlist `id`, in order.
    ///
    /// # Errors
    /// Fails when the database cannot be written.
    pub fn append_to_playlist(&mut self, id: i64, paths: &[PathBuf]) -> Result<(), LibraryError> {
        let transaction = self.connection.transaction()?;
        let next_position: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM playlist_entries WHERE playlist_id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        for (offset, path) in paths.iter().enumerate() {
            transaction.execute(
                "INSERT INTO playlist_entries (playlist_id, position, track_path)
                 VALUES (?1, ?2, ?3)",
                params![id, next_position + offset as i64, path_text(path)],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Removes the entry at `position` (0-based, among every entry) from playlist `id`; the
    /// following entries move up by one.
    ///
    /// # Errors
    /// Fails when the database cannot be written.
    pub fn remove_from_playlist(&mut self, id: i64, position: usize) -> Result<(), LibraryError> {
        let remaining: Vec<PathBuf> = self
            .playlist_paths(id)?
            .into_iter()
            .enumerate()
            .filter(|(index, _)| *index != position)
            .map(|(_, path)| path)
            .collect();
        self.replace_playlist_entries(id, &remaining)
    }

    /// Makes `paths` the entries of playlist `id`, in order (several removals at once, or
    /// putting back the entries an undo restores).
    ///
    /// # Errors
    /// Fails when the database cannot be written.
    pub fn replace_playlist_entries(
        &mut self,
        id: i64,
        paths: &[PathBuf],
    ) -> Result<(), LibraryError> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM playlist_entries WHERE playlist_id = ?1",
            params![id],
        )?;
        for (index, path) in paths.iter().enumerate() {
            transaction.execute(
                "INSERT INTO playlist_entries (playlist_id, position, track_path)
                 VALUES (?1, ?2, ?3)",
                params![id, index as i64, path_text(path)],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Shows `image_bytes` as the cover of playlist `id`.
    ///
    /// # Errors
    /// Fails when the image is too large or the database cannot be written.
    pub fn set_playlist_artwork(
        &mut self,
        id: i64,
        image_bytes: &[u8],
    ) -> Result<(), LibraryError> {
        if image_bytes.len() as u64 > MAX_COVER_BYTES {
            return Err(LibraryError::CoverTooLarge);
        }
        let hash = self.store_artwork(image_bytes)?;
        self.connection.execute(
            "UPDATE playlists SET artwork_hash = ?2 WHERE id = ?1",
            params![id, hash],
        )?;
        self.delete_unused_artworks()
    }

    /// Removes the cover of playlist `id`.
    ///
    /// # Errors
    /// Fails when the database cannot be written.
    pub fn clear_playlist_artwork(&mut self, id: i64) -> Result<(), LibraryError> {
        self.connection.execute(
            "UPDATE playlists SET artwork_hash = NULL WHERE id = ?1",
            params![id],
        )?;
        self.delete_unused_artworks()
    }

    fn playlists(&self) -> Result<Vec<Playlist>, LibraryError> {
        let artworks = self.artworks()?;
        let mut statement = self
            .connection
            .prepare("SELECT id, name, artwork_hash FROM playlists ORDER BY id")?;
        let headers: Vec<(i64, String, Option<String>)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<_, _>>()?;
        headers
            .into_iter()
            .map(|(id, name, artwork_hash)| {
                Ok(Playlist {
                    id,
                    name,
                    track_paths: self.playlist_paths(id)?,
                    artwork: artwork_hash.and_then(|hash| artworks.get(&hash).cloned()),
                })
            })
            .collect()
    }

    fn playlist_paths(&self, id: i64) -> Result<Vec<PathBuf>, LibraryError> {
        let mut statement = self.connection.prepare(
            "SELECT track_path FROM playlist_entries WHERE playlist_id = ?1 ORDER BY position",
        )?;
        let paths = statement
            .query_map(params![id], |row| row.get::<_, String>(0))?
            .map(|path| path.map(PathBuf::from))
            .collect::<Result<_, _>>()?;
        Ok(paths)
    }

    fn text_overrides(&self) -> Result<HashMap<AlbumKey, AlbumTextOverride>, LibraryError> {
        let mut statement = self
            .connection
            .prepare("SELECT directory, album, title, artist FROM album_text_overrides")?;
        let overrides = statement
            .query_map([], |row| {
                Ok((
                    AlbumKey {
                        directory: PathBuf::from(row.get::<_, String>(0)?),
                        title: row.get(1)?,
                    },
                    AlbumTextOverride {
                        title: row.get(2)?,
                        artist: row.get(3)?,
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        Ok(overrides)
    }

    /// Goes back to the files' own cover for `album`.
    ///
    /// # Errors
    /// Fails when the database cannot be written.
    pub fn clear_album_artwork(&mut self, album: &AlbumKey) -> Result<(), LibraryError> {
        self.connection.execute(
            "DELETE FROM album_artwork_overrides WHERE directory = ?1 AND album = ?2",
            params![path_text(&album.directory), album.title],
        )?;
        self.delete_unused_artworks()
    }

    fn artworks(&self) -> Result<HashMap<String, Arc<[u8]>>, LibraryError> {
        let mut statement = self.connection.prepare("SELECT hash, data FROM artworks")?;
        let artworks = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    Arc::from(row.get::<_, Vec<u8>>(1)?),
                ))
            })?
            .collect::<Result<_, _>>()?;
        Ok(artworks)
    }

    fn known_file_stamps(&self) -> Result<HashMap<PathBuf, FileStamp>, LibraryError> {
        let mut statement = self
            .connection
            .prepare("SELECT path, file_size, modified_nanoseconds FROM tracks")?;
        let stamps = statement
            .query_map([], |row| {
                Ok((
                    PathBuf::from(row.get::<_, String>(0)?),
                    FileStamp {
                        size: row.get(1)?,
                        modified_nanoseconds: row.get(2)?,
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        Ok(stamps)
    }

    fn upsert_track(
        &mut self,
        root: &Path,
        track: &MusicFile,
        stamp: FileStamp,
    ) -> Result<(), LibraryError> {
        let artwork_hash = match &track.artwork {
            Some(bytes) => Some(self.store_artwork(bytes)?),
            None => None,
        };
        self.connection.execute(
            "INSERT INTO tracks (path, root, file_size, modified_nanoseconds, title, album, artist,
                                 disc_number, track_number, sample_rate_hz, bits_per_sample,
                                 duration_seconds, artwork_hash, year, sort_album,
                                 sort_album_artist, genre, added_seconds, codec, is_lossy)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                     ?19, ?20)
             ON CONFLICT (path) DO UPDATE SET
                year = excluded.year, sort_album = excluded.sort_album,
                sort_album_artist = excluded.sort_album_artist, genre = excluded.genre,
                root = excluded.root, file_size = excluded.file_size,
                modified_nanoseconds = excluded.modified_nanoseconds, title = excluded.title,
                album = excluded.album, artist = excluded.artist,
                disc_number = excluded.disc_number, track_number = excluded.track_number,
                sample_rate_hz = excluded.sample_rate_hz,
                bits_per_sample = excluded.bits_per_sample,
                codec = excluded.codec, is_lossy = excluded.is_lossy,
                duration_seconds = excluded.duration_seconds,
                artwork_hash = excluded.artwork_hash",
            params![
                path_text(&track.path),
                path_text(root),
                stamp.size,
                stamp.modified_nanoseconds,
                track.title,
                track.album,
                track.artist,
                sql_integer(track.disc_number),
                sql_integer(track.track_number),
                track.sample_rate_hz,
                track.bits_per_sample,
                sql_integer(track.duration_seconds),
                artwork_hash,
                track.year,
                track.sort_album,
                track.sort_album_artist,
                track.genre,
                now_seconds(),
                track.codec,
                track.is_lossy,
            ],
        )?;
        Ok(())
    }

    /// Stores a cover once, keyed by the MD5 of its bytes (deduplication, not security).
    fn store_artwork(&mut self, bytes: &[u8]) -> Result<String, LibraryError> {
        let hash: String = Md5::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.connection.execute(
            "INSERT OR IGNORE INTO artworks (hash, data) VALUES (?1, ?2)",
            params![hash, bytes],
        )?;
        Ok(hash)
    }

    fn delete_unused_artworks(&mut self) -> Result<(), LibraryError> {
        self.connection.execute(
            "DELETE FROM artworks WHERE hash NOT IN (
                SELECT artwork_hash FROM tracks WHERE artwork_hash IS NOT NULL
                UNION SELECT artwork_hash FROM album_artwork_overrides
                UNION SELECT artwork_hash FROM playlists WHERE artwork_hash IS NOT NULL
                UNION SELECT artwork_hash FROM artist_images WHERE artwork_hash IS NOT NULL)",
            [],
        )?;
        Ok(())
    }

    /// The chosen cover of `album`, if any (used by tests and diagnostics).
    ///
    /// # Errors
    /// Fails when the database cannot be read.
    pub fn album_artwork_override(
        &self,
        album: &AlbumKey,
    ) -> Result<Option<Vec<u8>>, LibraryError> {
        Ok(self
            .connection
            .query_row(
                "SELECT data FROM artworks JOIN album_artwork_overrides
                 ON artworks.hash = album_artwork_overrides.artwork_hash
                 WHERE directory = ?1 AND album = ?2",
                params![path_text(&album.directory), album.title],
                |row| row.get(0),
            )
            .optional()?)
    }
}

/// Brings a database created by an older version up to the current schema. `CREATE TABLE IF
/// NOT EXISTS` never adds columns to an existing table, so added columns are handled here.
fn migrate(connection: &Connection) -> Result<(), LibraryError> {
    // Version 5 reads the year, sort tags and genre: add the columns, then mark every known file
    // as changed so the next synchronization reads its tags again.
    if !has_column(connection, "tracks", "year")? {
        for column in [
            "year INTEGER",
            "sort_album TEXT",
            "sort_album_artist TEXT",
            "genre TEXT",
            "added_seconds INTEGER NOT NULL DEFAULT 0",
            "play_count INTEGER NOT NULL DEFAULT 0",
            "last_played_seconds INTEGER",
        ] {
            connection.execute(&format!("ALTER TABLE tracks ADD COLUMN {column}"), [])?;
        }
        connection.execute("UPDATE tracks SET file_size = -1", [])?;
    }
    // Version 8 reads every audio format: until then every track was FLAC, which the column
    // defaults state, so no file needs reading again.
    if !has_column(connection, "tracks", "codec")? {
        for column in [
            "codec TEXT NOT NULL DEFAULT 'FLAC'",
            "is_lossy INTEGER NOT NULL DEFAULT 0",
        ] {
            connection.execute(&format!("ALTER TABLE tracks ADD COLUMN {column}"), [])?;
        }
    }
    if !has_column(connection, "playlists", "artwork_hash")? {
        connection.execute(
            "ALTER TABLE playlists ADD COLUMN artwork_hash TEXT REFERENCES artworks(hash)",
            [],
        )?;
    }
    Ok(())
}

fn has_column(connection: &Connection, table: &str, column: &str) -> Result<bool, LibraryError> {
    Ok(connection
        .prepare("SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2")?
        .exists(params![table, column])?)
}

/// Seconds since the Unix epoch, for "added" and "last played" dates.
fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Size and modification time: a file whose stamp is unchanged is not read again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    size: i64,
    modified_nanoseconds: i64,
}

impl FileStamp {
    fn of(path: &Path) -> Option<FileStamp> {
        let metadata = std::fs::metadata(path).ok()?;
        let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
        Some(FileStamp {
            size: i64::try_from(metadata.len()).ok()?,
            modified_nanoseconds: i64::try_from(modified.as_nanos()).ok()?,
        })
    }
}

/// SQLite integers are signed 64-bit; counts beyond that range (never met in tags) saturate.
fn sql_integer(count: u64) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

/// Inverse of `sql_integer`; a negative value (a corrupted row) reads as 0.
fn stored_count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Paths are stored as text; non-UTF-8 parts (rare on macOS and Windows) are replaced, which
/// only affects display and lookups of such paths.
fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
