//! The last listening session (audit 2026-10-13, §4 item 12): the queue and the position in the
//! current track, so the app reopens where the user left. Restored *paused*: the app never
//! plays sound on its own.
//!
//! Stored as JSON beside the settings, written atomically. A missing or unreadable file means
//! "no session", never an error shown to the user.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::play_queue::QueueSnapshot;

const SESSION_FILE_NAME: &str = "session.json";
const TEMPORARY_FILE_NAME: &str = "session.json.tmp";

/// What the app reopens on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Session {
    /// The play queue: context, position in it, tracks queued by hand, current track.
    pub(crate) queue: QueueSnapshot,
    /// Position in the current track, in whole seconds.
    pub(crate) position_seconds: u64,
}

impl Session {
    /// The stored session, if there is a usable one.
    pub(crate) fn load() -> Option<Session> {
        Session::load_from(&session_file_path()?)
    }

    /// Stores the session (atomic: temporary file, then rename).
    pub(crate) fn save(&self) -> anyhow::Result<()> {
        let path = session_file_path().context("no app data directory could be determined")?;
        self.save_to(&path)
    }

    fn load_from(path: &Path) -> Option<Session> {
        let bytes = fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        let directory = path.parent().context("session path has no parent")?;
        fs::create_dir_all(directory)
            .with_context(|| format!("cannot create {}", directory.display()))?;
        let temporary_path = directory.join(TEMPORARY_FILE_NAME);
        let mut file = fs::File::create(&temporary_path)
            .with_context(|| format!("cannot create {}", temporary_path.display()))?;
        file.write_all(&serde_json::to_vec(self)?)
            .with_context(|| format!("cannot write {}", temporary_path.display()))?;
        file.sync_all()
            .with_context(|| format!("cannot flush {}", temporary_path.display()))?;
        fs::rename(&temporary_path, path)
            .with_context(|| format!("cannot move session file to {}", path.display()))
    }
}

fn session_file_path() -> Option<PathBuf> {
    Some(crate::settings::app_data_directory()?.join(SESSION_FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique empty directory under the system temp directory, never the real app folder.
    fn fresh_test_directory(test_name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "pistolame-session-test-{}-{test_name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("test directory");
        directory
    }

    #[test]
    fn a_session_round_trips_through_the_file() {
        let path = fresh_test_directory("round-trip")
            .join("nested")
            .join(SESSION_FILE_NAME);
        let session = Session {
            queue: QueueSnapshot {
                context: vec!["a1.flac".into(), "a2.flac".into()],
                context_name: "Album".into(),
                context_position: 1,
                up_next: vec!["q1.flac".into()],
                current: Some("a2.flac".into()),
            },
            position_seconds: 83,
        };
        session.save_to(&path).expect("saved");
        assert_eq!(Session::load_from(&path), Some(session));
    }

    #[test]
    fn a_missing_or_damaged_file_is_no_session() {
        let path = fresh_test_directory("damaged").join(SESSION_FILE_NAME);
        assert_eq!(Session::load_from(&path), None);
        fs::write(&path, b"{ not json").expect("written");
        assert_eq!(Session::load_from(&path), None);
    }
}
