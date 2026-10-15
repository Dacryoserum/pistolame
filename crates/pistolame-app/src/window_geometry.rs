//! Reopens the window where and as large as it was at the last exit.

use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, LogicalPosition, LogicalSize};

use crate::AppWindow;
use crate::settings::WindowGeometry;

/// The smallest window the layout supports (`min-width` / `min-height` of `AppWindow`).
const MINIMUM_WIDTH_LOGICAL_PIXELS: i32 = 1100;
const MINIMUM_HEIGHT_LOGICAL_PIXELS: i32 = 700;
/// How much of the title bar must stay on a screen for a saved position to be kept, in
/// logical pixels: enough to grab the window.
const MINIMUM_VISIBLE_LOGICAL_PIXELS: i32 = 80;

/// Applies the saved size and position before the window is shown. Once shown, a position
/// left on a screen that is no longer connected is dropped (the system places the window).
pub(crate) fn restore(window: &AppWindow, geometry: WindowGeometry) {
    let width = geometry.width.max(MINIMUM_WIDTH_LOGICAL_PIXELS);
    let height = geometry.height.max(MINIMUM_HEIGHT_LOGICAL_PIXELS);
    window
        .window()
        .set_size(LogicalSize::new(width as f32, height as f32));
    window
        .window()
        .set_position(LogicalPosition::new(geometry.x as f32, geometry.y as f32));
    let weak = window.as_weak();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let is_on_a_screen = window
            .window()
            .with_winit_window(|winit_window| {
                let scale = winit_window.scale_factor();
                winit_window.available_monitors().any(|monitor| {
                    let position = monitor.position().to_logical::<f64>(scale);
                    let size = monitor.size().to_logical::<f64>(scale);
                    is_title_bar_visible(
                        geometry,
                        position.x as i32,
                        position.y as i32,
                        size.width as i32,
                        size.height as i32,
                    )
                })
            })
            .unwrap_or(true);
        if !is_on_a_screen {
            window
                .window()
                .set_position(LogicalPosition::new(80.0, 80.0));
        }
    });
}

/// The window's position and size now, `None` while minimized or maximized (the normal
/// geometry is then not the one to reopen with).
pub(crate) fn capture(window: &AppWindow) -> Option<WindowGeometry> {
    let slint_window = window.window();
    if slint_window.is_minimized() || slint_window.is_maximized() {
        return None;
    }
    let scale = slint_window.scale_factor();
    let position = slint_window.position().to_logical(scale);
    let size = slint_window.size().to_logical(scale);
    Some(WindowGeometry {
        x: position.x.round() as i32,
        y: position.y.round() as i32,
        width: size.width.round() as i32,
        height: size.height.round() as i32,
    })
}

/// True when the top strip of `geometry` overlaps the screen at (x, y) of that size enough to
/// grab the window.
fn is_title_bar_visible(
    geometry: WindowGeometry,
    screen_x: i32,
    screen_y: i32,
    screen_width: i32,
    screen_height: i32,
) -> bool {
    let overlap_width =
        (geometry.x + geometry.width).min(screen_x + screen_width) - geometry.x.max(screen_x);
    let is_top_on_screen = geometry.y >= screen_y && geometry.y < screen_y + screen_height;
    overlap_width >= MINIMUM_VISIBLE_LOGICAL_PIXELS && is_top_on_screen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window_at(x: i32, y: i32) -> WindowGeometry {
        WindowGeometry {
            x,
            y,
            width: 1280,
            height: 820,
        }
    }

    #[test]
    fn a_window_on_the_screen_is_kept() {
        assert!(is_title_bar_visible(window_at(100, 50), 0, 0, 1920, 1080));
    }

    #[test]
    fn a_window_on_a_screen_now_gone_is_not() {
        assert!(!is_title_bar_visible(window_at(2200, 50), 0, 0, 1920, 1080));
        assert!(!is_title_bar_visible(
            window_at(100, -900),
            0,
            0,
            1920,
            1080
        ));
    }

    #[test]
    fn a_window_mostly_off_the_side_is_kept_while_its_title_bar_can_be_grabbed() {
        assert!(is_title_bar_visible(window_at(1800, 50), 0, 0, 1920, 1080));
        assert!(!is_title_bar_visible(window_at(1870, 50), 0, 0, 1920, 1080));
    }
}
