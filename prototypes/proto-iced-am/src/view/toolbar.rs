//! Toolbar: transport cluster, LCD with progress, volume and output button.

use common::catalog::signal_summary;
use common::tokens::ColorTokens;
use iced::alignment::{Horizontal, Vertical};
use iced::widget::{column, container, image, mouse_area, row, slider, stack};
use iced::{Background, Border, ContentFit, Element, Length, Padding, border};

use super::components::{badge_button, icon, icon_button, signal_badge_spec, single_line, spacer};
use super::sidebar::SIDEBAR_WIDTH;
use crate::app::{App, Message};
use crate::assets::Icon;
use crate::style::{BODY_MEDIUM, CALLOUT, CAPTION, color, filled};

const TOOLBAR_HEIGHT: f32 = 60.0;
const TRANSPORT_WIDTH: f32 = 172.0;
const RIGHT_CLUSTER_WIDTH: f32 = 168.0;
const TOOLBAR_SIDE_PADDING: f32 = 20.0;
const LCD_MIN_WIDTH: f32 = 360.0;
const LCD_MAX_WIDTH: f32 = 560.0;
const MIN_SIDE_GAP: f32 = 24.0;
const LCD_HEIGHT: f32 = 44.0;

/// LCD placement derived from the window width (section 6 of the plan).
pub struct ToolbarGeometry {
    pub lcd_width: f32,
    pub side_gap: f32,
    pub lcd_right_in_window: f32,
}

pub fn toolbar_geometry(window_width: f32) -> ToolbarGeometry {
    let toolbar_width = window_width - SIDEBAR_WIDTH;
    let fixed_width = 2.0 * TOOLBAR_SIDE_PADDING + TRANSPORT_WIDTH + RIGHT_CLUSTER_WIDTH;
    let lcd_width =
        (toolbar_width - fixed_width - 2.0 * MIN_SIDE_GAP).clamp(LCD_MIN_WIDTH, LCD_MAX_WIDTH);
    let side_gap = (toolbar_width - fixed_width - lcd_width) / 2.0;
    let lcd_x = TOOLBAR_SIDE_PADDING + TRANSPORT_WIDTH + side_gap;
    ToolbarGeometry {
        lcd_width,
        side_gap,
        lcd_right_in_window: SIDEBAR_WIDTH + lcd_x + lcd_width,
    }
}

fn transport<'a>(app: &App, tokens: &'static ColorTokens) -> Element<'a, Message> {
    let toggle_tint = |is_on: bool| {
        color(if is_on {
            tokens.accent
        } else {
            tokens.text_secondary
        })
    };
    let primary_tint = color(tokens.text_primary);
    let play_icon = if app.playback.is_playing {
        Icon::PauseFill
    } else {
        Icon::PlayFill
    };
    row![
        icon_button(
            app,
            tokens,
            Icon::Shuffle,
            16.0,
            28.0,
            toggle_tint(app.is_shuffle_on),
            Message::ToggleShuffle
        ),
        icon_button(
            app,
            tokens,
            Icon::SkipBackFill,
            18.0,
            32.0,
            primary_tint,
            Message::PreviousPressed
        ),
        icon_button(
            app,
            tokens,
            play_icon,
            22.0,
            36.0,
            primary_tint,
            Message::TogglePlay
        ),
        icon_button(
            app,
            tokens,
            Icon::SkipForwardFill,
            18.0,
            32.0,
            primary_tint,
            Message::NextPressed
        ),
        icon_button(
            app,
            tokens,
            Icon::Repeat,
            16.0,
            28.0,
            toggle_tint(app.is_repeat_on),
            Message::ToggleRepeat
        ),
    ]
    .spacing(4)
    .align_y(Vertical::Center)
    .width(TRANSPORT_WIDTH)
    .into()
}

