//! FLAC fixtures generated at test time with `flacenc`, so no audio file is ever committed.
//!
//! Every fixture keeps its exact original PCM next to the encoded file: decoder tests compare
//! decoded samples to that PCM, never to another decoder's output. `flacenc` computes the
//! STREAMINFO MD5 with its own implementation of RFC 9639 section 8.2, which gives an
//! independent reference for our canonical MD5 layout.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use flacenc::bitsink::ByteSink;
use flacenc::component::BitRepr;
use flacenc::config;
use flacenc::error::Verify;
use flacenc::source::MemSource;

use super::high_sample_rate::{FLACENC_MAXIMUM_SAMPLE_RATE_HZ, write_stream_with_sample_rate};
use super::signal::{pseudo_random_interleaved_pcm, sine_interleaved_pcm};
use crate::format::{ChannelMask, StreamFormat};

/// Samples per FLAC block. 4096 is the libFLAC default for rates above 48 kHz and valid for all
/// fixture rates (RFC 9639 section 9.1.1 allows fixed sizes up to 65535).
const FIXTURE_BLOCK_SIZE_SAMPLES: usize = 4096;
/// Length of the `fLaC` marker that starts every FLAC file (RFC 9639 section 6).
const FLAC_MARKER_LENGTH_BYTES: usize = 4;
/// Length of the metadata block header (RFC 9639 section 8.1).
const METADATA_BLOCK_HEADER_LENGTH_BYTES: usize = 4;
/// Length of the STREAMINFO block body (RFC 9639 section 8.2).
const STREAMINFO_BODY_LENGTH_BYTES: usize = 34;
/// Length of the MD5 field, the last 16 bytes of STREAMINFO.
const MD5_LENGTH_BYTES: usize = 16;
/// Offset of the STREAMINFO body: right after the marker and the metadata block header.
const STREAMINFO_BODY_OFFSET_BYTES: usize =
    FLAC_MARKER_LENGTH_BYTES + METADATA_BLOCK_HEADER_LENGTH_BYTES;
/// Length of the minimum and of the maximum block size fields, which start STREAMINFO.
const BLOCK_SIZE_FIELD_LENGTH_BYTES: usize = 2;
/// Offset of the first audio frame: marker, STREAMINFO header and STREAMINFO body.
pub const FIRST_AUDIO_FRAME_OFFSET_BYTES: usize =
    FLAC_MARKER_LENGTH_BYTES + METADATA_BLOCK_HEADER_LENGTH_BYTES + STREAMINFO_BODY_LENGTH_BYTES;
/// Offset of the STREAMINFO MD5 field (26..42 for a file whose only metadata is STREAMINFO).
pub const STREAMINFO_MD5_OFFSET_BYTES: usize = FIRST_AUDIO_FRAME_OFFSET_BYTES - MD5_LENGTH_BYTES;
/// Mask flipping every bit of the corrupted byte, so the change can never be a no-op.
const CORRUPTION_XOR_MASK: u8 = 0xFF;

/// Process-wide counter that keeps generated file names unique across threads.
static NEXT_FIXTURE_FILE_NUMBER: AtomicU64 = AtomicU64::new(0);

/// Kind of signal a fixture is encoded from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixtureSignal {
    /// Seeded pseudo-random samples with forced extreme values (mostly verbatim subframes).
    PseudoRandom,
    /// Sine tone (exercises the encoder's LPC subframes).
    Sine,
}

/// Damage applied to the encoded bytes after encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixtureDamage {
    /// File left exactly as the encoder wrote it (valid STREAMINFO MD5).
    None,
    /// STREAMINFO MD5 zeroed: "MD5 not stored", the common case in real libraries.
    ZeroedStreamInfoMd5,
    /// One byte flipped in the middle of the audio frames (MD5 stays as written).
    CorruptedAudioFrameByte,
}

