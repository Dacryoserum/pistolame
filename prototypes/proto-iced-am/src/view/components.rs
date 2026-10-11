//! Reusable widgets: text boxes, icons, badges, pill buttons, signal path, equalizer, track row.

use common::catalog::{CatalogAlbum, CatalogTrack, SignalStep, SignalSummary, format_clock};
use common::playback::{PAUSED_EQUALIZER_LEVELS, equalizer_levels};
use common::tokens::ColorTokens;
use iced::alignment::{Horizontal, Vertical};
use iced::widget::{Space, button, column, container, row, stack, svg, text};
use iced::{Border, Color, Element, Length, Padding};

use crate::app::{App, Message};
use crate::assets::Icon;
use crate::style::{BODY, BODY_MEDIUM, CAPTION, HEADLINE, LABEL, Role, color, filled, flat_button};

/// Single-line text in a fixed-height box, vertically centered, clipped on overflow
/// (iced 0.14 has no ellipsis).
pub fn single_line<'a>(
    content: impl Into<String>,
    role: Role,
    text_color: Color,
    width: Length,
    alignment: Horizontal,
) -> Element<'a, Message> {
    let text_width = if width == Length::Shrink {
        Length::Shrink
    } else {
        Length::Fill
    };
    container(
        text(content.into())
            .font(role.font)
            .size(role.size)
            .line_height(text::LineHeight::Absolute(role.box_height.into()))
            .wrapping(text::Wrapping::None)
            .width(text_width)
            .align_x(alignment)
            .color(text_color),
    )
    .width(width)
    .height(role.box_height)
    .align_y(Vertical::Center)
    .clip(true)
    .into()
}

/// Square tinted icon.
pub fn icon<'a>(app: &App, which: Icon, size: f32, tint: Color) -> Element<'a, Message> {
    svg(app.icons.handle(which))
        .width(size)
        .height(size)
        .style(move |_, _| svg::Style { color: Some(tint) })
        .into()
}

/// Empty box of a fixed size.
pub fn spacer<'a>(width: impl Into<Length>, height: impl Into<Length>) -> Element<'a, Message> {
    Space::new().width(width).height(height).into()
}

/// Toolbar icon button (28/32/36 square, radius 6).
pub fn icon_button<'a>(
    app: &App,
    tokens: &'static ColorTokens,
    which: Icon,
    icon_size: f32,
    button_size: f32,
    tint: Color,
    message: Message,
) -> Element<'a, Message> {
    button(container(icon(app, which, icon_size, tint)).center(Length::Fill))
        .padding(0)
        .width(button_size)
        .height(button_size)
        .style(flat_button(
            None,
            tokens.fill_icon_hover,
            tokens.fill_icon_pressed,
            6.0,
        ))
        .on_press(message)
        .into()
}

/// Colors and text of one badge variant (section 12.2 of the plan).
pub struct BadgeSpec {
    pub icon: Icon,
    pub label: String,
    pub foreground: Color,
    pub background_token: u32,
}

pub fn signal_badge_spec(tokens: &ColorTokens, summary: &SignalSummary) -> BadgeSpec {
    if summary.is_bit_perfect {
        BadgeSpec {
            icon: Icon::SealCheckFill,
            label: summary.badge_label.clone(),
            foreground: color(tokens.status_ok),
            background_token: tokens.status_ok_tint,
        }
    } else {
        BadgeSpec {
            icon: Icon::SlidersHorizontal,
            label: summary.badge_label.clone(),
            foreground: color(tokens.text_primary),
            background_token: tokens.fill_control,
        }
    }
}

pub fn authenticity_badge_spec(tokens: &ColorTokens, album: &CatalogAlbum) -> BadgeSpec {
    if album.authenticity.is_authentic() {
        BadgeSpec {
            icon: Icon::SealCheckFill,
            label: "Authentique".to_string(),
            foreground: color(tokens.status_ok),
            background_token: tokens.status_ok_tint,
        }
    } else {
        BadgeSpec {
            icon: Icon::WarningFill,
            label: "Douteux".to_string(),
            foreground: color(tokens.status_caution),
            background_token: tokens.status_caution_tint,
        }
    }
}

