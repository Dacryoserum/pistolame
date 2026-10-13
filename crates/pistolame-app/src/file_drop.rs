//! Files and folders dropped from the Finder or the Explorer join the library (audit
//! 2026-10-13, §4 item 16). Validated by `prototypes/spike-slint-backend`.
//!
//! Slint has no drop API of its own; with the winit backend, `on_winit_window_event` sees the
//! raw `HoveredFile` / `DroppedFile` events. A dropped folder arrives as its path. Several dropped
//! items arrive as several events, so paths are only collected here and imported in one batch by
//! the UI tick.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint::ComponentHandle;
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::{EventResult, WinitWindowAccessor};

use crate::{AppState, AppWindow};

/// Paths dropped since the last tick, shared with the window hook.
pub(crate) type DroppedPaths = Rc<RefCell<Vec<PathBuf>>>;

/// Starts collecting dropped paths into `dropped` and shows the drop veil while hovering. Does
/// nothing on a backend other than winit (the app then imports through its menus only).
pub(crate) fn install(window: &AppWindow, dropped: DroppedPaths) {
    let weak = window.as_weak();
    window
        .window()
        .on_winit_window_event(move |_window, event| {
            let Some(window) = weak.upgrade() else {
                return EventResult::Propagate;
            };
            let state = window.global::<AppState>();
            match event {
                WindowEvent::HoveredFile(_) => state.set_is_drop_hovering(true),
                WindowEvent::HoveredFileCancelled => state.set_is_drop_hovering(false),
                WindowEvent::DroppedFile(path) => {
                    state.set_is_drop_hovering(false);
                    dropped.borrow_mut().push(path.clone());
                }
                _ => {}
            }
            // Slint must still see every event (resize, redraw, input).
            EventResult::Propagate
        });
}