/// Everything needed to generate one fixture file.
#[derive(Clone, Copy, Debug)]
pub struct FixtureSpec {
    /// Short name, used in the generated file name.
    pub name: &'static str,
    /// Number of interleaved channels.
    pub channel_count: u8,
    /// Significant bits per sample.
    pub bits_per_sample: u8,
    /// Sample rate in hertz.
    pub sample_rate_hz: u32,
    /// Duration in whole seconds (frames = seconds * rate).
    pub duration_seconds: u32,
    /// Signal the PCM is built from.
    pub signal: FixtureSignal,
    /// Seed of the pseudo-random signal (ignored for sine).
    pub random_seed: u64,
    /// Damage applied after encoding.
    pub damage: FixtureDamage,
}

impl FixtureSpec {
    /// Number of sample frames in the fixture.
    pub fn frame_count(&self) -> usize {
        (self.duration_seconds as usize) * (self.sample_rate_hz as usize)
    }
}

/// Builds a pseudo-random, undamaged stereo spec; the constants below override what differs.
const fn stereo_random_spec(
    name: &'static str,
    bits_per_sample: u8,
    sample_rate_hz: u32,
    duration_seconds: u32,
    random_seed: u64,
) -> FixtureSpec {
    FixtureSpec {
        name,
        channel_count: 2,
        bits_per_sample,
        sample_rate_hz,
        duration_seconds,
        signal: FixtureSignal::PseudoRandom,
        random_seed,
        damage: FixtureDamage::None,
    }
}

/// 16-bit/44.1 kHz stereo, 3 s: the main 16-bit path.
pub const CD_STEREO: FixtureSpec = stereo_random_spec("cd_stereo", 16, 44_100, 3, 0x01);
/// Same format as [`CD_STEREO`] built from a sine, so the encoder emits LPC subframes.
pub const CD_STEREO_SINE: FixtureSpec = FixtureSpec {
    name: "cd_stereo_sine",
    signal: FixtureSignal::Sine,
    ..CD_STEREO
};
/// 24-bit/44.1 kHz stereo, 1 s (present in the user's library).
pub const HIRES_44_1: FixtureSpec = stereo_random_spec("hires_44_1", 24, 44_100, 1, 0x02);
/// 24-bit/48 kHz stereo, 1 s (present in the user's library).
pub const HIRES_48: FixtureSpec = stereo_random_spec("hires_48", 24, 48_000, 1, 0x03);
/// 24-bit/88.2 kHz stereo, 1 s (present in the user's library).
pub const HIRES_88_2: FixtureSpec = stereo_random_spec("hires_88_2", 24, 88_200, 1, 0x04);
/// 24-bit/96 kHz stereo, 3 s: the main 24-bit path.
pub const HIRES_96: FixtureSpec = stereo_random_spec("hires_96", 24, 96_000, 3, 0x05);
/// 24-bit/192 kHz stereo, 3 s: the highest rate in M1 (generated only, no real file).
pub const HIRES_192: FixtureSpec = stereo_random_spec("hires_192", 24, 192_000, 3, 0x06);
/// 20-bit/48 kHz stereo, 1 s: a depth that is not a byte multiple.
pub const ODD_20BIT: FixtureSpec = stereo_random_spec("odd_20bit", 20, 48_000, 1, 0x07);
/// 16-bit/44.1 kHz mono, 1 s: mono path and refusal path.
pub const MONO_16: FixtureSpec = FixtureSpec {
    name: "mono_16",
    channel_count: 1,
    ..stereo_random_spec("mono_16", 16, 44_100, 1, 0x08)
};
/// 16-bit/44.1 kHz stereo, 1 s, STREAMINFO MD5 zeroed after encoding.
pub const NO_MD5: FixtureSpec = FixtureSpec {
    name: "no_md5",
    damage: FixtureDamage::ZeroedStreamInfoMd5,
    ..stereo_random_spec("no_md5", 16, 44_100, 1, 0x09)
};
/// 16-bit/44.1 kHz stereo, 2 s, one byte flipped in the middle of the audio frames.
pub const CORRUPT: FixtureSpec = FixtureSpec {
    name: "corrupt",
    damage: FixtureDamage::CorruptedAudioFrameByte,
    ..stereo_random_spec("corrupt", 16, 44_100, 2, 0x0A)
};

