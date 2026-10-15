//! The "À suivre" panel.

use super::undo::UndoableChange;
use super::*;

/// Rows listed at most under "Ensuite dans la file" and "Ensuite depuis": a long playlist would
/// otherwise build hundreds of rows (with artwork) each time the panel is redrawn.
const MAXIMUM_QUEUE_SECTION_ROWS: usize = 100;

impl UiBridge {
    /// Redraws the "À suivre" panel.
    pub(super) fn refresh_queue_view(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let queue = self.play_queue.borrow();
        let albums = self.albums.borrow();
        // One pass over the library, then each row is a lookup (not a library scan per row).
        let locations: std::collections::HashMap<&std::path::Path, (&Album, &MusicFile)> = albums
            .iter()
            .flat_map(|album| {
                album
                    .tracks
                    .iter()
                    .map(move |track| (track.path.as_path(), (album, track)))
            })
            .collect();
        let row = |path: &PathBuf| {
            let (album, track) = locations.get(path.as_path())?;
            Some(QueueRow {
                title: track.title.as_str().into(),
                subtitle: format!("{} — {}", album.artist, album.title).into(),
                duration_label: presentation::clock(track.duration_seconds).into(),
                artwork: album.artwork.clone(),
            })
        };
        let rows = |paths: &mut dyn Iterator<Item = &PathBuf>| -> ModelRc<QueueRow> {
            ModelRc::new(VecModel::from(
                paths
                    .take(MAXIMUM_QUEUE_SECTION_ROWS)
                    .filter_map(row)
                    .collect::<Vec<_>>(),
            ))
        };
        let current = queue.current().and_then(row);
        state.set_has_queue_current(current.is_some());
        state.set_queue_current(current.unwrap_or_default());
        state.set_queue_played(rows(&mut queue.played()));
        state.set_queue_up_next(rows(&mut queue.up_next()));
        state.set_queue_up_next_hidden_count(
            queue
                .up_next()
                .count()
                .saturating_sub(MAXIMUM_QUEUE_SECTION_ROWS) as i32,
        );
        let upcoming = queue.upcoming();
        state.set_queue_upcoming(rows(&mut upcoming.iter()));
        state.set_queue_upcoming_hidden_count(
            upcoming.len().saturating_sub(MAXIMUM_QUEUE_SECTION_ROWS) as i32,
        );
        state.set_queue_context_name(queue.context_name().into());
    }

    pub(super) fn connect_queue(self: &Rc<Self>, state: &AppState<'_>) {
        let handle = self.clone();
        state.on_track_action(move |target, action| handle.run_track_action(target, action));
        let handle = self.clone();
        state.on_queue_remove(move |index| {
            if let Ok(index) = usize::try_from(index) {
                handle.play_queue.borrow_mut().remove_up_next(index);
                handle.refresh_queue_view();
                handle.notice_with_undo(
                    "Retiré de la file d’attente".to_owned(),
                    UndoableChange::QueueEdit,
                );
            }
        });
        let handle = self.clone();
        state.on_queue_clear(move || {
            handle.play_queue.borrow_mut().clear_up_next();
            handle.refresh_queue_view();
            handle.notice_with_undo("File d’attente vidée".to_owned(), UndoableChange::QueueEdit);
        });
        let handle = self.clone();
        state.on_queue_move(move |index, is_earlier| {
            let has_moved = usize::try_from(index).is_ok_and(|index| {
                handle
                    .play_queue
                    .borrow_mut()
                    .move_up_next(index, is_earlier)
            });
            if has_moved {
                // The "Annuler" shown is for an earlier edit, no longer the last one.
                handle.forget_pending_undo();
                handle.refresh_queue_view();
            }
        });
        let handle = self.clone();
        state.on_queue_play_up_next(move |index| {
            let path = usize::try_from(index)
                .ok()
                .and_then(|index| handle.play_queue.borrow_mut().jump_to_up_next(index));
            if let Some(path) = path {
                handle.start_playback(path);
            }
        });
        let handle = self.clone();
        state.on_queue_play_upcoming(move |index| {
            let path = usize::try_from(index)
                .ok()
                .and_then(|index| handle.play_queue.borrow_mut().jump_to_upcoming(index));
            if let Some(path) = path {
                handle.start_playback(path);
            }
        });
        let handle = self.clone();
        state.on_queue_play_played(move |index| {
            let path = usize::try_from(index)
                .ok()
                .and_then(|index| handle.play_queue.borrow_mut().play_from_played(index));
            if let Some(path) = path {
                handle.start_playback(path);
            }
        });
    }
}
