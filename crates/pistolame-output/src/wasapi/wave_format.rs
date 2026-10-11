//! Translation between the neutral `DeviceSampleFormat` and the Windows wave-format structures.
//! Pure data conversion, no COM calls.
//!
//! Sources:
//! - WAVEFORMATEXTENSIBLE: <https://learn.microsoft.com/en-us/windows/win32/api/mmreg/ns-mmreg-waveformatextensible>
//! - WAVEFORMATEX: <https://learn.microsoft.com/en-us/windows/win32/api/mmeapi/ns-mmeapi-waveformatex>
//! - Extensible Wave-Format Descriptors (valid bits in the most significant bits):
//!   <https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/extensible-wave-format-descriptors>
//! - Device Formats (WAVEFORMATEX accepted alone by some drivers for 1 or 2 channels):
//!   <https://learn.microsoft.com/en-us/windows/win32/coreaudio/device-formats>

use windows::Win32::Media::Audio::{WAVE_FORMAT_PCM, WAVEFORMATEX, WAVEFORMATEXTENSIBLE};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};

use crate::{ChannelMask, DeviceSampleFormat, SystemMixFormat, ValidBitsAlignment};

/// `cbSize` of a WAVEFORMATEXTENSIBLE: the bytes that follow the WAVEFORMATEX header (22, the
/// documented minimum for WAVE_FORMAT_EXTENSIBLE).
pub(crate) const WAVEFORMATEXTENSIBLE_EXTRA_BYTES: u16 =
    (size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>()) as u16;
/// Number of bits in one byte.
const BITS_PER_BYTE: u16 = 8;
/// Container size, in bits, for which the stand-alone WAVEFORMATEX retry applies (B5.4 step 3).
const PLAIN_WAVE_FORMAT_RETRY_BITS: u8 = 16;
/// Most channels for which a driver may want a stand-alone WAVEFORMATEX (Device Formats docs).
const PLAIN_WAVE_FORMAT_MAXIMUM_CHANNELS: u8 = 2;

/// The structure handed to WASAPI for one candidate. Only the extensible form can state valid
/// bits separately from the container, so the plain form exists only for the 16-bit retry.
#[derive(Clone, Copy)]
pub(crate) enum WaveFormatRepresentation {
    /// WAVEFORMATEXTENSIBLE with the PCM subformat.
    Extensible(WAVEFORMATEXTENSIBLE),
    /// Stand-alone WAVEFORMATEX with WAVE_FORMAT_PCM (16-bit, 1 or 2 channels only).
    Plain(WAVEFORMATEX),
}

impl WaveFormatRepresentation {
    /// Pointer for the WASAPI calls, which take a `WAVEFORMATEX*` that may point at the header of
    /// a WAVEFORMATEXTENSIBLE (its first member). Valid while `self` is alive and not moved.
    pub(crate) fn as_wave_format_pointer(&self) -> *const WAVEFORMATEX {
        match self {
            WaveFormatRepresentation::Extensible(extensible) => {
                std::ptr::from_ref(extensible).cast::<WAVEFORMATEX>()
            }
            WaveFormatRepresentation::Plain(plain) => std::ptr::from_ref(plain),
        }
    }
}

/// The WAVEFORMATEX header shared by both representations (B5.4 step 1).
fn wave_format_header(
    format: &DeviceSampleFormat,
    format_tag: u32,
    extra_bytes: u16,
) -> WAVEFORMATEX {
    let block_align_bytes = u16::from(format.channel_count) * u16::from(format.container_bytes);
    WAVEFORMATEX {
        wFormatTag: format_tag as u16,
        nChannels: u16::from(format.channel_count),
        nSamplesPerSec: format.sample_rate_hz,
        nAvgBytesPerSec: format.sample_rate_hz * u32::from(block_align_bytes),
        nBlockAlign: block_align_bytes,
        wBitsPerSample: u16::from(format.container_bytes) * BITS_PER_BYTE,
        cbSize: extra_bytes,
    }
}

/// WAVEFORMATEXTENSIBLE for a candidate: container bits in `wBitsPerSample`, valid bits in
/// `wValidBitsPerSample`, the source channel mask unchanged, PCM subformat.
pub(crate) fn extensible_wave_format(format: &DeviceSampleFormat) -> WAVEFORMATEXTENSIBLE {
    let mut extensible = WAVEFORMATEXTENSIBLE {
        Format: wave_format_header(
            format,
            WAVE_FORMAT_EXTENSIBLE,
            WAVEFORMATEXTENSIBLE_EXTRA_BYTES,
        ),
        dwChannelMask: format.channel_mask.0,
        SubFormat: KSDATAFORMAT_SUBTYPE_PCM,
        ..Default::default()
    };
    extensible.Samples.wValidBitsPerSample = u16::from(format.valid_bits_per_sample);
    extensible
}

