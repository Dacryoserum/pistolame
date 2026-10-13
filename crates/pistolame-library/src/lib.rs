//! File metadata and the persistent library. Scanning never modifies music files.
#![forbid(unsafe_code)]

mod store;

pub use store::{
    AlbumKey, AlbumTextOverride, LibraryError, LibrarySnapshot, LibraryStore, Playlist,
    SynchronizationReport,
};

use std::path::{Path, PathBuf};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::{FormatOptions, TrackType, probe::Hint};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTag};

/// One local FLAC and its display metadata, independent of any UI toolkit.
#[derive(Clone, Debug)]
pub struct MusicFile {
    /// Original path used by the playback engine.
    pub path: PathBuf,
    /// Track title, falling back to the filename.
    pub title: String,
    /// Album title, falling back to the parent directory.
    pub album: String,
    /// Album artist, or track artist if absent.
    pub artist: String,
    /// Disc number used before the track number when sorting.
    pub disc_number: u64,
    /// Track number from the tags.
    pub track_number: u64,
    /// Source sample rate in hertz.
    pub sample_rate_hz: u32,
    /// Source precision in bits.
    pub bits_per_sample: u32,
    /// Declared duration in seconds.
    pub duration_seconds: u64,
    /// Release year (release date first, then recording date, then original date).
    pub year: Option<u32>,
    /// Album title to sort by (ALBUMSORT tag), when the files provide one.
    pub sort_album: Option<String>,
    /// Album artist to sort by (ALBUMARTISTSORT tag), when the files provide one.
    pub sort_album_artist: Option<String>,
    /// Genre tag.
    pub genre: Option<String>,
    /// When the track entered the library (seconds since the Unix epoch; 0 outside the library).
    pub added_seconds: i64,
    /// How many times playback of the track started.
    pub play_count: u32,
    /// When playback of the track last started (seconds since the Unix epoch).
    pub last_played_seconds: Option<i64>,
    /// Encoded embedded artwork, decoded only when an album is displayed.
    pub artwork: Option<std::sync::Arc<[u8]>>,
}

/// A file that cannot be indexed. The UI reports it without interrupting other imports.
#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    /// The file cannot be read.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// The FLAC parser rejected the file.
    #[error("{0}")]
    Decode(#[from] symphonia::core::errors::Error),
    /// No audio stream could be identified.
    #[error("no audio stream")]
    NoAudio,
}

