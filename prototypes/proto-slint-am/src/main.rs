//! Pistolame "Apple Music"-style mockup, Slint 1.18.1 version.
//!
//! Throwaway prototype: fake catalog and transport from the `common` crate, UI in `ui/`.
//! Accessibility labels are set on every interactive control (Slint ships AccessKit).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod controller;
mod models;
mod theme;

use controller::Controller;
use slint::{ComponentHandle, TimerMode};
use std::rc::Rc;
use std::time::Duration;

slint::include_modules!();

/// Frame period of the progress and equalizer updates (about 60 Hz).
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

fn main() -> Result<(), slint::PlatformError> {
    let window = AppWindow::new()?;
    let controller = Controller::new(&window);
    controller.initialize();
    wire_callbacks(&window, &controller);

    let timer = slint::Timer::default();
    let tick_controller = controller.clone();
    timer.start(TimerMode::Repeated, FRAME_INTERVAL, move || {
        tick_controller.tick();
    });

    window.run()
}

fn wire_callbacks(window: &AppWindow, controller: &Rc<Controller>) {
    let app_state = window.global::<AppState>();
    let handle = controller.clone();
    app_state.on_search_changed(move |query| handle.search_changed(&query));
    let handle = controller.clone();
    app_state.on_open_album(move |index| handle.open_album(index as usize));
    let handle = controller.clone();
    app_state.on_play_track(move |album, track| handle.play_track(album as usize, track as usize));
    let handle = controller.clone();
    app_state.on_play_detail_album(move || handle.play_detail_album());
    let handle = controller.clone();
    app_state.on_shuffle_detail_album(move || handle.shuffle_detail_album());
    let handle = controller.clone();
    app_state.on_toggle_play(move || handle.toggle_play());
    let handle = controller.clone();
    app_state.on_previous_track(move || handle.previous_track());
    let handle = controller.clone();
    app_state.on_next_track(move || handle.next_track());
    let handle = controller.clone();
    app_state.on_seek(move |fraction| handle.seek(fraction));
    let handle = controller.clone();
    app_state.on_volume_changed(move |volume| handle.set_volume(volume.clamp(0, 100) as u8));
    let handle = controller.clone();
    app_state.on_toggle_theme(move || handle.toggle_theme());
}