/// Whether B5.4 step 3 applies: a refused 16-bit (container and valid bits) mono or stereo
/// candidate is retried as a stand-alone WAVEFORMATEX. Never for other depths, which
/// WAVEFORMATEX cannot describe without ambiguity.
pub(crate) fn should_retry_as_plain_wave_format(format: &DeviceSampleFormat) -> bool {
    format.channel_count <= PLAIN_WAVE_FORMAT_MAXIMUM_CHANNELS
        && u16::from(format.container_bytes) * BITS_PER_BYTE
            == u16::from(PLAIN_WAVE_FORMAT_RETRY_BITS)
        && format.valid_bits_per_sample == PLAIN_WAVE_FORMAT_RETRY_BITS
}

/// Stand-alone WAVEFORMATEX for a candidate that passes `should_retry_as_plain_wave_format`.
pub(crate) fn plain_wave_format(format: &DeviceSampleFormat) -> WAVEFORMATEX {
    wave_format_header(format, WAVE_FORMAT_PCM, 0)
}

/// Reads back the integer format a representation describes, so `NegotiatedOutput` reports
/// what the platform accepted rather than what was requested. Returns `None` for anything that
/// is not integer PCM in a 2-, 3- or 4-byte container.
pub(crate) fn device_sample_format_from_representation(
    representation: &WaveFormatRepresentation,
) -> Option<DeviceSampleFormat> {
    match representation {
        WaveFormatRepresentation::Extensible(extensible) => {
            let header = extensible.Format;
            let sub_format = extensible.SubFormat;
            if u32::from(header.wFormatTag) != WAVE_FORMAT_EXTENSIBLE
                || sub_format != KSDATAFORMAT_SUBTYPE_PCM
            {
                return None;
            }
            // SAFETY: every member of the `Samples` union is a `u16`, so reading any of them is
            // reading initialized plain data; for PCM the documented member is
            // `wValidBitsPerSample`.
            let valid_bits_per_sample = unsafe { extensible.Samples.wValidBitsPerSample };
            integer_format_from_header(
                &header,
                valid_bits_per_sample,
                ChannelMask(extensible.dwChannelMask),
            )
        }
        WaveFormatRepresentation::Plain(plain) => {
            if u32::from(plain.wFormatTag) != WAVE_FORMAT_PCM {
                return None;
            }
            let channel_count = u8::try_from(plain.nChannels).ok()?;
            // A WAVEFORMATEX carries no mask; mono and stereo imply the default layout
            // (KSAUDIO_SPEAKER_MONO / KSAUDIO_SPEAKER_STEREO), which is the FLAC default too.
            let implied_mask = ChannelMask::default_for_flac_channel_count(channel_count)?;
            integer_format_from_header(plain, plain.wBitsPerSample, implied_mask)
        }
    }
}

fn integer_format_from_header(
    header: &WAVEFORMATEX,
    valid_bits_per_sample: u16,
    channel_mask: ChannelMask,
) -> Option<DeviceSampleFormat> {
    let container_bits = header.wBitsPerSample;
    if !container_bits.is_multiple_of(BITS_PER_BYTE) {
        return None;
    }
    let container_bytes = u8::try_from(container_bits / BITS_PER_BYTE).ok()?;
    if !(2..=4).contains(&container_bytes) || valid_bits_per_sample > container_bits {
        return None;
    }
    Some(DeviceSampleFormat {
        sample_rate_hz: header.nSamplesPerSec,
        container_bytes,
        valid_bits_per_sample: u8::try_from(valid_bits_per_sample).ok()?,
        alignment: ValidBitsAlignment::MostSignificant,
        channel_count: u8::try_from(header.nChannels).ok()?,
        channel_mask,
    })
}

/// The system mixer's format (from `GetMixFormat`) as a plain header. Floats are recognized by
/// WAVE_FORMAT_IEEE_FLOAT.
pub(crate) fn system_mix_format_from_plain(header: &WAVEFORMATEX) -> SystemMixFormat {
    SystemMixFormat {
        sample_rate_hz: header.nSamplesPerSec,
        bits_per_sample: u8::try_from(header.wBitsPerSample).unwrap_or(u8::MAX),
        is_float: u32::from(header.wFormatTag) == WAVE_FORMAT_IEEE_FLOAT,
        channel_count: u8::try_from(header.nChannels).unwrap_or(u8::MAX),
    }
}

