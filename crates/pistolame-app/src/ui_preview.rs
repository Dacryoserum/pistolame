//! Manual rendering smoke test. It never starts audio playback.
#[cfg(test)]
mod tests {
    use crate::*;
    use slint::ComponentHandle;
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;
    use std::time::Duration;

    #[test]
    #[ignore = "opens a desktop window to render the UI without playing audio"]
    fn render_album_preview() -> anyhow::Result<()> {
        // The preview imports into a throwaway library, never into the user's.
        let database = std::env::temp_dir().join("pistolame-preview-library.sqlite3");
        let _ = std::fs::remove_file(&database);
        let window = AppWindow::new()?;
        let bridge = ui_bridge::UiBridge::with_library_database(&window, Some(database))?;
        bridge.connect(&window);
        if std::env::var_os("PISTOLAME_PREVIEW_LIGHT").is_some() {
            window.global::<AppState>().invoke_toggle_theme();
        }
        if std::env::var_os("PISTOLAME_PREVIEW_SMALL").is_some() {
            window
                .window()
                .set_size(slint::LogicalSize::new(1100.0, 700.0));
        }

        let root = std::env::var_os("PISTOLAME_PREVIEW_MUSIC").map(std::path::PathBuf::from);
        if let Some(root) = root {
            bridge.synchronize_library(vec![root]);
        }
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(33),
            move || bridge.tick(),
        );
        if std::env::var_os("PISTOLAME_PREVIEW_DETAIL").is_some() {
            let weak = window.as_weak();
            slint::Timer::single_shot(Duration::from_secs(1), move || {
                if let Some(window) = weak.upgrade() {
                    window.global::<AppState>().invoke_show_album(0);
                    if std::env::var_os("PISTOLAME_PREVIEW_QUALITY").is_some() {
                        window.global::<AppState>().set_detail_scroll_y(-1100.0);
                    }
                }
            });
        }
        // Sidebar item to show (1 = Artistes, 3 = Morceaux, 6 = Réglages…).
        if let Some(item) = std::env::var("PISTOLAME_PREVIEW_ITEM")
            .ok()
            .and_then(|item| item.parse::<i32>().ok())
        {
            window.global::<AppState>().invoke_select_item(item);
        }
        if std::env::var_os("PISTOLAME_PREVIEW_POPOVER").is_some() {
            window.global::<AppState>().set_is_popover_open(true);
        }
        let result = Rc::new(RefCell::new(None));
        let result_target = result.clone();
        let weak = window.as_weak();
        window.show()?;
        slint::Timer::single_shot(Duration::from_secs(4), move || {
            let capture = (|| -> anyhow::Result<()> {
                let window = weak
                    .upgrade()
                    .ok_or_else(|| anyhow::anyhow!("window closed"))?;
                let pixels = window.window().take_snapshot()?;
                let path = std::env::var_os("PISTOLAME_PREVIEW_OUTPUT")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::path::PathBuf::from("ui-preview.ppm"));
                let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
                write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
                for rgba in pixels.as_bytes().as_chunks::<4>().0 {
                    file.write_all(&rgba[..3])?;
                }
                file.flush()?;
                Ok(())
            })();
            *result_target.borrow_mut() = Some(capture);
            let _ = slint::quit_event_loop();
        });
        slint::run_event_loop()?;
        result
            .borrow_mut()
            .take()
            .ok_or_else(|| anyhow::anyhow!("snapshot timer did not fire"))?
    }
}
