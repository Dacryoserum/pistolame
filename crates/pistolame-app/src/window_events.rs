//! The single raw window-event hook of the app.
//!
//! Slint keeps only ONE `on_winit_window_event` filter per window (a second call replaces the
//! first, `i-slint-backend-winit` 1.18.1 `window_event_filter.set`), so every feature that needs
//! raw winit events is dispatched from here: file drop (`file_drop`) and the mouse Back / Forward
//! buttons (navigation history).
//!
//! The hook runs on the UI thread, outside any bridge borrow, so invoking the Slint navigation
//! callbacks from it is as safe as a click handler.

use slint::ComponentHandle;
use slint::winit_030::winit::event::{ElementState, MouseButton, WindowEvent};
use slint::winit_030::{EventResult, WinitWindowAccessor};

use crate::file_drop::{self, DroppedPaths};
use crate::{AppState, AppWindow};

/// Installs the window hook: collects dropped paths into `dropped`, and turns the mouse Back /
/// Forward buttons into `navigate-back` / `navigate-forward`. Does nothing on a backend other
/// than winit (the app then imports through its menus only).
pub(crate) fn install(window: &AppWindow, dropped: DroppedPaths) {
    let weak = window.as_weak();
    window
        .window()
        .on_winit_window_event(move |_window, event| {
            let Some(window) = weak.upgrade() else {
                return EventResult::Propagate;
            };
            let state = window.global::<AppState>();
            file_drop::handle_window_event(&state, event, &dropped);
            handle_mouse_navigation_buttons(&state, event);
            // Slint must still see every event (resize, redraw, input).
            EventResult::Propagate
        });
}

/// Mouse button 4 (Back) and 5 (Forward), on press, navigate through the history.
fn handle_mouse_navigation_buttons(state: &AppState<'_>, event: &WindowEvent) {
    let WindowEvent::MouseInput {
        state: ElementState::Pressed,
        button,
        ..
    } = event
    else {
        return;
    };
    match button {
        MouseButton::Back => state.invoke_navigate_back(),
        MouseButton::Forward => state.invoke_navigate_forward(),
        _ => {}
    }
}
