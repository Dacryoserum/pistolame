//! The text "Copier le diagnostic" puts on the clipboard: what a user pastes into an issue.

use std::path::Path;

/// Log lines included: enough to see what led to a problem, short enough to paste anywhere.
pub(crate) const LOG_LINES_IN_DIAGNOSTIC: usize = 200;
/// Shown in place of the user's home folder, so the text names no account.
const HOME_DIRECTORY_PLACEHOLDER: &str = "~";

/// Version and system, the latest crash report if any, then the end of the log. The user's home
/// folder is replaced by `~` everywhere: paths of music files and folders stay readable, the
/// account name does not leave the machine.
pub(crate) fn compose(
    log_text: &str,
    latest_crash_report: Option<&str>,
    home_directory: Option<&Path>,
) -> String {
    let mut text = format!(
        "Pistolame {} — {} {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    if let Some(report) = latest_crash_report {
        text.push_str("\n=== Latest crash report ===\n");
        text.push_str(report.trim_end());
        text.push('\n');
    }
    text.push_str(&format!(
        "\n=== Log (last {LOG_LINES_IN_DIAGNOSTIC} lines) ===\n"
    ));
    text.push_str(last_lines(log_text, LOG_LINES_IN_DIAGNOSTIC));
    if !text.ends_with('\n') {
        text.push('\n');
    }
    match home_directory.and_then(Path::to_str) {
        Some(home) if !home.is_empty() => text.replace(home, HOME_DIRECTORY_PLACEHOLDER),
        _ => text,
    }
}

/// The last `line_count` lines of `text` (all of it when shorter).
fn last_lines(text: &str, line_count: usize) -> &str {
    let trimmed = text.trim_end_matches('\n');
    let start = trimmed
        .rmatch_indices('\n')
        .nth(line_count.saturating_sub(1))
        .map_or(0, |(index, _)| index + 1);
    if line_count == 0 {
        ""
    } else {
        &trimmed[start..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_lines_keeps_the_end() {
        assert_eq!(last_lines("a\nb\nc\nd\n", 2), "c\nd");
        assert_eq!(last_lines("a\nb", 5), "a\nb");
        assert_eq!(last_lines("a\nb", 0), "");
        assert_eq!(last_lines("", 3), "");
    }

    #[test]
    fn diagnostic_starts_with_version_and_system() {
        let text = compose("line\n", None, None);
        assert!(text.starts_with(&format!("Pistolame {} — ", env!("CARGO_PKG_VERSION"))));
        assert!(text.ends_with("lines) ===\nline\n"));
        assert!(!text.contains("crash report"));
    }

    #[test]
    fn crash_report_comes_before_the_log() {
        let text = compose("log line", Some("Message: boom\n"), None);
        let report_position = text.find("Message: boom").unwrap_or(usize::MAX);
        let log_position = text.find("log line").unwrap_or(0);
        assert!(report_position < log_position);
    }

    #[test]
    fn home_folder_is_hidden() {
        let text = compose(
            "error shown: cannot read /Users/someone/Music/a.flac",
            None,
            Some(Path::new("/Users/someone")),
        );
        assert!(text.contains("cannot read ~/Music/a.flac"));
        assert!(!text.contains("someone"));
    }
}
