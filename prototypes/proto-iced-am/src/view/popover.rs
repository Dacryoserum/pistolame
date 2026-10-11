//! Signal-path popover (section 8 of the plan).

use common::catalog::{signal_path, signal_summary};
use common::tokens::ColorTokens;
use iced::alignment::{Horizontal, Vertical};
use iced::widget::{column, container, row, text};
use iced::{Background, Border, Element, Length, Shadow, Vector, border};

use super::components::{icon, signal_path as signal_path_widget, single_line, spacer};
use crate::app::{App, Message};
use crate::assets::Icon;
use crate::style::{
    BODY, BODY_MEDIUM, CALLOUT, CALLOUT_MULTILINE_HEIGHT, POPOVER_TITLE, color, filled,
};

pub const POPOVER_WIDTH: f32 = 380.0;
pub const POPOVER_TOP: f32 = 64.0;

fn separator<'a>(tokens: &'static ColorTokens) -> Element<'a, Message> {
    container(spacer(Length::Fill, 1))
        .style(filled(tokens.separator, 0.0))
        .into()
}

pub fn popover<'a>(app: &'a App, tokens: &'static ColorTokens) -> Element<'a, Message> {
    let track = app.playback.current_track(&app.albums);
    let volume = app.playback.volume_percent;
    let summary = signal_summary(track.format, volume);
    let steps = signal_path(track.format, volume);

    let (header_icon, header_tint) = if summary.is_bit_perfect {
        (Icon::SealCheckFill, tokens.status_ok)
    } else {
        (Icon::SlidersHorizontal, tokens.text_secondary)
    };
    let header = row![
        icon(app, header_icon, 18.0, color(header_tint)),
        single_line(
            summary.title,
            POPOVER_TITLE,
            color(tokens.text_primary),
            Length::Fill,
            Horizontal::Left,
        ),
    ]
    .spacing(8)
    .height(20)
    .align_y(Vertical::Center);
    let description = text(summary.description)
        .font(CALLOUT.font)
        .size(CALLOUT.size)
        .line_height(text::LineHeight::Absolute(CALLOUT_MULTILINE_HEIGHT.into()))
        .color(color(tokens.text_secondary))
        .width(Length::Fill);
    let md5_row = row![
        single_line(
            "Vérification MD5 en direct",
            BODY,
            color(tokens.text_secondary),
            Length::Fill,
            Horizontal::Left,
        ),
        single_line(
            app.playback.md5_progress_label(&app.albums),
            BODY_MEDIUM,
            color(tokens.text_primary),
            Length::Shrink,
            Horizontal::Right,
        ),
    ];
    let content = column![
        header,
        spacer(1, 6),
        description,
        spacer(1, 12),
        separator(tokens),
        spacer(1, 12),
        signal_path_widget(tokens, &steps),
        spacer(1, 12),
        separator(tokens),
        spacer(1, 12),
        md5_row,
    ];
    container(content)
        .padding(16)
        .width(POPOVER_WIDTH)
        .style(move |_| container::Style {
            background: Some(Background::Color(color(tokens.surface_raised))),
            border: Border {
                color: color(tokens.border_raised),
                width: 1.0,
                radius: border::radius(10),
            },
            shadow: Shadow {
                color: color(tokens.shadow_popover),
                offset: Vector::new(0.0, 8.0),
                blur_radius: 24.0,
            },
            ..Default::default()
        })
        .into()
}
