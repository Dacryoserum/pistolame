//! Serialization of a stream whose real sample rate exceeds what `flacenc` 0.5.1 accepts.
//!
//! `flacenc` refuses to build a STREAMINFO above 96 kHz, although the FLAC format allows up to
//! 1 048 575 Hz (RFC 9639 section 8.2). Sample rate never influences how samples are coded, so
//! the stream is encoded with a nominal rate of 96 kHz and only the labels are rewritten here:
//! the 20-bit rate field of STREAMINFO and the rate of every frame header. The frame headers are
//! rebuilt through `flacenc`'s own types, so their CRCs are computed by `flacenc`, not by us.

use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, Frame, FrameHeader, FrameOffset, Stream};

use super::flac_fixtures::{FIRST_AUDIO_FRAME_OFFSET_BYTES, FixtureError};

/// Highest sample rate `flacenc` 0.5.1 accepts (its STREAMINFO verification).
pub(super) const FLACENC_MAXIMUM_SAMPLE_RATE_HZ: u32 = 96_000;
/// Offset of the packed rate/channels/depth/total-samples field in the file (marker, block
/// header, then 10 bytes of block and frame sizes; RFC 9639 section 8.2).
const PACKED_FIELDS_OFFSET_BYTES: usize = 4 + 4 + 10;
/// The sample rate occupies the 20 high bits of the packed field.
const SAMPLE_RATE_FIELD_BITS: u32 = 20;
/// Number of bits in the packed field's first bytes that hold the rate field plus 4 spare bits.
const RATE_FIELD_LEADING_BYTES: usize = 3;
/// Bits of the third leading byte that do not belong to the rate field.
const SPARE_BITS_IN_THIRD_BYTE: u32 =
    (RATE_FIELD_LEADING_BYTES as u32) * 8 - SAMPLE_RATE_FIELD_BITS;

/// Writes `stream`, encoded at the nominal rate, with `target_sample_rate_hz` as its rate.
pub(super) fn write_stream_with_sample_rate(
    stream: &Stream,
    target_sample_rate_hz: u32,
) -> Result<Vec<u8>, FixtureError> {
    let encoding_error = |error: &dyn std::fmt::Display| FixtureError::Encoding(error.to_string());
    let mut nominal_sink = ByteSink::new();
    stream
        .write(&mut nominal_sink)
        .map_err(|error| encoding_error(&error))?;
    let mut stream_bytes = nominal_sink.as_slice()[..FIRST_AUDIO_FRAME_OFFSET_BYTES].to_vec();
    patch_streaminfo_sample_rate(&mut stream_bytes, target_sample_rate_hz);

    let bits_per_sample = stream.stream_info().bits_per_sample();
    for frame_number in 0..stream.frame_count() {
        let Some(frame) = stream.frame(frame_number) else {
            continue;
        };
        let relabelled =
            relabel_frame(frame, frame_number, bits_per_sample, target_sample_rate_hz)?;
        let mut frame_sink = ByteSink::new();
        relabelled
            .write(&mut frame_sink)
            .map_err(|error| encoding_error(&error))?;
        stream_bytes.extend_from_slice(frame_sink.as_slice());
    }
    Ok(stream_bytes)
}

fn patch_streaminfo_sample_rate(stream_bytes: &mut [u8], sample_rate_hz: u32) {
    debug_assert!(stream_bytes.len() >= FIRST_AUDIO_FRAME_OFFSET_BYTES);
    let rate_field = sample_rate_hz << SPARE_BITS_IN_THIRD_BYTE;
    let leading = &mut stream_bytes
        [PACKED_FIELDS_OFFSET_BYTES..PACKED_FIELDS_OFFSET_BYTES + RATE_FIELD_LEADING_BYTES];
    let preserved_spare_bits =
        leading[RATE_FIELD_LEADING_BYTES - 1] & ((1 << SPARE_BITS_IN_THIRD_BYTE) - 1);
    let rate_bytes = rate_field.to_be_bytes();
    leading[0] = rate_bytes[1];
    leading[1] = rate_bytes[2];
    leading[2] = rate_bytes[3] | preserved_spare_bits;
}

fn relabel_frame(
    frame: &Frame,
    frame_number: usize,
    bits_per_sample: usize,
    sample_rate_hz: u32,
) -> Result<Frame, FixtureError> {
    let encoding_error = |error: &dyn std::fmt::Display| FixtureError::Encoding(error.to_string());
    let (original_header, subframes) = frame.clone().into_parts();
    let header = FrameHeader::new(
        original_header.block_size(),
        original_header.channel_assignment().clone(),
        bits_per_sample,
        sample_rate_hz as usize,
        FrameOffset::Frame(frame_number as u32),
    )
    .map_err(|error| encoding_error(&error))?;
    Frame::new(header, subframes.into_iter()).map_err(|error| encoding_error(&error))
}