const BADGE_HEIGHT: f32 = 20.0;
const BADGE_PADDING: f32 = 8.0;

fn badge_content<'a>(app: &App, spec: &BadgeSpec) -> Element<'a, Message> {
    row![
        icon(app, spec.icon, 12.0, spec.foreground),
        single_line(
            spec.label.clone(),
            LABEL,
            spec.foreground,
            Length::Shrink,
            Horizontal::Left
        ),
    ]
    .spacing(4)
    .align_y(Vertical::Center)
    .into()
}

/// Non-interactive badge pill.
pub fn badge<'a>(app: &App, spec: &BadgeSpec) -> Element<'a, Message> {
    container(badge_content(app, spec))
        .padding([0.0, BADGE_PADDING])
        .height(BADGE_HEIGHT)
        .align_y(Vertical::Center)
        .style(filled(spec.background_token, BADGE_HEIGHT / 2.0))
        .into()
}

/// Badge that opens the signal popover; hover draws a 1 px inside border in its foreground.
pub fn badge_button<'a>(app: &App, spec: &BadgeSpec, message: Message) -> Element<'a, Message> {
    let background_token = spec.background_token;
    let foreground = spec.foreground;
    button(
        container(badge_content(app, spec))
            .height(BADGE_HEIGHT)
            .align_y(Vertical::Center),
    )
    .padding([0.0, BADGE_PADDING])
    .style(move |_, status| {
        let is_hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(color(background_token).into()),
            border: Border {
                color: if is_hovered {
                    foreground
                } else {
                    Color::TRANSPARENT
                },
                width: 1.0,
                radius: (BADGE_HEIGHT / 2.0).into(),
            },
            ..Default::default()
        }
    })
    .on_press(message)
    .into()
}

/// Pill button 112x32; primary uses the accent fill.
pub fn pill_button<'a>(
    app: &App,
    tokens: &'static ColorTokens,
    which: Icon,
    label: &'static str,
    is_primary: bool,
    message: Message,
) -> Element<'a, Message> {
    let content_color = color(if is_primary {
        tokens.text_on_accent
    } else {
        tokens.accent
    });
    let style = if is_primary {
        flat_button(
            Some(tokens.accent),
            tokens.accent_hover,
            tokens.accent_pressed,
            16.0,
        )
    } else {
        flat_button(
            Some(tokens.fill_control),
            tokens.fill_control_hover,
            tokens.fill_icon_pressed,
            16.0,
        )
    };
    button(
        container(
            row![
                icon(app, which, 14.0, content_color),
                single_line(
                    label,
                    HEADLINE,
                    content_color,
                    Length::Shrink,
                    Horizontal::Left
                ),
            ]
            .spacing(6)
            .align_y(Vertical::Center),
        )
        .center(Length::Fill),
    )
    .padding(0)
    .width(112)
    .height(32)
    .style(style)
    .on_press(message)
    .into()
}

