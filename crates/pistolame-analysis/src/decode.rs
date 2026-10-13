//! Decodes an audio file into per-channel f32 sample buffers using `symphonia`.
//!
//! `symphonia` is the sole decoder for analysis purposes (see AGENTS.md "Décisions
//! actées") — `claxon` is deliberately not used here.

use std::fs::File;
use std::path::Path;

use symphonia::core::audio::Channels;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use serde::Serialize;

use crate::tags::{self, EncoderTagMatch};

/// Whether the decoder got through the whole stream, and how it did not.
///
/// Every measurement downstream describes the samples that survived, so a report built on a
/// partial decode is a report about a different, shorter file than the one the user picked.
/// That has to travel with the numbers rather than be inferred from a count buried in
/// `FileInfo`: `transcode_detect` withholds its verdict on an incomplete decode, and the UI
/// says which measurements to distrust.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct DecodeStatus {
    /// False when any audio was skipped or the stream ended before the container said it
    /// would. The one field a caller should branch on.
    pub complete: bool,
    /// Packets the decoder rejected and this module skipped over. Skipping is the right
    /// recovery — a corrupt tail should not fail the whole analysis — but doing it without
    /// telling anyone would hide exactly the defect an integrity check exists to surface.
    pub skipped_packets: usize,
    /// The demuxer asked to be restarted mid-file (chained OGG streams, a format change)
    /// and decoding stopped there instead of continuing into the next segment. Everything
    /// past that point is missing from the analysis, silently, unless this says so.
    pub stopped_early: bool,
    /// The channels did not come out the same length.
    ///
    /// Every downstream stage handles that differently — duration and sample count read the
    /// first channel, the spectrum and the stereo image take the shortest, playback takes the
    /// shortest — so a report on such a file quietly describes several different lengths at
    /// once. It only happens on a damaged or truncated stream, so it is recorded as one.
    pub channels_unequal: bool,
}

impl DecodeStatus {
    fn resolve(skipped_packets: usize, stopped_early: bool, channels: &[Vec<f32>]) -> Self {
        let lengths = || channels.iter().map(Vec::len);
        let channels_unequal = lengths()
            .min()
            .zip(lengths().max())
            .is_some_and(|(a, b)| a != b);
        Self {
            complete: skipped_packets == 0 && !stopped_early && !channels_unequal,
            skipped_packets,
            stopped_early,
            channels_unequal,
        }
    }
}

/// Decoded PCM and the channel positions required to interpret it correctly.
pub struct DecodedAudio {
    pub sample_rate: u32,
    pub channels: usize,
    /// Canonical speaker positions (or the decoder's explicit unknown/discrete layout).
    pub channel_layout: Channels,
    pub codec_short_name: String,
    pub container_short_name: String,
    pub bits_per_sample: Option<u32>,
    /// One buffer per channel, full track length, samples normalized to [-1.0, 1.0].
    pub channel_samples: Vec<Vec<f32>>,
    /// `Some(true)`/`Some(false)`: the decoder checked the file's own embedded checksum
    /// (e.g. FLAC's STREAMINFO MD5) against what was actually decoded. `None`: this
    /// codec/container has no such embedded checksum to check (MP3, AAC, WAV, ...) — not
    /// an error, just nothing to verify.
    pub integrity_verified: Option<bool>,
    /// Container tag values matching a known lossy-encoder signature — see tags.rs
    /// module docs on why this is weaker evidence than the spectral indicators.
    pub encoder_tag_matches: Vec<EncoderTagMatch>,
    /// Whether the whole stream made it through the decoder — see [`DecodeStatus`].
    pub decode_status: DecodeStatus,
}

