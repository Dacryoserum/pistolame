//! Scans container tags (Vorbis comments, ID3, ...) for leftover traces of a lossy
//! encoder — e.g. a FLAC whose `ENCODER` comment still reads "LAME3.100" because the
//! MP3→FLAC conversion tool didn't strip it. A weaker, more circumstantial signal than
//! the spectral analysis in `spectral.rs`/`transcode_detect.rs`: many legitimate lossless
//! conversion pipelines strip tags, and plenty of genuine transcodes carry no residual
//! metadata at all. Treated asymmetrically in `transcode_detect.rs`: a match is real
//! evidence, but the *absence* of a match is not evidence of authenticity.

use serde::Serialize;
use symphonia::core::formats::FormatReader;
use symphonia::core::meta::RawValue;

/// Substrings (case-insensitive) of encoders that **only** ever produce lossy output, so
/// finding one named as the encoding tool is genuine evidence a lossy stage happened. Not
/// exhaustive — a miss here means "no match found", not "no lossy encoder was involved".
///
/// Deliberately excludes `itunes`: iTunes/Music.app is one of the most widely used
/// *lossless* CD rippers there is (ALAC), and it stamps its name into the encoder tag of
/// those files just as it does for AAC purchases. Treating it as a lossy signature made
/// every iTunes-ripped ALAC read as "probably transcoded". A tool that can produce either
/// format is not evidence of which one happened, so it does not belong on this list.
const KNOWN_LOSSY_ENCODER_PATTERNS: &[&str] = &[
    "lame",
    "libmp3lame",
    "qaac",
    "nero aac",
    "faac",
    "fdk-aac",
    "fraunhofer",
    "gogo",
    "xing",
    "blade",
];

/// Only tag keys naming the *encoding tool* are scanned. Free-text fields are not:
/// matching against every value in the file meant a track whose comment happened to read
/// "ripped from CD with iTunes" — or an album titled after a codec — was scored as a
/// transcode on the strength of prose. Substring-matched case-insensitively, so this
/// covers ENCODER, ENCODED_BY, ENCODER_SETTINGS, MP4's `©too`, and friends.
const ENCODER_TAG_KEY_MARKERS: &[&str] =
    &["encoder", "encoded", "encoding", "tool", "software", "too"];

fn is_encoder_tag_key(key: &str) -> bool {
    let key_lower = key.to_lowercase();
    ENCODER_TAG_KEY_MARKERS
        .iter()
        .any(|marker| key_lower.contains(marker))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct EncoderTagMatch {
    pub tag_key: String,
    pub tag_value: String,
    pub matched_pattern: String,
}

/// Reads whatever metadata is available immediately after probing (works for header-based
/// tags: FLAC/OGG Vorbis comments, ID3v2 — not APEv2/ID3v1 trailers, which would require
/// scanning the whole file first; out of scope for this first pass).
pub fn scan_for_lossy_encoder_traces(format: &mut Box<dyn FormatReader>) -> Vec<EncoderTagMatch> {
    let Some(revision) = format.metadata().current().cloned() else {
        return Vec::new();
    };

    let mut matches = Vec::new();
    for tag in &revision.media.tags {
        if !is_encoder_tag_key(&tag.raw.key) {
            continue;
        }
        let Some(value_str) = tag_value_as_string(&tag.raw.value) else {
            continue;
        };
        let value_lower = value_str.to_lowercase();
        for pattern in KNOWN_LOSSY_ENCODER_PATTERNS {
            if value_lower.contains(pattern) {
                matches.push(EncoderTagMatch {
                    tag_key: tag.raw.key.clone(),
                    tag_value: value_str.clone(),
                    matched_pattern: (*pattern).to_string(),
                });
            }
        }
    }
    matches
}

fn tag_value_as_string(value: &RawValue) -> Option<String> {
    match value {
        RawValue::String(s) => Some(s.to_string()),
        RawValue::StringList(list) => Some(list.join(" ")),
        _ => None,
    }
}
