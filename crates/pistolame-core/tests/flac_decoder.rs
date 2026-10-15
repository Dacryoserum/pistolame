//! Decoder tests against the generated FLAC fixtures (plan B10.4). Each fixture carries the exact
//! PCM it was encoded from, so every comparison is bit for bit.

#![cfg(feature = "test-support")]

use std::path::{Path, PathBuf};

use pistolame_core::testing::{
    ALL_FIXTURES, CD_STEREO, CORRUPT, GeneratedFixture, MONO_16, NO_MD5, generate_fixture,
};
use pistolame_core::{
    AudioDecoder, DecodeError, DecodedBlock, DecodedPcmHasher, DecodedSegmentEnd, DecoderRegistry,
    SourceStreamInfo, TrackId,
};

/// Block size of the generated fixtures, in frames.
const FIXTURE_BLOCK_SIZE_FRAMES: u64 = 4096;
/// Length of an ID3v1 tag.
const ID3V1_TAG_LENGTH_BYTES: usize = 128;
/// Length of the APEv2 footer, and of the item data written before it in these tests.
const APE_FOOTER_LENGTH_BYTES: usize = 32;
const APE_ITEM_DATA_LENGTH_BYTES: usize = 24;

/// Decodes every block until the end of the stream; returns the samples and the block count.
fn decode_to_end(decoder: &mut dyn AudioDecoder) -> (Vec<i32>, usize) {
    let mut samples = Vec::new();
    let mut block_count = 0;
    let mut expected_first_frame_index = 0_u64;
    let channel_count = usize::from(decoder.stream_info().format.channel_count);
    while let Some(block) = decoder.next_block().expect("fixture must decode") {
        assert_eq!(block.first_frame_index, expected_first_frame_index);
        expected_first_frame_index += (block.interleaved_samples.len() / channel_count) as u64;
        samples.extend_from_slice(block.interleaved_samples);
        block_count += 1;
    }
    (samples, block_count)
}

fn open_path(path: &Path) -> Box<dyn AudioDecoder> {
    DecoderRegistry::with_default_providers()
        .open(path)
        .expect("file must open")
}

fn open(fixture: &GeneratedFixture) -> Box<dyn AudioDecoder> {
    open_path(&fixture.path)
}

/// A modified copy of a fixture file, deleted on drop.
struct VariantFile(PathBuf);

impl VariantFile {
    fn new(fixture: &GeneratedFixture, modify: impl FnOnce(&mut Vec<u8>)) -> Self {
        let mut bytes = std::fs::read(&fixture.path).expect("read fixture");
        modify(&mut bytes);
        let path = fixture.path.with_extension("variant.flac");
        std::fs::write(&path, bytes).expect("write variant");
        Self(path)
    }
}

impl Drop for VariantFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn append_id3v1_tag(bytes: &mut Vec<u8>) {
    let mut tag = vec![0_u8; ID3V1_TAG_LENGTH_BYTES];
    tag[..3].copy_from_slice(b"TAG");
    bytes.extend_from_slice(&tag);
}

fn append_ape_v2_tag(bytes: &mut Vec<u8>) {
    bytes.extend_from_slice(&[0x41; APE_ITEM_DATA_LENGTH_BYTES]);
    let mut footer = [0_u8; APE_FOOTER_LENGTH_BYTES];
    footer[..8].copy_from_slice(b"APETAGEX");
    footer[8..12].copy_from_slice(&2000_u32.to_le_bytes());
    let tag_size = (APE_ITEM_DATA_LENGTH_BYTES + APE_FOOTER_LENGTH_BYTES) as u32;
    footer[12..16].copy_from_slice(&tag_size.to_le_bytes());
    bytes.extend_from_slice(&footer);
}

fn is_zero(md5: &[u8; 16]) -> bool {
    md5.iter().all(|&byte| byte == 0)
}

#[test]
fn every_fixture_decodes_to_its_original_pcm() {
    for spec in ALL_FIXTURES.iter().filter(|spec| spec.name != CORRUPT.name) {
        let fixture = generate_fixture(spec);
        let mut decoder = open(&fixture);
        let (samples, block_count) = decode_to_end(decoder.as_mut());
        assert!(block_count > 0, "{}", spec.name);
        assert_eq!(samples, fixture.original_interleaved_pcm, "{}", spec.name);
    }
}

