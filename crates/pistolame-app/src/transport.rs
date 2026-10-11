//! Album sequencing policy, independent of the engine and widgets.
pub(crate) fn can_advance(track: usize, count: usize, repeat: bool, shuffle: bool) -> bool {
    count > 0 && (track + 1 < count || repeat || (shuffle && count > 1))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn album_end_stops_unless_a_playback_mode_continues_it() {
        assert!(!can_advance(2, 3, false, false));
        assert!(can_advance(1, 3, false, false));
        assert!(can_advance(2, 3, true, false));
        assert!(can_advance(2, 3, false, true));
        assert!(!can_advance(0, 0, true, true));
        assert!(!can_advance(0, 1, false, true));
    }
}
