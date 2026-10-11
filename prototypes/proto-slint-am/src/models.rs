//! Conversion of the `common` catalog into the Slint structs and images.

use crate::{AlbumCard, DataCell, DetailInfo, PathStep, TrackRowData};
use common::artwork::{ARTWORK_SIZE_PIXELS, ARTWORK_VARIANT_COUNT, artwork_rgba};
use common::catalog::{
    AuthenticityFinding, CatalogAlbum, SignalStep, format_clock, format_frequency,
};
use common::spectrogram::{SPECTROGRAM_HEIGHT_PIXELS, SPECTROGRAM_WIDTH_PIXELS, spectrogram_rgba};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

fn image_from_rgba(rgba_bytes: &[u8], width_pixels: u32, height_pixels: u32) -> Image {
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        rgba_bytes,
        width_pixels,
        height_pixels,
    ))
}

/// Builds the artwork variants once; Slint images are reference counted, so clones are cheap.
pub fn build_artworks() -> Vec<Image> {
    (0..ARTWORK_VARIANT_COUNT)
        .map(|variant| {
            image_from_rgba(
                &artwork_rgba(variant),
                ARTWORK_SIZE_PIXELS,
                ARTWORK_SIZE_PIXELS,
            )
        })
        .collect()
}

/// Indices of the albums matching the search query, in catalog order.
pub fn visible_album_indices(albums: &[CatalogAlbum], query: &str) -> Vec<usize> {
    (0..albums.len())
        .filter(|&index| albums[index].matches_query(query))
        .collect()
}

/// Grid cards for the given catalog indices.
pub fn album_cards(
    albums: &[CatalogAlbum],
    visible_indices: &[usize],
    artworks: &[Image],
) -> Vec<AlbumCard> {
    visible_indices
        .iter()
        .map(|&index| {
            let album = &albums[index];
            AlbumCard {
                catalog_index: index as i32,
                title: album.title.as_str().into(),
                artist: album.artist.as_str().into(),
                artwork: artworks[album.artwork_variant].clone(),
            }
        })
        .collect()
}

/// Rows of the detail track list.
pub fn track_rows(album: &CatalogAlbum) -> Vec<TrackRowData> {
    album
        .tracks
        .iter()
        .map(|track| TrackRowData {
            number: track.number as i32,
            title: track.title.as_str().into(),
            format_label: track.format.short_label().into(),
            duration_label: format_clock(track.duration_seconds).into(),
        })
        .collect()
}

/// Signal path steps as Slint structs.
pub fn path_steps(steps: Vec<SignalStep>) -> Vec<PathStep> {
    steps
        .into_iter()
        .map(|step| PathStep {
            label: step.label.into(),
            value: step.value.into(),
            is_transparent: step.is_transparent,
        })
        .collect()
}

/// Everything the detail page shows; the spectrogram is generated here, when the page opens.
pub fn detail_info(album_index: usize, album: &CatalogAlbum, artworks: &[Image]) -> DetailInfo {
    let report = &album.authenticity;
    let spectrogram_pixels = spectrogram_rgba(
        report.cutoff_fraction_of_nyquist(album.format),
        report.finding == AuthenticityFinding::LossyTranscode,
        album_index as u64 + 1,
    );
    let cell = |label: &str, value: String| DataCell {
        label: label.into(),
        value: value.into(),
    };
    let cells = vec![
        cell(
            "Résolution effective",
            report.effective_resolution_label(album.format),
        ),
        cell("Coupure spectrale", report.spectral_cutoff_label()),
        cell("Plage dynamique", report.dynamic_range_label()),
        cell("Écrêtage", report.clipping_label()),
        cell("Contrôle MD5", report.md5_label()),
    ];
    DetailInfo {
        title: album.title.as_str().into(),
        artist: album.artist.as_str().into(),
        meta_line: album.meta_line().into(),
        artwork: artworks[album.artwork_variant].clone(),
        footer_line: album.footer_line().into(),
        is_authentic: report.is_authentic(),
        verdict_label: report.verdict_label().into(),
        summary: report.summary(album.format).into(),
        spectrogram: image_from_rgba(
            &spectrogram_pixels,
            SPECTROGRAM_WIDTH_PIXELS,
            SPECTROGRAM_HEIGHT_PIXELS,
        ),
        should_mark_cutoff: report.should_mark_cutoff(),
        cutoff_fraction: report.cutoff_fraction_of_nyquist(album.format),
        cutoff_label: format!("Coupure {}", report.spectral_cutoff_label()).into(),
        nyquist_label: format_frequency(album.format.nyquist_hz()).into(),
        zero_label: format_frequency(0).into(),
        cells: slint::ModelRc::new(slint::VecModel::from(cells)),
    }
}
