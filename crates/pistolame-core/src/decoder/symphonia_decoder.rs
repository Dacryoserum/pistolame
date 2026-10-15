//! Decoding of every container and codec symphonia 0.6.1 reads (FLAC, ALAC, WAV/AIFF PCM,
//! MP3, AAC, Vorbis), into right-justified integer PCM.
//!
//! Policy (walking-skeleton plan B3.3, B4, B8): integer codecs stay integer end to end, with no
//! dither and no gain; symphonia's own `verify` option stays off (the pipeline hashes the
//! decoded samples itself), and on damaged data the decoder returns the blocks before the
//! damage, then one typed error, never silently skipping audio.
//!
//! Codecs whose decoder outputs floating point (MP3, AAC, Vorbis, float PCM) are rounded to
//! 24-bit integers, without dither. [`SourceCodec::is_decoded_as_float`] says so, and the
//! signal path is then never reported as bit-perfect.
//!
//! Known limits, by design in M1:
//! - The channel mask is always the default one of RFC 9639 section 9.1.3 for the channel
//!   count (the `WAVEFORMATEXTENSIBLE_CHANNEL_MASK` Vorbis comment and WAV masks are not read).
//! - symphonia cannot open files whose STREAMINFO minimum block size is not conformant; they
//!   are reported as [`DecodeError::CorruptData`] at open.
//! - A mid-stream change of sample rate, bit depth or channel count never reaches the decoder:
//!   symphonia's frame parser rejects frame headers that disagree with STREAMINFO, so for FLAC
//!   it surfaces as [`DecodeError::CorruptData`], not [`DecodeError::FormatChangedMidStream`].
//! - Trailing ID3v1 and APEv2 tags are hidden from symphonia in FLAC and MP3 files (see
//!   `trailing_tags`); other containers delimit their own audio.
//! - Containers of lossy codecs often estimate their length, so the end of such a stream is
//!   not checked against it ([`SourceStreamInfo::is_total_frames_exact`]).

use std::path::{Path, PathBuf};

use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::well_known::CODEC_ID_FLAC;
use symphonia::core::codecs::audio::{
    AudioCodecParameters, AudioDecoder as SymphoniaAudioDecoder, AudioDecoderOptions,
    VerificationCheck,
};
use symphonia::core::errors::{Error as SymphoniaError, SeekErrorKind};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::packet::Packet;
use symphonia::core::units::Timestamp;

use super::trailing_tags::TrailingTagTrimmedFile;
use super::{
    AudioDecoder, DecodeError, DecodedBlock, DecoderProvider, SourceCodec, SourceStreamInfo,
    declared_bits_per_sample, is_supported_audio_file,
};
use crate::format::{ChannelMask, StreamFormat};

/// Extensions of the files whose trailing ID3v1 / APEv2 tags are hidden from symphonia: the
/// two formats where taggers append them and the parser would read them as damaged audio.
const TRAILING_TAG_FILE_EXTENSIONS: &[&str] = &["flac", "mp3"];
/// Highest bit depth the pipeline supports in M1 (plan B4, known limit 1).
const MAXIMUM_SUPPORTED_BITS_PER_SAMPLE: u32 = 24;
/// Lowest bit depth accepted (RFC 9639's minimum; integer PCM starts at 8).
const MINIMUM_SUPPORTED_BITS_PER_SAMPLE: u32 = 4;
/// Precision of the integers float decoder output is rounded to: the widest depth the pipeline
/// carries, so the rounding error stays far below any lossy codec's own noise.
const FLOAT_DECODE_OUTPUT_BITS_PER_SAMPLE: u8 = 24;
/// Full scale of a 24-bit sample (2^23): float 1.0 maps to it.
const FLOAT_DECODE_FULL_SCALE: f32 = 8_388_608.0;
/// Width of the `i32` word symphonia left-justifies every sample in.
const SYMPHONIA_SAMPLE_BITS: u32 = i32::BITS;
/// Offset of the maximum block size (big-endian u16, in frames) in the STREAMINFO body
/// (RFC 9639 section 8.2).
const STREAMINFO_MAXIMUM_BLOCK_SIZE_OFFSET: usize = 2;
/// Block size assumed for buffer pre-sizing when STREAMINFO cannot be read.
const FALLBACK_BLOCK_SIZE_FRAMES: usize = 4096;

