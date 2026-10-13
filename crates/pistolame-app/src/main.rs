//! Pistolame desktop player using the approved album-oriented layout.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use slint::ComponentHandle;
mod presentation;
mod settings;
mod transport;
mod ui_bridge;
#[cfg(test)]
mod ui_preview;

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
    let window = AppWindow::new()?;
    let bridge = ui_bridge::UiBridge::new(&window)?;
    bridge.connect(&window);
    // Shows the stored library, then rescans it; paths given on the command line are added.
    let paths: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    bridge.synchronize_library(paths);
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        pistolame_core::timing::UI_REFRESH_INTERVAL,
        move || bridge.tick(),
    );
    window.run()?;
    Ok(())
}

#[cfg(test)]
mod visual_comparison;
