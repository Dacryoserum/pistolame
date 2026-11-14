//! The log file: one text line per event, rotated by size so it never fills the disk.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};

/// Suffix of the previous file, kept beside the current one after a rotation.
const PREVIOUS_FILE_SUFFIX: &str = ".1";

/// A log file that keeps at most two files of `maximum_bytes`: the current one and the previous
/// one. Every write goes straight to the system (no buffer), so the last lines before a crash are
/// on disk. Writing takes a lock and can block on the disk: never call it from the real-time
/// audio thread (AGENTS.md).
pub(crate) struct RotatingLogFile {
    current_path: PathBuf,
    previous_path: PathBuf,
    maximum_bytes: u64,
    state: Mutex<OpenLogFile>,
}

struct OpenLogFile {
    /// `None` when reopening after a rotation failed; lines are then dropped.
    file: Option<File>,
    written_bytes: u64,
}

impl RotatingLogFile {
    /// Opens (or creates) `file_name` in `directory`, creating the directory, and appends to it.
    pub(crate) fn open(
        directory: &Path,
        file_name: &str,
        maximum_bytes: u64,
    ) -> std::io::Result<Self> {
        fs::create_dir_all(directory)?;
        let current_path = directory.join(file_name);
        let previous_path = directory.join(format!("{file_name}{PREVIOUS_FILE_SUFFIX}"));
        let file = open_for_append(&current_path)?;
        let written_bytes = file.metadata()?.len();
        Ok(Self {
            current_path,
            previous_path,
            maximum_bytes,
            state: Mutex::new(OpenLogFile {
                file: Some(file),
                written_bytes,
            }),
        })
    }

    /// Appends one line. A failed write is dropped: a log that cannot be written must never stop
    /// the app.
    pub(crate) fn append_line(&self, line: &str) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.append_locked(&mut state, line);
    }

    /// Like [`RotatingLogFile::append_line`], but gives up instead of waiting for the lock. The
    /// panic hook uses it: the panic may have happened while this very thread held the lock.
    pub(crate) fn try_append_line(&self, line: &str) {
        let mut state: MutexGuard<'_, OpenLogFile> = match self.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        self.append_locked(&mut state, line);
    }

    fn append_locked(&self, state: &mut OpenLogFile, line: &str) {
        let mut text = String::with_capacity(line.len() + 1);
        text.push_str(line);
        text.push('\n');
        let text_bytes = u64::try_from(text.len()).unwrap_or(u64::MAX);
        if state.written_bytes > 0
            && state.written_bytes.saturating_add(text_bytes) > self.maximum_bytes
        {
            self.rotate(state);
        }
        let Some(file) = state.file.as_mut() else {
            return;
        };
        if file.write_all(text.as_bytes()).is_ok() {
            state.written_bytes = state.written_bytes.saturating_add(text_bytes);
        }
    }

    /// The current file becomes the previous one (replacing it) and a new current file starts.
    /// If the rename fails, the current file is emptied instead, so it still cannot grow forever.
    fn rotate(&self, state: &mut OpenLogFile) {
        // Closed first: Windows cannot rename a file that is open.
        state.file = None;
        state.written_bytes = 0;
        state.file = if fs::rename(&self.current_path, &self.previous_path).is_ok() {
            open_for_append(&self.current_path).ok()
        } else {
            File::create(&self.current_path).ok()
        };
    }
}

fn open_for_append(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Unique empty directory under the system temp directory.
    fn fresh_test_directory(test_name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "pistolame-log-file-test-{}-{test_name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        directory
    }

    #[test]
    fn lines_are_appended_across_openings() {
        let directory = fresh_test_directory("append");
        RotatingLogFile::open(&directory, "test.log", 1_000)
            .unwrap()
            .append_line("first");
        RotatingLogFile::open(&directory, "test.log", 1_000)
            .unwrap()
            .append_line("second");
        let text = fs::read_to_string(directory.join("test.log")).unwrap();
        assert_eq!(text, "first\nsecond\n");
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_full_file_becomes_the_previous_one() {
        let directory = fresh_test_directory("rotate");
        let log = RotatingLogFile::open(&directory, "test.log", 10).unwrap();
        log.append_line("aaaaaaaa"); // 9 bytes: fits
        log.append_line("bbbbbbbb"); // would reach 18: rotates first
        log.append_line("cccccccc"); // rotates again, replacing the previous file
        assert_eq!(
            fs::read_to_string(directory.join("test.log")).unwrap(),
            "cccccccc\n"
        );
        assert_eq!(
            fs::read_to_string(directory.join("test.log.1")).unwrap(),
            "bbbbbbbb\n"
        );
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_line_longer_than_the_limit_is_still_written() {
        let directory = fresh_test_directory("long-line");
        let log = RotatingLogFile::open(&directory, "test.log", 4).unwrap();
        log.append_line("longer than four bytes");
        assert_eq!(
            fs::read_to_string(directory.join("test.log")).unwrap(),
            "longer than four bytes\n"
        );
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn try_append_writes_when_the_lock_is_free() {
        let directory = fresh_test_directory("try-append");
        let log = RotatingLogFile::open(&directory, "test.log", 1_000).unwrap();
        log.try_append_line("from the panic hook");
        assert_eq!(
            fs::read_to_string(directory.join("test.log")).unwrap(),
            "from the panic hook\n"
        );
        fs::remove_dir_all(&directory).unwrap();
    }
}
