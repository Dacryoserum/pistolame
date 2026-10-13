//! Extended M3U export (`.m3u8`, UTF-8), the playlist format every player reads.
//! Reference: the de facto "Extended M3U" directives #EXTM3U and #EXTINF.

use std::path::PathBuf;

/// One playlist line.
pub(crate) struct M3uEntry {
    pub(crate) path: PathBuf,
    pub(crate) duration_seconds: u64,
    pub(crate) artist: String,
    pub(crate) title: String,
}

/// The file text: a header, then `#EXTINF:<seconds>,<artist> - <title>` and the absolute path
/// for each track. Line breaks inside tags are replaced so each entry stays on two lines.
pub(crate) fn write_m3u(entries: &[M3uEntry]) -> String {
    let clean = |text: &str| text.replace(['\r', '\n'], " ");
    let mut text = String::from("#EXTM3U\n");
    for entry in entries {
        text.push_str(&format!(
            "#EXTINF:{},{} - {}\n{}\n",
            entry.duration_seconds,
            clean(&entry.artist),
            clean(&entry.title),
            entry.path.display()
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_written_in_extended_m3u() {
        let text = write_m3u(&[M3uEntry {
            path: PathBuf::from("/Music/Ramin Djawadi/Main Title.flac"),
            duration_seconds: 106,
            artist: "Ramin Djawadi".into(),
            title: "Main\nTitle".into(),
        }]);
        assert_eq!(
            text,
            "#EXTM3U\n#EXTINF:106,Ramin Djawadi - Main Title\n/Music/Ramin Djawadi/Main Title.flac\n"
        );
    }
}
