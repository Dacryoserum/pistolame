//! Artist photos for the "Artistes" view, from Deezer's public API (free, no key):
//! `https://api.deezer.com/search/artist?q=<name>`, documented at developers.deezer.com/api.
//!
//! Only the artist's name leaves the computer, and only when the user keeps "Photos d'artistes"
//! on (Réglages). A photo is used only when Deezer's artist has exactly the library's name
//! (accents and case aside): a compilation's "Various Artists" must not get a random face.
//! Lookups run on a worker thread, one at a time, and every result (found or not) is stored in
//! the library so it is not asked again.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::Deserialize;

/// Deezer's artist search.
const SEARCH_URL: &str = "https://api.deezer.com/search/artist?limit=5&q=";
/// Pause between two artists: Deezer allows 50 requests per 5 seconds; we stay far below.
const PAUSE_BETWEEN_ARTISTS: Duration = Duration::from_millis(400);
/// Longest time one request may take, in seconds.
const REQUEST_TIMEOUT_SECONDS: &str = "15";
/// An artist without a photo is asked again after 30 days (photos get added online).
pub(crate) const RETRY_AFTER_SECONDS: i64 = 30 * 24 * 3600;
/// Deezer's URL for "no picture": the image hash between the slashes is empty.
const MISSING_PICTURE_MARKER: &str = "/artist//";

/// One photo found (or not) for an artist, sent back to the UI thread.
pub(crate) struct ArtistPhoto {
    pub(crate) artist: String,
    pub(crate) image: Option<Vec<u8>>,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    data: Vec<SearchArtist>,
}

#[derive(Deserialize)]
struct SearchArtist {
    name: String,
    /// 500 × 500, enough for the 160 px tiles on a Retina screen.
    #[serde(default)]
    picture_big: Option<String>,
}

/// The search URL for `artist`.
fn search_url(artist: &str) -> String {
    format!("{SEARCH_URL}{}", percent_encode(artist))
}

/// Encodes everything but unreserved characters (RFC 3986 §2.3).
fn percent_encode(text: &str) -> String {
    let mut encoded = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The picture URL of the result named exactly `artist`, if it has a real picture.
fn pick_picture(response: &[u8], artist: &str) -> Option<String> {
    let response: SearchResponse = serde_json::from_slice(response).ok()?;
    let wanted = crate::search::normalize(artist.trim());
    response
        .data
        .into_iter()
        .find(|candidate| crate::search::normalize(candidate.name.trim()) == wanted)?
        .picture_big
        .filter(|url| url.starts_with("https://") && !url.contains(MISSING_PICTURE_MARKER))
}

/// Names never worth a lookup: placeholders taggers write for compilations or unknowns.
fn is_placeholder(artist: &str) -> bool {
    const PLACEHOLDERS: [&str; 6] = [
        "various artists",
        "various",
        "artistes divers",
        "divers",
        "unknown artist",
        "artiste inconnu",
    ];
    let normalized = crate::search::normalize(artist.trim());
    normalized.is_empty() || PLACEHOLDERS.contains(&normalized.as_str())
}

/// Looks for photos on a worker thread.
pub(crate) struct ArtistPhotoWorker {
    requests: Sender<Vec<String>>,
    results: Receiver<ArtistPhoto>,
}

impl ArtistPhotoWorker {
    /// Starts the worker; it stores each result in the library at `database` too.
    pub(crate) fn start(database: PathBuf) -> Option<ArtistPhotoWorker> {
        let (requests, pending) = unbounded::<Vec<String>>();
        let (found, results) = unbounded();
        std::thread::Builder::new()
            .name("pistolame-artist-photos".into())
            .spawn(move || {
                while let Ok(artists) = pending.recv() {
                    for artist in artists {
                        let image = if is_placeholder(&artist) {
                            None
                        } else {
                            look_up(&artist)
                        };
                        if let Ok(mut store) = pistolame_library::LibraryStore::open(&database) {
                            // A failed write only means asking again next time.
                            let _ = store.set_artist_image(&artist, image.as_deref());
                        }
                        if found.send(ArtistPhoto { artist, image }).is_err() {
                            return;
                        }
                        std::thread::sleep(PAUSE_BETWEEN_ARTISTS);
                    }
                }
            })
            .ok()?;
        Some(ArtistPhotoWorker { requests, results })
    }

    /// Asks for photos of `artists` (already filtered to those due).
    pub(crate) fn request(&self, artists: Vec<String>) {
        if !artists.is_empty() {
            let _ = self.requests.send(artists);
        }
    }

    /// The photos found since the last call.
    pub(crate) fn take_results(&self) -> Vec<ArtistPhoto> {
        self.results.try_iter().collect()
    }
}

/// Searches Deezer and downloads the photo. `None` on no match or any network problem.
fn look_up(artist: &str) -> Option<Vec<u8>> {
    let response = fetch(&search_url(artist))?;
    let picture = pick_picture(&response, artist)?;
    fetch(&picture).filter(|bytes| !bytes.is_empty())
}

/// GET with the system's curl, like the updater (no HTTP library in the app).
fn fetch(url: &str) -> Option<Vec<u8>> {
    let mut command = Command::new("curl");
    command
        .args(["-fsSL", "--max-time", REQUEST_TIMEOUT_SECONDS])
        .arg(url);
    hide_console_window(&mut command);
    let output = command.output().ok()?;
    output.status.success().then_some(output.stdout)
}

/// Keeps Windows from flashing a console for each curl (the app has no console of its own).
#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NO_WINDOW`, from the Windows process creation flags.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_encoded_for_the_query() {
        assert_eq!(
            search_url("Björk & Co"),
            "https://api.deezer.com/search/artist?limit=5&q=Bj%C3%B6rk%20%26%20Co"
        );
    }

    #[test]
    fn only_an_exact_name_with_a_real_picture_is_used() {
        let response = br#"{"data":[
            {"name":"Howard Leslie Shore","picture_big":"https://cdn/images/artist//500x500.jpg"},
            {"name":"howard shore","picture_big":"https://cdn/images/artist/b46a/500x500.jpg"}]}"#;
        assert_eq!(
            pick_picture(response, "Howard Shore"),
            Some("https://cdn/images/artist/b46a/500x500.jpg".to_owned())
        );
        let gershwin =
            br#"{"data":[{"name":"George Gershwin","picture_big":"https://cdn/a/x.jpg"}]}"#;
        assert_eq!(pick_picture(gershwin, "Various Artists"), None);
        let no_picture =
            br#"{"data":[{"name":"Rare","picture_big":"https://cdn/images/artist//500x500.jpg"}]}"#;
        assert_eq!(pick_picture(no_picture, "Rare"), None);
        assert_eq!(pick_picture(b"not json", "Rare"), None);
    }

    #[test]
    fn compilation_placeholders_are_never_looked_up() {
        assert!(is_placeholder("Various Artists"));
        assert!(is_placeholder("  "));
        assert!(!is_placeholder("Hans Zimmer"));
    }
}