/// Five-row signal path (section 12.5).
pub fn signal_path<'a>(tokens: &'static ColorTokens, steps: &[SignalStep]) -> Element<'a, Message> {
    const ROW_HEIGHT: f32 = 32.0;
    const CONNECTOR_SEGMENT_HEIGHT: f32 = 12.0;
    let last_index = steps.len().saturating_sub(1);
    let rows = steps.iter().enumerate().map(|(index, step)| {
        let dot_token = if step.is_transparent {
            tokens.status_ok
        } else {
            tokens.status_caution
        };
        let connector = |top: f32| {
            container(
                container(spacer(2, CONNECTOR_SEGMENT_HEIGHT)).style(filled(tokens.separator, 0.0)),
            )
            .padding(Padding {
                top,
                left: 7.0,
                ..Padding::ZERO
            })
        };
        let mut layers = stack![].width(Length::Fill).height(ROW_HEIGHT);
        if index > 0 {
            layers = layers.push(connector(0.0));
        }
        if index < last_index {
            layers = layers.push(connector(20.0));
        }
        layers
            .push(
                container(container(spacer(8, 8)).style(filled(dot_token, 4.0))).padding(Padding {
                    top: 12.0,
                    left: 4.0,
                    ..Padding::ZERO
                }),
            )
            .push(
                container(
                    row![
                        single_line(
                            step.label,
                            BODY,
                            color(tokens.text_secondary),
                            Length::Fixed(72.0),
                            Horizontal::Left,
                        ),
                        single_line(
                            step.value.clone(),
                            BODY_MEDIUM,
                            color(tokens.text_primary),
                            Length::Fill,
                            Horizontal::Left,
                        ),
                    ]
                    .spacing(8),
                )
                .padding(Padding {
                    top: 8.0,
                    left: 24.0,
                    ..Padding::ZERO
                }),
            )
            .into()
    });
    column(rows.collect::<Vec<Element<'a, Message>>>()).into()
}

/// Three animated bars in the 24x16 number box (section 12.6).
fn equalizer_bars<'a>(app: &App, tokens: &'static ColorTokens) -> Element<'a, Message> {
    let levels = if app.playback.is_playing {
        equalizer_levels(app.equalizer_time_seconds)
    } else {
        PAUSED_EQUALIZER_LEVELS
    };
    let bars = levels.iter().map(|level| {
        let height = (3.0 + 9.0 * level).round();
        container(spacer(3, height))
            .style(filled(tokens.accent, 1.0))
            .into()
    });
    container(
        row![spacer(11, 1)]
            .extend(bars.collect::<Vec<Element<'a, Message>>>())
            .spacing(2)
            .align_y(Vertical::Bottom),
    )
    .width(24)
    .height(16)
    .padding(Padding {
        bottom: 2.0,
        ..Padding::ZERO
    })
    .align_y(Vertical::Bottom)
    .into()
}

/// One track row (section 12.4).
pub fn track_row<'a>(
    app: &App,
    tokens: &'static ColorTokens,
    album_index: usize,
    track_index: usize,
    track: &CatalogTrack,
) -> Element<'a, Message> {
    let is_current =
        app.playback.album_index == album_index && app.playback.track_index == track_index;
    let number_box: Element<'a, Message> = if is_current {
        equalizer_bars(app, tokens)
    } else {
        single_line(
            track.number.to_string(),
            BODY,
            color(tokens.text_secondary),
            Length::Fixed(24.0),
            Horizontal::Right,
        )
    };
    let title_role = if is_current { BODY_MEDIUM } else { BODY };
    let base_token = (track_index % 2 == 1).then_some(tokens.fill_row_zebra);
    container(
        button(
            container(
                row![
                    number_box,
                    single_line(
                        track.title.clone(),
                        title_role,
                        color(tokens.text_primary),
                        Length::Fill,
                        Horizontal::Left,
                    ),
                    single_line(
                        track.format.short_label(),
                        CAPTION,
                        color(tokens.text_secondary),
                        Length::Fixed(64.0),
                        Horizontal::Left,
                    ),
                    single_line(
                        format_clock(track.duration_seconds),
                        BODY,
                        color(tokens.text_secondary),
                        Length::Fixed(44.0),
                        Horizontal::Right,
                    ),
                ]
                .spacing(16)
                .align_y(Vertical::Center),
            )
            .padding([0.0, 12.0])
            .height(Length::Fill)
            .align_y(Vertical::Center),
        )
        .padding(0)
        .width(Length::Fill)
        .height(36)
        .style(flat_button(
            base_token,
            tokens.fill_row_hover,
            tokens.fill_row_pressed,
            6.0,
        ))
        .on_press(Message::TrackPressed(album_index, track_index)),
    )
    .padding([0.0, 32.0])
    .into()
}