/// Every fixture of the test plan (B10.1), plus the sine variant of the CD fixture.
pub const ALL_FIXTURES: [FixtureSpec; 11] = [
    CD_STEREO,
    CD_STEREO_SINE,
    HIRES_44_1,
    HIRES_48,
    HIRES_88_2,
    HIRES_96,
    HIRES_192,
    ODD_20BIT,
    MONO_16,
    NO_MD5,
    CORRUPT,
];

/// Why a fixture could not be generated.
#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    /// The channel count has no default FLAC layout.
    #[error("unsupported channel count {0}")]
    UnsupportedChannelCount(u8),
    /// `flacenc` rejected the configuration or the samples.
    #[error("FLAC encoding failed: {0}")]
    Encoding(String),
    /// The temporary file could not be written.
    #[error("cannot write fixture file {path}: {source}")]
    Io {
        /// File that could not be written.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
}

/// A generated fixture: the file on disk plus the exact PCM it was encoded from.
/// The file is deleted when the value is dropped.
#[derive(Debug)]
pub struct GeneratedFixture {
    /// Location of the FLAC file under the system temporary directory.
    pub path: PathBuf,
    /// Original interleaved samples (right-justified `i32`), the reference for decode tests.
    pub original_interleaved_pcm: Vec<i32>,
    /// Format a correct decoder must report for the file.
    pub format: StreamFormat,
    /// STREAMINFO MD5 as stored in the file (all zero when it was zeroed).
    pub streaminfo_md5: [u8; 16],
}