fn lcd<'a>(app: &'a App, tokens: &'static ColorTokens, lcd_width: f32) -> Element<'a, Message> {
    let playback = &app.playback;
    let album = &app.albums[playback.album_index];
    let track = playback.current_track(&app.albums);
    let summary = signal_summary(track.format, playback.volume_percent);

    let thumbnail = container(
        image(app.artworks[album.artwork_variant].clone())
            .width(36)
            .height(36)
            .content_fit(ContentFit::Fill),
    );
    let (elapsed, remaining) = if app.is_lcd_hovered {
        (
            playback.elapsed_label(),
            playback.remaining_label(&app.albums),
        )
    } else {
        (String::new(), String::new())
    };
    let secondary = color(tokens.text_secondary);
    let text_lines = column![
        spacer(1, 6),
        single_line(
            track.title.clone(),
            BODY_MEDIUM,
            color(tokens.text_primary),
            Length::Fill,
            Horizontal::Center
        ),
        row![
            single_line(
                elapsed,
                CAPTION,
                secondary,
                Length::Fixed(40.0),
                Horizontal::Left
            ),
            single_line(
                album.lcd_subtitle(),
                CALLOUT,
                secondary,
                Length::Fill,
                Horizontal::Center
            ),
            single_line(
                remaining,
                CAPTION,
                secondary,
                Length::Fixed(40.0),
                Horizontal::Right
            ),
        ],
    ];
    let is_hovered = app.is_lcd_hovered;
    let seek = slider(
        0.0..=1.0,
        playback.progress_fraction(&app.albums),
        Message::Seek,
    )
    .step(0.001_f32)
    .height(8)
    .style(move |_, _| slider::Style {
        rail: slider::Rail {
            backgrounds: (
                Background::Color(color(tokens.rail_fill)),
                Background::Color(color(tokens.rail)),
            ),
            width: 3.0,
            border: border::rounded(1.5),
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle {
                radius: if is_hovered { 4.5 } else { 0.0 },
            },
            background: Background::Color(color(tokens.scrubber_thumb)),
            border_width: 0.0,
            border_color: iced::Color::TRANSPARENT,
        },
    });
    let progress_layer = column![spacer(1, 36), seek];
    let text_column = stack![text_lines, progress_layer]
        .width(Length::Fill)
        .height(LCD_HEIGHT);

    let badge = badge_button(
        app,
        &signal_badge_spec(tokens, &summary),
        Message::PopoverToggled,
    );
    let body = container(
        row![
            thumbnail,
            spacer(8, 1),
            text_column,
            spacer(12, 1),
            badge,
            spacer(8, 1),
        ]
        .padding(Padding {
            left: 4.0,
            ..Padding::ZERO
        })
        .align_y(Vertical::Center),
    )
    .width(lcd_width)
    .height(LCD_HEIGHT)
    .align_y(Vertical::Center)
    .style(move |_| iced::widget::container::Style {
        background: Some(Background::Color(color(tokens.surface_lcd))),
        border: Border {
            color: color(tokens.border_lcd),
            width: 1.0,
            radius: border::radius(6),
        },
        ..Default::default()
    });
    mouse_area(body)
        .on_enter(Message::LcdHoverChanged(true))
        .on_exit(Message::LcdHoverChanged(false))
        .into()
}

fn right_cluster<'a>(app: &App, tokens: &'static ColorTokens) -> Element<'a, Message> {
    let secondary = color(tokens.text_secondary);
    let output_tint = if app.is_popover_open {
        color(tokens.accent)
    } else {
        secondary
    };
    let volume = slider(
        0..=100u8,
        app.playback.volume_percent,
        Message::VolumeChanged,
    )
    .width(88)
    .height(28)
    .style(move |_, _| slider::Style {
        rail: slider::Rail {
            backgrounds: (
                Background::Color(color(tokens.rail_fill)),
                Background::Color(color(tokens.rail)),
            ),
            width: 3.0,
            border: border::rounded(1.5),
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle { radius: 5.5 },
            background: Background::Color(color(tokens.scrubber_thumb)),
            border_width: 0.0,
            border_color: iced::Color::TRANSPARENT,
        },
    });
    row![
        icon(app, Icon::SpeakerLow, 14.0, secondary),
        spacer(6, 1),
        volume,
        spacer(6, 1),
        icon(app, Icon::SpeakerHigh, 14.0, secondary),
        spacer(12, 1),
        icon_button(
            app,
            tokens,
            Icon::SpeakerHifi,
            18.0,
            28.0,
            output_tint,
            Message::PopoverToggled
        ),
    ]
    .align_y(Vertical::Center)
    .width(RIGHT_CLUSTER_WIDTH)
    .into()
}

pub fn toolbar<'a>(app: &'a App, tokens: &'static ColorTokens) -> Element<'a, Message> {
    let geometry = toolbar_geometry(app.window_size.width);
    let content = container(
        row![
            transport(app, tokens),
            spacer(geometry.side_gap, 1),
            lcd(app, tokens, geometry.lcd_width),
            spacer(Length::Fill, 1),
            right_cluster(app, tokens),
        ]
        .align_y(Vertical::Center),
    )
    .padding([0.0, TOOLBAR_SIDE_PADDING])
    .width(Length::Fill)
    .height(TOOLBAR_HEIGHT)
    .align_y(Vertical::Center)
    .style(filled(tokens.surface_toolbar, 0.0));
    let bottom_edge =
        container(container(spacer(Length::Fill, 1)).style(filled(tokens.separator_chrome, 0.0)))
            .width(Length::Fill)
            .height(TOOLBAR_HEIGHT)
            .align_y(Vertical::Bottom);
    stack![content, bottom_edge]
        .width(Length::Fill)
        .height(TOOLBAR_HEIGHT)
        .into()
}
