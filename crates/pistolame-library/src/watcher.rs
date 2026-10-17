//! Folder watching (audit 2026-10-13, §4 item 9): tells the app when music may have been added,
//! removed or renamed under the library folders, so it can synchronize without a restart.
//!
//! It reports which paths changed, so the app synchronizes those paths only (a full rescan of a
//! large library walks every folder); the synchronization still compares file stamps and reads
//! only what changed, so a burst of events costs one cheap pass.

use std::path::{Path, PathBuf};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// What changed under the library folders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderChange {
    /// These files or folders were created, rewritten, removed or renamed.
    Paths(Vec<PathBuf>),
    /// The system lost events (its buffer overflowed): only a full synchronization is safe.
    Unknown,
}

/// Watches the library folders while it is alive.
pub struct FolderWatcher {
    // Kept alive: dropping it stops the system notifications.
    _watcher: RecommendedWatcher,
    watched_roots: Vec<PathBuf>,
}

impl FolderWatcher {
    /// Starts watching `roots` (folders recursively, single files too). `on_change` runs on a
    /// system thread for every relevant event, so it must only pass the change on (send it on
    /// a channel).
    /// A root that cannot be watched, such as an unplugged drive, is skipped.
    ///
    /// # Errors
    /// Fails when the system watcher cannot be created at all.
    pub fn watch(
        roots: &[PathBuf],
        on_change: impl Fn(FolderChange) + Send + 'static,
    ) -> Result<FolderWatcher, notify::Error> {
        let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
            let change = match result {
                Ok(event) => change_of(&event),
                // The system watcher failed (on Windows, an overflow of its buffer): events
                // may be lost, only a full synchronization is safe.
                Err(_) => Some(FolderChange::Unknown),
            };
            if let Some(change) = change {
                on_change(change);
            }
        })?;
        let watched_roots = roots
            .iter()
            .filter(|root| watcher.watch(root, RecursiveMode::Recursive).is_ok())
            .cloned()
            .collect();
        Ok(FolderWatcher {
            _watcher: watcher,
            watched_roots,
        })
    }

    /// The roots actually watched (those that existed when watching started).
    pub fn watched_roots(&self) -> &[PathBuf] {
        &self.watched_roots
    }
}

/// What `event` may change in the library, if anything. Reads and metadata-only changes (access
/// times) change nothing. A removed or renamed path can no longer be examined, so it is always
/// reported (a needless path costs the library one indexed lookup); a created or rewritten one
/// only when it is a supported audio file or a folder, possibly full of music.
fn change_of(event: &Event) -> Option<FolderChange> {
    use notify::event::ModifyKind;
    if event.need_rescan() {
        return Some(FolderChange::Unknown);
    }
    let paths: Vec<PathBuf> = match event.kind {
        EventKind::Access(_) | EventKind::Other | EventKind::Modify(ModifyKind::Metadata(_)) => {
            return None;
        }
        EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_)) => event.paths.clone(),
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any => event
            .paths
            .iter()
            .filter(|path| may_hold_music(path))
            .cloned()
            .collect(),
    };
    (!paths.is_empty()).then_some(FolderChange::Paths(paths))
}

/// A supported audio file, or a folder (possibly full of music).
fn may_hold_music(path: &Path) -> bool {
    pistolame_core::is_supported_audio_file(path) || path.extension().is_none() || path.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, MetadataKind, ModifyKind};

    fn event(kind: EventKind, path: &str) -> Event {
        Event::new(kind).add_path(PathBuf::from(path))
    }

    fn paths(change: Option<FolderChange>) -> Vec<PathBuf> {
        match change {
            Some(FolderChange::Paths(paths)) => paths,
            other => panic!("expected paths, got {other:?}"),
        }
    }

    #[test]
    fn new_flac_files_and_folders_count() {
        assert_eq!(
            paths(change_of(&event(
                EventKind::Create(CreateKind::File),
                "/music/Album/01.FLAC"
            ))),
            vec![PathBuf::from("/music/Album/01.FLAC")]
        );
        assert_eq!(
            paths(change_of(&event(
                EventKind::Remove(notify::event::RemoveKind::Folder),
                "/music/Album"
            ))),
            vec![PathBuf::from("/music/Album")]
        );
    }

    #[test]
    fn a_removed_or_renamed_path_counts_whatever_its_name() {
        // "Vol.2" looks like a file with an extension, but may have been a folder of music.
        assert_eq!(
            paths(change_of(&event(
                EventKind::Remove(notify::event::RemoveKind::Any),
                "/music/Vol.2"
            ))),
            vec![PathBuf::from("/music/Vol.2")]
        );
        assert_eq!(
            paths(change_of(&event(
                EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::From)),
                "/music/Vol.2"
            ))),
            vec![PathBuf::from("/music/Vol.2")]
        );
    }

    #[test]
    fn lost_events_ask_for_a_full_synchronization() {
        let lost = Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
        assert_eq!(change_of(&lost), Some(FolderChange::Unknown));
    }

    #[test]
    fn other_files_reads_and_metadata_changes_do_not() {
        assert!(
            change_of(&event(
                EventKind::Create(CreateKind::File),
                "/music/Album/cover.jpg"
            ))
            .is_none()
        );
        assert!(
            change_of(&event(
                EventKind::Access(AccessKind::Read),
                "/music/Album/01.flac"
            ))
            .is_none()
        );
        assert!(
            change_of(&event(
                EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)),
                "/music/Album/01.flac"
            ))
            .is_none()
        );
    }

    #[test]
    fn a_new_file_in_a_watched_folder_is_signaled() {
        let folder =
            std::env::temp_dir().join(format!("pistolame-watcher-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).expect("test folder");
        let (sender, receiver) = std::sync::mpsc::channel();
        let watcher = FolderWatcher::watch(std::slice::from_ref(&folder), move |change| {
            let _ = sender.send(change);
        })
        .expect("watcher");
        assert_eq!(watcher.watched_roots(), std::slice::from_ref(&folder));
        // FSEvents can take a moment to start delivering.
        std::thread::sleep(std::time::Duration::from_millis(300));
        std::fs::write(folder.join("new.flac"), b"not really audio").expect("written");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut signaled = false;
        while let Some(left) = deadline.checked_duration_since(std::time::Instant::now())
            && let Ok(change) = receiver.recv_timeout(left)
        {
            if let FolderChange::Paths(paths) = change
                && paths.iter().any(|path| path.ends_with("new.flac"))
            {
                signaled = true;
                break;
            }
        }
        drop(watcher);
        let _ = std::fs::remove_dir_all(&folder);
        assert!(signaled, "no change signaled for a new FLAC file");
    }

    #[test]
    fn a_missing_root_is_skipped() {
        let watcher =
            FolderWatcher::watch(&[PathBuf::from("/definitely/not/a/real/folder")], |_| {})
                .expect("watcher");
        assert!(watcher.watched_roots().is_empty());
    }
}