#[test]
fn stream_info_matches_every_fixture() {
    for spec in ALL_FIXTURES {
        let fixture = generate_fixture(&spec);
        let decoder = open(&fixture);
        let info = decoder.stream_info();
        assert_eq!(info.format, fixture.format, "{}", spec.name);
        assert_eq!(
            info.total_frames,
            Some(spec.frame_count() as u64),
            "{}",
            spec.name
        );
        let expected_md5 = (!is_zero(&fixture.streaminfo_md5)).then_some(fixture.streaminfo_md5);
        assert_eq!(info.embedded_md5, expected_md5, "{}", spec.name);
    }
}

#[test]
fn fixture_without_md5_reports_none() {
    let fixture = generate_fixture(&NO_MD5);
    assert_eq!(open(&fixture).stream_info().embedded_md5, None);
}

#[test]
fn fixture_with_md5_reports_it() {
    let fixture = generate_fixture(&MONO_16);
    assert_eq!(
        open(&fixture).stream_info().embedded_md5,
        Some(fixture.streaminfo_md5)
    );
}

/// Shifts block indices so that `decode_to_end` can check them after a seek.
struct SeekedView<'decoder>(&'decoder mut dyn AudioDecoder, u64);

impl AudioDecoder for SeekedView<'_> {
    fn stream_info(&self) -> &SourceStreamInfo {
        self.0.stream_info()
    }

    fn next_block(&mut self) -> Result<Option<DecodedBlock<'_>>, DecodeError> {
        let offset = self.1;
        Ok(self.0.next_block()?.map(|mut block| {
            block.first_frame_index -= offset;
            block
        }))
    }

    fn seek_to_frame(&mut self, frame_index: u64) -> Result<u64, DecodeError> {
        self.0.seek_to_frame(frame_index)
    }
}

#[test]
fn seeking_then_dropping_extra_frames_gives_the_original_tail_on_every_fixture() {
    for spec in ALL_FIXTURES.iter().filter(|spec| spec.name != CORRUPT.name) {
        let fixture = generate_fixture(spec);
        let channel_count = usize::from(fixture.format.channel_count);
        let total_frames = (fixture.original_interleaved_pcm.len() / channel_count) as u64;
        let mut decoder = open(&fixture);
        for target_frame in [0, 1, 4_095, 4_096, total_frames / 2, total_frames - 1] {
            let actual_frame = decoder.seek_to_frame(target_frame).expect("seek");
            assert!(actual_frame <= target_frame, "{}", spec.name);
            let (samples, _) = decode_to_end(&mut SeekedView(decoder.as_mut(), actual_frame));
            let dropped_samples = (target_frame - actual_frame) as usize * channel_count;
            assert_eq!(
                samples[dropped_samples..],
                fixture.original_interleaved_pcm[target_frame as usize * channel_count..],
                "{} seek to {target_frame}",
                spec.name
            );
        }
    }
}

#[test]
fn seeking_to_the_start_after_the_end_decodes_everything_again() {
    let fixture = generate_fixture(&MONO_16);
    let mut decoder = open(&fixture);
    let (first_pass, _) = decode_to_end(decoder.as_mut());
    assert_eq!(first_pass, fixture.original_interleaved_pcm);
    assert_eq!(decoder.seek_to_frame(0).expect("seek"), 0);
    let (second_pass, _) = decode_to_end(decoder.as_mut());
    assert_eq!(second_pass, fixture.original_interleaved_pcm);
}

#[test]
fn seeking_to_or_past_the_end_is_not_an_error() {
    let fixture = generate_fixture(&MONO_16);
    let total_frames = fixture.original_interleaved_pcm.len() as u64;
    let mut decoder = open(&fixture);
    for target_frame in [total_frames, total_frames + 1, u64::MAX / 2] {
        assert_eq!(
            decoder.seek_to_frame(target_frame).expect("seek"),
            total_frames
        );
        assert!(matches!(decoder.next_block(), Ok(None)));
    }
    // The decoder is still usable afterwards.
    assert_eq!(decoder.seek_to_frame(0).expect("seek"), 0);
    let (samples, _) = decode_to_end(decoder.as_mut());
    assert_eq!(samples, fixture.original_interleaved_pcm);
}

