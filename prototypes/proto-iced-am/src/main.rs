//! Pistolame "Apple Music"-style mockup, iced 0.14 version.
//!
//! Known gaps compared with the Slint version (accepted by the plan, section 15 and 16.2):
//! - iced 0.14 has no accessibility tree (no AccessKit): no screen reader support.
//! - Single-line text is clipped, not elided with an ellipsis.
//! - Popover open/close and LCD hover reveal are instant, not animated.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod assets;
mod style;
mod view;

use app::App;
use iced::{Font, Size, Theme, window};

fn main() -> iced::Result {
    let mut application = iced::application(App::new, App::update, App::view)
        .title("Pistolame")
        .default_font(Font::with_name(assets::INTER_FAMILY_NAME))
        .window(window::Settings {
            size: Size::new(app::INITIAL_WINDOW_WIDTH, app::INITIAL_WINDOW_HEIGHT),
            min_size: Some(Size::new(1100.0, 700.0)),
            ..Default::default()
        })
        .theme(|app: &App| {
            if app.is_dark {
                Theme::Dark
            } else {
                Theme::Light
            }
        })
        .style(App::window_style)
        .subscription(App::subscription);
    for font_bytes in assets::FONT_FILES {
        application = application.font(font_bytes);
    }
    application.run()
}
