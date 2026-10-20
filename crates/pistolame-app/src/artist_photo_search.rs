//! The photo the user picks for an artist: searches Deezer's public API for any name typed
//! (`https://api.deezer.com/search/artist?q=<name>`, developers.deezer.com/api) and offers every
//! artist found with a picture, so a namesake's face can be replaced by the right one.
//!
//! Only the typed text leaves the computer, and only when the user searches. Requests run on a
//! worker thread; each carries a generation so the UI drops the answers of older requests.

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::Deserialize;

use crate::artist_photos::{fetch, is_real_picture, percent_encode};

/// Deezer's artist search; a dozen results fill three rows of four in the picker
/// (`ArtistPhotoPicker` in components.slint).
const SEARCH_URL: &str = "https://api.deezer.com/search/artist?limit=12&q=";

/// An artist offered in the picker.
pub(crate) struct PhotoCandidate {
    pub(crate) name: String,
    /// Deezer fans, to tell namesakes apart.
    pub(crate) fan_count: u64,
    /// The picture stored when this candidate is chosen.
    pub(crate) picture_url: String,
    /// The small picture shown in the picker; `None` when it could not be downloaded.
    pub(crate) thumbnail: Option<Vec<u8>>,
}

/// What the worker sends back, tagged with the generation of its request.
pub(crate) enum PhotoSearchOutcome {
    /// The artists found; `is_failed` when Deezer could not be reached or answered nonsense.
    Candidates {
        generation: u64,
        candidates: Vec<PhotoCandidate>,
        is_failed: bool,
    },
    /// The chosen picture, `None` when it could not be downloaded as an image.
    Picture {
        generation: u64,
        bytes: Option<Vec<u8>>,
    },
}

enum PhotoSearchRequest {
    Search { generation: u64, query: String },
    Download { generation: u64, url: String },
}

#[derive(Deserialize)]
struct SearchResponse {
    data: Vec<SearchArtist>,
}

#[derive(Deserialize)]
struct SearchArtist {
    name: String,
    #[serde(default)]
    nb_fan: u64,
    /// 250 × 250, for the picker's tiles.
    #[serde(default)]
    picture_medium: Option<String>,
    /// 500 × 500, the size the automatic lookup stores too.
    #[serde(default)]
    picture_big: Option<String>,
}

/// The artists of a Deezer answer that have a real picture: (name, fans, thumbnail URL,
/// picture URL). `None` when the answer is not a search result.
fn found_artists(response: &[u8]) -> Option<Vec<(String, u64, String, String)>> {
    let response: SearchResponse = serde_json::from_slice(response).ok()?;
    Some(
        response
            .data
            .into_iter()
            .filter_map(|artist| {
                let thumbnail = artist.picture_medium.filter(|url| is_real_picture(url))?;
                let picture = artist.picture_big.filter(|url| is_real_picture(url))?;
                Some((artist.name, artist.nb_fan, thumbnail, picture))
            })
            .collect(),
    )
}

/// How many fans, short: "850 fans", "12 k fans", "1,4 M fans".
pub(crate) fn fan_count_label(fan_count: u64) -> String {
    match fan_count {
        0..=999 => format!("{fan_count} fan{}", if fan_count > 1 { "s" } else { "" }),
        1_000..=999_999 => format!("{} k fans", fan_count / 1_000),
        _ => {
            let tenths = fan_count / 100_000;
            format!("{},{} M fans", tenths / 10, tenths % 10)
        }
    }
}

/// Searches and downloads on a worker thread.
pub(crate) struct ArtistPhotoSearch {
    requests: Sender<PhotoSearchRequest>,
    outcomes: Receiver<PhotoSearchOutcome>,
}

