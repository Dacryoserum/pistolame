//! The play queue, modeled on Spotify's and Apple Music's (research of 2026-10-12):
//! - a *context*: the album or playlist being played, in order, with the current position;
//! - *up next*: tracks the user added by hand ("Lire ensuite" at the front, "Ajouter à la file
//!   d'attente" at the back). They always play before the rest of the context, are never
//!   shuffled, and leave the queue once played.
//!
//! Tracks are kept as paths, so a library rescan that renumbers albums cannot shift them.

use std::collections::VecDeque;
use std::path::PathBuf;

/// What plays now and next.
#[derive(Debug, Default)]
pub(crate) struct PlayQueue {
    context: Vec<PathBuf>,
    context_name: String,
    context_position: usize,
    up_next: VecDeque<PathBuf>,
    current: Option<PathBuf>,
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
        self.current = Some(current.clone());
        Some(current)
    }

    /// "Lire ensuite": `paths` play right after the current track, in their order, before any
    /// track already queued.
    pub(crate) fn play_next(&mut self, paths: Vec<PathBuf>) {
        for path in paths.into_iter().rev() {
            self.up_next.push_front(path);
        }
    }

    /// "Ajouter à la file d'attente": `paths` play after every track already queued.
    pub(crate) fn add_to_queue(&mut self, paths: Vec<PathBuf>) {
        self.up_next.extend(paths);
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
            self.current = Some(path.clone());
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
        self.jump_to_context_position((self.context_position + count - 1) % count)
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
            self.up_next.remove(index);
        }
    }

    /// Empties the tracks queued by hand.
    pub(crate) fn clear_up_next(&mut self) {
        self.up_next.clear();
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

    fn jump_to_context_position(&mut self, position: usize) -> Option<PathBuf> {
        let path = self.context.get(position)?.clone();
        self.context_position = position;
        self.current = Some(path.clone());
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

    #[test]
    fn previous_wraps_inside_the_context() {
        let mut queue = album();
        assert_eq!(queue.previous(), Some("a3".into()));
        assert_eq!(queue.current(), Some(&PathBuf::from("a3")));
    }
}
