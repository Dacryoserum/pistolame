//! Decoder contract: turns an encoded file into integer PCM. Implementations plug in through
//! [`DecoderProvider`]; today symphonia covers every supported format.

use crate::format::StreamFormat;

mod codec;
mod registry;
mod symphonia_decoder;
mod trailing_tags;

pub use codec::{
    SUPPORTED_AUDIO_FILE_EXTENSIONS, SourceCodec, declared_bits_per_sample, is_supported_audio_file,
};
pub use registry::DecoderRegistry;
pub use symphonia_decoder::{SymphoniaDecoder, SymphoniaDecoderProvider};

/// Facts about the source the rest of the pipeline relies on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceStreamInfo {
    /// Format of the decoded samples.
    pub format: StreamFormat,
    /// Total frames if the container states it (FLAC STREAMINFO "total samples", 0 = unknown).
    pub total_frames: Option<u64>,
    /// Whether `total_frames` is exact. Containers of lossy codecs often estimate it (an MP3
    /// without a Xing header), so a stream ending elsewhere is not treated as damaged.
    pub is_total_frames_exact: bool,
    /// MD5 of the decoded audio as stored by the encoder; `None` if absent or all zeros
    /// (RFC 9639 section 8.2: "A value of 0 signifies that the value is not known").
    pub embedded_md5: Option<[u8; 16]>,
    /// The codec, and whether its samples are exact integers (see [`SourceCodec`]).
    pub codec: SourceCodec,
}

/// A block of decoded samples, borrowed from the decoder's internal buffer.
pub struct DecodedBlock<'decoder> {
    /// Index of the first frame of this block within the track.
    pub first_frame_index: u64,
    /// Interleaved, right-justified samples (`frame_count * channel_count` values).
    pub interleaved_samples: &'decoder [i32],
}

/// Turns an encoded file into integer PCM. Implementations must be bit-exact for integer
/// codecs: no dither, no float, no gain. Codecs that only decode to float are rounded to
/// integers and say so in [`SourceStreamInfo::codec`]. Runs on the decode thread (allocation
/// allowed, but buffers are reused).
pub trait AudioDecoder: Send {
    /// Facts about the opened source.
    fn stream_info(&self) -> &SourceStreamInfo;
    /// Next block, or `Ok(None)` at end of stream. The format of every block equals
    /// `stream_info().format`; a mid-stream change is reported as `DecodeError::FormatChangedMidStream`
    /// where the codec can detect it. For FLAC it cannot: the parser rejects frame headers that
    /// disagree with STREAMINFO, so the change surfaces as `DecodeError::CorruptData`.
    fn next_block(&mut self) -> Result<Option<DecodedBlock<'_>>, DecodeError>;
    /// Repositions so the next block starts at or before `frame_index`; returns the actual
    /// first frame index of the next block (callers drop the extra frames).
    fn seek_to_frame(&mut self, frame_index: u64) -> Result<u64, DecodeError>;
}

/// Opens decoders for the formats it supports. New formats plug in here (open/closed).
pub trait DecoderProvider: Send + Sync {
    /// Whether this provider recognizes the file at `path`.
    fn can_open(&self, path: &std::path::Path) -> bool;
    /// Opens a decoder for the file at `path`.
    fn open(&self, path: &std::path::Path) -> Result<Box<dyn AudioDecoder>, DecodeError>;
}

/// Why a file could not be decoded.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// File that failed.
        path: std::path::PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// No provider recognizes the file.
    #[error("no decoder for this file type")]
    UnsupportedFileType,
    /// The source bit depth is outside what the pipeline supports.
    #[error("{bits_per_sample}-bit audio is not supported yet")]
    UnsupportedBitDepth {
        /// Bit depth found in the file.
        bits_per_sample: u8,
    },
    /// The audio data is damaged.
    #[error("corrupt or unreadable audio data near frame {near_frame_index}")]
    CorruptData {
        /// Approximate frame where the damage was detected.
        near_frame_index: u64,
        /// Decoder-specific description, for diagnostics.
        detail: String,
    },
    /// The stream changed sample format after it started.
    #[error("sample format changed in the middle of the stream")]
    FormatChangedMidStream,
}
