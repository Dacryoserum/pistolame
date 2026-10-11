//! A file source that hides trailing ID3v1 and APEv2 tags from symphonia.
//!
//! Real libraries are full of FLAC files with such a tag after the last audio frame. The FLAC
//! frame parser would read it as damaged audio, so the decoder opens the file through
//! [`TrailingTagTrimmedFile`], whose length and reads stop where the audio ends.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

use symphonia::core::io::MediaSource;

/// An ID3v1 tag is always exactly 128 bytes, at the very end of the file.
const ID3V1_TAG_LENGTH_BYTES: u64 = 128;
/// First bytes of an ID3v1 tag.
const ID3V1_MARKER: &[u8] = b"TAG";
/// An APEv2 footer (and header, when present) is always 32 bytes.
const APE_FOOTER_LENGTH_BYTES: u64 = 32;
/// Preamble shared by the APEv2 header and footer.
const APE_PREAMBLE: &[u8] = b"APETAGEX";
/// Offset of the tag size (items plus footer, header excluded; little-endian u32) in the footer.
const APE_TAG_SIZE_FIELD_OFFSET: usize = 12;
/// Offset of the flags (little-endian u32) in the footer.
const APE_FLAGS_FIELD_OFFSET: usize = 20;
/// Footer flag: the tag also has a 32-byte header in front of its items.
const APE_HAS_HEADER_FLAG: u32 = 1 << 31;
/// Length of the APEv2 header.
const APE_HEADER_LENGTH_BYTES: u64 = 32;

/// A [`File`] whose end is moved before any trailing ID3v1 / APEv2 tag.
pub(super) struct TrailingTagTrimmedFile {
    file: File,
    visible_length_bytes: u64,
    position_bytes: u64,
}

impl TrailingTagTrimmedFile {
    /// Detects trailing tags in `file` and positions it at the start.
    pub(super) fn new(mut file: File) -> io::Result<Self> {
        let total_length_bytes = file.metadata()?.len();
        let visible_length_bytes = audio_end_offset(&mut file, total_length_bytes)?;
        file.seek(SeekFrom::Start(0))?;
        Ok(Self {
            file,
            visible_length_bytes,
            position_bytes: 0,
        })
    }
}

impl Read for TrailingTagTrimmedFile {
    fn read(&mut self, destination: &mut [u8]) -> io::Result<usize> {
        let remaining_bytes = self
            .visible_length_bytes
            .saturating_sub(self.position_bytes);
        let readable_bytes = destination
            .len()
            .min(usize::try_from(remaining_bytes).unwrap_or(usize::MAX));
        let read_bytes = self.file.read(&mut destination[..readable_bytes])?;
        self.position_bytes += read_bytes as u64;
        Ok(read_bytes)
    }
}

impl Seek for TrailingTagTrimmedFile {
    fn seek(&mut self, target: SeekFrom) -> io::Result<u64> {
        let invalid_position = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the file",
            )
        };
        let new_position_bytes = match target {
            SeekFrom::Start(offset) => offset,
            SeekFrom::Current(offset) => self
                .position_bytes
                .checked_add_signed(offset)
                .ok_or_else(invalid_position)?,
            SeekFrom::End(offset) => self
                .visible_length_bytes
                .checked_add_signed(offset)
                .ok_or_else(invalid_position)?,
        };
        self.file.seek(SeekFrom::Start(new_position_bytes))?;
        self.position_bytes = new_position_bytes;
        Ok(new_position_bytes)
    }
}

impl MediaSource for TrailingTagTrimmedFile {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.visible_length_bytes)
    }
}

/// Offset where the audio ends: the file is `audio [APEv2] [ID3v1]`.
fn audio_end_offset(file: &mut File, total_length_bytes: u64) -> io::Result<u64> {
    let mut end_offset = total_length_bytes;
    if let Some(start_offset) = id3v1_start_offset(file, end_offset)? {
        end_offset = start_offset;
    }
    if let Some(start_offset) = ape_tag_start_offset(file, end_offset)? {
        end_offset = start_offset;
    }
    Ok(end_offset)
}

fn read_exact_at(file: &mut File, offset: u64, destination: &mut [u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(destination)
}

fn id3v1_start_offset(file: &mut File, end_offset: u64) -> io::Result<Option<u64>> {
    let Some(start_offset) = end_offset.checked_sub(ID3V1_TAG_LENGTH_BYTES) else {
        return Ok(None);
    };
    let mut marker = [0_u8; ID3V1_MARKER.len()];
    read_exact_at(file, start_offset, &mut marker)?;
    Ok((marker == ID3V1_MARKER).then_some(start_offset))
}

fn ape_tag_start_offset(file: &mut File, end_offset: u64) -> io::Result<Option<u64>> {
    let Some(footer_offset) = end_offset.checked_sub(APE_FOOTER_LENGTH_BYTES) else {
        return Ok(None);
    };
    let mut footer = [0_u8; APE_FOOTER_LENGTH_BYTES as usize];
    read_exact_at(file, footer_offset, &mut footer)?;
    if !footer.starts_with(APE_PREAMBLE) {
        return Ok(None);
    }
    let read_u32 = |offset: usize| {
        u32::from_le_bytes([
            footer[offset],
            footer[offset + 1],
            footer[offset + 2],
            footer[offset + 3],
        ])
    };
    let items_and_footer_bytes = u64::from(read_u32(APE_TAG_SIZE_FIELD_OFFSET));
    let header_bytes = if read_u32(APE_FLAGS_FIELD_OFFSET) & APE_HAS_HEADER_FLAG != 0 {
        APE_HEADER_LENGTH_BYTES
    } else {
        0
    };
    let tag_bytes = items_and_footer_bytes + header_bytes;
    // An implausible size means this is not really a tag: leave the bytes alone.
    if items_and_footer_bytes < APE_FOOTER_LENGTH_BYTES || tag_bytes > end_offset {
        return Ok(None);
    }
    Ok(Some(end_offset - tag_bytes))
}