pub fn decode_file(path: &Path) -> Result<DecodedAudio, String> {
    let file = File::open(path).map_err(|e| format!("cannot open file: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("unsupported or corrupt file: {e}"))?;

    let container_short_name = format.format_info().short_name.to_string();
    let encoder_tag_matches = tags::scan_for_lossy_encoder_traces(&mut format);

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| "no decodable audio track found".to_string())?
        .clone();

    let audio_params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| "track has no audio codec parameters".to_string())?
        .clone();

    // `verify(true)`: ask the decoder to check the file's own embedded checksum (e.g.
    // FLAC's STREAMINFO MD5 of the decoded PCM) if it has one — see
    // `decoder.finalize()` below. Free correctness signal; symphonia computes it as part
    // of normal decoding when supported, no separate pass needed.
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&audio_params, &AudioDecoderOptions::default().verify(true))
        .map_err(|e| format!("unsupported codec: {e}"))?;

    let codec_short_name = decoder.codec_info().short_name.to_string();
    let bits_per_sample = audio_params.bits_per_sample;
    let track_id = track.id;

    let mut sample_rate: u32 = 0;
    let mut channels: usize = 0;
    let mut channel_layout = Channels::None;
    let mut channel_samples: Vec<Vec<f32>> = Vec::new();
    let mut interleaved_buf: Vec<f32> = Vec::new();
    let mut skipped_packets: usize = 0;
    let mut stopped_early = false;

    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            // Advanced feature (chained OGG streams etc.) — treat as end of what we can
            // decode rather than restarting the whole pipeline. Recorded, not swallowed:
            // whatever follows the reset point is missing from every measurement below, and
            // a verdict built on the first segment of a chained file would be a verdict
            // about a fragment presented as one about the file.
            Err(SymphoniaError::ResetRequired) => {
                stopped_early = true;
                break;
            }
            Err(e) => return Err(format!("demux error: {e}")),
        };

        if packet.track_id != track_id {
            continue;
        }

        let audio_buf = match decoder.decode(&packet) {
            Ok(buf) => buf,
            Err(SymphoniaError::IoError(_)) | Err(SymphoniaError::DecodeError(_)) => {
                skipped_packets += 1;
                continue;
            }
            Err(e) => return Err(format!("decode error: {e}")),
        };

        if channel_samples.is_empty() {
            channels = audio_buf.spec().channels().count();
            if channels == 0 {
                return Err("decoded audio reports zero channels".to_string());
            }
            sample_rate = audio_buf.spec().rate();
            channel_layout = audio_buf.spec().channels().clone();
            channel_samples = vec![Vec::new(); channels];
        } else if sample_rate != audio_buf.spec().rate()
            || channel_layout != *audio_buf.spec().channels()
        {
            // Concatenating different rates/layouts makes every subsequent sample index
            // describe the wrong time or speaker. Keep only the first, explicitly partial,
            // segment just as for a demuxer ResetRequired.
            stopped_early = true;
            break;
        }

        interleaved_buf.resize(audio_buf.samples_interleaved(), 0.0f32);
        audio_buf.copy_to_slice_interleaved(&mut interleaved_buf);

        for (i, sample) in interleaved_buf.iter().enumerate() {
            if !sample.is_finite() {
                return Err("decoded audio contains a non-finite sample".to_string());
            }
            channel_samples[i % channels].push(*sample);
        }
    }

    if sample_rate == 0 || channel_samples.iter().all(|c| c.is_empty()) {
        return Err("no audio frames could be decoded from this file".to_string());
    }

    let integrity_verified = decoder.finalize().verify_ok;
    let decode_status = DecodeStatus::resolve(skipped_packets, stopped_early, &channel_samples);

    Ok(DecodedAudio {
        sample_rate,
        channels,
        channel_layout,
        codec_short_name,
        container_short_name,
        bits_per_sample,
        channel_samples,
        integrity_verified,
        encoder_tag_matches,
        decode_status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Channels that came out different lengths are a damaged decode. Nothing reported this
    /// before, while every downstream stage silently picked a different length to work from.
    #[test]
    fn unequal_channel_lengths_mark_the_decode_incomplete() {
        let ragged = DecodeStatus::resolve(0, false, &[vec![0.0; 100], vec![0.0; 99]]);
        assert!(ragged.channels_unequal);
        assert!(!ragged.complete);

        let even = DecodeStatus::resolve(0, false, &[vec![0.0; 100], vec![0.0; 100]]);
        assert!(!even.channels_unequal);
        assert!(even.complete);

        // Mono cannot be ragged, and an empty set has nothing to compare.
        assert!(!DecodeStatus::resolve(0, false, &[vec![0.0; 100]]).channels_unequal);
        assert!(!DecodeStatus::resolve(0, false, &[]).channels_unequal);
    }
}
