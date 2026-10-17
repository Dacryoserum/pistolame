//! "Annuler" in the confirmation after a removal: puts back what was just removed, instead of
//! asking to confirm beforehand (audit Psysonic §8).

use super::*;

/// Removals are confirmed a little longer, so there is time to click "Annuler".
const UNDO_NOTICE_DISPLAY_DURATION: Duration = Duration::from_secs(7);
const UNDO_LABEL: &str = "Annuler";

/// What the "Annuler" of the current confirmation puts back.
pub(super) enum UndoableChange {
    /// An edit of the tracks queued by hand (the queue keeps its own undo steps).
    QueueEdit,
    /// The entries of a playlist before some were removed.
    PlaylistEntries {
        playlist_id: i64,
        previous_paths: Vec<PathBuf>,
    },
    /// Favorite marks before a change: each of `paths` goes back to being a favorite or not.
    Favorites {
        paths: Vec<PathBuf>,
        previous_favorites: Vec<PathBuf>,
    },
    /// Tracks removed from the library, to show again.
    LibraryRemoval { paths: Vec<PathBuf> },
}

impl UiBridge {
    pub(super) fn connect_undo(self: &Rc<Self>, state: &AppState<'_>) {
        let handle = self.clone();
        state.on_message_action(move || handle.undo_pending_change());
        let handle = self.clone();
        state.on_queue_undo(move || handle.undo_queue_edit(true));
        let handle = self.clone();
        state.on_queue_redo(move || handle.undo_queue_edit(false));
    }

    /// Shows `text` with "Annuler", which puts `change` back.
    pub(super) fn notice_with_undo(&self, text: String, change: UndoableChange) {
        self.notice_for(text, UNDO_NOTICE_DISPLAY_DURATION);
        *self.pending_undo.borrow_mut() = Some(change);
        if let Some(window) = self.window.upgrade() {
            window
                .global::<AppState>()
                .set_message_action_label(UNDO_LABEL.into());
        }
    }

    /// Drops the "Annuler" of the previous confirmation (a new message replaces it).
    pub(super) fn forget_pending_undo(&self) {
        self.pending_undo.borrow_mut().take();
        if let Some(window) = self.window.upgrade() {
            window
                .global::<AppState>()
                .set_message_action_label("".into());
        }
    }

    fn undo_pending_change(&self) {
        let Some(change) = self.pending_undo.borrow_mut().take() else {
            return;
        };
        if let Some(window) = self.window.upgrade() {
            let state = window.global::<AppState>();
            state.set_message("".into());
            state.set_message_action_label("".into());
        }
        match change {
            UndoableChange::QueueEdit => self.undo_queue_edit(true),
            UndoableChange::PlaylistEntries {
                playlist_id,
                previous_paths,
            } => self.restore_playlist_entries(playlist_id, previous_paths),
            UndoableChange::Favorites {
                paths,
                previous_favorites,
            } => self.restore_favorites(&paths, &previous_favorites),
            UndoableChange::LibraryRemoval { paths } => {
                if let Some(snapshot) = self.with_library(|store| {
                    store.restore_tracks(&paths)?;
                    store.snapshot()
                }) {
                    self.replace_albums(snapshot);
                }
            }
        }
    }

    /// Ctrl/Cmd+Z (`is_undo`) or Ctrl/Cmd+Shift+Z on the tracks queued by hand.
    pub(super) fn undo_queue_edit(&self, is_undo: bool) {
        let has_changed = {
            let mut queue = self.play_queue.borrow_mut();
            if is_undo {
                queue.undo_edit()
            } else {
                queue.redo_edit()
            }
        };
        if has_changed {
            self.refresh_queue_view();
            self.notice(
                if is_undo {
                    "File d’attente : modification annulée"
                } else {
                    "File d’attente : modification rétablie"
                }
                .to_owned(),
            );
        }
    }

    fn restore_playlist_entries(&self, playlist_id: i64, previous_paths: Vec<PathBuf>) {
        if self
            .with_library(|store| store.replace_playlist_entries(playlist_id, &previous_paths))
            .is_none()
        {
            return;
        }
        self.clear_selection();
        if let Some(playlist) = self
            .playlists
            .borrow_mut()
            .iter_mut()
            .find(|playlist| playlist.id == playlist_id)
        {
            playlist.track_paths = previous_paths;
        }
        self.show_playlists();
    }

    fn restore_favorites(&self, paths: &[PathBuf], previous_favorites: &[PathBuf]) {
        let is_previous_favorite = |path: &PathBuf| previous_favorites.contains(path);
        if self
            .with_library(|store| {
                paths
                    .iter()
                    .try_for_each(|path| store.set_favorite(path, is_previous_favorite(path)))
            })
            .is_none()
        {
            return;
        }
        {
            let mut favorites = self.favorites.borrow_mut();
            for path in paths {
                if is_previous_favorite(path) {
                    favorites.insert(path.clone());
                } else {
                    favorites.remove(path);
                }
            }
        }
        self.refresh_favorite_marks();
    }
}
