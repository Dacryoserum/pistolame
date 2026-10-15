//! The "À suivre" panel.

use super::undo::UndoableChange;
use super::*;

/// Context tracks listed under "Ensuite depuis" at most: a long playlist would otherwise build
/// hundreds of rows (with artwork) each time the panel is redrawn.
const MAXIMUM_UPCOMING_ROWS: usize = 100;

impl UiBridge {
    /// Redraws the "À suivre" panel.
    pub(super) fn refresh_queue_view(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        let queue = self.play_queue.borrow();
        let row = |path: &PathBuf| self.queue_row(path);
        let current = queue.current().and_then(row);
        state.set_has_queue_current(current.is_some());
        state.set_queue_current(current.unwrap_or_default());
        state.set_queue_played(ModelRc::new(VecModel::from(
            queue.played().filter_map(row).collect::<Vec<_>>(),
        )));
        state.set_queue_up_next(ModelRc::new(VecModel::from(
            queue.up_next().filter_map(row).collect::<Vec<_>>(),
        )));
        let upcoming = queue.upcoming();
        state.set_queue_upcoming(ModelRc::new(VecModel::from(
            upcoming
                .iter()
                .take(MAXIMUM_UPCOMING_ROWS)
                .filter_map(row)
                .collect::<Vec<_>>(),
        )));
        state.set_queue_upcoming_hidden_count(
            upcoming.len().saturating_sub(MAXIMUM_UPCOMING_ROWS) as i32
        );
        state.set_queue_context_name(queue.context_name().into());
    }

    pub(super) fn queue_row(&self, path: &std::path::Path) -> Option<QueueRow> {
        let (album, track) = self.track_location(path)?;
        let albums = self.albums.borrow();
        let album = albums.get(album)?;
        let track = album.tracks.get(track)?;
        Some(QueueRow {
            title: track.title.as_str().into(),
            subtitle: format!("{} — {}", album.artist, album.title).into(),
            duration_label: presentation::clock(track.duration_seconds).into(),
            artwork: album.artwork.clone(),
        })
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
