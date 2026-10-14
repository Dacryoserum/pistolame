//! Files and folders dropped from the Finder or the Explorer join the library (audit
//! 2026-10-13, §4 item 16). Validated by `prototypes/spike-slint-backend`.
//!
//! Slint has no drop API of its own; with the winit backend, `on_winit_window_event` sees the
//! raw `HoveredFile` / `DroppedFile` events. A dropped folder arrives as its path. Several dropped
//! items arrive as several events, so paths are only collected here and imported in one batch by
//! the UI tick. The winit hook itself lives in `window_events`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use slint::winit_030::winit::event::WindowEvent;

use crate::AppState;

/// Paths dropped since the last tick, shared with the window hook.
pub(crate) type DroppedPaths = Rc<RefCell<Vec<PathBuf>>>;

/// Reacts to the drag-and-drop events of one winit window event: shows the drop veil while
/// hovering and collects dropped paths into `dropped`. Other events are ignored. Called by the
/// single hook installed in `window_events`.
pub(crate) fn handle_window_event(
    state: &AppState<'_>,
    event: &WindowEvent,
    dropped: &DroppedPaths,
) {
    match event {
        WindowEvent::HoveredFile(_) => state.set_is_drop_hovering(true),
        WindowEvent::HoveredFileCancelled => state.set_is_drop_hovering(false),
        WindowEvent::DroppedFile(path) => {
            state.set_is_drop_hovering(false);
            dropped.borrow_mut().push(path.clone());
        }
        _ => {}
    }
}