/// Opens every supported audio file (recognized by extension, see
/// [`super::SUPPORTED_AUDIO_FILE_EXTENSIONS`]) with symphonia.
#[derive(Clone, Copy, Debug, Default)]
pub struct SymphoniaDecoderProvider;

impl DecoderProvider for SymphoniaDecoderProvider {
    fn can_open(&self, path: &Path) -> bool {
        is_supported_audio_file(path)
    }

    fn open(&self, path: &Path) -> Result<Box<dyn AudioDecoder>, DecodeError> {
        Ok(Box::new(SymphoniaDecoder::open(path)?))
    }
}

/// Where the next packet must start, in symphonia's absolute frame timestamps.
///
/// The first packet read anchors the stream start, so a stream whose first frame is not
/// numbered 0 is still checked for gaps; every frame index handed out is relative to that
/// start. A seek issued before any packet was read assumes the stream starts at frame 0.
#[derive(Debug, Default)]
struct FrameCursor {
    stream_start: Option<u64>,
    expected_next_frame: Option<u64>,
}

/// Audio is missing between two packets (a frame was dropped by the parser).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FrameGap {
    expected_first_frame_index: u64,
    found_timestamp: i64,
}

impl FrameCursor {
    fn stream_start(&self) -> u64 {
        self.stream_start.unwrap_or(0)
    }

    /// Relative index of the frame the next packet must start at.
    fn expected_first_frame_index(&self) -> u64 {
        self.expected_next_frame
            .map_or(0, |expected| expected.saturating_sub(self.stream_start()))
    }

    /// Checks a packet's timestamp; returns its first frame index relative to the stream start.
    fn accept_packet(&mut self, timestamp: i64) -> Result<u64, FrameGap> {
        let absolute = u64::try_from(timestamp).ok();
        if let Some(absolute) = absolute {
            self.stream_start.get_or_insert(absolute);
        }
        let gap = FrameGap {
            expected_first_frame_index: self.expected_first_frame_index(),
            found_timestamp: timestamp,
        };
        let absolute = absolute.ok_or(gap)?;
        if self
            .expected_next_frame
            .is_some_and(|expected| expected != absolute)
        {
            return Err(gap);
        }
        absolute.checked_sub(self.stream_start()).ok_or(gap)
    }

    /// Records that the packet starting at `first_frame_index` held `frame_count` frames.
    fn complete_block(&mut self, first_frame_index: u64, frame_count: u64) {
        self.expected_next_frame = Some(self.stream_start() + first_frame_index + frame_count);
    }

    /// Absolute timestamp of a relative frame index.
    fn to_absolute(&self, frame_index: u64) -> Option<u64> {
        self.stream_start().checked_add(frame_index)
    }

    /// Makes `absolute` the next expected frame (after a seek).
    fn reposition(&mut self, absolute: u64) {
        self.stream_start.get_or_insert(0);
        self.expected_next_frame = Some(absolute);
    }
}

/// A probed file: everything needed to read packets from the start.
struct OpenedStream {
    format_reader: Box<dyn FormatReader>,
    track_id: u32,
    total_frames: Option<u64>,
    codec_parameters: AudioCodecParameters,
}

/// Probes `path` (hiding trailing tags where they occur) and selects its audio track.
fn open_stream(path: &Path) -> Result<OpenedStream, DecodeError> {
    let io_error = |source| DecodeError::Io {
        path: path.to_path_buf(),
        source,
    };
    let file = std::fs::File::open(path).map_err(io_error)?;
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    let media_source = if TRAILING_TAG_FILE_EXTENSIONS
        .iter()
        .any(|tagged| extension.eq_ignore_ascii_case(tagged))
    {
        let source = TrailingTagTrimmedFile::new(file).map_err(io_error)?;
        MediaSourceStream::new(Box::new(source), Default::default())
    } else {
        MediaSourceStream::new(Box::new(file), Default::default())
    };
    let mut hint = Hint::new();
    hint.with_extension(extension);
    let format_reader = symphonia::default::get_probe()
        .probe(
            &hint,
            media_source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| classify_open_error(path, error))?;
    let track = format_reader
        .default_track(TrackType::Audio)
        .ok_or(DecodeError::UnsupportedFileType)?;
    let Some(CodecParameters::Audio(codec_parameters)) = track.codec_params.clone() else {
        return Err(DecodeError::UnsupportedFileType);
    };
    // Frame indices are packet timestamps: a container counting in other units (Matroska's
    // milliseconds) cannot be followed frame by frame.
    let is_counted_in_frames = match (track.time_base, codec_parameters.sample_rate) {
        (Some(time_base), Some(sample_rate_hz)) => {
            time_base.numer.get() == 1 && time_base.denom.get() == sample_rate_hz
        }
        (None, _) => true,
        (Some(_), None) => false,
    };
    if !is_counted_in_frames {
        return Err(DecodeError::UnsupportedFileType);
    }
    let (track_id, total_frames) = (track.id, track.num_frames);
    Ok(OpenedStream {
        format_reader,
        track_id,
        total_frames,
        codec_parameters,
    })
}