impl Drop for GeneratedFixture {
    fn drop(&mut self) {
        // Best effort: a leftover temporary file is harmless and must not mask a test result.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Generates the fixture described by `spec`.
///
/// # Panics
/// Panics with the [`FixtureError`] message when generation fails, which only happens on a bad
/// spec or an unwritable temporary directory. Tests have nothing useful to do with that error;
/// use [`try_generate_fixture`] to handle it.
pub fn generate_fixture(spec: &FixtureSpec) -> GeneratedFixture {
    try_generate_fixture(spec)
        .unwrap_or_else(|error| panic!("cannot generate fixture {}: {error}", spec.name))
}

/// Fallible form of [`generate_fixture`].
pub fn try_generate_fixture(spec: &FixtureSpec) -> Result<GeneratedFixture, FixtureError> {
    let channel_mask = ChannelMask::default_for_flac_channel_count(spec.channel_count)
        .ok_or(FixtureError::UnsupportedChannelCount(spec.channel_count))?;
    let original_interleaved_pcm = build_pcm(spec);
    let mut encoded_bytes = encode_flac(spec, &original_interleaved_pcm)?;
    exclude_last_block_from_minimum_block_size(&mut encoded_bytes);
    apply_damage(&mut encoded_bytes, spec.damage);
    let path = unique_temporary_path(spec.name);
    std::fs::write(&path, &encoded_bytes).map_err(|source| FixtureError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(GeneratedFixture {
        path,
        original_interleaved_pcm,
        format: StreamFormat {
            sample_rate_hz: spec.sample_rate_hz,
            bits_per_sample: spec.bits_per_sample,
            channel_count: spec.channel_count,
            channel_mask,
        },
        streaminfo_md5: read_streaminfo_md5(&encoded_bytes),
    })
}

/// Sets the STREAMINFO MD5 of an encoded FLAC byte buffer to zero ("MD5 not stored").
pub fn zero_streaminfo_md5(flac_bytes: &mut [u8]) {
    flac_bytes[STREAMINFO_MD5_OFFSET_BYTES..FIRST_AUDIO_FRAME_OFFSET_BYTES].fill(0);
}

/// Flips every bit of one byte in the middle of the audio frames, leaving headers and the stored
/// MD5 untouched so the damage is only detectable through the decoded samples.
pub fn corrupt_audio_frame_byte(flac_bytes: &mut [u8]) {
    let audio_length = flac_bytes.len() - FIRST_AUDIO_FRAME_OFFSET_BYTES;
    flac_bytes[FIRST_AUDIO_FRAME_OFFSET_BYTES + audio_length / 2] ^= CORRUPTION_XOR_MASK;
}

/// Applies [`zero_streaminfo_md5`] to a file on disk.
pub fn zero_streaminfo_md5_in_file(path: &Path) -> std::io::Result<()> {
    rewrite_file(path, zero_streaminfo_md5)
}

/// Applies [`corrupt_audio_frame_byte`] to a file on disk.
pub fn corrupt_audio_frame_byte_in_file(path: &Path) -> std::io::Result<()> {
    rewrite_file(path, corrupt_audio_frame_byte)
}

fn rewrite_file(path: &Path, modify: fn(&mut [u8])) -> std::io::Result<()> {
    let mut bytes = std::fs::read(path)?;
    modify(&mut bytes);
    std::fs::write(path, bytes)
}

fn build_pcm(spec: &FixtureSpec) -> Vec<i32> {
    match spec.signal {
        FixtureSignal::PseudoRandom => pseudo_random_interleaved_pcm(
            spec.random_seed,
            spec.frame_count(),
            spec.channel_count,
            spec.bits_per_sample,
        ),
        FixtureSignal::Sine => sine_interleaved_pcm(
            spec.frame_count(),
            spec.channel_count,
            spec.bits_per_sample,
            spec.sample_rate_hz,
        ),
    }
}

fn encode_flac(spec: &FixtureSpec, interleaved_pcm: &[i32]) -> Result<Vec<u8>, FixtureError> {
    let encoding_error = |error: &dyn std::fmt::Display| FixtureError::Encoding(error.to_string());
    let encoder_config = encoder_config_for(spec.signal)
        .into_verified()
        .map_err(|(_, error)| encoding_error(&error))?;
    let source = MemSource::from_samples(
        interleaved_pcm,
        usize::from(spec.channel_count),
        usize::from(spec.bits_per_sample),
        spec.sample_rate_hz.min(FLACENC_MAXIMUM_SAMPLE_RATE_HZ) as usize,
    );
    let stream =
        flacenc::encode_with_fixed_block_size(&encoder_config, source, FIXTURE_BLOCK_SIZE_SAMPLES)
            .map_err(|error| encoding_error(&error))?;
    if spec.sample_rate_hz > FLACENC_MAXIMUM_SAMPLE_RATE_HZ {
        return write_stream_with_sample_rate(&stream, spec.sample_rate_hz);
    }
    let mut sink = ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|error| encoding_error(&error))?;
    Ok(sink.as_slice().to_vec())
}

/// Encoder settings for a signal kind. Random samples cannot be predicted, so the encoder would
/// choose verbatim subframes anyway; skipping its predictor and stereo searches keeps the whole
/// fixture set fast in unoptimized builds without changing the output. The sine keeps the full
/// search so that LPC subframes are produced.
fn encoder_config_for(signal: FixtureSignal) -> config::Encoder {
    let mut encoder_config = config::Encoder::default();
    if signal == FixtureSignal::PseudoRandom {
        encoder_config.stereo_coding.use_leftside = false;
        encoder_config.stereo_coding.use_rightside = false;
        encoder_config.stereo_coding.use_midside = false;
        encoder_config.subframe_coding.use_constant = false;
        encoder_config.subframe_coding.use_fixed = false;
        encoder_config.subframe_coding.use_lpc = false;
    }
    encoder_config
}

/// `flacenc` stores the length of the short last block as the stream's minimum block size, but
/// RFC 9639 section 8.2 excludes the last block from that field, and decoders such as symphonia
/// use `minimum == maximum` to recognise a fixed-block-size stream. Copying the maximum over the
/// minimum restores the specified meaning (the last block is never longer than the maximum).
fn exclude_last_block_from_minimum_block_size(flac_bytes: &mut [u8]) {
    let minimum = STREAMINFO_BODY_OFFSET_BYTES;
    let maximum = minimum + BLOCK_SIZE_FIELD_LENGTH_BYTES;
    flac_bytes.copy_within(maximum..maximum + BLOCK_SIZE_FIELD_LENGTH_BYTES, minimum);
}

fn apply_damage(flac_bytes: &mut [u8], damage: FixtureDamage) {
    match damage {
        FixtureDamage::None => {}
        FixtureDamage::ZeroedStreamInfoMd5 => zero_streaminfo_md5(flac_bytes),
        FixtureDamage::CorruptedAudioFrameByte => corrupt_audio_frame_byte(flac_bytes),
    }
}

fn read_streaminfo_md5(flac_bytes: &[u8]) -> [u8; 16] {
    let mut md5 = [0_u8; MD5_LENGTH_BYTES];
    md5.copy_from_slice(&flac_bytes[STREAMINFO_MD5_OFFSET_BYTES..FIRST_AUDIO_FRAME_OFFSET_BYTES]);
    md5
}

fn unique_temporary_path(fixture_name: &str) -> PathBuf {
    let file_number = NEXT_FIXTURE_FILE_NUMBER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "pistolame-{fixture_name}-{}-{file_number}.flac",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use md5::{Digest, Md5};

    use super::*;

    /// Offset, inside the body, of the packed rate/channels/depth/total-samples field.
    const PACKED_FIELDS_OFFSET_BYTES: usize = 10;
    /// Width in bits of the sample rate field.
    const SAMPLE_RATE_FIELD_BITS: u32 = 20;
    /// Width in bits of the channel count minus one field.
    const CHANNELS_FIELD_BITS: u32 = 3;
    /// Width in bits of the bits-per-sample minus one field.
    const BITS_PER_SAMPLE_FIELD_BITS: u32 = 5;
    /// Width in bits of the total samples field.
    const TOTAL_SAMPLES_FIELD_BITS: u32 = 36;
    /// Budget for generating the whole B10.1 set in a debug build.
    const GENERATION_BUDGET: Duration = Duration::from_secs(2);

    struct StreamInfoFields {
        sample_rate_hz: u32,
        channel_count: u8,
        bits_per_sample: u8,
        total_samples_per_channel: u64,
        md5: [u8; 16],
    }

    fn mask(bit_count: u32) -> u64 {
        (1_u64 << bit_count) - 1
    }

    fn parse_streaminfo(flac_bytes: &[u8]) -> StreamInfoFields {
        let body = &flac_bytes[STREAMINFO_BODY_OFFSET_BYTES..FIRST_AUDIO_FRAME_OFFSET_BYTES];
        let mut packed_bytes = [0_u8; 8];
        packed_bytes
            .copy_from_slice(&body[PACKED_FIELDS_OFFSET_BYTES..PACKED_FIELDS_OFFSET_BYTES + 8]);
        let packed = u64::from_be_bytes(packed_bytes);
        let total_samples = packed & mask(TOTAL_SAMPLES_FIELD_BITS);
        let depth_shift = TOTAL_SAMPLES_FIELD_BITS;
        let channels_shift = depth_shift + BITS_PER_SAMPLE_FIELD_BITS;
        let rate_shift = channels_shift + CHANNELS_FIELD_BITS;
        let mut md5 = [0_u8; 16];
        md5.copy_from_slice(&body[STREAMINFO_BODY_LENGTH_BYTES - MD5_LENGTH_BYTES..]);
        StreamInfoFields {
            sample_rate_hz: ((packed >> rate_shift) & mask(SAMPLE_RATE_FIELD_BITS)) as u32,
            channel_count: (((packed >> channels_shift) & mask(CHANNELS_FIELD_BITS)) + 1) as u8,
            bits_per_sample: (((packed >> depth_shift) & mask(BITS_PER_SAMPLE_FIELD_BITS)) + 1)
                as u8,
            total_samples_per_channel: total_samples,
            md5,
        }
    }

    /// RFC 9639 section 8.2 layout: interleaved samples, each as little-endian signed integer
    /// over `ceil(bits / 8)` bytes (sign-extended). Private on purpose: `pcm.rs` is another task.
    fn independent_md5(interleaved_pcm: &[i32], bits_per_sample: u8) -> [u8; 16] {
        let bytes_per_sample = usize::from(bits_per_sample).div_ceil(8);
        let mut hasher = Md5::new();
        for sample in interleaved_pcm {
            hasher.update(&sample.to_le_bytes()[..bytes_per_sample]);
        }
        hasher.finalize().into()
    }

    #[test]
    fn every_fixture_is_a_valid_flac_with_matching_streaminfo_and_md5() {
        for spec in ALL_FIXTURES {
            let fixture = generate_fixture(&spec);
            let bytes = std::fs::read(&fixture.path).expect("fixture file must be readable");
            assert_eq!(&bytes[..4], b"fLaC", "{}", spec.name);
            let info = parse_streaminfo(&bytes);
            assert_eq!(info.sample_rate_hz, spec.sample_rate_hz, "{}", spec.name);
            assert_eq!(info.channel_count, spec.channel_count, "{}", spec.name);
            assert_eq!(info.bits_per_sample, spec.bits_per_sample, "{}", spec.name);
            assert_eq!(
                info.total_samples_per_channel as usize,
                spec.frame_count(),
                "{}",
                spec.name
            );
            assert_eq!(
                fixture.original_interleaved_pcm.len(),
                spec.frame_count() * usize::from(spec.channel_count)
            );
            assert_eq!(info.md5, fixture.streaminfo_md5, "{}", spec.name);
            let expected_md5 =
                independent_md5(&fixture.original_interleaved_pcm, spec.bits_per_sample);
            match spec.damage {
                FixtureDamage::ZeroedStreamInfoMd5 => assert_eq!(info.md5, [0; 16]),
                _ => assert_eq!(info.md5, expected_md5, "{}", spec.name),
            }
        }
    }

    #[test]
    fn fixture_format_describes_the_spec() {
        let fixture = generate_fixture(&MONO_16);
        assert_eq!(fixture.format.channel_count, 1);
        assert_eq!(fixture.format.channel_mask, ChannelMask(0x4));
        assert_eq!(fixture.format.sample_rate_hz, 44_100);
        assert_eq!(fixture.format.bits_per_sample, 16);
    }

    #[test]
    fn md5_zeroing_touches_only_the_md5_field() {
        let fixture = generate_fixture(&CD_STEREO);
        let original = std::fs::read(&fixture.path).expect("fixture file must be readable");
        let mut patched = original.clone();
        zero_streaminfo_md5(&mut patched);
        assert_eq!(STREAMINFO_MD5_OFFSET_BYTES, 26);
        assert_eq!(&patched[26..42], &[0; 16]);
        assert_eq!(&patched[..26], &original[..26]);
        assert_eq!(&patched[42..], &original[42..]);
    }

    #[test]
    fn audio_corruption_changes_exactly_one_byte_after_the_headers() {
        let fixture = generate_fixture(&CD_STEREO);
        let original = std::fs::read(&fixture.path).expect("fixture file must be readable");
        let mut corrupted = original.clone();
        corrupt_audio_frame_byte(&mut corrupted);
        let changed_offsets: Vec<usize> = (0..original.len())
            .filter(|&offset| original[offset] != corrupted[offset])
            .collect();
        assert_eq!(changed_offsets.len(), 1);
        assert!(changed_offsets[0] > FIRST_AUDIO_FRAME_OFFSET_BYTES);
        assert!(changed_offsets[0] < original.len());
    }

    #[test]
    fn file_helpers_patch_the_file_on_disk() {
        let fixture = generate_fixture(&CD_STEREO);
        let original = std::fs::read(&fixture.path).expect("fixture file must be readable");
        zero_streaminfo_md5_in_file(&fixture.path).expect("zeroing must succeed");
        let zeroed = std::fs::read(&fixture.path).expect("fixture file must be readable");
        assert_eq!(&zeroed[26..42], &[0; 16]);
        corrupt_audio_frame_byte_in_file(&fixture.path).expect("corruption must succeed");
        let corrupted = std::fs::read(&fixture.path).expect("fixture file must be readable");
        assert_ne!(zeroed, corrupted);
        assert_eq!(original.len(), corrupted.len());
    }

    #[test]
    fn damaged_fixtures_report_the_stored_md5() {
        assert_eq!(generate_fixture(&NO_MD5).streaminfo_md5, [0; 16]);
        let corrupt = generate_fixture(&CORRUPT);
        assert_eq!(
            corrupt.streaminfo_md5,
            independent_md5(&corrupt.original_interleaved_pcm, 16)
        );
    }

    #[test]
    fn dropping_a_fixture_removes_its_file() {
        let fixture = generate_fixture(&MONO_16);
        let path = fixture.path.clone();
        assert!(path.exists());
        drop(fixture);
        assert!(!path.exists());
    }

    /// Wall-clock budgets are flaky on shared CI runners (the Windows runner took over 2 s while
    /// other tests ran in parallel), so this check runs on demand: `cargo test -- --ignored`.
    #[test]
    #[ignore = "timing check, run locally with --ignored"]
    fn generating_the_whole_set_stays_within_the_time_budget() {
        let started_at = Instant::now();
        let fixtures: Vec<GeneratedFixture> = ALL_FIXTURES.iter().map(generate_fixture).collect();
        let elapsed = started_at.elapsed();
        assert_eq!(fixtures.len(), ALL_FIXTURES.len());
        assert!(elapsed < GENERATION_BUDGET, "took {elapsed:?}");
    }

    /// Decodes a file with symphonia into interleaved samples, right-justified to
    /// `bits_per_sample`, and returns them with the sample rate symphonia reports.
    /// Independent of any decoder code of this crate (that is another task).
    fn decode_with_symphonia(path: &Path, bits_per_sample: u8) -> (Vec<i32>, u32) {
        use symphonia::core::codecs::audio::AudioDecoderOptions;
        use symphonia::core::formats::{FormatOptions, TrackType, probe::Hint};
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;
        let file = Box::new(std::fs::File::open(path).expect("fixture must open"));
        let stream = MediaSourceStream::new(file, Default::default());
        let mut format = symphonia::default::get_probe()
            .probe(
                &Hint::new(),
                stream,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .unwrap_or_else(|error| panic!("{path:?} not recognised as FLAC: {error:?}"));
        let track = format.default_track(TrackType::Audio).expect("audio track");
        let codec_parameters = track
            .codec_params
            .as_ref()
            .and_then(|parameters| parameters.audio())
            .expect("audio parameters");
        let sample_rate_hz = codec_parameters.sample_rate.expect("sample rate");
        let mut decoder = symphonia::default::get_codecs()
            .make_audio_decoder(codec_parameters, &AudioDecoderOptions::default())
            .expect("FLAC decoder");
        let mut decoded = Vec::new();
        let mut chunk: Vec<i32> = Vec::new();
        while let Some(packet) = format.next_packet().expect("packet") {
            let audio = decoder.decode(&packet).expect("every frame must decode");
            chunk.resize(audio.samples_interleaved(), 0);
            audio.copy_to_slice_interleaved(&mut chunk);
            decoded.extend(
                chunk
                    .iter()
                    .map(|&sample| sample >> (32 - u32::from(bits_per_sample))),
            );
        }
        (decoded, sample_rate_hz)
    }

    #[test]
    fn undamaged_fixtures_decode_to_their_original_pcm() {
        for spec in ALL_FIXTURES {
            if spec.damage == FixtureDamage::CorruptedAudioFrameByte {
                continue;
            }
            let fixture = generate_fixture(&spec);
            let (decoded, sample_rate_hz) =
                decode_with_symphonia(&fixture.path, spec.bits_per_sample);
            assert_eq!(sample_rate_hz, spec.sample_rate_hz, "{}", spec.name);
            assert!(decoded == fixture.original_interleaved_pcm, "{}", spec.name);
        }
    }

    /// Cross-check with the reference decoder; run manually where `flac` is installed.
    #[test]
    #[ignore = "needs the reference `flac` command-line tool"]
    fn reference_flac_tool_accepts_the_valid_fixtures() {
        for spec in ALL_FIXTURES {
            if spec.damage != FixtureDamage::None {
                continue;
            }
            let fixture = generate_fixture(&spec);
            let status = std::process::Command::new("flac")
                .arg("-t")
                .arg("--silent")
                .arg(&fixture.path)
                .status()
                .expect("the `flac` tool must be on PATH");
            assert!(status.success(), "flac -t failed for {}", spec.name);
        }
    }
}