/// The system mixer's format when `GetMixFormat` returned a WAVEFORMATEXTENSIBLE (the usual
/// case). Floats are recognized by the KSDATAFORMAT_SUBTYPE_IEEE_FLOAT subformat.
pub(crate) fn system_mix_format_from_extensible(
    extensible: &WAVEFORMATEXTENSIBLE,
) -> SystemMixFormat {
    let header = extensible.Format;
    let sub_format = extensible.SubFormat;
    SystemMixFormat {
        is_float: sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
        ..system_mix_format_from_plain(&header)
    }
}

/// Reads the mix format behind a pointer returned by `IAudioClient::GetMixFormat`.
///
/// # Safety
/// `mix_format` must point at a valid WAVEFORMATEX whose `cbSize` correctly states how many
/// bytes follow it, as `GetMixFormat` guarantees.
pub(crate) unsafe fn system_mix_format_from_pointer(
    mix_format: *const WAVEFORMATEX,
) -> SystemMixFormat {
    // SAFETY: the caller guarantees a valid WAVEFORMATEX; `read_unaligned` copes with the
    // packed layout.
    let header = unsafe { mix_format.read_unaligned() };
    let is_extensible = u32::from(header.wFormatTag) == WAVE_FORMAT_EXTENSIBLE
        && header.cbSize >= WAVEFORMATEXTENSIBLE_EXTRA_BYTES;
    if is_extensible {
        // SAFETY: the tag and `cbSize` say a full WAVEFORMATEXTENSIBLE is present at this
        // address (WAVEFORMATEXTENSIBLE docs).
        let extensible = unsafe { mix_format.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned() };
        system_mix_format_from_extensible(&extensible)
    } else {
        system_mix_format_from_plain(&header)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StreamFormat, lossless_device_format_candidates};

    fn stereo_source(bits_per_sample: u8, sample_rate_hz: u32) -> StreamFormat {
        StreamFormat {
            sample_rate_hz,
            bits_per_sample,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        }
    }

    fn valid_bits(extensible: &WAVEFORMATEXTENSIBLE) -> u16 {
        // SAFETY: test reads a `u16` union member that was just written.
        unsafe { extensible.Samples.wValidBitsPerSample }
    }

    #[test]
    fn extra_bytes_is_the_documented_twenty_two() {
        assert_eq!(WAVEFORMATEXTENSIBLE_EXTRA_BYTES, 22);
        assert_eq!(size_of::<WAVEFORMATEX>(), 18);
        assert_eq!(size_of::<WAVEFORMATEXTENSIBLE>(), 40);
    }

    #[test]
    fn extensible_fields_are_exact_for_every_24_bit_candidate_at_96_khz() {
        // (container bytes, valid bits, block align, average bytes per second)
        let expected = [
            (3u8, 24u16, 6u16, 576_000u32),
            (4, 24, 8, 768_000),
            (4, 32, 8, 768_000),
        ];
        let candidates = lossless_device_format_candidates(
            &stereo_source(24, 96_000),
            ValidBitsAlignment::MostSignificant,
        );
        assert_eq!(candidates.len(), expected.len());
        for (candidate, (container_bytes, valid, block_align, average_bytes)) in
            candidates.iter().zip(expected)
        {
            assert_eq!(candidate.container_bytes, container_bytes);
            let extensible = extensible_wave_format(candidate);
            let header = extensible.Format;
            assert_eq!(u32::from(header.wFormatTag), WAVE_FORMAT_EXTENSIBLE);
            assert_eq!({ header.wFormatTag }, 0xFFFE);
            assert_eq!({ header.cbSize }, 22);
            assert_eq!({ header.nChannels }, 2);
            assert_eq!({ header.nSamplesPerSec }, 96_000);
            assert_eq!({ header.wBitsPerSample }, u16::from(container_bytes) * 8);
            assert_eq!({ header.nBlockAlign }, block_align);
            assert_eq!({ header.nAvgBytesPerSec }, average_bytes);
            assert_eq!(valid_bits(&extensible), valid);
            assert_eq!({ extensible.dwChannelMask }, 0x3);
            assert_eq!({ extensible.SubFormat }, KSDATAFORMAT_SUBTYPE_PCM);
        }
    }

    #[test]
    fn extensible_fields_are_exact_for_16_bit_in_3_bytes_at_44_1_khz() {
        let format = DeviceSampleFormat {
            sample_rate_hz: 44_100,
            container_bytes: 3,
            valid_bits_per_sample: 16,
            alignment: ValidBitsAlignment::MostSignificant,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        };
        let extensible = extensible_wave_format(&format);
        let header = extensible.Format;
        assert_eq!({ header.wBitsPerSample }, 24);
        assert_eq!(valid_bits(&extensible), 16);
        assert_eq!({ header.nBlockAlign }, 6);
        assert_eq!({ header.nAvgBytesPerSec }, 264_600);
    }

    #[test]
    fn pcm_subformat_guid_is_the_documented_value() {
        assert_eq!(
            KSDATAFORMAT_SUBTYPE_PCM,
            windows::core::GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71)
        );
    }

    #[test]
    fn plain_retry_applies_only_to_16_bit_mono_or_stereo() {
        let candidates = lossless_device_format_candidates(
            &stereo_source(16, 44_100),
            ValidBitsAlignment::MostSignificant,
        );
        let retried: Vec<(u8, u8)> = candidates
            .iter()
            .filter(|candidate| should_retry_as_plain_wave_format(candidate))
            .map(|candidate| (candidate.container_bytes, candidate.valid_bits_per_sample))
            .collect();
        assert_eq!(retried, vec![(2, 16)]);

        let six_channels = DeviceSampleFormat {
            channel_count: 6,
            channel_mask: ChannelMask(0x3F),
            ..candidates[0]
        };
        assert!(!should_retry_as_plain_wave_format(&six_channels));
        let mono = DeviceSampleFormat {
            channel_count: 1,
            channel_mask: ChannelMask(0x4),
            ..candidates[0]
        };
        assert!(should_retry_as_plain_wave_format(&mono));
        for candidate in lossless_device_format_candidates(
            &stereo_source(24, 44_100),
            ValidBitsAlignment::MostSignificant,
        ) {
            assert!(!should_retry_as_plain_wave_format(&candidate));
        }
    }

    #[test]
    fn plain_wave_format_fields_are_exact() {
        let candidates = lossless_device_format_candidates(
            &stereo_source(16, 48_000),
            ValidBitsAlignment::MostSignificant,
        );
        let plain = plain_wave_format(&candidates[0]);
        assert_eq!(u32::from(plain.wFormatTag), WAVE_FORMAT_PCM);
        assert_eq!({ plain.cbSize }, 0);
        assert_eq!({ plain.wBitsPerSample }, 16);
        assert_eq!({ plain.nBlockAlign }, 4);
        assert_eq!({ plain.nAvgBytesPerSec }, 192_000);
    }

    #[test]
    fn read_back_returns_the_same_format_for_every_candidate() {
        for bits_per_sample in [16, 20, 24] {
            for candidate in lossless_device_format_candidates(
                &stereo_source(bits_per_sample, 192_000),
                ValidBitsAlignment::MostSignificant,
            ) {
                let representation =
                    WaveFormatRepresentation::Extensible(extensible_wave_format(&candidate));
                assert_eq!(
                    device_sample_format_from_representation(&representation),
                    Some(candidate)
                );
                if should_retry_as_plain_wave_format(&candidate) {
                    let plain = WaveFormatRepresentation::Plain(plain_wave_format(&candidate));
                    assert_eq!(
                        device_sample_format_from_representation(&plain),
                        Some(candidate)
                    );
                }
            }
        }
    }

    #[test]
    fn read_back_rejects_a_float_format() {
        let mut extensible = extensible_wave_format(&DeviceSampleFormat {
            sample_rate_hz: 48_000,
            container_bytes: 4,
            valid_bits_per_sample: 32,
            alignment: ValidBitsAlignment::MostSignificant,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        });
        extensible.SubFormat = KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
        assert_eq!(
            device_sample_format_from_representation(&WaveFormatRepresentation::Extensible(
                extensible
            )),
            None
        );
    }

    #[test]
    fn float_extensible_mix_format_is_reported_as_float() {
        let mut extensible = extensible_wave_format(&DeviceSampleFormat {
            sample_rate_hz: 48_000,
            container_bytes: 4,
            valid_bits_per_sample: 32,
            alignment: ValidBitsAlignment::MostSignificant,
            channel_count: 2,
            channel_mask: ChannelMask(0x3),
        });
        extensible.SubFormat = KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
        assert_eq!(
            system_mix_format_from_extensible(&extensible),
            SystemMixFormat {
                sample_rate_hz: 48_000,
                bits_per_sample: 32,
                is_float: true,
                channel_count: 2,
            }
        );
        // SAFETY: the pointer refers to a complete local WAVEFORMATEXTENSIBLE.
        let through_pointer = unsafe {
            system_mix_format_from_pointer(std::ptr::from_ref(&extensible).cast::<WAVEFORMATEX>())
        };
        assert!(through_pointer.is_float);
    }

    #[test]
    fn integer_plain_mix_format_is_reported_as_integer() {
        let plain = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 2,
            nSamplesPerSec: 44_100,
            nAvgBytesPerSec: 176_400,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        assert_eq!(
            system_mix_format_from_plain(&plain),
            SystemMixFormat {
                sample_rate_hz: 44_100,
                bits_per_sample: 16,
                is_float: false,
                channel_count: 2,
            }
        );
    }
}