/// Decoder for one audio file. Produces right-justified `i32` samples in a reused buffer.
pub struct SymphoniaDecoder {
    path: PathBuf,
    format_reader: Box<dyn FormatReader>,
    audio_decoder: Box<dyn SymphoniaAudioDecoder>,
    track_id: u32,
    stream_info: SourceStreamInfo,
    /// Bits symphonia shifted every integer sample left by; undone before the samples leave.
    left_shift: u32,
    /// Interleaved samples of the last decoded block.
    sample_buffer: Vec<i32>,
    /// Interleaved float output of the last packet, before rounding (float codecs only).
    float_buffer: Vec<f32>,
    frame_cursor: FrameCursor,
    /// Set after the end of the stream or after an error was returned: no more blocks.
    finished: bool,
}

impl SymphoniaDecoder {
    /// Opens `path` and reads the stream information.
    ///
    /// # Errors
    /// [`DecodeError::Io`] when the file cannot be read, [`DecodeError::UnsupportedFileType`]
    /// when no supported container or codec is found, [`DecodeError::UnsupportedBitDepth`]
    /// above 24 bits, [`DecodeError::CorruptData`] when the headers are unusable.
    pub fn open(path: &Path) -> Result<Self, DecodeError> {
        let opened = open_stream(path)?;
        let codec = SourceCodec::from_codec_id(opened.codec_parameters.codec);
        let (format, left_shift) = stream_format_from(&opened.codec_parameters, &codec)?;
        let embedded_md5 = match opened.codec_parameters.verification_check {
            Some(VerificationCheck::Md5(md5)) => Some(md5),
            _ => None,
        };
        // `verify` stays off: the pipeline computes its own digest of the samples it transmits.
        let audio_decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&opened.codec_parameters, &AudioDecoderOptions::default())
            .map_err(|error| classify_open_error(path, error))?;
        let buffer_capacity_samples =
            maximum_block_size_frames(&opened.codec_parameters) * usize::from(format.channel_count);
        Ok(Self {
            path: path.to_path_buf(),
            format_reader: opened.format_reader,
            audio_decoder,
            track_id: opened.track_id,
            stream_info: SourceStreamInfo {
                format,
                total_frames: opened.total_frames.filter(|&frames| frames > 0),
                is_total_frames_exact: !codec.is_lossy,
                embedded_md5,
                codec,
            },
            left_shift,
            sample_buffer: Vec::with_capacity(buffer_capacity_samples),
            float_buffer: Vec::new(),
            frame_cursor: FrameCursor::default(),
            finished: false,
        })
    }

    /// Decodes the next packet of the track into `sample_buffer`. Returns the index of its
    /// first frame, or `None` at the end of the stream.
    ///
    /// The end is only checked against STREAMINFO's total when that total is known: a file
    /// truncated at a frame boundary whose total is 0 (unknown) ends with `Ok(None)` and
    /// cannot be told from a complete one.
    fn decode_next_packet(&mut self) -> Result<Option<u64>, DecodeError> {
        loop {
            let Some(first_frame_index) = self.decode_one_packet()? else {
                return self.check_stream_is_complete();
            };
            // Lossy decoders can trim a whole packet away (encoder delay); skip it.
            if !self.sample_buffer.is_empty() {
                return Ok(Some(first_frame_index));
            }
        }
    }

    /// Reads and decodes one packet; `None` when there are no packets left.
    fn decode_one_packet(&mut self) -> Result<Option<u64>, DecodeError> {
        let Some(packet) = self.read_next_track_packet()? else {
            return Ok(None);
        };
        // The parser drops frames whose checksum fails and resynchronizes on the next one; the
        // jump in the timestamp is the only trace, and it must not be played over.
        // The first audible frame comes after the encoder delay: an MP3 or Vorbis stream starts
        // with a packet whose timestamp is minus that delay.
        let trim_start = i64::try_from(packet.trim_start.get()).unwrap_or(i64::MAX);
        let first_frame_index = self
            .frame_cursor
            .accept_packet(packet.pts.get().saturating_add(trim_start))
            .map_err(|gap| {
                corrupt_data(
                    gap.expected_first_frame_index,
                    format!(
                        "audio is missing: expected a frame starting at {}, found timestamp {}",
                        gap.expected_first_frame_index, gap.found_timestamp
                    ),
                )
            })?;
        self.decode_into_sample_buffer(&packet, first_frame_index)?;
        let channel_count = usize::from(self.stream_info.format.channel_count);
        // A lossy decoder primes itself on the first packet after a seek and outputs nothing
        // for it; the stream still moves on by the packet's duration. A lossless decoder never
        // does, so there the missing audio surfaces as a gap at the next packet.
        let frame_count = match (self.sample_buffer.len() / channel_count) as u64 {
            0 if self.stream_info.codec.is_lossy => packet.dur.get(),
            decoded_frame_count => decoded_frame_count,
        };
        self.frame_cursor
            .complete_block(first_frame_index, frame_count);
        Ok(Some(first_frame_index))
    }

    fn read_next_track_packet(&mut self) -> Result<Option<Packet>, DecodeError> {
        loop {
            match self.format_reader.next_packet() {
                Ok(Some(packet)) if packet.track_id == self.track_id => return Ok(Some(packet)),
                Ok(Some(_other_track)) => {}
                Ok(None) => return Ok(None),
                Err(error) => {
                    return Err(classify_demux_error(
                        &self.path,
                        error,
                        self.frame_cursor.expected_first_frame_index(),
                    ));
                }
            }
        }
    }

    /// Decodes `packet` and stores its right-justified interleaved samples in `sample_buffer`.
    fn decode_into_sample_buffer(
        &mut self,
        packet: &Packet,
        first_frame_index: u64,
    ) -> Result<(), DecodeError> {
        let decoded = self
            .audio_decoder
            .decode(packet)
            .map_err(|error| classify_decode_error(error, first_frame_index))?;
        let channel_count = usize::from(self.stream_info.format.channel_count);
        // MP3 and Vorbis frames carry their own rate and layout; playing a frame that changed
        // either at the opened format would be at the wrong speed or on the wrong speakers.
        if decoded.spec().channels().count() != channel_count
            || decoded.spec().rate() != self.stream_info.format.sample_rate_hz
        {
            return Err(DecodeError::FormatChangedMidStream);
        }
        let valid_frames = valid_frame_range(decoded.frames(), packet);
        let valid_samples = valid_frames.start * channel_count..valid_frames.end * channel_count;
        if self.stream_info.codec.is_decoded_as_float {
            decoded.copy_to_vec_interleaved(&mut self.float_buffer);
            round_float_samples_to_integers(
                &self.float_buffer[valid_samples],
                &mut self.sample_buffer,
            );
            return Ok(());
        }
        if matches!(
            decoded,
            GenericAudioBufferRef::F32(_) | GenericAudioBufferRef::F64(_)
        ) {
            return Err(corrupt_data(
                first_frame_index,
                "an integer codec produced floating-point samples".to_owned(),
            ));
        }
        decoded.copy_to_vec_interleaved(&mut self.sample_buffer);
        self.sample_buffer.truncate(valid_samples.end);
        self.sample_buffer.drain(..valid_samples.start);
        restore_right_justified(&mut self.sample_buffer, self.left_shift).map_err(|sample_index| {
            corrupt_data(
                first_frame_index + (sample_index / channel_count) as u64,
                "samples are not left-justified as symphonia is expected to produce them"
                    .to_owned(),
            )
        })
    }

    /// Called at the end of the packets: a stream shorter than STREAMINFO announces was cut.
    fn check_stream_is_complete(&self) -> Result<Option<u64>, DecodeError> {
        let delivered_frames = self.frame_cursor.expected_first_frame_index();
        match self.stream_info.total_frames {
            Some(total_frames)
                if self.stream_info.is_total_frames_exact && delivered_frames < total_frames =>
            {
                Err(corrupt_data(
                    delivered_frames,
                    format!(
                        "stream ends at frame {delivered_frames} but the container announces {total_frames}"
                    ),
                ))
            }
            _ => Ok(None),
        }
    }

    /// Parks the decoder at the end of the stream and returns `position_frame_index`.
    fn finish_at(&mut self, position_frame_index: u64) -> u64 {
        self.finished = true;
        self.sample_buffer.clear();
        position_frame_index
    }

    /// Seeks a freshly opened reader and checks where it landed. `None` means the position is
    /// past the end of the stream.
    fn seek_fresh_reader(&mut self, frame_index: u64) -> Result<Option<u64>, DecodeError> {
        let out_of_range = || corrupt_data(frame_index, "seek position is out of range".to_owned());
        let absolute_target = self
            .frame_cursor
            .to_absolute(frame_index)
            .and_then(|absolute| i64::try_from(absolute).ok())
            .ok_or_else(out_of_range)?;
        let seek_result = self.format_reader.seek(
            SeekMode::Accurate,
            SeekTo::Timestamp {
                ts: Timestamp::new(absolute_target),
                track_id: self.track_id,
            },
        );
        let seeked_to = match seek_result {
            Ok(seeked_to) => seeked_to,
            Err(SymphoniaError::SeekError(SeekErrorKind::OutOfRange)) => return Ok(None),
            Err(error) => return Err(classify_seek_error(&self.path, error, frame_index)),
        };
        self.audio_decoder.reset();
        let actual_absolute = u64::try_from(seeked_to.actual_ts.get()).map_err(|_| {
            corrupt_data(
                frame_index,
                "the container reported a negative position".to_owned(),
            )
        })?;
        self.frame_cursor.reposition(actual_absolute);
        let actual_frame_index = self.frame_cursor.expected_first_frame_index();
        if actual_frame_index > frame_index {
            return Err(corrupt_data(
                frame_index,
                format!("seek landed at frame {actual_frame_index}, after the requested frame"),
            ));
        }
        Ok(Some(actual_frame_index))
    }
}