#[test]
fn trailing_id3v1_tag_is_ignored() {
    let fixture = generate_fixture(&MONO_16);
    let variant = VariantFile::new(&fixture, append_id3v1_tag);
    let mut decoder = open_path(&variant.0);
    let (samples, _) = decode_to_end(decoder.as_mut());
    assert_eq!(samples, fixture.original_interleaved_pcm);
}

#[test]
fn trailing_ape_v2_footer_is_ignored() {
    let fixture = generate_fixture(&MONO_16);
    let variant = VariantFile::new(&fixture, append_ape_v2_tag);
    let mut decoder = open_path(&variant.0);
    let (samples, _) = decode_to_end(decoder.as_mut());
    assert_eq!(samples, fixture.original_interleaved_pcm);
}

#[test]
fn trailing_ape_v2_footer_followed_by_id3v1_is_ignored() {
    let fixture = generate_fixture(&MONO_16);
    let variant = VariantFile::new(&fixture, |bytes| {
        append_ape_v2_tag(bytes);
        append_id3v1_tag(bytes);
    });
    let mut decoder = open_path(&variant.0);
    let (samples, _) = decode_to_end(decoder.as_mut());
    assert_eq!(samples, fixture.original_interleaved_pcm);
    // Seeking works through the trimmed source too.
    assert_eq!(decoder.seek_to_frame(0).expect("seek"), 0);
    let (again, _) = decode_to_end(decoder.as_mut());
    assert_eq!(again, fixture.original_interleaved_pcm);
}

#[test]
fn corrupt_fixture_stops_after_the_good_part_with_a_typed_error() {
    let fixture = generate_fixture(&CORRUPT);
    let channel_count = usize::from(fixture.format.channel_count);
    let total_frames = (fixture.original_interleaved_pcm.len() / channel_count) as u64;
    let mut decoder = open(&fixture);
    let mut samples = Vec::new();
    let error = loop {
        match decoder.next_block() {
            Ok(Some(block)) => samples.extend_from_slice(block.interleaved_samples),
            Ok(None) => panic!("the corrupt fixture must not end cleanly"),
            Err(error) => break error,
        }
    };
    let DecodeError::CorruptData {
        near_frame_index, ..
    } = error
    else {
        panic!("expected CorruptData, got {error:?}");
    };
    let delivered_frames = (samples.len() / channel_count) as u64;
    assert_eq!(samples, fixture.original_interleaved_pcm[..samples.len()]);
    assert_eq!(delivered_frames, near_frame_index);
    // The damaged byte is in the middle of the audio: its frame starts less than one block
    // before the middle, and everything before that frame must have been delivered.
    let damaged_frame_start_upper_bound = total_frames / 2;
    assert!(
        delivered_frames + 2 * FIXTURE_BLOCK_SIZE_FRAMES >= damaged_frame_start_upper_bound,
        "only {delivered_frames} frames delivered"
    );
    assert!(delivered_frames <= damaged_frame_start_upper_bound);
    assert!(matches!(decoder.next_block(), Ok(None)));
}

#[test]
fn truncated_file_with_a_known_total_reports_corrupt_data_after_the_prefix() {
    let fixture = generate_fixture(&CD_STEREO);
    let channel_count = usize::from(fixture.format.channel_count);
    let variant = VariantFile::new(&fixture, |bytes| bytes.truncate(bytes.len() / 2));
    let mut decoder = open_path(&variant.0);
    let mut samples = Vec::new();
    let error = loop {
        match decoder.next_block() {
            Ok(Some(block)) => samples.extend_from_slice(block.interleaved_samples),
            Ok(None) => panic!("a truncated file must not end cleanly"),
            Err(error) => break error,
        }
    };
    let DecodeError::CorruptData {
        near_frame_index, ..
    } = error
    else {
        panic!("expected CorruptData, got {error:?}");
    };
    assert!(!samples.is_empty());
    assert!(samples.len() < fixture.original_interleaved_pcm.len());
    assert_eq!(samples, fixture.original_interleaved_pcm[..samples.len()]);
    assert_eq!((samples.len() / channel_count) as u64, near_frame_index);
}

