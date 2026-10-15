//! Reopens the window where and as large as it was at the last exit.
//!
//! The position is kept in physical pixels: with screens at different scales (100 % and 150 %),
//! logical coordinates do not form one desktop space on Windows. The size is logical, so the
//! window keeps its layout size on a screen of another scale.

use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, LogicalSize, PhysicalPosition};

use crate::AppWindow;
use crate::settings::WindowGeometry;

/// The smallest window the layout supports (`min-width` / `min-height` of `AppWindow`).
const MINIMUM_WIDTH_LOGICAL_PIXELS: i32 = 1100;
const MINIMUM_HEIGHT_LOGICAL_PIXELS: i32 = 700;
/// How much of the title bar must stay on a screen for a saved position to be kept, in
/// physical pixels: enough to grab the window.
const MINIMUM_VISIBLE_PHYSICAL_PIXELS: i32 = 80;
/// Where a window whose screen is gone reopens, in physical pixels.
const FALLBACK_POSITION_PHYSICAL_PIXELS: i32 = 80;

/// A rectangle in physical pixels of the desktop.
#[derive(Clone, Copy, Debug)]
struct PhysicalRectangle {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

/// Applies the saved size and position before the window is shown. Once shown, a position
/// left on a screen that is no longer connected moves back to the main screen.
pub(crate) fn restore(window: &AppWindow, geometry: WindowGeometry) {
    let width = geometry.width.max(MINIMUM_WIDTH_LOGICAL_PIXELS);
    let height = geometry.height.max(MINIMUM_HEIGHT_LOGICAL_PIXELS);
    window
        .window()
        .set_size(LogicalSize::new(width as f32, height as f32));
    window
        .window()
        .set_position(PhysicalPosition::new(geometry.x, geometry.y));
    let weak = window.as_weak();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let is_on_a_screen = window
            .window()
            .with_winit_window(|winit_window| {
                let window_rectangle = PhysicalRectangle {
                    x: geometry.x,
                    y: geometry.y,
                    width: (f64::from(width) * winit_window.scale_factor()) as i32,
                    height: (f64::from(height) * winit_window.scale_factor()) as i32,
                };
                winit_window.available_monitors().any(|monitor| {
                    let position = monitor.position();
                    let size = monitor.size();
                    is_title_bar_visible(
                        window_rectangle,
                        PhysicalRectangle {
                            x: position.x,
                            y: position.y,
                            width: size.width as i32,
                            height: size.height as i32,
                        },
                    )
                })
            })
            .unwrap_or(true);
        if !is_on_a_screen {
            window.window().set_position(PhysicalPosition::new(
                FALLBACK_POSITION_PHYSICAL_PIXELS,
                FALLBACK_POSITION_PHYSICAL_PIXELS,
            ));
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
    let position = slint_window.position();
    let size = slint_window.size().to_logical(slint_window.scale_factor());
    Some(WindowGeometry {
        x: position.x,
        y: position.y,
        width: size.width.round() as i32,
        height: size.height.round() as i32,
    })
}

/// True when the top strip of `window` overlaps `screen` enough to grab the window.
fn is_title_bar_visible(window: PhysicalRectangle, screen: PhysicalRectangle) -> bool {
    let overlap_width =
        (window.x + window.width).min(screen.x + screen.width) - window.x.max(screen.x);
    let is_top_on_screen = window.y >= screen.y && window.y < screen.y + screen.height;
    overlap_width >= MINIMUM_VISIBLE_PHYSICAL_PIXELS && is_top_on_screen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window_at(x: i32, y: i32) -> PhysicalRectangle {
        PhysicalRectangle {
            x,
            y,
            width: 1280,
            height: 820,
        }
    }

    const MAIN_SCREEN: PhysicalRectangle = PhysicalRectangle {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };
    /// A 150 % screen on the right of the main one (physical pixels).
    const SECOND_SCREEN: PhysicalRectangle = PhysicalRectangle {
        x: 1920,
        y: 0,
        width: 3840,
        height: 2160,
    };

    #[test]
    fn a_window_on_the_screen_is_kept() {
        assert!(is_title_bar_visible(window_at(100, 50), MAIN_SCREEN));
        assert!(is_title_bar_visible(window_at(2400, 50), SECOND_SCREEN));
    }

    #[test]
    fn a_window_on_a_screen_now_gone_is_not() {
        assert!(!is_title_bar_visible(window_at(2400, 50), MAIN_SCREEN));
        assert!(!is_title_bar_visible(window_at(100, -900), MAIN_SCREEN));
    }

    #[test]
    fn a_window_mostly_off_the_side_is_kept_while_its_title_bar_can_be_grabbed() {
        assert!(is_title_bar_visible(window_at(1800, 50), MAIN_SCREEN));
        assert!(!is_title_bar_visible(window_at(1870, 50), MAIN_SCREEN));
    }
}
