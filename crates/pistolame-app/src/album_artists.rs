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

/// `artists` with `old` called `new`; when `new` already is among them, the two become one
/// (renaming "WeRenoi" to "Werenoi" merges the two spellings).
pub(crate) fn renamed(artists: &[String], old: &str, new: &str) -> Vec<String> {
    let names: Vec<&str> = artists
        .iter()
        .map(|name| {
            // The new spelling also replaces the old ones of `new` itself.
            if name == old || name.eq_ignore_ascii_case(new) {
                new
            } else {
                name.as_str()
            }
        })
        .collect();
    split(&names.join(";"))
}

/// Another artist of `known` whose name differs from `artist` only by case: the likely
/// spelling to merge it with.
pub(crate) fn namesake<'a>(artist: &str, known: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    known
        .into_iter()
        .find(|name| *name != artist && name.eq_ignore_ascii_case(artist))
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
    fn renaming_an_artist_keeps_the_others_and_merges_spellings() {
        assert_eq!(
            renamed(&names(&["WeRenoi", "Gims"]), "WeRenoi", "Werenoi"),
            names(&["Werenoi", "Gims"])
        );
        assert_eq!(
            renamed(&names(&["Gims", "WeRenoi"]), "Gims", "werenoi"),
            names(&["werenoi"])
        );
        assert_eq!(
            renamed(&names(&["WeRenoi", "Gims"]), "Gims", "werenoi"),
            names(&["werenoi"])
        );
        assert_eq!(renamed(&names(&["Air"]), "Beck", "B"), names(&["Air"]));
    }

    #[test]
    fn a_namesake_differs_only_by_case() {
        let known = ["Werenoi", "WeRenoi", "Gims"];
        assert_eq!(namesake("WeRenoi", known.into_iter()), Some("Werenoi"));
        assert_eq!(namesake("Gims", known.into_iter()), None);
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
