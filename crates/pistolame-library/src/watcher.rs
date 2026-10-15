//! Folder watching (audit 2026-10-13, §4 item 9): tells the app when music may have been added,
//! removed or renamed under the library folders, so it can synchronize without a restart.
//!
//! It only signals "something changed"; the synchronization itself compares file stamps and
//! reads only what changed, so a burst of events costs one cheap pass.

use std::path::{Path, PathBuf};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Watches the library folders while it is alive.
pub struct FolderWatcher {
    // Kept alive: dropping it stops the system notifications.
    _watcher: RecommendedWatcher,
    watched_roots: Vec<PathBuf>,
}

impl FolderWatcher {
    /// Starts watching `roots` (folders recursively, single files too). `on_change` runs on a
    /// system thread for every relevant change, so it must only signal (send on a channel).
    /// A root that cannot be watched, such as an unplugged drive, is skipped.
    ///
    /// # Errors
    /// Fails when the system watcher cannot be created at all.
    pub fn watch(
        roots: &[PathBuf],
        on_change: impl Fn() + Send + 'static,
    ) -> Result<FolderWatcher, notify::Error> {
        let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
            if result.is_ok_and(|event| is_relevant(&event)) {
                on_change();
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

/// Whether `event` may change the library: a supported audio file or a folder created, removed, renamed or
/// rewritten. Reads and metadata-only changes (access times) are ignored.
fn is_relevant(event: &Event) -> bool {
    let is_relevant_kind = matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(_) | EventKind::Any
    ) && !matches!(
        event.kind,
        EventKind::Modify(notify::event::ModifyKind::Metadata(_))
    );
    is_relevant_kind && event.paths.iter().any(|path| may_hold_music(path))
}

/// A supported audio file, or something without extension (a folder, possibly full of music).
fn may_hold_music(path: &Path) -> bool {
    path.extension().is_none() || pistolame_core::is_supported_audio_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, MetadataKind, ModifyKind};

    fn event(kind: EventKind, path: &str) -> Event {
        Event::new(kind).add_path(PathBuf::from(path))
    }

    #[test]
    fn new_flac_files_and_folders_count() {
        assert!(is_relevant(&event(
            EventKind::Create(CreateKind::File),
            "/music/Album/01.FLAC"
        )));
        assert!(is_relevant(&event(
            EventKind::Remove(notify::event::RemoveKind::Folder),
            "/music/Album"
        )));
    }

    #[test]
    fn other_files_reads_and_metadata_changes_do_not() {
        assert!(!is_relevant(&event(
            EventKind::Create(CreateKind::File),
            "/music/Album/cover.jpg"
        )));
        assert!(!is_relevant(&event(
            EventKind::Access(AccessKind::Read),
            "/music/Album/01.flac"
        )));
        assert!(!is_relevant(&event(
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)),
            "/music/Album/01.flac"
        )));
    }

    #[test]
    fn a_new_file_in_a_watched_folder_is_signaled() {
        let folder =
            std::env::temp_dir().join(format!("pistolame-watcher-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).expect("test folder");
        let (sender, receiver) = std::sync::mpsc::channel();
        let watcher = FolderWatcher::watch(std::slice::from_ref(&folder), move || {
            let _ = sender.send(());
        })
        .expect("watcher");
        assert_eq!(watcher.watched_roots(), std::slice::from_ref(&folder));
        // FSEvents can take a moment to start delivering.
        std::thread::sleep(std::time::Duration::from_millis(300));
        std::fs::write(folder.join("new.flac"), b"not really audio").expect("written");
        let signaled = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok();
        drop(watcher);
        let _ = std::fs::remove_dir_all(&folder);
        assert!(signaled, "no change signaled for a new FLAC file");
    }

    #[test]
    fn a_missing_root_is_skipped() {
        let watcher =
            FolderWatcher::watch(&[PathBuf::from("/definitely/not/a/real/folder")], || {})
                .expect("watcher");
        assert!(watcher.watched_roots().is_empty());
    }
}
