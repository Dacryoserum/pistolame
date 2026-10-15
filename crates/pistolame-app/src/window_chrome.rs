//! The window frame: a unified title bar on macOS, the system title bar elsewhere.
//!
//! On macOS the window content runs under a transparent title bar (like the system Music and
//! Finder apps): the window buttons sit in the sidebar and the transport toolbar is the top of
//! the window. The content then receives the clicks a title bar would take, so empty chrome
//! areas call `begin-window-drag`, which hands the drag to the system (and zooms on a double
//! click, the default macOS title-bar behavior). Windows and Linux keep their native frame.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::ComponentHandle;
use slint::winit_030::WinitWindowAccessor;

use crate::{AppState, AppWindow};

/// Room kept at the top of the sidebar for the macOS window buttons, in logical pixels: the
/// buttons are 14 px high and centered 18 px below the top of a 28 px title bar, plus a gap.
#[cfg(target_os = "macos")]
const TITLE_BAR_INSET_LOGICAL_PIXELS: f32 = 38.0;

/// Two presses closer than this are a double click. AppKit's default `doubleClickInterval` is
/// 0.5 s; the system setting is not exposed through winit.
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);

/// Selects the winit backend with a transparent, full-size title bar. Must run before the
/// first window is created; does nothing outside macOS.
pub(crate) fn select_backend() -> Result<(), slint::PlatformError> {
    #[cfg(target_os = "macos")]
    {
        use slint::winit_030::winit::platform::macos::WindowAttributesExtMacOS;
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .with_winit_window_attributes_hook(|attributes| {
                attributes
                    .with_titlebar_transparent(true)
                    .with_title_hidden(true)
                    .with_fullsize_content_view(true)
            })
            .select()?;
    }
    Ok(())
}

/// Tells the UI how much room the window buttons need and connects window dragging.
pub(crate) fn install(window: &AppWindow) {
    let state = window.global::<AppState>();
    #[cfg(target_os = "macos")]
    state.set_title_bar_inset(TITLE_BAR_INSET_LOGICAL_PIXELS);
    let last_press: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
    let weak = window.as_weak();
    state.on_begin_window_drag(move || {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let now = Instant::now();
        let is_double_click = last_press
            .get()
            .is_some_and(|previous| now.duration_since(previous) < DOUBLE_CLICK_INTERVAL);
        // A double click is complete: the next press starts a new pair.
        last_press.set(if is_double_click { None } else { Some(now) });
        window.window().with_winit_window(|winit_window| {
            if is_double_click {
                winit_window.set_maximized(!winit_window.is_maximized());
            } else {
                // Fails only when the button is no longer down; nothing to do then.
                let _ = winit_window.drag_window();
            }
        });
    });
}
