//! Criterion 3: OS file and folder drop through Slint's winit hook.
//!
//! Slint has no drop API of its own; `unstable-winit-030` exposes
//! `WinitWindowAccessor::on_winit_window_event`, which sees raw winit events. A dropped folder
//! arrives as a normal `DroppedFile` carrying the folder path.

use crate::MainWindow;
use slint::winit_030::winit::event::WindowEvent;
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::rc::Rc;

pub fn install_file_drop(main_window: &MainWindow) {
    let dropped_paths = Rc::new(VecModel::<SharedString>::default());
    main_window.set_dropped_paths(ModelRc::from(dropped_paths.clone()));

    let window_handle = main_window.as_weak();
    main_window.window().on_winit_window_event(move |_window, event| {
        let Some(main_window) = window_handle.upgrade() else {
            return EventResult::Propagate;
        };
        match event {
            WindowEvent::HoveredFile(_) => main_window.set_is_drag_hovering(true),
            WindowEvent::HoveredFileCancelled => main_window.set_is_drag_hovering(false),
            WindowEvent::DroppedFile(path) => {
                main_window.set_is_drag_hovering(false);
                let kind = if path.is_dir() { "folder" } else { "file" };
                let line = format!("[{kind}] {}", path.display());
                println!("dropped {line}");
                dropped_paths.push(line.into());
            }
            _ => {}
        }
        // Slint must still see every event (resize, redraw, input).
        EventResult::Propagate
    });
}