impl ArtistPhotoSearch {
    /// Starts the worker; `None` if the thread cannot start.
    pub(crate) fn start() -> Option<ArtistPhotoSearch> {
        let (requests, pending) = unbounded::<PhotoSearchRequest>();
        let (sender, outcomes) = unbounded();
        std::thread::Builder::new()
            .name("pistolame-artist-photo-search".into())
            .spawn(move || {
                while let Ok(request) = pending.recv() {
                    let outcome = match request {
                        PhotoSearchRequest::Search { generation, query } => {
                            search(generation, &query)
                        }
                        PhotoSearchRequest::Download { generation, url } => {
                            PhotoSearchOutcome::Picture {
                                generation,
                                bytes: fetch(&url).filter(|bytes| {
                                    bytes.len() as u64 <= pistolame_library::MAX_COVER_BYTES
                                        && crate::artwork_cache::is_readable_image(bytes)
                                }),
                            }
                        }
                    };
                    if sender.send(outcome).is_err() {
                        return;
                    }
                }
            })
            .ok()?;
        Some(ArtistPhotoSearch { requests, outcomes })
    }

    /// Looks for the artists named like `query`.
    pub(crate) fn search(&self, generation: u64, query: &str) {
        let _ = self.requests.send(PhotoSearchRequest::Search {
            generation,
            query: query.to_owned(),
        });
    }

    /// Downloads the picture at `url`.
    pub(crate) fn download(&self, generation: u64, url: &str) {
        let _ = self.requests.send(PhotoSearchRequest::Download {
            generation,
            url: url.to_owned(),
        });
    }

    /// The answers received since the last call.
    pub(crate) fn take_outcomes(&self) -> Vec<PhotoSearchOutcome> {
        self.outcomes.try_iter().collect()
    }
}

/// Runs one search, then downloads the thumbnails all at once (one after the other, a dozen
/// take several seconds).
fn search(generation: u64, query: &str) -> PhotoSearchOutcome {
    let found = fetch(&format!("{SEARCH_URL}{}", percent_encode(query.trim())))
        .and_then(|response| found_artists(&response));
    let Some(found) = found else {
        return PhotoSearchOutcome::Candidates {
            generation,
            candidates: Vec::new(),
            is_failed: true,
        };
    };
    let candidates = std::thread::scope(|scope| {
        let downloads: Vec<_> = found
            .into_iter()
            .map(|(name, fan_count, thumbnail_url, picture_url)| {
                // Without a thread (the system refused one), the tile shows no picture.
                let thumbnail = std::thread::Builder::new()
                    .spawn_scoped(scope, move || fetch(&thumbnail_url))
                    .ok();
                (name, fan_count, picture_url, thumbnail)
            })
            .collect();
        downloads
            .into_iter()
            .map(|(name, fan_count, picture_url, thumbnail)| PhotoCandidate {
                name,
                fan_count,
                picture_url,
                thumbnail: thumbnail.and_then(|download| download.join().ok().flatten()),
            })
            .collect()
    });
    PhotoSearchOutcome::Candidates {
        generation,
        candidates,
        is_failed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_artists_with_a_real_picture_are_offered() {
        let response = br#"{"data":[
            {"name":"Maes","nb_fan":1400000,
             "picture_medium":"https://cdn/images/artist/ab/250x250.jpg",
             "picture_big":"https://cdn/images/artist/ab/500x500.jpg"},
            {"name":"Maes Other","nb_fan":3,
             "picture_medium":"https://cdn/images/artist//250x250.jpg",
             "picture_big":"https://cdn/images/artist//500x500.jpg"}]}"#;
        assert_eq!(
            found_artists(response),
            Some(vec![(
                "Maes".to_owned(),
                1_400_000,
                "https://cdn/images/artist/ab/250x250.jpg".to_owned(),
                "https://cdn/images/artist/ab/500x500.jpg".to_owned()
            )])
        );
        assert_eq!(found_artists(br#"{"error":{"code":4}}"#), None);
        assert_eq!(found_artists(b"not json"), None);
    }

    #[test]
    fn fan_counts_read_short() {
        assert_eq!(fan_count_label(1), "1 fan");
        assert_eq!(fan_count_label(850), "850 fans");
        assert_eq!(fan_count_label(12_345), "12 k fans");
        assert_eq!(fan_count_label(1_449_999), "1,4 M fans");
    }
}
