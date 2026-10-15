//! Which files Pistolame opens, and what their codec means for the bit-perfect promise.

use std::path::Path;

use symphonia::core::codecs::audio::well_known::{
    CODEC_ID_AAC, CODEC_ID_ALAC, CODEC_ID_FLAC, CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_MP3,
    CODEC_ID_PCM_F32BE, CODEC_ID_PCM_F32BE_PLANAR, CODEC_ID_PCM_F32LE, CODEC_ID_PCM_F32LE_PLANAR,
    CODEC_ID_PCM_F64BE, CODEC_ID_PCM_F64BE_PLANAR, CODEC_ID_PCM_F64LE, CODEC_ID_PCM_F64LE_PLANAR,
    CODEC_ID_PCM_S32LE, CODEC_ID_PCM_U8_PLANAR, CODEC_ID_VORBIS,
};
use symphonia::core::codecs::audio::{AudioCodecId, AudioCodecParameters};

/// File extensions (without the dot, compared case-insensitively) of the containers the
/// decoder opens: FLAC, WAV, AIFF, MPEG-4 (ALAC or AAC), CAF, MP3 and Ogg Vorbis. The single
/// list the library scan, the folder watcher and the file picker share.
pub const SUPPORTED_AUDIO_FILE_EXTENSIONS: &[&str] = &[
    "flac", "wav", "wave", "aif", "aiff", "aifc", "m4a", "mp4", "caf", "mp3", "mp2", "mp1", "ogg",
    "oga",
];

/// Whether `path` has one of the [`SUPPORTED_AUDIO_FILE_EXTENSIONS`].
pub fn is_supported_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            SUPPORTED_AUDIO_FILE_EXTENSIONS
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported))
        })
}

/// Bit depth of an integer codec as the file declares it, before any audio is decoded: the
/// codec parameters, or for ALAC its magic cookie. `None` for lossy codecs.
pub fn declared_bits_per_sample(parameters: &AudioCodecParameters) -> Option<u32> {
    parameters
        .bits_per_sample
        .or_else(|| alac_bit_depth(parameters))
}

/// Bit depth from the ALAC magic cookie, which MPEG-4 containers do not repeat elsewhere.
///
/// Apple's ALAC specification (ALACMagicCookieDescription.txt): the cookie may start with a
/// 12-byte `frma` atom, then a 12-byte `alac` atom, before the `ALACSpecificConfig`, whose
/// `bitDepth` byte follows `frameLength` (u32) and `compatibleVersion` (u8).
fn alac_bit_depth(parameters: &AudioCodecParameters) -> Option<u32> {
    /// Length of an atom header (size, type) plus the version and flags word.
    const PREFIX_ATOM_LENGTH_BYTES: usize = 12;
    /// Offset of `bitDepth` in `ALACSpecificConfig`.
    const BIT_DEPTH_OFFSET: usize = 5;
    if parameters.codec != CODEC_ID_ALAC {
        return None;
    }
    let mut cookie = parameters.extra_data.as_deref()?;
    for prefix_atom in [b"frma", b"alac"] {
        if cookie.get(4..8) == Some(prefix_atom.as_slice()) {
            cookie = cookie.get(PREFIX_ATOM_LENGTH_BYTES..)?;
        }
    }
    cookie
        .get(BIT_DEPTH_OFFSET)
        .map(|&bit_depth| u32::from(bit_depth))
}

/// What the codec of a source implies for the samples the pipeline receives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceCodec {
    /// Short name shown to the user ("FLAC", "ALAC", "PCM", "MP3", "AAC", "Vorbis").
    pub display_name: &'static str,
    /// The encoder discarded audio information (MP3, AAC, Vorbis): the file can never be
    /// played as the original master, whatever the output path does.
    pub is_lossy: bool,
    /// The decoder produces floating-point samples, which the pipeline rounds to 24-bit
    /// integers. That rounding is processing, so such a source is never bit-perfect.
    pub is_decoded_as_float: bool,
}

impl SourceCodec {
    /// FLAC: integer, exact.
    pub const FLAC: Self = Self::lossless("FLAC");

    /// Integer codecs whose samples reach the pipeline exactly as encoded.
    const fn lossless(display_name: &'static str) -> Self {
        Self {
            display_name,
            is_lossy: false,
            is_decoded_as_float: false,
        }
    }

