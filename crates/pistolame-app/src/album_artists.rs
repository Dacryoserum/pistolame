//! Albums with several artists ("Various Artists" compilations, duets, collaborations). The
//! list is stored in the album's artist override, joined by `; ` (the library schema is
//! unchanged; the music files are never touched). Pure logic.

/// Joins artists in the stored override. A `;` never appears in a name kept by [`split`].
const STORED_SEPARATOR: &str = "; ";

/// The artists named in a stored artist field, trimmed, without blanks or duplicates.
pub(crate) fn split(stored: &str) -> Vec<String> {
    let mut artists: Vec<String> = Vec::new();
    for name in stored
        .split(';')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        if !artists.iter().any(|known| known.eq_ignore_ascii_case(name)) {
            artists.push(name.to_owned());
        }
    }
    artists
}

/// The stored form of `artists`.
pub(crate) fn join_for_storage(artists: &[String]) -> String {
    artists.join(STORED_SEPARATOR)
}

/// How an album's artists read: "A", "A & B", "A, B & C".
pub(crate) fn display(artists: &[String]) -> String {
    match artists {
        [] => String::new(),
        [only] => only.clone(),
        [first @ .., last] => format!("{} & {last}", first.join(", ")),
    }
}

/// The distinct track artists of an album that are not already in `artists`: one-click
/// suggestions in the editor (a compilation's tracks name its real artists).
pub(crate) fn suggestions<'a>(
    track_artists: impl Iterator<Item = &'a str>,
    artists: &[String],
) -> Vec<String> {
    let mut suggested: Vec<String> = Vec::new();
    for name in track_artists.flat_map(split) {
        let is_known = artists
            .iter()
            .chain(&suggested)
            .any(|known| known.eq_ignore_ascii_case(&name));
        if !is_known {
            suggested.push(name);
        }
    }
    suggested
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn stored_lists_round_trip_without_blanks_or_duplicates() {
        assert_eq!(
            split(" Hans Zimmer ;; Lisa Gerrard; hans zimmer"),
            names(&["Hans Zimmer", "Lisa Gerrard"])
        );
        assert_eq!(join_for_storage(&names(&["A", "B"])), "A; B");
        assert_eq!(
            split(&join_for_storage(&names(&["A", "B"]))),
            names(&["A", "B"])
        );
    }

    #[test]
    fn artists_read_naturally() {
        assert_eq!(display(&names(&["Howard Shore"])), "Howard Shore");
        assert_eq!(display(&names(&["A", "B"])), "A & B");
        assert_eq!(display(&names(&["A", "B", "C"])), "A, B & C");
    }

    #[test]
    fn suggestions_come_from_tracks_and_skip_known_artists() {
        let tracks = ["Daft Punk", "Air", "daft punk", "Air; Beck"];
        assert_eq!(
            suggestions(tracks.into_iter(), &names(&["Air"])),
            names(&["Daft Punk", "Beck"])
        );
    }
}