impl AudioDecoder for SymphoniaDecoder {
    fn stream_info(&self) -> &SourceStreamInfo {
        &self.stream_info
    }

    /// After the end of the stream, or after an error has been returned once, every further
    /// call returns `Ok(None)`: the caller already knows how the stream ended. A seek rearms it.
    ///
    /// `first_frame_index` is relative to the first frame of the stream.
    fn next_block(&mut self) -> Result<Option<DecodedBlock<'_>>, DecodeError> {
        if self.finished {
            return Ok(None);
        }
        match self.decode_next_packet() {
            Ok(Some(first_frame_index)) => Ok(Some(DecodedBlock {
                first_frame_index,
                interleaved_samples: &self.sample_buffer,
            })),
            Ok(None) => {
                self.finished = true;
                Ok(None)
            }
            Err(error) => {
                self.finished = true;
                self.sample_buffer.clear();
                Err(error)
            }
        }
    }

    /// Seeking to or past the end is not an error: the decoder is parked at the end, the
    /// next block is `Ok(None)` and the returned position is the total (or the requested frame
    /// when the total is unknown).
    ///
    /// The file is reopened on every seek so that no parser state from before the seek can
    /// reject the frames that follow it.
    fn seek_to_frame(&mut self, frame_index: u64) -> Result<u64, DecodeError> {
        self.finished = true;
        if let Some(total_frames) = self.stream_info.total_frames
            && frame_index >= total_frames
        {
            return Ok(self.finish_at(total_frames));
        }
        let reopened = open_stream(&self.path)?;
        self.format_reader = reopened.format_reader;
        self.track_id = reopened.track_id;
        match self.seek_fresh_reader(frame_index) {
            Ok(Some(actual_frame_index)) => {
                self.finished = false;
                Ok(actual_frame_index)
            }
            Ok(None) => Ok(self.finish_at(frame_index)),
            Err(error) => Err(error),
        }
    }
}