/// Reads tags, stream information and the first embedded cover without decoding audio.
///
/// # Errors
/// Returns a typed error for unreadable files or unsupported streams.
pub fn read_music_file(path: &Path) -> Result<MusicFile, MetadataError> {
    let stream = MediaSourceStream::new(Box::new(std::fs::File::open(path)?), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("flac");
    let mut reader = symphonia::default::get_probe().probe(
        &hint,
        stream,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;
    let track = reader
        .default_track(TrackType::Audio)
        .ok_or(MetadataError::NoAudio)?;
    let Some(CodecParameters::Audio(parameters)) = &track.codec_params else {
        return Err(MetadataError::NoAudio);
    };
    let rate = parameters.sample_rate.unwrap_or(0);
    let mut file = MusicFile {
        path: path.to_owned(),
        title: path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        album: path
            .parent()
            .and_then(Path::file_name)
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        artist: String::new(),
        disc_number: 0,
        track_number: 0,
        sample_rate_hz: rate,
        bits_per_sample: parameters.bits_per_sample.unwrap_or(0),
        duration_seconds: track.num_frames.unwrap_or(0) / u64::from(rate.max(1)),
        year: None,
        sort_album: None,
        sort_album_artist: None,
        genre: None,
        added_seconds: 0,
        play_count: 0,
        last_played_seconds: None,
        artwork: None,
    };
    if let Some(revision) = reader.metadata().current() {
        let mut album_artist = None;
        let mut years = YearCandidates::default();
        for tag in &revision.media.tags {
            match &tag.std {
                Some(StandardTag::ReleaseYear(year)) => years.release = Some(u32::from(*year)),
                Some(StandardTag::ReleaseDate(date)) => {
                    years.release = years.release.or(year_of(date))
                }
                Some(StandardTag::RecordingYear(year)) => years.recording = Some(u32::from(*year)),
                Some(StandardTag::RecordingDate(date)) => {
                    years.recording = years.recording.or(year_of(date))
                }
                Some(StandardTag::OriginalReleaseYear(year)) => {
                    years.original = Some(u32::from(*year))
                }
                Some(StandardTag::OriginalReleaseDate(date)) => {
                    years.original = years.original.or(year_of(date))
                }
                Some(StandardTag::SortAlbum(value)) => file.sort_album = non_empty(value),
                Some(StandardTag::SortAlbumArtist(value)) => {
                    file.sort_album_artist = non_empty(value)
                }
                Some(StandardTag::Genre(value)) => file.genre = non_empty(value),
                Some(StandardTag::TrackTitle(value)) => file.title = value.to_string(),
                Some(StandardTag::Album(value)) => file.album = value.to_string(),
                Some(StandardTag::Artist(value)) => file.artist = value.to_string(),
                Some(StandardTag::AlbumArtist(value)) => album_artist = Some(value.to_string()),
                Some(StandardTag::TrackNumber(value)) => file.track_number = *value,
                Some(StandardTag::DiscNumber(value)) => file.disc_number = *value,
                _ => {}
            }
        }
        if let Some(artist) = album_artist {
            file.artist = artist;
        }
        file.year = years.release.or(years.recording).or(years.original);
        // The FLAC metadata parser bounds individual blocks; only one cover is retained.
        file.artwork = revision
            .media
            .visuals
            .first()
            .map(|visual| visual.data.to_vec().into());
    }
    if file.artwork.is_none() {
        file.artwork = directory_cover(path);
    }
    Ok(file)
}

/// Years found in the tags, by kind; the release year wins.
#[derive(Default)]
struct YearCandidates {
    release: Option<u32>,
    recording: Option<u32>,
    original: Option<u32>,
}

/// The year of a date tag ("2011", "2011-04-17", "2011/04"): its first four digits, if plausible.
fn year_of(date: &str) -> Option<u32> {
    let digits: String = date.trim().chars().take(4).collect();
    let year: u32 = digits.parse().ok()?;
    (1000..=9999).contains(&year).then_some(year)
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Recursively collects FLAC paths, without following symbolic links.
///
/// Errors are kept alongside successful files, so unreadable folders are never silently omitted.
pub fn collect_flac_paths(roots: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    let mut pending = roots.to_vec();
    let mut files = std::collections::BTreeSet::new();
    let mut errors = Vec::new();
    while let Some(path) = pending.pop() {
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                errors.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            match std::fs::read_dir(&path) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) => pending.push(entry.path()),
                            Err(error) => errors.push(format!("{}: {error}", path.display())),
                        }
                    }
                }
                Err(error) => errors.push(format!("{}: {error}", path.display())),
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("flac"))
            && !is_apple_double(&path)
        {
            files.insert(path);
        }
    }
    (files.into_iter().collect(), errors)
}

fn is_apple_double(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with(APPLE_DOUBLE_PREFIX))
}

/// Largest cover image accepted, from a folder or chosen by the user (16 MiB, far above any
/// real album cover, small enough to keep the library light).
pub const MAX_COVER_BYTES: u64 = 16 * 1024 * 1024;

/// Prefix of AppleDouble files, the metadata companions macOS writes on non-Apple file systems
/// (`._name.flac`); they are not audio.
const APPLE_DOUBLE_PREFIX: &str = "._";

