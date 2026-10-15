//! The play queue, modeled on Spotify's and Apple Music's (research of 2026-10-12):
//! - a *context*: the album or playlist being played, in order, with the current position;
//! - *up next*: tracks the user added by hand ("Lire ensuite" at the front, "Ajouter à la file
//!   d'attente" at the back). They always play before the rest of the context, are never
//!   shuffled, and leave the queue once played.
//!
//! Tracks are kept as paths, so a library rescan that renumbers albums cannot shift them.

use std::collections::VecDeque;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The whole queue as plain data, to store it between runs (`crate::session`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct QueueSnapshot {
    pub(crate) context: Vec<PathBuf>,
    pub(crate) context_name: String,
    pub(crate) context_position: usize,
    pub(crate) up_next: Vec<PathBuf>,
    pub(crate) current: Option<PathBuf>,
}

/// How many tracks played before the current one the queue remembers ("Historique").
const PLAYED_HISTORY_LENGTH: usize = 50;
/// How many edits of the tracks queued by hand Ctrl/Cmd+Z can undo.
const UNDOABLE_EDIT_COUNT: usize = 50;

/// What plays now and next.
#[derive(Debug, Default)]
pub(crate) struct PlayQueue {
    context: Vec<PathBuf>,
    context_name: String,
    context_position: usize,
    up_next: VecDeque<PathBuf>,
    current: Option<PathBuf>,
    /// The tracks played before the current one, oldest first. Not stored between runs.
    played: VecDeque<PathBuf>,
    /// The "up next" list before each edit (newest last), and the ones undone.
    undo_states: Vec<VecDeque<PathBuf>>,
    redo_states: Vec<VecDeque<PathBuf>>,
}

impl PlayQueue {
    /// Plays `paths` (an album or a playlist named `name`) from `position`. The tracks the
    /// user queued by hand are kept and still play first.
    pub(crate) fn play_context(
        &mut self,
        paths: Vec<PathBuf>,
        position: usize,
        name: String,
    ) -> Option<PathBuf> {
        let current = paths.get(position)?.clone();
        self.context = paths;
        self.context_name = name;
        self.context_position = position;
        self.set_current(current.clone());
        Some(current)
    }

    /// "Lire ensuite": `paths` play right after the current track, in their order, before any
    /// track already queued.
    pub(crate) fn play_next(&mut self, paths: Vec<PathBuf>) {
        self.remember_edit();
        for path in paths.into_iter().rev() {
            self.up_next.push_front(path);
        }
    }

    /// "Ajouter à la file d'attente": `paths` play after every track already queued.
    pub(crate) fn add_to_queue(&mut self, paths: Vec<PathBuf>) {
        self.remember_edit();
        self.up_next.extend(paths);
    }

    /// Moves queued track `index` one place earlier (`is_earlier`) or later. False when it
    /// cannot move (first or last, or no such track).
    pub(crate) fn move_up_next(&mut self, index: usize, is_earlier: bool) -> bool {
        let other = if is_earlier {
            index.checked_sub(1)
        } else {
            Some(index + 1)
        };
        let Some(other) = other.filter(|&other| other < self.up_next.len()) else {
            return false;
        };
        if index >= self.up_next.len() {
            return false;
        }
        self.remember_edit();
        self.up_next.swap(index, other);
        true
    }

    /// Undoes the last edit of the tracks queued by hand (Ctrl/Cmd+Z). False when none is left.
    pub(crate) fn undo_edit(&mut self) -> bool {
        let Some(previous) = self.undo_states.pop() else {
            return false;
        };
        let undone = std::mem::replace(&mut self.up_next, previous);
        self.redo_states.push(undone);
        true
    }

    /// Redoes the last undone edit (Ctrl/Cmd+Shift+Z). False when none is left.
    pub(crate) fn redo_edit(&mut self) -> bool {
        let Some(next) = self.redo_states.pop() else {
            return false;
        };
        let redone = std::mem::replace(&mut self.up_next, next);
        self.undo_states.push(redone);
        true
    }

    /// The tracks played before the current one, most recent first.
    pub(crate) fn played(&self) -> impl Iterator<Item = &PathBuf> {
        self.played.iter().rev()
    }

    /// "Lire à partir d'ici" on a played track (`index` as listed by [`PlayQueue::played`]): in
    /// the context, playing goes on from it; otherwise it plays before the queued tracks.
    pub(crate) fn play_from_played(&mut self, index: usize) -> Option<PathBuf> {
        let path = self.played().nth(index)?.clone();
        if let Some(position) = self.context.iter().position(|candidate| *candidate == path) {
            return self.jump_to_context_position(position);
        }
        self.up_next.push_front(path);
        self.advance(None, false)
    }

    /// Keeps the "up next" list before an edit, so it can be undone; a new edit forgets the
    /// undone ones.
    fn remember_edit(&mut self) {
        if self.undo_states.len() == UNDOABLE_EDIT_COUNT {
            self.undo_states.remove(0);
        }
        self.undo_states.push(self.up_next.clone());
        self.redo_states.clear();
    }

    /// Makes `path` the current track; the one it replaces joins the played history.
    fn set_current(&mut self, path: PathBuf) {
        if let Some(previous) = self.current.replace(path)
            && Some(&previous) != self.current.as_ref()
        {
            if self.played.len() == PLAYED_HISTORY_LENGTH {
                self.played.pop_front();
            }
            self.played.push_back(previous);
        }
    }

    /// Moves to the next track: the first queued one, otherwise the next of the context.
    /// `shuffled_position` is a random context position other than the current one, chosen by
    /// the caller when shuffle is on. `None` at the end of the context without repeat.
    pub(crate) fn advance(
        &mut self,
        shuffled_position: Option<usize>,
        is_repeat_on: bool,
    ) -> Option<PathBuf> {
        if let Some(path) = self.up_next.pop_front() {
            // Undoing an earlier edit would bring back the track that just left the list.
            self.undo_states.clear();
            self.redo_states.clear();
            self.set_current(path.clone());
            return Some(path);
        }
        let count = self.context.len();
        if count == 0 {
            return None;
        }
        let next = match shuffled_position {
            Some(position) if position < count => position,
            _ if self.context_position + 1 < count => self.context_position + 1,
            _ if is_repeat_on => 0,
            _ => return None,
        };
        self.jump_to_context_position(next)
    }

    /// Moves to the previous track of the context (wrapping to the last one).
    pub(crate) fn previous(&mut self) -> Option<PathBuf> {
        let count = self.context.len();
        if count == 0 {
            return None;
        }
        // Going back is not listening history: the track left stays in "Ensuite depuis" only.
        let position = (self.context_position + count - 1) % count;
        let path = self.context.get(position)?.clone();
        self.context_position = position;
        self.current = Some(path.clone());
        Some(path)
    }

    /// Plays queued track `index`; the tracks queued before it are skipped.
    pub(crate) fn jump_to_up_next(&mut self, index: usize) -> Option<PathBuf> {
        if index >= self.up_next.len() {
            return None;
        }
        self.up_next.drain(..index);
        self.advance(None, false)
    }

    /// Plays the context track `offset` places after the current one (0 = the next one).
    pub(crate) fn jump_to_upcoming(&mut self, offset: usize) -> Option<PathBuf> {
        self.jump_to_context_position(self.context_position + 1 + offset)
    }

    /// Removes queued track `index`.
    pub(crate) fn remove_up_next(&mut self, index: usize) {
        if index < self.up_next.len() {
            self.remember_edit();
            self.up_next.remove(index);
        }
    }

    /// Empties the tracks queued by hand.
    pub(crate) fn clear_up_next(&mut self) {
        if !self.up_next.is_empty() {
            self.remember_edit();
            self.up_next.clear();
        }
    }

    /// Whether something plays after the current track.
    pub(crate) fn has_next(&self, is_repeat_on: bool, is_shuffle_on: bool) -> bool {
        !self.up_next.is_empty()
            || crate::transport::can_advance(
                self.context_position,
                self.context.len(),
                is_repeat_on,
                is_shuffle_on,
            )
    }

    /// Number of tracks in the context.
    pub(crate) fn context_len(&self) -> usize {
        self.context.len()
    }

    /// Position of the current context track.
    pub(crate) fn context_position(&self) -> usize {
        self.context_position
    }

    /// Name of the album or playlist being played.
    pub(crate) fn context_name(&self) -> &str {
        &self.context_name
    }

    /// The tracks queued by hand, in playing order.
    pub(crate) fn up_next(&self) -> impl Iterator<Item = &PathBuf> {
        self.up_next.iter()
    }

    /// The context tracks after the current one, in order.
    pub(crate) fn upcoming(&self) -> &[PathBuf] {
        self.context
            .get(self.context_position + 1..)
            .unwrap_or_default()
    }

    /// The track playing now.
    pub(crate) fn current(&self) -> Option<&PathBuf> {
        self.current.as_ref()
    }

    /// The queue as plain data.
    pub(crate) fn snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            context: self.context.clone(),
            context_name: self.context_name.clone(),
            context_position: self.context_position,
            up_next: self.up_next.iter().cloned().collect(),
            current: self.current.clone(),
        }
    }

    /// A queue from stored data. A position past the end of the context is clamped to it.
    pub(crate) fn from_snapshot(snapshot: QueueSnapshot) -> PlayQueue {
        PlayQueue {
            context_position: snapshot
                .context_position
                .min(snapshot.context.len().saturating_sub(1)),
            context: snapshot.context,
            context_name: snapshot.context_name,
            up_next: snapshot.up_next.into(),
            current: snapshot.current,
            ..PlayQueue::default()
        }
    }

    fn jump_to_context_position(&mut self, position: usize) -> Option<PathBuf> {
        let path = self.context.get(position)?.clone();
        self.context_position = position;
        self.set_current(path.clone());
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    fn album() -> PlayQueue {
        let mut queue = PlayQueue::default();
        queue.play_context(paths(&["a1", "a2", "a3"]), 0, "Album".into());
        queue
    }

    #[test]
    fn the_context_plays_in_order_and_stops_at_the_end() {
        let mut queue = album();
        assert_eq!(queue.advance(None, false), Some("a2".into()));
        assert_eq!(queue.advance(None, false), Some("a3".into()));
        assert_eq!(queue.advance(None, false), None);
        assert_eq!(queue.advance(None, true), Some("a1".into()));
    }

    #[test]
    fn queued_tracks_play_before_the_rest_of_the_context() {
        let mut queue = album();
        queue.add_to_queue(paths(&["q1"]));
        queue.play_next(paths(&["n1", "n2"]));
        assert_eq!(
            queue.up_next().cloned().collect::<Vec<_>>(),
            paths(&["n1", "n2", "q1"])
        );
        assert_eq!(queue.advance(None, false), Some("n1".into()));
        assert_eq!(queue.advance(None, false), Some("n2".into()));
        assert_eq!(queue.advance(None, false), Some("q1".into()));
        // The context resumes where it was.
        assert_eq!(queue.advance(None, false), Some("a2".into()));
    }

    #[test]
    fn shuffle_never_reorders_the_tracks_queued_by_hand() {
        let mut queue = album();
        queue.add_to_queue(paths(&["q1"]));
        assert_eq!(queue.advance(Some(2), false), Some("q1".into()));
        assert_eq!(queue.advance(Some(2), false), Some("a3".into()));
    }

    #[test]
    fn a_new_context_keeps_the_queue() {
        let mut queue = album();
        queue.add_to_queue(paths(&["q1"]));
        queue.play_context(paths(&["p1", "p2"]), 1, "Playlist".into());
        assert_eq!(queue.context_name(), "Playlist");
        assert_eq!(queue.advance(None, false), Some("q1".into()));
        assert!(!queue.has_next(false, false));
    }

    #[test]
    fn jumping_into_the_queue_skips_what_is_before() {
        let mut queue = album();
        queue.add_to_queue(paths(&["q1", "q2", "q3"]));
        assert_eq!(queue.jump_to_up_next(1), Some("q2".into()));
        assert_eq!(queue.up_next().cloned().collect::<Vec<_>>(), paths(&["q3"]));
        assert_eq!(queue.jump_to_upcoming(1), Some("a3".into()));
        assert!(queue.upcoming().is_empty());
    }

    #[test]
    fn removing_and_clearing_queued_tracks() {
        let mut queue = album();
        queue.add_to_queue(paths(&["q1", "q2"]));
        queue.remove_up_next(0);
        assert_eq!(queue.up_next().cloned().collect::<Vec<_>>(), paths(&["q2"]));
        queue.clear_up_next();
        assert_eq!(queue.up_next().count(), 0);
        assert_eq!(queue.upcoming(), &paths(&["a2", "a3"])[..]);
    }

    fn queued(queue: &PlayQueue) -> Vec<PathBuf> {
        queue.up_next().cloned().collect()
    }

    #[test]
    fn edits_of_the_queue_can_be_undone_and_redone() {
        let mut queue = album();
        queue.add_to_queue(paths(&["q1", "q2"]));
        queue.remove_up_next(0);
        queue.clear_up_next();
        assert!(queue.undo_edit());
        assert_eq!(queued(&queue), paths(&["q2"]));
        assert!(queue.undo_edit());
        assert_eq!(queued(&queue), paths(&["q1", "q2"]));
        assert!(queue.redo_edit());
        assert_eq!(queued(&queue), paths(&["q2"]));
        // A new edit forgets what was undone.
        queue.add_to_queue(paths(&["q3"]));
        assert!(!queue.redo_edit());
        assert!(queue.undo_edit());
        assert!(queue.undo_edit());
        assert!(queue.undo_edit());
        assert_eq!(queued(&queue), paths(&[]));
        assert!(!queue.undo_edit());
    }

    #[test]
    fn playing_a_queued_track_forgets_the_undo_steps() {
        let mut queue = album();
        queue.add_to_queue(paths(&["q1"]));
        queue.advance(None, false);
        assert!(!queue.undo_edit());
        assert_eq!(queued(&queue), paths(&[]));
    }

    #[test]
    fn queued_tracks_move_one_place_at_a_time() {
        let mut queue = album();
        queue.add_to_queue(paths(&["q1", "q2", "q3"]));
        assert!(queue.move_up_next(2, true));
        assert_eq!(queued(&queue), paths(&["q1", "q3", "q2"]));
        assert!(queue.move_up_next(0, false));
        assert_eq!(queued(&queue), paths(&["q3", "q1", "q2"]));
        assert!(!queue.move_up_next(0, true));
        assert!(!queue.move_up_next(2, false));
        assert!(!queue.move_up_next(7, true));
        assert!(queue.undo_edit());
        assert_eq!(queued(&queue), paths(&["q1", "q3", "q2"]));
    }

    #[test]
    fn played_tracks_are_listed_most_recent_first() {
        let mut queue = album();
        queue.advance(None, false);
        queue.advance(None, false);
        assert_eq!(
            queue.played().cloned().collect::<Vec<_>>(),
            paths(&["a2", "a1"])
        );
    }

    #[test]
    fn going_back_does_not_fill_the_history() {
        let mut queue = album();
        queue.advance(None, false);
        queue.advance(None, false);
        assert_eq!(queue.previous(), Some("a2".into()));
        assert_eq!(
            queue.played().cloned().collect::<Vec<_>>(),
            paths(&["a2", "a1"])
        );
    }

    #[test]
    fn playing_from_a_played_context_track_goes_on_from_it() {
        let mut queue = album();
        queue.advance(None, false);
        queue.advance(None, false);
        // "a1" is second most recent.
        assert_eq!(queue.play_from_played(1), Some("a1".into()));
        assert_eq!(queue.advance(None, false), Some("a2".into()));
    }

    #[test]
    fn playing_from_a_played_track_outside_the_context_queues_it_first() {
        let mut queue = album();
        queue.play_next(paths(&["n1"]));
        queue.advance(None, false);
        queue.advance(None, false);
        assert_eq!(queue.current(), Some(&"a2".into()));
        // Played: "n1", then "a1".
        assert_eq!(queue.play_from_played(0), Some("n1".into()));
        assert_eq!(queue.advance(None, false), Some("a3".into()));
    }

    #[test]
    fn a_snapshot_restores_the_same_queue() {
        let mut queue = album();
        queue.advance(None, false);
        queue.add_to_queue(paths(&["q1"]));
        let mut restored = PlayQueue::from_snapshot(queue.snapshot());
        assert_eq!(restored.current(), Some(&PathBuf::from("a2")));
        assert_eq!(restored.context_name(), "Album");
        assert_eq!(restored.advance(None, false), Some("q1".into()));
        assert_eq!(restored.advance(None, false), Some("a3".into()));
    }

    #[test]
    fn previous_wraps_inside_the_context() {
        let mut queue = album();
        assert_eq!(queue.previous(), Some("a3".into()));
        assert_eq!(queue.current(), Some(&PathBuf::from("a3")));
    }
}
