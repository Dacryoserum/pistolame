//! Album detail screen: header, track list and quality section.

use common::catalog::{CatalogAlbum, format_frequency, signal_path, signal_summary};
use common::spectrogram::SPECTROGRAM_HEIGHT_PIXELS;
use common::tokens::ColorTokens;
use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, column, container, image, row, scrollable, stack, text};
use iced::{ContentFit, Element, Length, Padding};

use super::components::{
    authenticity_badge_spec, badge, icon, pill_button, signal_badge_spec,
    signal_path as signal_path_widget, single_line, spacer, track_row,
};
use crate::app::{App, Message, detail_scroll_id};
use crate::assets::Icon;
use crate::style::{
    BODY, BODY_MEDIUM, BODY_MULTILINE_HEIGHT, CALLOUT, CALLOUT_MEDIUM, CAPTION, DISPLAY, HEADLINE,
    TITLE_2, TITLE_3, artwork_style, color, filled, flat_button, scrollbar_style,
};

const GUTTER: f32 = 32.0;
const ARTWORK_SIZE: f32 = 240.0;
const SPECTROGRAM_DISPLAY_HEIGHT: f32 = 128.0;
const SIGNAL_COLUMN_WIDTH: f32 = 360.0;
const COLUMNS_GAP: f32 = 48.0;

fn back_button<'a>(app: &App, tokens: &'static ColorTokens) -> Element<'a, Message> {
    let accent = color(tokens.accent);
    container(
        button(
            container(
                row![
                    icon(app, Icon::CaretLeft, 16.0, accent),
                    single_line(
                        "Albums",
                        BODY_MEDIUM,
                        accent,
                        Length::Shrink,
                        Horizontal::Left
                    ),
                ]
                .spacing(4)
                .align_y(Vertical::Center),
            )
            .padding(Padding {
                left: 6.0,
                right: 8.0,
                ..Padding::ZERO
            })
            .height(Length::Fill)
            .align_y(Vertical::Center),
        )
        .padding(0)
        .height(28)
        .style(flat_button(
            None,
            tokens.fill_row_hover,
            tokens.fill_row_pressed,
            6.0,
        ))
        .on_press(Message::BackPressed),
    )
    .padding(Padding {
        left: 26.0,
        ..Padding::ZERO
    })
    .into()
}

fn header<'a>(
    app: &'a App,
    tokens: &'static ColorTokens,
    album_index: usize,
    album: &'a CatalogAlbum,
) -> Element<'a, Message> {
    let summary_text = |content: String, role, token| {
        single_line(content, role, color(token), Length::Fill, Horizontal::Left)
    };
    let info = column![
        spacer(1, 100),
        summary_text(album.title.clone(), DISPLAY, tokens.text_primary),
        spacer(1, 2),
        summary_text(album.artist.clone(), TITLE_2, tokens.accent),
        spacer(1, 8),
        row![
            single_line(
                album.meta_line(),
                CALLOUT_MEDIUM,
                color(tokens.text_secondary),
                Length::Shrink,
                Horizontal::Left
            ),
            badge(app, &authenticity_badge_spec(tokens, album)),
        ]
        .spacing(8)
        .height(20)
        .align_y(Vertical::Center),
        spacer(1, 20),
        row![
            pill_button(
                app,
                tokens,
                Icon::PlayFill,
                "Lecture",
                true,
                Message::AlbumPlayRequested(album_index)
            ),
            pill_button(
                app,
                tokens,
                Icon::Shuffle,
                "Aléatoire",
                false,
                Message::ShuffleAlbumPressed(album_index)
            ),
        ]
        .spacing(12),
    ]
    .width(Length::Fill);
    let artwork = container(
        image(app.artworks[album.artwork_variant].clone())
            .width(ARTWORK_SIZE)
            .height(ARTWORK_SIZE)
            .content_fit(ContentFit::Fill),
    )
    .style(artwork_style(tokens));
    row![artwork, info]
        .spacing(28)
        .padding(Padding {
            left: GUTTER,
            right: GUTTER,
            ..Padding::ZERO
        })
        .into()
}

fn spectrogram<'a>(
    app: &'a App,
    tokens: &'static ColorTokens,
    album: &'a CatalogAlbum,
) -> Element<'a, Message> {
    let label_color = color(tokens.spectrogram_label);
    let corner_label = |content: String, top: f32| {
        container(single_line(
            content,
            CAPTION,
            label_color,
            Length::Fixed(120.0),
            Horizontal::Left,
        ))
        .padding(Padding {
            top,
            left: 8.0,
            ..Padding::ZERO
        })
    };
    let mut layers = stack![]
        .width(Length::Fill)
        .height(SPECTROGRAM_DISPLAY_HEIGHT);
    if let Some(handle) = &app.detail_spectrogram {
        layers = layers.push(
            image(handle.clone())
                .width(Length::Fill)
                .height(SPECTROGRAM_DISPLAY_HEIGHT)
                .content_fit(ContentFit::Fill)
                .border_radius(6.0),
        );
    }
    layers = layers
        .push(corner_label(
            format_frequency(album.format.nyquist_hz()),
            6.0,
        ))
        .push(corner_label(format_frequency(0), 108.0));
    if album.authenticity.should_mark_cutoff() {
        let cutoff_fraction = album.authenticity.cutoff_fraction_of_nyquist(album.format);
        let cutoff_y = ((1.0 - cutoff_fraction) * SPECTROGRAM_HEIGHT_PIXELS as f32).round();
        let cutoff_color = color(tokens.spectrogram_cutoff);
        layers = layers
            .push(
                container(
                    container(spacer(Length::Fill, 1))
                        .style(filled(tokens.spectrogram_cutoff, 0.0)),
                )
                .padding(Padding {
                    top: cutoff_y,
                    ..Padding::ZERO
                }),
            )
            .push(
                container(single_line(
                    format!("Coupure {}", album.authenticity.spectral_cutoff_label()),
                    CAPTION,
                    cutoff_color,
                    Length::Fixed(200.0),
                    Horizontal::Right,
                ))
                .width(Length::Fill)
                .align_x(Horizontal::Right)
                .padding(Padding {
                    top: (cutoff_y - 16.0).max(0.0),
                    right: 8.0,
                    ..Padding::ZERO
                }),
            );
    }
    layers.into()
}

