//! Technical file metadata (container, codec, sample rate, duration, average bitrate).
//!
//! Tag/comment metadata (title, artist, ...) is out of scope for V0.1.

use std::path::Path;

use serde::Serialize;

use crate::decode::{DecodeStatus, DecodedAudio};

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct FileInfo {
    pub filename: String,
    pub container: String,
    pub codec: String,
    pub sample_rate_hz: u32,
    pub bit_depth: Option<u32>,
    pub channels: usize,
    pub duration_seconds: f64,
    pub nyquist_hz: u32,
    pub file_size_bytes: u64,
    pub sample_count: usize,
    /// Average bitrate only — variable bitrate reporting is a later iteration.
    pub bitrate_kbps: Option<f64>,
    /// `Some(true)`: the file's own embedded checksum matches what was decoded (FLAC
    /// STREAMINFO MD5). `Some(false)`: mismatch — the file is corrupt/truncated/edited.
    /// `None`: this codec has no such embedded checksum (MP3, AAC, WAV, ...).
    pub integrity_verified: Option<bool>,
    /// Whether the whole stream reached the analysis. Anything but `complete` means every
    /// measurement in this report describes a shorter, gap-ridden version of the track —
    /// see `decode.rs`.
    pub decode_status: DecodeStatus,
}

pub fn build_file_info(path: &Path, decoded: &DecodedAudio) -> Result<FileInfo, String> {
    let file_size_bytes = std::fs::metadata(path)
        .map_err(|e| format!("cannot read file metadata: {e}"))?
        .len();

    let sample_count = decoded
        .channel_samples
        .first()
        .map(|c| c.len())
        .unwrap_or(0);
    let duration_seconds = if decoded.sample_rate > 0 {
        sample_count as f64 / decoded.sample_rate as f64
    } else {
        0.0
    };

    let bitrate_kbps = if duration_seconds > 0.0 {
        Some((file_size_bytes as f64 * 8.0 / duration_seconds) / 1000.0)
    } else {
        None
    };

    Ok(FileInfo {
        filename: path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        container: decoded.container_short_name.clone(),
        codec: decoded.codec_short_name.clone(),
        sample_rate_hz: decoded.sample_rate,
        bit_depth: decoded.bits_per_sample,
        channels: decoded.channels,
        duration_seconds,
        nyquist_hz: decoded.sample_rate / 2,
        file_size_bytes,
        sample_count,
        bitrate_kbps,
        integrity_verified: decoded.integrity_verified,
        decode_status: decoded.decode_status,
    })
}