/// Largest packet in frames, for sizing the sample buffer: STREAMINFO's maximum block size
/// for FLAC, the container's figure otherwise.
fn maximum_block_size_frames(parameters: &AudioCodecParameters) -> usize {
    let flac_maximum_block_size = || {
        parameters
            .extra_data
            .as_deref()
            .filter(|_| parameters.codec == CODEC_ID_FLAC)
            .and_then(|streaminfo| {
                streaminfo.get(
                    STREAMINFO_MAXIMUM_BLOCK_SIZE_OFFSET..STREAMINFO_MAXIMUM_BLOCK_SIZE_OFFSET + 2,
                )
            })
            .map(|bytes| usize::from(u16::from_be_bytes([bytes[0], bytes[1]])))
    };
    flac_maximum_block_size()
        .or_else(|| {
            parameters
                .max_frames_per_packet
                .and_then(|frames| usize::try_from(frames).ok())
        })
        .filter(|&frames| frames > 0)
        .unwrap_or(FALLBACK_BLOCK_SIZE_FRAMES)
}

/// Builds the stream format and the left shift symphonia applied from the codec parameters.
/// Float decoders get the 24-bit format their output is rounded to, and no shift.
fn stream_format_from(
    parameters: &AudioCodecParameters,
    codec: &SourceCodec,
) -> Result<(StreamFormat, u32), DecodeError> {
    let bits_per_sample = if codec.is_decoded_as_float {
        u32::from(FLOAT_DECODE_OUTPUT_BITS_PER_SAMPLE)
    } else {
        declared_bits_per_sample(parameters)
            .ok_or_else(|| corrupt_data(0, "the stream has no bit depth".to_owned()))?
    };
    if !(MINIMUM_SUPPORTED_BITS_PER_SAMPLE..=MAXIMUM_SUPPORTED_BITS_PER_SAMPLE)
        .contains(&bits_per_sample)
    {
        return Err(DecodeError::UnsupportedBitDepth {
            bits_per_sample: u8::try_from(bits_per_sample).unwrap_or(u8::MAX),
        });
    }
    let sample_rate_hz = parameters
        .sample_rate
        .filter(|&rate| rate > 0)
        .ok_or_else(|| corrupt_data(0, "the stream has no sample rate".to_owned()))?;
    let channel_count = parameters
        .channels
        .as_ref()
        .and_then(|channels| u8::try_from(channels.count()).ok())
        .ok_or_else(|| corrupt_data(0, "the stream has an invalid channel count".to_owned()))?;
    let channel_mask = ChannelMask::default_for_flac_channel_count(channel_count)
        .ok_or_else(|| corrupt_data(0, format!("{channel_count} channels are not supported")))?;
    let format = StreamFormat {
        sample_rate_hz,
        // Range-checked above: 4..=24.
        bits_per_sample: bits_per_sample as u8,
        channel_count,
        channel_mask,
    };
    let left_shift = if codec.is_decoded_as_float {
        0
    } else {
        SYMPHONIA_SAMPLE_BITS - bits_per_sample
    };
    Ok((format, left_shift))
}

