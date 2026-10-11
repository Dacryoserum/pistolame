//! Colors, text roles and reusable style functions.

use common::tokens::{ColorTokens, DARK, LIGHT, split_rgba};
use iced::widget::{button, container, scrollable};
use iced::{Background, Border, Color, Font, Shadow, Theme, Vector, border};

use crate::assets::{INTER, INTER_DISPLAY_BOLD, INTER_MEDIUM, INTER_SEMIBOLD};

/// Converts a `0xRRGGBBAA` design token to an iced color.
pub fn color(token: u32) -> Color {
    let [red, green, blue, alpha] = split_rgba(token);
    Color::from_rgba8(red, green, blue, f32::from(alpha) / 255.0)
}

/// Token table of the active theme.
pub fn tokens(is_dark: bool) -> &'static ColorTokens {
    if is_dark { &DARK } else { &LIGHT }
}

/// Typographic role of DESIGN.md section 3: font, size and single-line box height.
#[derive(Clone, Copy)]
pub struct Role {
    pub font: Font,
    pub size: f32,
    pub box_height: f32,
}

const fn role(font: Font, size: f32, box_height: f32) -> Role {
    Role {
        font,
        size,
        box_height,
    }
}

pub const DISPLAY: Role = role(INTER_DISPLAY_BOLD, 26.0, 32.0);
pub const TITLE_2: Role = role(INTER_MEDIUM, 20.0, 26.0);
pub const TITLE_3: Role = role(INTER_SEMIBOLD, 17.0, 22.0);
pub const POPOVER_TITLE: Role = role(INTER_SEMIBOLD, 15.0, 20.0);
pub const HEADLINE: Role = role(INTER_SEMIBOLD, 13.0, 16.0);
pub const BODY_MEDIUM: Role = role(INTER_MEDIUM, 13.0, 16.0);
pub const BODY: Role = role(INTER, 13.0, 16.0);
pub const CALLOUT: Role = role(INTER, 12.0, 16.0);
pub const CALLOUT_MEDIUM: Role = role(INTER_MEDIUM, 12.0, 16.0);
pub const LABEL: Role = role(INTER_SEMIBOLD, 11.0, 14.0);
pub const CAPTION: Role = role(INTER_MEDIUM, 11.0, 14.0);

/// Multi-line line heights (DESIGN.md: body 18, callout 17).
pub const BODY_MULTILINE_HEIGHT: f32 = 18.0;
pub const CALLOUT_MULTILINE_HEIGHT: f32 = 17.0;

/// Plain filled rounded box.
pub fn filled(token: u32, radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(color(token))),
        border: border::rounded(radius),
        ..Default::default()
    }
}

/// Flat button: optional base fill, hover and pressed fills.
pub fn flat_button(
    base_token: Option<u32>,
    hover_token: u32,
    pressed_token: u32,
    radius: f32,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let fill_token = match status {
            button::Status::Active | button::Status::Disabled => base_token,
            button::Status::Hovered => Some(hover_token),
            button::Status::Pressed => Some(pressed_token),
        };
        button::Style {
            background: fill_token.map(|token| Background::Color(color(token))),
            border: border::rounded(radius),
            ..Default::default()
        }
    }
}

/// Button without any visual state (the content draws its own hover).
pub fn invisible_button() -> impl Fn(&Theme, button::Status) -> button::Style {
    |_, _| button::Style::default()
}

/// Shadow of artwork (DESIGN.md level 1).
pub fn artwork_style(tokens: &'static ColorTokens) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(color(tokens.surface_content))),
        shadow: Shadow {
            color: color(tokens.shadow_artwork),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 8.0,
        },
        ..Default::default()
    }
}

/// Thin overlay scrollbar, transparent rail.
pub fn scrollbar_style(
    tokens: &'static ColorTokens,
) -> impl Fn(&Theme, scrollable::Status) -> scrollable::Style {
    move |theme, status| {
        let mut style = scrollable::default(theme, status);
        style.container = container::Style::default();
        for rail in [&mut style.vertical_rail, &mut style.horizontal_rail] {
            rail.background = None;
            rail.border = Border::default();
            rail.scroller.background = Background::Color(color(tokens.scrollbar_thumb));
            rail.scroller.border = border::rounded(3);
        }
        style
    }
}
