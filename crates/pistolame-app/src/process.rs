//! Child processes the app starts in the background (curl for updates and artist photos).

use std::process::Command;

/// Keeps Windows from flashing a console window for `command`: the app has no console of its
/// own, so every console program it starts would otherwise open one.
#[cfg(windows)]
pub(crate) fn hide_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NO_WINDOW`, from the Windows process creation flags.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

/// Other systems open no window for a child process.
#[cfg(not(windows))]
pub(crate) fn hide_console_window(_command: &mut Command) {}