/// Prefer conventional cover names, then any JPEG or PNG beside the audio.
fn directory_cover(path: &Path) -> Option<std::sync::Arc<[u8]>> {
    let mut pictures: Vec<PathBuf> = std::fs::read_dir(path.parent()?)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|extension| {
                ["jpg", "jpeg", "png"]
                    .iter()
                    .any(|format| extension.eq_ignore_ascii_case(format))
            })
        })
        .collect();
    pictures.sort_by_key(|path| {
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        (
            !matches!(name.as_str(), "cover" | "folder" | "front"),
            path.clone(),
        )
    });
    pictures.into_iter().find_map(|path| {
        if std::fs::metadata(&path).ok()?.len() > MAX_COVER_BYTES {
            return None;
        }
        std::fs::read(path).ok().map(Into::into)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_missing_root_is_reported() {
        let (paths, errors) =
            collect_flac_paths(&[PathBuf::from("missing-pistolame-library-test")]);
        assert!(paths.is_empty());
        assert_eq!(errors.len(), 1);
    }
    #[test]
    fn generated_flac_metadata_and_duplicate_roots_are_consistent() {
        let fixture =
            pistolame_core::testing::generate_fixture(&pistolame_core::testing::CD_STEREO);
        let track = read_music_file(&fixture.path).expect("generated FLAC metadata");
        assert_eq!(track.sample_rate_hz, 44_100);
        assert_eq!(track.bits_per_sample, 16);
        assert_eq!(track.duration_seconds, 3);
        let (paths, errors) = collect_flac_paths(&[fixture.path.clone(), fixture.path.clone()]);
        assert!(errors.is_empty());
        assert_eq!(paths, vec![fixture.path.clone()]);
    }

    #[test]
    fn vorbis_tags_use_album_artist_and_track_numbers() {
        // RFC 9639 section 8.6: little-endian vendor and comment lengths.
        let fixture =
            pistolame_core::testing::generate_fixture(&pistolame_core::testing::CD_STEREO);
        let mut bytes = std::fs::read(&fixture.path).expect("fixture");
        let tags = [
            "TITLE=Test title",
            "ALBUM=Test album",
            "ARTIST=Track artist",
            "ALBUMARTIST=Album artist",
            "TRACKNUMBER=12",
            "DISCNUMBER=2",
            "DATE=2011-04-17",
            "ALBUMSORT=Game of Thrones 01",
            "GENRE=Soundtrack",
        ];
        let mut comments = 0u32.to_le_bytes().to_vec();
        comments.extend_from_slice(&(tags.len() as u32).to_le_bytes());
        for tag in tags {
            comments.extend_from_slice(&(tag.len() as u32).to_le_bytes());
            comments.extend_from_slice(tag.as_bytes());
        }
        bytes[4] &= 0x7f;
        let mut block = vec![0x84];
        block.extend_from_slice(&(comments.len() as u32).to_be_bytes()[1..]);
        block.extend_from_slice(&comments);
        bytes.splice(42..42, block);
        std::fs::write(&fixture.path, bytes).expect("tagged fixture");
        let track = read_music_file(&fixture.path).expect("tagged metadata");
        assert_eq!(track.title, "Test title");
        assert_eq!(track.album, "Test album");
        assert_eq!(track.artist, "Album artist");
        assert_eq!((track.disc_number, track.track_number), (2, 12));
        assert_eq!(track.year, Some(2011));
        assert_eq!(track.sort_album.as_deref(), Some("Game of Thrones 01"));
        assert_eq!(track.genre.as_deref(), Some("Soundtrack"));
    }

    #[test]
    fn apple_double_companions_are_skipped() {
        let folder = std::env::temp_dir().join("pistolame-library-apple-double");
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).expect("test folder");
        std::fs::write(folder.join("._01 Track.flac"), b"metadata").expect("companion");
        std::fs::write(folder.join("01 Track.flac"), b"audio").expect("track");
        let (paths, errors) = collect_flac_paths(std::slice::from_ref(&folder));
        assert!(errors.is_empty());
        assert_eq!(paths, vec![folder.join("01 Track.flac")]);
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn years_come_from_any_date_shape() {
        assert_eq!(year_of("2011"), Some(2011));
        assert_eq!(year_of("2011-04-17"), Some(2011));
        assert_eq!(year_of(" 1999/12 "), Some(1999));
        assert_eq!(year_of("unknown"), None);
        assert_eq!(year_of("12"), None);
    }

    #[test]
    fn an_unreadable_track_returns_an_error() {
        assert!(read_music_file(Path::new("missing-pistolame-test.flac")).is_err());
    }
}
