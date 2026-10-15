//! Covers decoded once, off the UI thread, at the size the interface draws them.
//!
//! A cover is often stored at 1400 to 5000 pixels per side: decoded as is, one 5000 × 5000
//! cover takes 100 MB of memory (and as much again as a GPU texture), and decoding it on the UI
//! thread stalls the window. The library worker decodes every cover of a snapshot in parallel,
//! scaled down to [`THUMBNAIL_LONGEST_SIDE_PIXELS`]. The thumbnails are kept by content in
//! memory (a rescan decodes only new covers) and as small JPEG files beside the library (the
//! next start reads them instead of the full-size covers).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use image::imageops::FilterType;
use pistolame_library::LibrarySnapshot;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Longest side of a decoded cover, in pixels. The largest cover on screen is 300 points (the
/// current-track page), 600 pixels on a 2× display only there; the grid and album page draw
/// 176 to 240 points, so 512 keeps them sharp on a Retina display.
pub(crate) const THUMBNAIL_LONGEST_SIDE_PIXELS: u32 = 512;
/// JPEG quality of the stored thumbnails: no visible loss at this size, about 60 KB each.
const STORED_THUMBNAIL_JPEG_QUALITY: u8 = 90;
const STORED_THUMBNAIL_EXTENSION: &str = "jpg";
/// The folder of stored thumbnails, beside the library database.
const THUMBNAIL_FOLDER_NAME: &str = "thumbnails";

/// Decoded pixels, shareable between threads; turned into a `slint::Image` on the UI thread.
pub(crate) type Thumbnail = SharedPixelBuffer<Rgba8Pixel>;

/// Decoded covers by content, kept between library updates, in memory and on disk.
#[derive(Clone, Default)]
pub(crate) struct ThumbnailCache {
    in_memory: Arc<Mutex<HashMap<String, Thumbnail>>>,
    /// Where thumbnails are stored; `None` keeps them in memory only.
    folder: Option<PathBuf>,
}

impl ThumbnailCache {
    /// A cache storing its files beside `library_database` (in memory only without one).
    pub(crate) fn beside(library_database: Option<&Path>) -> ThumbnailCache {
        ThumbnailCache {
            in_memory: Arc::default(),
            folder: library_database
                .and_then(Path::parent)
                .map(|folder| folder.join(THUMBNAIL_FOLDER_NAME)),
        }
    }

    /// The thumbnail of the cover `bytes` known as `key`: stored, or decoded and stored now.
    fn load_or_decode(&self, key: &str, bytes: &[u8]) -> Option<Thumbnail> {
        let stored_path = self
            .folder
            .as_ref()
            .map(|folder| folder.join(key).with_extension(STORED_THUMBNAIL_EXTENSION));
        if let Some(thumbnail) = stored_path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|stored| decode(&stored))
        {
            return Some(thumbnail);
        }
        let thumbnail = decode(bytes)?;
        if let Some(path) = stored_path {
            // A failed write only costs a decode at the next start.
            let _ = store(&thumbnail, &path);
        }
        Some(thumbnail)
    }

    /// Deletes the stored thumbnails of covers no longer in the library.
    fn remove_stored_except(&self, used: &HashSet<String>) {
        let Some(entries) = self
            .folder
            .as_ref()
            .and_then(|folder| std::fs::read_dir(folder).ok())
        else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_used = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| used.contains(stem));
            if !is_used {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// Decodes `bytes` (JPEG or PNG) and scales it down to fit the thumbnail size.
pub(crate) fn decode(bytes: &[u8]) -> Option<Thumbnail> {
    let image = image::load_from_memory(bytes).ok()?;
    let longest_side = image.width().max(image.height());
    let image = if longest_side > THUMBNAIL_LONGEST_SIDE_PIXELS {
        image.resize(
            THUMBNAIL_LONGEST_SIDE_PIXELS,
            THUMBNAIL_LONGEST_SIDE_PIXELS,
            // Averages the source pixels each output pixel covers: no aliasing on large ratios.
            FilterType::Triangle,
        )
    } else {
        image
    };
    let rgba = image.into_rgba8();
    Some(SharedPixelBuffer::clone_from_slice(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
    ))
}

/// Writes `thumbnail` as a JPEG file (covers are opaque; the alpha channel is dropped).
fn store(thumbnail: &Thumbnail, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let rgb: Vec<u8> = thumbnail
        .as_bytes()
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|[red, green, blue, _alpha]| [*red, *green, *blue])
        .collect();
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    image::codecs::jpeg::JpegEncoder::new_with_quality(file, STORED_THUMBNAIL_JPEG_QUALITY)
        .encode(
            &rgb,
            thumbnail.width(),
            thumbnail.height(),
            image::ExtendedColorType::Rgb8,
        )?;
    Ok(())
}

/// The covers of one snapshot, looked up by the address of their bytes (one allocation per
/// cover in a snapshot). Each entry keeps its bytes alive, so no other image can reuse the
/// address while the entry exists.
#[derive(Default)]
pub(crate) struct SnapshotThumbnails {
    by_address: HashMap<usize, (Arc<[u8]>, Thumbnail)>,
}

impl SnapshotThumbnails {
    /// Decodes every cover of `snapshot` (tracks, chosen covers, artist photos, playlists)
    /// that `cache` does not hold yet, in parallel, and forgets the covers no longer used.
    pub(crate) fn of(snapshot: &LibrarySnapshot, cache: &ThumbnailCache) -> SnapshotThumbnails {
        let mut covers: HashMap<usize, &Arc<[u8]>> = HashMap::new();
        let all_covers = snapshot
            .tracks
            .iter()
            .filter_map(|track| track.artwork.as_ref())
            .chain(snapshot.artwork_overrides.values())
            .chain(snapshot.artist_images.values())
            .chain(
                snapshot
                    .playlists
                    .iter()
                    .filter_map(|playlist| playlist.artwork.as_ref()),
            );
        for bytes in all_covers {
            covers.entry(address_of(bytes)).or_insert(bytes);
        }
        let keyed: Vec<(usize, String, &Arc<[u8]>)> = covers
            .into_par_iter()
            .map(|(address, bytes)| (address, content_key(bytes), bytes))
            .collect();
        let missing: Vec<(&String, &Arc<[u8]>)> = {
            let known = cache
                .in_memory
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            keyed
                .iter()
                .filter(|(_, key, _)| !known.contains_key(key))
                .map(|(_, key, bytes)| (key, *bytes))
                .collect()
        };
        let loaded: Vec<(String, Thumbnail)> = missing
            .par_iter()
            .filter_map(|(key, bytes)| {
                cache
                    .load_or_decode(key, bytes)
                    .map(|thumbnail| ((*key).clone(), thumbnail))
            })
            .collect();
        let used: HashSet<String> = keyed.iter().map(|(_, key, _)| key.clone()).collect();
        if !missing.is_empty() {
            cache.remove_stored_except(&used);
        }
        let mut known = cache
            .in_memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        known.extend(loaded);
        known.retain(|key, _| used.contains(key));
        SnapshotThumbnails {
            by_address: keyed
                .iter()
                .filter_map(|(address, key, bytes)| {
                    known
                        .get(key)
                        .map(|thumbnail| (*address, (Arc::clone(bytes), thumbnail.clone())))
                })
                .collect(),
        }
    }

    /// The image of `bytes`: its decoded thumbnail, or a decode now for bytes that are not
    /// part of the snapshot (a cover just chosen in an editor). Empty when it cannot be read.
    pub(crate) fn image(&self, bytes: Option<&Arc<[u8]>>) -> slint::Image {
        let Some(bytes) = bytes else {
            return slint::Image::default();
        };
        self.by_address
            .get(&address_of(bytes))
            .map(|(_, thumbnail)| thumbnail.clone())
            .or_else(|| decode(bytes))
            .map(slint::Image::from_rgba8)
            .unwrap_or_default()
    }
}

fn address_of(bytes: &Arc<[u8]>) -> usize {
    Arc::as_ptr(bytes).cast::<u8>() as usize
}

/// Identifies a cover by its bytes (the same cover in two snapshots, two files or two runs):
/// SHA-256, stable across Rust versions unlike the standard hasher, in hexadecimal.
fn content_key(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_of_size(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(width, height, image::Rgba([200, 100, 50, 255]))
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("encoded");
        bytes
    }

    #[test]
    fn a_large_cover_is_scaled_down_keeping_its_shape() {
        let thumbnail = decode(&png_of_size(2000, 1000)).expect("decoded");
        assert_eq!((thumbnail.width(), thumbnail.height()), (512, 256));
    }

    #[test]
    fn a_small_cover_keeps_its_size() {
        let thumbnail = decode(&png_of_size(300, 300)).expect("decoded");
        assert_eq!((thumbnail.width(), thumbnail.height()), (300, 300));
    }

    #[test]
    fn unreadable_bytes_give_no_image() {
        assert!(decode(b"not an image").is_none());
        let thumbnails = SnapshotThumbnails::default();
        assert_eq!(
            thumbnails
                .image(Some(&Arc::from(&b"junk"[..])))
                .size()
                .width,
            0
        );
    }

    #[test]
    fn a_new_run_reads_the_stored_thumbnail_and_unused_files_are_removed() {
        let folder =
            std::env::temp_dir().join(format!("pistolame-thumbnails-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let database = folder.join("library.sqlite3");
        let cover: Arc<[u8]> = Arc::from(png_of_size(1024, 1024));
        let mut snapshot = LibrarySnapshot::default();
        snapshot.artist_images.insert("A".into(), cover.clone());
        SnapshotThumbnails::of(&snapshot, &ThumbnailCache::beside(Some(&database)));
        let stored = folder
            .join(THUMBNAIL_FOLDER_NAME)
            .join(format!("{}.jpg", content_key(&cover)));
        assert!(stored.is_file());
        // A fresh cache (next start) finds it on disk.
        let thumbnails =
            SnapshotThumbnails::of(&snapshot, &ThumbnailCache::beside(Some(&database)));
        assert_eq!(thumbnails.image(Some(&cover)).size().width, 512);
        // A library without that cover removes its file.
        let other: Arc<[u8]> = Arc::from(png_of_size(8, 8));
        let mut snapshot = LibrarySnapshot::default();
        snapshot.artist_images.insert("B".into(), other);
        SnapshotThumbnails::of(&snapshot, &ThumbnailCache::beside(Some(&database)));
        assert!(!stored.exists());
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn the_cache_keeps_only_the_covers_still_used() {
        let cache = ThumbnailCache::default();
        let cover: Arc<[u8]> = Arc::from(png_of_size(10, 10));
        let mut snapshot = LibrarySnapshot::default();
        snapshot.artist_images.insert("A".into(), cover.clone());
        let thumbnails = SnapshotThumbnails::of(&snapshot, &cache);
        assert_eq!(thumbnails.image(Some(&cover)).size().width, 10);
        assert_eq!(cache.in_memory.lock().expect("lock").len(), 1);
        SnapshotThumbnails::of(&LibrarySnapshot::default(), &cache);
        assert!(cache.in_memory.lock().expect("lock").is_empty());
    }
}
