//! Pistolame desktop player using the approved album-oriented layout.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use slint::ComponentHandle;
mod album_sort;
mod m3u;
mod media_controls;
mod play_queue;
mod presentation;
mod search;
mod session;
mod settings;
mod track_sort;
mod transport;
mod ui_bridge;
#[cfg(test)]
mod ui_preview;
mod updater;

#[allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::todo,
    clippy::dbg_macro
)]
mod generated_ui {
    slint::include_modules!();
}
use generated_ui::*;

fn main() -> anyhow::Result<()> {
    settings::migrate_legacy_app_data();
    // The copy an update replaced is deleted once this new copy runs.
    updater::clean_up_previous_copy();
    let window = AppWindow::new()?;
    let bridge = ui_bridge::UiBridge::new(&window)?;
    bridge.connect(&window);
    // Shows the stored library, then rescans it; paths given on the command line are added.
    let paths: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    bridge.synchronize_library(paths);
    // Development builds only: open on a sidebar item (1 = Artistes, 3 = Morceaux…) to check a
    // screen without clicking through the app.
    #[cfg(debug_assertions)]
    if let Some(item) = std::env::var("PISTOLAME_DEBUG_START_ITEM")
        .ok()
        .and_then(|item| item.parse::<i32>().ok())
    {
        window.global::<AppState>().invoke_select_item(item);
    }
    let timer = slint::Timer::default();
    let ticking_bridge = bridge.clone();
    timer.start(
        slint::TimerMode::Repeated,
        pistolame_core::timing::UI_REFRESH_INTERVAL,
        move || ticking_bridge.tick(),
    );
    window.run()?;
    bridge.save_session();
    Ok(())
}

#[cfg(test)]
mod visual_comparison;