fn data_cell<'a>(
    tokens: &'static ColorTokens,
    label: &'static str,
    value: String,
) -> Element<'a, Message> {
    column![
        single_line(
            label,
            CAPTION,
            color(tokens.text_secondary),
            Length::Fill,
            Horizontal::Left
        ),
        spacer(1, 2),
        single_line(
            value,
            BODY_MEDIUM,
            color(tokens.text_primary),
            Length::Fill,
            Horizontal::Left
        ),
    ]
    .width(Length::Fill)
    .into()
}

fn authenticity_column<'a>(
    app: &'a App,
    tokens: &'static ColorTokens,
    album: &'a CatalogAlbum,
) -> Element<'a, Message> {
    let report = &album.authenticity;
    let format = album.format;
    let data_grid = column![
        row![
            data_cell(
                tokens,
                "Résolution effective",
                report.effective_resolution_label(format)
            ),
            data_cell(tokens, "Coupure spectrale", report.spectral_cutoff_label()),
        ]
        .spacing(16),
        row![
            data_cell(tokens, "Plage dynamique", report.dynamic_range_label()),
            data_cell(tokens, "Écrêtage", report.clipping_label()),
        ]
        .spacing(16),
        row![
            data_cell(tokens, "Contrôle MD5", report.md5_label()),
            spacer(Length::Fill, 1)
        ]
        .spacing(16),
    ]
    .spacing(12);
    column![
        row![
            single_line(
                "Authenticité",
                HEADLINE,
                color(tokens.text_primary),
                Length::Fill,
                Horizontal::Left
            ),
            badge(app, &authenticity_badge_spec(tokens, album)),
        ]
        .height(20)
        .align_y(Vertical::Center),
        spacer(1, 8),
        text(report.summary(format))
            .font(BODY.font)
            .size(BODY.size)
            .line_height(text::LineHeight::Absolute(BODY_MULTILINE_HEIGHT.into()))
            .color(color(tokens.text_primary))
            .width(Length::Fill),
        spacer(1, 12),
        spectrogram(app, tokens, album),
        spacer(1, 12),
        data_grid,
    ]
    .width(Length::Fill)
    .into()
}

fn signal_column<'a>(
    app: &'a App,
    tokens: &'static ColorTokens,
    album: &'a CatalogAlbum,
) -> Element<'a, Message> {
    let volume = app.playback.volume_percent;
    let summary = signal_summary(album.format, volume);
    let steps = signal_path(album.format, volume);
    column![
        row![
            single_line(
                "Chemin du signal",
                HEADLINE,
                color(tokens.text_primary),
                Length::Fill,
                Horizontal::Left
            ),
            badge(app, &signal_badge_spec(tokens, &summary)),
        ]
        .height(20)
        .align_y(Vertical::Center),
        spacer(1, 12),
        signal_path_widget(tokens, &steps),
    ]
    .width(SIGNAL_COLUMN_WIDTH)
    .into()
}

fn track_list<'a>(
    app: &'a App,
    tokens: &'static ColorTokens,
    album_index: usize,
    album: &'a CatalogAlbum,
) -> Element<'a, Message> {
    let rows = album
        .tracks
        .iter()
        .enumerate()
        .map(|(track_index, track)| track_row(app, tokens, album_index, track_index, track));
    column(rows).into()
}

pub fn album_detail<'a>(
    app: &'a App,
    tokens: &'static ColorTokens,
    album_index: usize,
) -> Element<'a, Message> {
    let album = &app.albums[album_index];
    let page = column![
        spacer(1, 16),
        back_button(app, tokens),
        spacer(1, 16),
        header(app, tokens, album_index, album),
        spacer(1, 28),
        track_list(app, tokens, album_index, album),
        spacer(1, 12),
        container(single_line(
            album.footer_line(),
            CALLOUT,
            color(tokens.text_secondary),
            Length::Fill,
            Horizontal::Left,
        ))
        .padding(Padding {
            left: 44.0,
            right: GUTTER,
            ..Padding::ZERO
        }),
        spacer(1, 40),
        container(single_line(
            "Qualité",
            TITLE_3,
            color(tokens.text_primary),
            Length::Fill,
            Horizontal::Left
        ))
        .padding(Padding {
            left: GUTTER,
            right: GUTTER,
            ..Padding::ZERO
        }),
        spacer(1, 16),
        row![
            signal_column(app, tokens, album),
            spacer(COLUMNS_GAP, 1),
            authenticity_column(app, tokens, album),
        ]
        .padding(Padding {
            left: GUTTER,
            right: GUTTER,
            ..Padding::ZERO
        })
        .align_y(Vertical::Top),
        spacer(1, 40),
    ]
    .width(Length::Fill);
    scrollable(page)
        .id(detail_scroll_id())
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::new()
                .width(6)
                .scroller_width(6)
                .margin(3),
        ))
        .style(scrollbar_style(tokens))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