/// Frames of a decoded packet that are audio, as opposed to encoder delay or padding.
///
/// symphonia's MP3 and Vorbis decoders already drop the frames the container marks for
/// trimming; a decoder that does not returns exactly `dur + trim_start + trim_end` frames, and
/// only then are the trims applied here.
fn valid_frame_range(decoded_frames: usize, packet: &Packet) -> std::ops::Range<usize> {
    let to_frames = |duration: symphonia::core::units::Duration| {
        usize::try_from(duration.get()).unwrap_or(usize::MAX)
    };
    let (trim_start, trim_end) = (to_frames(packet.trim_start), to_frames(packet.trim_end));
    let is_untrimmed = trim_start.saturating_add(trim_end) > 0
        && to_frames(packet.dur)
            .checked_add(trim_start)
            .and_then(|frames| frames.checked_add(trim_end))
            == Some(decoded_frames);
    if is_untrimmed {
        trim_start..decoded_frames - trim_end
    } else {
        0..decoded_frames
    }
}

/// Rounds float samples (full scale ±1.0) to right-justified 24-bit integers, without dither,
/// clamping the overshoot lossy decoders produce on loud masters. NaN becomes 0.
fn round_float_samples_to_integers(float_samples: &[f32], integer_samples: &mut Vec<i32>) {
    const MINIMUM: f32 = -FLOAT_DECODE_FULL_SCALE;
    const MAXIMUM: f32 = FLOAT_DECODE_FULL_SCALE - 1.0;
    integer_samples.clear();
    integer_samples.extend(float_samples.iter().map(|&sample| {
        (sample * FLOAT_DECODE_FULL_SCALE)
            .round()
            .clamp(MINIMUM, MAXIMUM) as i32
    }));
}

