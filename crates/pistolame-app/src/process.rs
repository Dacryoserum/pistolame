//! Child processes the app starts in the background (curl for updates and artist photos).

use std::path::Path;
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

/// The argument of `explorer /select`: only the path may be quoted. `Command::arg` would quote
/// the whole `/select,<path>` as soon as the path holds a space, and Explorer then ignores it
/// and opens its default folder.
fn explorer_select_argument(path: &Path) -> String {
    let path = path.to_string_lossy();
    let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
    format!("/select,\"{}\"", path.replace('/', r"\"))
}

/// Adds the argument that makes Explorer open `path`'s folder with the file selected.
#[cfg(windows)]
pub(crate) fn add_explorer_select_argument(command: &mut Command, path: &Path) {
    use std::os::windows::process::CommandExt;
    command.raw_arg(explorer_select_argument(path));
}

/// Explorer exists on Windows only.
#[cfg(not(windows))]
pub(crate) fn add_explorer_select_argument(command: &mut Command, path: &Path) {
    command.arg(explorer_select_argument(path));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explorer_select_quotes_the_path_only() {
        assert_eq!(
            explorer_select_argument(Path::new(r"D:\Musique\John Williams\01 - Main.flac")),
            r#"/select,"D:\Musique\John Williams\01 - Main.flac""#
        );
    }

    #[test]
    fn explorer_select_takes_plain_windows_paths_only() {
        assert_eq!(
            explorer_select_argument(Path::new(r"\\?\D:/Musique/a.flac")),
            r#"/select,"D:\Musique\a.flac""#
        );
    }
}
