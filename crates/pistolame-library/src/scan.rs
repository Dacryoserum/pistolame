//! Walking the library folders: which audio files exist, with their size and date.
//!
//! The type and the stamp of every entry come from the directory listing itself, never from a
//! second lookup per file: on Windows `FindNextFileW` already returns them, so a walk costs one
//! system call per folder instead of two per file (`DirEntry::metadata`,
//! <https://doc.rust-lang.org/std/fs/struct.DirEntry.html#method.metadata>). On Unix it is one
//! `lstat` per file, as before.

use std::collections::BTreeMap;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use pistolame_core::is_supported_audio_file;

/// Prefix of AppleDouble files, the metadata companions macOS writes on non-Apple file systems
/// (`._name.flac`); they are not audio.
const APPLE_DOUBLE_PREFIX: &str = "._";

/// Size and modification time: a file whose stamp is unchanged is not read again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileStamp {
    pub(crate) size: i64,
    pub(crate) modified_nanoseconds: i64,
}

impl FileStamp {
    /// The stamp of a file from metadata already at hand; `None` when the system gives no
    /// modification time or the values do not fit SQLite's integers.
    pub(crate) fn of_metadata(metadata: &Metadata) -> Option<FileStamp> {
        let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
        Some(FileStamp {
            size: i64::try_from(metadata.len()).ok()?,
            modified_nanoseconds: i64::try_from(modified.as_nanos()).ok()?,
        })
    }
}

/// One audio file found by a walk. `stamp` is `None` when its metadata could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListedFile {
    pub(crate) path: PathBuf,
    pub(crate) stamp: Option<FileStamp>,
}

/// Every supported audio file under `roots` (each a folder or a file), in path order and
/// without duplicates. Symbolic links are not followed. Unreadable entries are reported as
/// display text beside the files found, never silently skipped.
pub(crate) fn list_audio_files(roots: &[PathBuf]) -> (Vec<ListedFile>, Vec<String>) {
    let mut files: BTreeMap<PathBuf, Option<FileStamp>> = BTreeMap::new();
    let mut errors = Vec::new();
    let mut pending_folders = Vec::new();
    for root in roots {
        match std::fs::symlink_metadata(root) {
            Ok(metadata) if metadata.file_type().is_symlink() => {}
            Ok(metadata) if metadata.is_dir() => pending_folders.push(root.clone()),
            Ok(metadata) => {
                if is_listed_audio_file(root) {
                    files.insert(root.clone(), FileStamp::of_metadata(&metadata));
                }
            }
            Err(error) => errors.push(format!("{}: {error}", root.display())),
        }
    }
    while let Some(folder) = pending_folders.pop() {
        let entries = match std::fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) => {
                errors.push(format!("{}: {error}", folder.display()));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    errors.push(format!("{}: {error}", folder.display()));
                    continue;
                }
            };
            let Ok(file_type) = entry.file_type() else {
                errors.push(format!("{}: type illisible", entry.path().display()));
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                pending_folders.push(path);
            } else if file_type.is_file() && is_listed_audio_file(&path) {
                let stamp = entry
                    .metadata()
                    .ok()
                    .and_then(|metadata| FileStamp::of_metadata(&metadata));
                files.insert(path, stamp);
            }
        }
    }
    let files = files
        .into_iter()
        .map(|(path, stamp)| ListedFile { path, stamp })
        .collect();
    (files, errors)
}

/// Whether `path` exists under exactly this name. On a case-insensitive file system (Windows,
/// macOS) `Album` also opens `album`: after a rename that only changed case, the old spelling
/// must count as gone, or its tracks would stay beside the new ones.
pub(crate) fn exists_with_exact_name(path: &Path) -> bool {
    if std::fs::symlink_metadata(path).is_err() {
        return false;
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return true;
    };
    match std::fs::read_dir(parent) {
        Ok(entries) => entries
            .flatten()
            .any(|entry| entry.file_name().as_os_str() == name),
        // The parent cannot be listed: trust the path, as a full synchronization would.
        Err(_) => true,
    }
}

fn is_listed_audio_file(path: &Path) -> bool {
    is_supported_audio_file(path) && !is_apple_double(path)
}

fn is_apple_double(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with(APPLE_DOUBLE_PREFIX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_folder(name: &str) -> PathBuf {
        let folder =
            std::env::temp_dir().join(format!("pistolame-scan-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).expect("test folder");
        folder
    }

    #[test]
    fn nested_audio_files_are_listed_once_with_their_stamp() {
        let folder = test_folder("nested");
        std::fs::create_dir_all(folder.join("Disc 1")).expect("subfolder");
        std::fs::write(folder.join("Disc 1").join("01.flac"), b"twelve bytes").expect("track");
        std::fs::write(folder.join("cover.jpg"), b"picture").expect("cover");
        let (files, errors) = list_audio_files(&[folder.clone(), folder.clone()]);
        assert!(errors.is_empty());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, folder.join("Disc 1").join("01.flac"));
        assert_eq!(files[0].stamp.map(|stamp| stamp.size), Some(12));
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_file_root_is_listed_with_its_stamp() {
        let folder = test_folder("file-root");
        let track = folder.join("single.flac");
        std::fs::write(&track, b"abc").expect("track");
        let (files, errors) = list_audio_files(std::slice::from_ref(&track));
        assert!(errors.is_empty());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].stamp.map(|stamp| stamp.size), Some(3));
        let _ = std::fs::remove_dir_all(&folder);
    }
}