/// Undoes symphonia's left shift in place with an arithmetic right shift, which restores the
/// exact right-justified value because the shifted-in low bits are zero.
///
/// This guards against symphonia no longer left-justifying samples as 0.6.1 does (or a frame
/// header whose bit depth disagrees with STREAMINFO): if any sample has a non-zero bit below the
/// stream bit depth, the shift would silently destroy data, so nothing is modified and the
/// index of the first such sample is returned.
fn restore_right_justified(samples: &mut [i32], left_shift: u32) -> Result<(), usize> {
    let low_bits_mask = (1_i32 << left_shift) - 1;
    if let Some(offending_index) = samples
        .iter()
        .position(|&sample| sample & low_bits_mask != 0)
    {
        return Err(offending_index);
    }
    for sample in samples.iter_mut() {
        *sample >>= left_shift;
    }
    Ok(())
}

fn corrupt_data(near_frame_index: u64, detail: String) -> DecodeError {
    DecodeError::CorruptData {
        near_frame_index,
        detail,
    }
}

/// Whether the error is a read error (as opposed to a file cut short, which is damage).
fn is_read_failure(source: &std::io::Error) -> bool {
    source.kind() != std::io::ErrorKind::UnexpectedEof
}

/// Failure while probing or building the decoder: not FLAC, unreadable, or damaged headers.
fn classify_open_error(path: &Path, error: SymphoniaError) -> DecodeError {
    match error {
        SymphoniaError::Unsupported(_) => DecodeError::UnsupportedFileType,
        SymphoniaError::IoError(source) if is_read_failure(&source) => DecodeError::Io {
            path: path.to_path_buf(),
            source,
        },
        other => corrupt_data(0, other.to_string()),
    }
}

/// Failure while reading the next packet from the container.
///
/// - `ResetRequired` (the stream changed under us): `FormatChangedMidStream`.
/// - A read error that is not an end of file: `Io`.
/// - Everything else, including a file cut short: `CorruptData`.
fn classify_demux_error(path: &Path, error: SymphoniaError, near_frame_index: u64) -> DecodeError {
    match error {
        SymphoniaError::ResetRequired => DecodeError::FormatChangedMidStream,
        SymphoniaError::IoError(source) if is_read_failure(&source) => DecodeError::Io {
            path: path.to_path_buf(),
            source,
        },
        other => corrupt_data(near_frame_index, other.to_string()),
    }
}

/// Failure while decoding one packet. Nyquist skips such packets; playback must not, so this
/// ends the stream after the last good block.
///
/// - `IoError`, `DecodeError` and the decoder's `Unsupported` (for example a negative LPC
///   shift, which RFC 9639 forbids): `CorruptData`.
/// - `ResetRequired`: `FormatChangedMidStream`.
/// - Any other error is fatal and also reported as `CorruptData`, with its message.
fn classify_decode_error(error: SymphoniaError, near_frame_index: u64) -> DecodeError {
    match error {
        SymphoniaError::ResetRequired => DecodeError::FormatChangedMidStream,
        other => corrupt_data(near_frame_index, other.to_string()),
    }
}

