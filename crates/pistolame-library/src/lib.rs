//! File metadata and the persistent library. Scanning never modifies music files.
#![forbid(unsafe_code)]

mod artwork;
mod scan;
mod store;
mod watcher;

pub use artwork::ArtworkId;
pub use store::{
    AlbumKey, AlbumTextOverride, LibraryError, LibrarySnapshot, LibraryStore, Playlist,
    SynchronizationReport,
};
pub use watcher::{FolderChange, FolderWatcher};

use pistolame_core::{SourceCodec, declared_bits_per_sample};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::{FormatOptions, TrackType, probe::Hint};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTag};

/// One local audio file and its display metadata, independent of any UI toolkit.
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
    /// Source precision in bits; 0 for lossy codecs, which have none.
    pub bits_per_sample: u32,
    /// Codec name shown to the user ("FLAC", "ALAC", "MP3", ...).
    pub codec: String,
    /// The codec discarded audio information (MP3, AAC, Vorbis).
    pub is_lossy: bool,
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
    /// The file's own cover (embedded, or an image beside it). The bytes stay in the library
    /// database; they are read only when the cover must be drawn.
    pub artwork: Option<ArtworkId>,
}

/// What reading one file yields: its metadata, and the bytes of its cover for the library to
/// store (`track.artwork` is their id).
#[derive(Clone, Debug)]
pub struct ScannedTrack {
    /// The metadata to store.
    pub track: MusicFile,
    /// The encoded cover named by `track.artwork`, if any.
    pub artwork_bytes: Option<Arc<[u8]>>,
}

/// A file that cannot be indexed. The UI reports it without interrupting other imports.
#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    /// The file cannot be read.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// The container or codec parser rejected the file.
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
pub fn read_music_file(path: &Path) -> Result<ScannedTrack, MetadataError> {
    read_music_file_sharing_covers(path, &SharedCovers::default())
}

/// Like [`read_music_file`], with the covers of the other files of one synchronization (see
/// [`SharedCovers`]).
pub(crate) fn read_music_file_sharing_covers(
    path: &Path,
    shared_covers: &SharedCovers,
) -> Result<ScannedTrack, MetadataError> {
    let stream = MediaSourceStream::new(Box::new(std::fs::File::open(path)?), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }
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
    let codec = SourceCodec::from_codec_id(parameters.codec);
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
        bits_per_sample: if codec.is_lossy {
            0
        } else {
            declared_bits_per_sample(parameters).unwrap_or(0)
        },
        codec: codec.display_name.to_owned(),
        is_lossy: codec.is_lossy,
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
    let mut cover = None;
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
        // The metadata parsers bound individual blocks; only one cover is retained.
        cover = revision.media.visuals.first().map(|visual| {
            let bytes: Arc<[u8]> = visual.data.to_vec().into();
            (ArtworkId::of_bytes(&bytes), bytes)
        });
    }
    if cover.is_none()
        && let Some(folder) = path.parent()
    {
        cover = shared_covers.cover_of(folder);
    }
    let (artwork, artwork_bytes) = cover.unzip();
    // Only the first file with this cover keeps its bytes, to store them once: the others
    // would hold identical megabytes until their batch is written.
    let artwork_bytes =
        artwork_bytes.filter(|_| artwork.as_ref().is_some_and(|id| shared_covers.claim(id)));
    file.artwork = artwork;
    Ok(ScannedTrack {
        track: file,
        artwork_bytes,
    })
}

/// A cover with its id.
type IdentifiedCover = (ArtworkId, Arc<[u8]>);

/// What the threads of one synchronization share about covers: the cover image found beside
/// the audio, by folder (an album without embedded art would otherwise read `cover.jpg` once
/// per track), and the covers whose bytes a file already took.
#[derive(Default)]
pub(crate) struct SharedCovers {
    by_folder: Mutex<HashMap<PathBuf, Option<IdentifiedCover>>>,
    claimed: Mutex<HashSet<ArtworkId>>,
}

impl SharedCovers {
    /// Forgets the folder covers read so far, so their bytes do not stay in memory for the
    /// whole synchronization (files come in path order: a folder is rarely met again later).
    pub(crate) fn forget_folders(&self) {
        self.by_folder
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    /// True the first time `artwork` is claimed in this synchronization.
    fn claim(&self, artwork: &ArtworkId) -> bool {
        self.claimed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(artwork.clone())
    }

    /// The cover of `folder` with its id, read (and hashed) at the first request only. Two
    /// threads asking at once may both read it; the result is the same.
    fn cover_of(&self, folder: &Path) -> Option<IdentifiedCover> {
        let known = self
            .by_folder
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(folder)
            .cloned();
        if let Some(cover) = known {
            return cover;
        }
        let cover = directory_cover(folder).map(|bytes| (ArtworkId::of_bytes(&bytes), bytes));
        self.by_folder
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(folder.to_owned(), cover.clone());
        cover
    }
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

/// Recursively collects the paths of supported audio files, without following symbolic links.
///
/// Errors are kept alongside successful files, so unreadable folders are never silently omitted.
pub fn collect_audio_paths(roots: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    let (files, errors) = scan::list_audio_files(roots);
    (files.into_iter().map(|file| file.path).collect(), errors)
}

/// Largest cover image accepted, from a folder or chosen by the user (16 MiB, far above any
/// real album cover, small enough to keep the library light).
pub const MAX_COVER_BYTES: u64 = 16 * 1024 * 1024;

/// Prefer conventional cover names, then any JPEG or PNG in `folder`.
fn directory_cover(folder: &Path) -> Option<Arc<[u8]>> {
    let mut pictures: Vec<PathBuf> = std::fs::read_dir(folder)
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
            collect_audio_paths(&[PathBuf::from("missing-pistolame-library-test")]);
        assert!(paths.is_empty());
        assert_eq!(errors.len(), 1);
    }
    #[test]
    fn generated_flac_metadata_and_duplicate_roots_are_consistent() {
        let fixture =
            pistolame_core::testing::generate_fixture(&pistolame_core::testing::CD_STEREO);
        let track = read_music_file(&fixture.path)
            .expect("generated FLAC metadata")
            .track;
        assert_eq!(track.sample_rate_hz, 44_100);
        assert_eq!(track.bits_per_sample, 16);
        assert_eq!(track.duration_seconds, 3);
        assert_eq!((track.codec.as_str(), track.is_lossy), ("FLAC", false));
        let (paths, errors) = collect_audio_paths(&[fixture.path.clone(), fixture.path.clone()]);
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
        let track = read_music_file(&fixture.path)
            .expect("tagged metadata")
            .track;
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
        let (paths, errors) = collect_audio_paths(std::slice::from_ref(&folder));
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