#[test]
fn registry_rejects_other_file_types_and_missing_files() {
    let registry = DecoderRegistry::with_default_providers();
    assert!(matches!(
        registry.open(Path::new("track.opus")),
        Err(DecodeError::UnsupportedFileType)
    ));
    assert!(matches!(
        registry.open(Path::new("missing-track.flac")),
        Err(DecodeError::Io { .. })
    ));
}

#[test]
fn file_that_is_not_flac_is_unsupported() {
    let path = std::env::temp_dir().join(format!("pistolame-not-flac-{}.flac", std::process::id()));
    std::fs::write(&path, b"this is definitely not a flac stream, just text").expect("write");
    let result = DecoderRegistry::with_default_providers().open(&path);
    std::fs::remove_file(&path).expect("cleanup");
    assert!(matches!(result, Err(DecodeError::UnsupportedFileType)));
}

/// RFC 9639 section 8.2 layout, written independently of `pcm.rs`: interleaved samples, each a
/// little-endian signed integer over `ceil(bits / 8)` bytes.
fn canonical_md5_of(interleaved_pcm: &[i32], bits_per_sample: u8) -> [u8; 16] {
    use md5::{Digest, Md5};
    let bytes_per_sample = usize::from(bits_per_sample).div_ceil(8);
    let mut hasher = Md5::new();
    for sample in interleaved_pcm {
        hasher.update(&sample.to_le_bytes()[..bytes_per_sample]);
    }
    hasher.finalize().into()
}

/// Decodes `fixture` through the decoder-side hasher (every block staged, then committed), as
/// the decode thread does; returns the report and the samples hashed.
fn hash_fixture(fixture: &GeneratedFixture) -> (pistolame_core::DecodedSegmentReport, Vec<i32>) {
    let mut decoder = open(fixture);
    let mut hasher = DecodedPcmHasher::new(TrackId(1), fixture.format);
    let mut hashed_samples = Vec::new();
    hasher.begin_segment(1, 0);
    let ended_by = loop {
        match decoder.next_block() {
            Ok(Some(block)) => {
                hasher
                    .stage_block(block.first_frame_index, block.interleaved_samples)
                    .expect("decoded blocks are contiguous whole frames in range");
                hasher.commit_staged_block();
                hashed_samples.extend_from_slice(block.interleaved_samples);
            }
            Ok(None) => break DecodedSegmentEnd::EndOfStream,
            Err(_) => break DecodedSegmentEnd::DecodeError,
        }
    };
    (hasher.finish_segment(ended_by), hashed_samples)
}

#[test]
fn decoder_side_hasher_matches_the_streaminfo_md5_of_every_fixture() {
    for spec in ALL_FIXTURES.iter().filter(|spec| spec.name != CORRUPT.name) {
        let fixture = generate_fixture(spec);
        let (report, _) = hash_fixture(&fixture);
        let original_frame_count =
            fixture.original_interleaved_pcm.len() as u64 / u64::from(fixture.format.channel_count);
        assert_eq!(
            report.ended_by,
            DecodedSegmentEnd::EndOfStream,
            "{}",
            spec.name
        );
        assert_eq!(report.frame_count, original_frame_count, "{}", spec.name);
        if spec.name == NO_MD5.name {
            assert!(is_zero(&fixture.streaminfo_md5));
            assert_eq!(
                report.md5,
                canonical_md5_of(
                    &fixture.original_interleaved_pcm,
                    fixture.format.bits_per_sample
                ),
                "{}",
                spec.name
            );
        } else {
            assert_eq!(report.md5, fixture.streaminfo_md5, "{}", spec.name);
        }
    }
}

#[test]
fn decoder_side_hasher_on_the_corrupt_fixture_hashes_only_the_good_part() {
    let fixture = generate_fixture(&CORRUPT);
    let (report, hashed_samples) = hash_fixture(&fixture);
    assert_eq!(report.ended_by, DecodedSegmentEnd::DecodeError);
    assert_ne!(report.md5, fixture.streaminfo_md5);
    assert_eq!(
        report.md5,
        canonical_md5_of(&hashed_samples, fixture.format.bits_per_sample)
    );
}