    /// Classifies a symphonia codec. Unknown codecs are treated as float decoders, the
    /// conservative choice: they are never reported as bit-perfect.
    pub fn from_codec_id(codec: AudioCodecId) -> Self {
        match codec {
            CODEC_ID_FLAC => Self::FLAC,
            CODEC_ID_ALAC => Self::lossless("ALAC"),
            codec if is_float_pcm(codec) => Self {
                display_name: "PCM flottant",
                is_lossy: false,
                is_decoded_as_float: true,
            },
            codec if is_integer_pcm(codec) => Self::lossless("PCM"),
            CODEC_ID_MP1 | CODEC_ID_MP2 => Self::lossy("MPEG audio"),
            CODEC_ID_MP3 => Self::lossy("MP3"),
            CODEC_ID_AAC => Self::lossy("AAC"),
            CODEC_ID_VORBIS => Self::lossy("Vorbis"),
            _ => Self::lossy("Autre"),
        }
    }

    const fn lossy(display_name: &'static str) -> Self {
        Self {
            display_name,
            is_lossy: true,
            is_decoded_as_float: true,
        }
    }
}

/// Integer PCM, signed or unsigned, every width and byte order: symphonia numbers them
/// contiguously from `CODEC_ID_PCM_S32LE` to `CODEC_ID_PCM_U8_PLANAR` (0x100..=0x11b); the
/// tests pin both ends so a renumbering cannot go unnoticed.
fn is_integer_pcm(codec: AudioCodecId) -> bool {
    (CODEC_ID_PCM_S32LE..=CODEC_ID_PCM_U8_PLANAR).contains(&codec)
}

fn is_float_pcm(codec: AudioCodecId) -> bool {
    [
        CODEC_ID_PCM_F32LE,
        CODEC_ID_PCM_F32LE_PLANAR,
        CODEC_ID_PCM_F32BE,
        CODEC_ID_PCM_F32BE_PLANAR,
        CODEC_ID_PCM_F64LE,
        CODEC_ID_PCM_F64LE_PLANAR,
        CODEC_ID_PCM_F64BE,
        CODEC_ID_PCM_F64BE_PLANAR,
    ]
    .contains(&codec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphonia::core::codecs::audio::well_known::{
        CODEC_ID_PCM_ALAW, CODEC_ID_PCM_S8, CODEC_ID_PCM_S16LE, CODEC_ID_PCM_S24BE,
        CODEC_ID_PCM_U32BE_PLANAR,
    };

    #[test]
    fn supported_extensions_are_recognized_case_insensitively() {
        for name in ["a.flac", "b.M4A", "c.mp3", "d.WAV", "e.aiff", "f.ogg"] {
            assert!(is_supported_audio_file(Path::new(name)), "{name}");
        }
        for name in ["cover.jpg", "notes.txt", "flac", "a.opus"] {
            assert!(!is_supported_audio_file(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn alac_bit_depth_is_read_from_the_magic_cookie_with_or_without_atoms() {
        let mut config = vec![0_u8; 24];
        config[5] = 24;
        let mut wrapped = vec![0, 0, 0, 36];
        wrapped.extend_from_slice(b"alac");
        wrapped.extend_from_slice(&[0; 4]);
        wrapped.extend_from_slice(&config);
        for cookie in [config, wrapped] {
            let mut parameters = AudioCodecParameters::new();
            parameters
                .for_codec(CODEC_ID_ALAC)
                .with_extra_data(cookie.into());
            assert_eq!(declared_bits_per_sample(&parameters), Some(24));
        }
        let mut mp3 = AudioCodecParameters::new();
        mp3.for_codec(CODEC_ID_MP3);
        assert_eq!(declared_bits_per_sample(&mp3), None);
    }

    #[test]
    fn integer_codecs_are_exact_and_lossy_codecs_are_decoded_as_float() {
        for codec in [
            CODEC_ID_FLAC,
            CODEC_ID_ALAC,
            CODEC_ID_PCM_S32LE,
            CODEC_ID_PCM_S16LE,
            CODEC_ID_PCM_S24BE,
            CODEC_ID_PCM_S8,
            CODEC_ID_PCM_U32BE_PLANAR,
            CODEC_ID_PCM_U8_PLANAR,
        ] {
            let source = SourceCodec::from_codec_id(codec);
            assert!(!source.is_lossy && !source.is_decoded_as_float, "{codec:?}");
        }
        for codec in [CODEC_ID_MP3, CODEC_ID_AAC, CODEC_ID_VORBIS] {
            let source = SourceCodec::from_codec_id(codec);
            assert!(source.is_lossy && source.is_decoded_as_float, "{codec:?}");
        }
        let float_pcm = SourceCodec::from_codec_id(CODEC_ID_PCM_F32LE);
        assert!(!float_pcm.is_lossy && float_pcm.is_decoded_as_float);
        // Companded PCM sits right after the integer range and must not fall into it.
        assert!(SourceCodec::from_codec_id(CODEC_ID_PCM_ALAW).is_decoded_as_float);
    }
}
