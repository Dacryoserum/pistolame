//! Pistolame desktop player using the approved album-oriented layout.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use slint::ComponentHandle;
mod album_artists;
mod album_sort;
mod analysis_worker;
mod artist_photos;
mod authenticity;
mod file_drop;
mod m3u;
mod media_controls;
mod navigation;
mod play_queue;
mod presentation;
mod process;
mod search;
mod session;
mod settings;
mod sleep_timer;
mod track_selection;
mod track_sort;
mod transport;
mod ui_bridge;
#[cfg(test)]
mod ui_preview;
mod updater;
mod window_chrome;
mod window_events;

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
    window_chrome::select_backend()?;
    let window = AppWindow::new()?;
    window_chrome::install(&window);
    let bridge = ui_bridge::UiBridge::new(&window)?;
    bridge.connect(&window);
    // A debug run must not overwrite the start section the user saved.
    #[cfg(debug_assertions)]
    if std::env::var_os("PISTOLAME_DEBUG_START_ITEM").is_some() {
        bridge.disable_remembering_start_section();
    }
    // Shows the stored library, then rescans it; paths given on the command line are added.
    let paths: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    bridge.synchronize_library(paths);
    // Development builds only: open on a sidebar item (1 = Artistes, 3 = Morceaux…, -1 = the
    // current track, -2 = the first album's editor) to check a
    // screen without clicking through the app.
    #[cfg(debug_assertions)]
    if let Some(item) = std::env::var("PISTOLAME_DEBUG_START_ITEM")
        .ok()
        .and_then(|item| item.parse::<i32>().ok())
    {
        if item < 0 {
            // Once the library (and the restored session) is shown.
            let weak = window.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_secs(3), move || {
                if let Some(window) = weak.upgrade() {
                    let state = window.global::<AppState>();
                    if item == -2 {
                        state.invoke_begin_album_edit(0);
                    } else {
                        state.invoke_show_now_playing();
                    }
                }
            });
        } else if let Some(section) = navigation::SidebarSection::from_legacy_debug_index(item) {
            window
                .global::<AppState>()
                .invoke_navigate_to_section(ui_bridge::slint_sidebar_section(section));
        }
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
