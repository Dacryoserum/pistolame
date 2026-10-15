//! Decoding of the non-FLAC lossless containers, bit for bit: WAV and AIFF files are written
//! here from known integer PCM, decoded through the default registry, and compared sample by
//! sample (AGENTS.md: bit-perfect paths compare output to expected values exactly).

use std::path::{Path, PathBuf};

use pistolame_core::{AudioDecoder, DecoderRegistry};

const SAMPLE_RATE_HZ: u32 = 48_000;
const CHANNEL_COUNT: u16 = 2;
const FRAME_COUNT: usize = 10_000;

/// Deterministic full-range samples for `bits_per_sample`, including both extremes.
fn known_samples(bits_per_sample: u32) -> Vec<i32> {
    let maximum = (1_i64 << (bits_per_sample - 1)) - 1;
    let minimum = -(1_i64 << (bits_per_sample - 1));
    let span = maximum - minimum + 1;
    let mut state = 0x2545_f491_u64;
    let mut samples: Vec<i32> = (0..FRAME_COUNT * usize::from(CHANNEL_COUNT))
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (minimum + (state % span as u64) as i64) as i32
        })
        .collect();
    samples[0] = maximum as i32;
    samples[1] = minimum as i32;
    samples
}

/// A file written in the system temporary folder, deleted on drop.
struct TemporaryFile(PathBuf);

impl TemporaryFile {
    fn new(name: &str, bytes: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!("pistolame-{}-{name}", std::process::id()));
        std::fs::write(&path, bytes).expect("write test file");
        Self(path)
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// RIFF/WAVE with a plain PCM `fmt ` chunk (Microsoft WAVEFORMAT, little-endian samples).
fn wav_bytes(samples: &[i32], bits_per_sample: u16) -> Vec<u8> {
    let bytes_per_sample = usize::from(bits_per_sample / 8);
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes()[..bytes_per_sample].to_vec())
        .collect();
    let block_align = CHANNEL_COUNT * bits_per_sample / 8;
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&CHANNEL_COUNT.to_le_bytes());
    bytes.extend_from_slice(&SAMPLE_RATE_HZ.to_le_bytes());
    bytes.extend_from_slice(&(SAMPLE_RATE_HZ * u32::from(block_align)).to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&data);
    bytes
}

/// The sample rate as the 80-bit IEEE 754 extended float AIFF stores it (exact for integers).
fn extended_float(value: u32) -> [u8; 10] {
    let exponent = 31 - value.leading_zeros();
    let mantissa = u64::from(value) << (63 - exponent);
    let mut bytes = [0_u8; 10];
    bytes[..2].copy_from_slice(&(16_383 + exponent as u16).to_be_bytes());
    bytes[2..].copy_from_slice(&mantissa.to_be_bytes());
    bytes
}

/// AIFF (Apple Audio Interchange File Format 1.3): `COMM` then `SSND`, big-endian samples.
fn aiff_bytes(samples: &[i32], bits_per_sample: u16) -> Vec<u8> {
    let bytes_per_sample = usize::from(bits_per_sample / 8);
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|sample| sample.to_be_bytes()[4 - bytes_per_sample..].to_vec())
        .collect();
    let mut common = CHANNEL_COUNT.to_be_bytes().to_vec();
    common.extend_from_slice(&(FRAME_COUNT as u32).to_be_bytes());
    common.extend_from_slice(&bits_per_sample.to_be_bytes());
    common.extend_from_slice(&extended_float(SAMPLE_RATE_HZ));
    let mut sound = 0_u64.to_be_bytes().to_vec();
    sound.extend_from_slice(&data);
    let mut body = b"AIFF".to_vec();
    for (id, chunk) in [(b"COMM", common), (b"SSND", sound)] {
        body.extend_from_slice(id);
        body.extend_from_slice(&(chunk.len() as u32).to_be_bytes());
        body.extend_from_slice(&chunk);
    }
    let mut bytes = b"FORM".to_vec();
    bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&body);
    bytes
}

fn decode_all(path: &Path) -> (Box<dyn AudioDecoder>, Vec<i32>) {
    let mut decoder = DecoderRegistry::with_default_providers()
        .open(path)
        .expect("file must open");
    let mut samples = Vec::new();
    while let Some(block) = decoder.next_block().expect("file must decode") {
        samples.extend_from_slice(block.interleaved_samples);
    }
    (decoder, samples)
}

fn assert_decodes_exactly(name: &str, bytes: &[u8], expected: &[i32], bits_per_sample: u8) {
    let file = TemporaryFile::new(name, bytes);
    let (decoder, samples) = decode_all(&file.0);
    let info = decoder.stream_info();
    assert_eq!(info.format.bits_per_sample, bits_per_sample, "{name}");
    assert_eq!(info.format.sample_rate_hz, SAMPLE_RATE_HZ, "{name}");
    assert_eq!(info.format.channel_count, CHANNEL_COUNT as u8, "{name}");
    assert_eq!(info.total_frames, Some(FRAME_COUNT as u64), "{name}");
    assert!(
        !info.codec.is_lossy && !info.codec.is_decoded_as_float,
        "{name}"
    );
    assert_eq!(info.embedded_md5, None, "{name}");
    assert!(samples == expected, "{name}: decoded samples differ");
}

#[test]
fn wav_16_and_24_bit_decode_bit_for_bit() {
    for bits in [16_u16, 24] {
        let expected = known_samples(u32::from(bits));
        assert_decodes_exactly(
            &format!("{bits}.wav"),
            &wav_bytes(&expected, bits),
            &expected,
            bits as u8,
        );
    }
}

#[test]
fn aiff_16_and_24_bit_decode_bit_for_bit() {
    for bits in [16_u16, 24] {
        let expected = known_samples(u32::from(bits));
        assert_decodes_exactly(
            &format!("{bits}.aiff"),
            &aiff_bytes(&expected, bits),
            &expected,
            bits as u8,
        );
    }
}

#[test]
fn seeking_in_a_wav_lands_on_the_exact_frame() {
    let expected = known_samples(16);
    let file = TemporaryFile::new("seek.wav", &wav_bytes(&expected, 16));
    let mut decoder = DecoderRegistry::with_default_providers()
        .open(&file.0)
        .expect("file must open");
    let target_frame = 6_000_u64;
    let landed_frame = decoder.seek_to_frame(target_frame).expect("seek");
    assert!(landed_frame <= target_frame);
    let block = decoder
        .next_block()
        .expect("decode after seek")
        .expect("audio after seek");
    assert_eq!(block.first_frame_index, landed_frame);
    let channels = usize::from(CHANNEL_COUNT);
    let start = landed_frame as usize * channels;
    assert_eq!(
        block.interleaved_samples,
        &expected[start..start + block.interleaved_samples.len()]
    );
}