/// Failure while seeking: a read error stays `Io`, anything else is `CorruptData` near the
/// requested frame. (A position past the end is handled earlier, as an end-of-stream result.)
fn classify_seek_error(
    path: &Path,
    error: SymphoniaError,
    requested_frame_index: u64,
) -> DecodeError {
    match error {
        SymphoniaError::IoError(source) if is_read_failure(&source) => DecodeError::Io {
            path: path.to_path_buf(),
            source,
        },
        other => corrupt_data(requested_frame_index, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphonia::core::units::Duration;

    #[test]
    fn restores_right_justified_values_when_low_bits_are_zero() {
        let mut samples = [0x1234_5600, -0x0000_0100, 0, i32::MIN];
        assert_eq!(restore_right_justified(&mut samples, 8), Ok(()));
        assert_eq!(samples, [0x12_3456, -1, 0, i32::MIN >> 8]);
    }

    #[test]
    fn rejects_samples_with_non_zero_low_bits_and_leaves_them_untouched() {
        let mut samples = [0x100, 0x200, 0x201, 0x300];
        assert_eq!(restore_right_justified(&mut samples, 8), Err(2));
        assert_eq!(samples, [0x100, 0x200, 0x201, 0x300]);
    }

    #[test]
    fn provider_recognizes_supported_extensions_case_insensitively() {
        let provider = SymphoniaDecoderProvider;
        assert!(provider.can_open(Path::new("a.flac")));
        assert!(provider.can_open(Path::new("A.FLAC")));
        assert!(provider.can_open(Path::new("a.m4a")));
        assert!(provider.can_open(Path::new("a.mp3")));
        assert!(!provider.can_open(Path::new("a.txt")));
        assert!(!provider.can_open(Path::new("flac")));
    }

    fn packet_with_trims(valid_frames: u64, trim_start: u64, trim_end: u64) -> Packet {
        let mut packet = Packet::new(0, Timestamp::new(0), Duration::from(valid_frames), [0_u8]);
        packet.trim_start = Duration::from(trim_start);
        packet.trim_end = Duration::from(trim_end);
        packet
    }

    #[test]
    fn untrimmed_decoder_output_has_its_delay_and_padding_removed() {
        let packet = packet_with_trims(1_000, 100, 24);
        assert_eq!(valid_frame_range(1_124, &packet), 100..1_100);
    }

    #[test]
    fn output_the_decoder_already_trimmed_is_kept_whole() {
        let packet = packet_with_trims(1_000, 100, 24);
        assert_eq!(valid_frame_range(1_000, &packet), 0..1_000);
        assert_eq!(valid_frame_range(0, &packet_with_trims(0, 0, 0)), 0..0);
    }

    #[test]
    fn float_samples_round_to_24_bit_integers_and_clamp() {
        let mut integers = vec![7];
        round_float_samples_to_integers(
            &[
                0.0,
                0.5,
                -0.5,
                1.0,
                -1.0,
                1.5,
                -1.5,
                1.0 / 8_388_608.0,
                f32::NAN,
            ],
            &mut integers,
        );
        assert_eq!(
            integers,
            [
                0, 4_194_304, -4_194_304, 8_388_607, -8_388_608, 8_388_607, -8_388_608, 1, 0
            ]
        );
    }

    #[test]
    fn symphonia_errors_are_classified() {
        let path = Path::new("x.flac");
        assert!(matches!(
            classify_demux_error(path, SymphoniaError::ResetRequired, 5),
            DecodeError::FormatChangedMidStream
        ));
        assert!(matches!(
            classify_decode_error(SymphoniaError::DecodeError("bad"), 7),
            DecodeError::CorruptData {
                near_frame_index: 7,
                ..
            }
        ));
        assert!(matches!(
            classify_decode_error(SymphoniaError::Unsupported("lpc"), 7),
            DecodeError::CorruptData { .. }
        ));
        let end_of_file = std::io::Error::from(std::io::ErrorKind::UnexpectedEof);
        assert!(matches!(
            classify_demux_error(path, SymphoniaError::IoError(end_of_file), 9),
            DecodeError::CorruptData {
                near_frame_index: 9,
                ..
            }
        ));
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(matches!(
            classify_demux_error(path, SymphoniaError::IoError(denied), 9),
            DecodeError::Io { .. }
        ));
    }

    #[test]
    fn frame_cursor_anchors_on_the_first_packet_and_reports_indices_from_the_stream_start() {
        let mut cursor = FrameCursor::default();
        assert_eq!(cursor.accept_packet(1_000), Ok(0));
        cursor.complete_block(0, 4_096);
        assert_eq!(cursor.accept_packet(5_096), Ok(4_096));
        cursor.complete_block(4_096, 4_096);
        assert_eq!(cursor.expected_first_frame_index(), 8_192);
        assert_eq!(cursor.to_absolute(8_192), Some(9_192));
    }

    #[test]
    fn frame_cursor_reports_a_gap_with_the_expected_relative_index() {
        let mut cursor = FrameCursor::default();
        assert_eq!(cursor.accept_packet(1_000), Ok(0));
        cursor.complete_block(0, 4_096);
        assert_eq!(
            cursor.accept_packet(9_192),
            Err(FrameGap {
                expected_first_frame_index: 4_096,
                found_timestamp: 9_192
            })
        );
        assert!(cursor.accept_packet(-1).is_err());
    }

    #[test]
    fn frame_cursor_after_a_seek_keeps_absolute_positions() {
        let mut cursor = FrameCursor::default();
        cursor.reposition(8_192);
        assert_eq!(cursor.expected_first_frame_index(), 8_192);
        assert_eq!(cursor.accept_packet(8_192), Ok(8_192));
        assert!(cursor.accept_packet(0).is_err());
    }
}
