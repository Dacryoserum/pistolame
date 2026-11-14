//! Diagnostics: the log file and the crash reports, so a problem on a user's machine leaves a
//! trace. Both live in the app's `logs` folder (`settings::log_directory`).
//!
//! The app logs through the `log` crate's macros (`log::info!`, `log::warn!`…). Never from the
//! real-time audio thread: a log line takes a lock and writes to disk (AGENTS.md).

mod crash_report;
mod diagnostic_text;
mod log_file;
mod timestamp;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use log::{Level, LevelFilter, Log, Metadata, Record};

use crash_report::CrashDetails;
use log_file::RotatingLogFile;
use timestamp::UtcDateTime;

const LOG_FILE_NAME: &str = "pistolame.log";
/// Size of each of the two log files. At the default level a session writes a few kilobytes, so
/// this holds weeks of use, and stays small enough to attach to an issue.
const MAXIMUM_LOG_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Overrides the level (`error`, `warn`, `info`, `debug`, `trace`), to ask a user for details.
const LOG_LEVEL_VARIABLE: &str = "PISTOLAME_LOG";
/// Module paths of the project's own crates (`pistolame_app::…`, `pistolame_core::…`).
const OWN_CRATES_TARGET_PREFIX: &str = "pistolame";
/// Other crates (Slint, winit…) only reach the file from this level: their `info` is noise.
const OTHER_CRATES_LEVEL: Level = Level::Warn;

static LOGGER: OnceLock<FileLogger> = OnceLock::new();

thread_local! {
    /// Kept for the whole session: on Linux the copied text is served by the app that copied it,
    /// and disappears when its clipboard handle is dropped (arboard's documentation).
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
}

/// Starts the log file and the panic hook. Call it once, first thing in `main`, after the app
/// folder has been moved from its legacy place (creating `logs` earlier would stop that move).
/// Never fails: without a log folder the app runs as before, without a log.
pub(crate) fn install() {
    let Some(directory) = crate::settings::log_directory() else {
        return;
    };
    install_panic_hook(directory.clone());
    install_logger(&directory);
    crash_report::delete_old_reports(&directory, crash_report::KEPT_CRASH_REPORTS);
    log::info!(
        "Pistolame {} started on {} {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
}

/// The text for "Copier le diagnostic": version, latest crash report, end of the log, with the
/// home folder hidden (see `diagnostic_text::compose`).
pub(crate) fn diagnostic_text() -> String {
    let directory = crate::settings::log_directory();
    let log_text = directory
        .as_deref()
        .map(|directory| log_file::read_both_files(directory, LOG_FILE_NAME))
        .unwrap_or_default();
    let latest_report = directory
        .as_deref()
        .and_then(crash_report::latest_report)
        .and_then(|path| std::fs::read(path).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    diagnostic_text::compose(
        &log_text,
        latest_report.as_deref(),
        dirs::home_dir().as_deref(),
    )
}

/// Puts [`diagnostic_text`] on the system clipboard. Called from the UI thread only.
pub(crate) fn copy_diagnostic_to_clipboard() -> Result<(), arboard::Error> {
    let text = diagnostic_text();
    CLIPBOARD.with(|cell| {
        let mut clipboard = cell.borrow_mut();
        let clipboard = match clipboard.as_mut() {
            Some(clipboard) => clipboard,
            None => clipboard.insert(arboard::Clipboard::new()?),
        };
        clipboard.set_text(text)
    })
}

/// Whether a crash report appeared since the user was last told about one. True once per report.
pub(crate) fn take_unseen_crash_report() -> bool {
    crate::settings::log_directory()
        .and_then(|directory| crash_report::take_unseen_report(&directory))
        .is_some()
}

fn install_logger(directory: &Path) {
    let file = match RotatingLogFile::open(directory, LOG_FILE_NAME, MAXIMUM_LOG_FILE_BYTES) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("Pistolame: no log file in {}: {error}", directory.display());
            return;
        }
    };
    let maximum_level = configured_level(std::env::var(LOG_LEVEL_VARIABLE).ok().as_deref());
    let logger = LOGGER.get_or_init(|| FileLogger {
        file,
        maximum_level,
    });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(maximum_level);
    }
}

/// `info` by default, `debug` in development builds; the environment variable overrides both.
fn configured_level(variable_value: Option<&str>) -> LevelFilter {
    let default_level = if cfg!(debug_assertions) {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };
    variable_value
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(default_level)
}

/// Writes a crash report for every panic, on any thread, then lets Rust's own hook print its
/// message as before. A worker thread that panics no longer dies unseen either.
fn install_panic_hook(directory: PathBuf) {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let details = CrashDetails::capture(panic);
        let report = crash_report::write_report(&directory, &details);
        if let Some(logger) = LOGGER.get() {
            let report_text = match &report {
                // The file name only: the full path holds the user's account name, and this
                // line is meant to be pasted into a public issue.
                Ok(path) => format!(
                    "report: {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ),
                Err(error) => format!("no report written: {error}"),
            };
            let line = format_line(
                &UtcDateTime::now(),
                Level::Error,
                "pistolame_app::panic",
                &format!("{} ({report_text})", details.summary_line()),
            );
            logger.file.try_append_line(&line);
        }
        previous_hook(panic);
    }));
}

/// The `log` crate's sink: filters by level and crate, then appends to the file.
struct FileLogger {
    file: RotatingLogFile,
    maximum_level: LevelFilter,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        is_logged(metadata.level(), metadata.target(), self.maximum_level)
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format_line(
            &UtcDateTime::now(),
            record.level(),
            record.target(),
            &record.args().to_string(),
        );
        // A development run also shows it in the terminal.
        if cfg!(debug_assertions) {
            eprintln!("{line}");
        }
        self.file.append_line(&line);
    }

    fn flush(&self) {}
}

/// Whether an event reaches the file: the project's crates down to `maximum_level`, the others
/// only from `OTHER_CRATES_LEVEL`.
fn is_logged(level: Level, target: &str, maximum_level: LevelFilter) -> bool {
    let is_own_crate = target.starts_with(OWN_CRATES_TARGET_PREFIX);
    level <= maximum_level && (is_own_crate || level <= OTHER_CRATES_LEVEL)
}

/// `2026-11-14 10:32:01Z WARN  pistolame_app::ui_bridge: message`. A multi-line message keeps
/// its following lines indented, so every new event still starts with a date.
fn format_line(time: &UtcDateTime, level: Level, target: &str, message: &str) -> String {
    format!(
        "{} {level:<5} {target}: {}",
        time.log_text(),
        message.replace('\n', "\n    ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_comes_from_the_variable_when_it_is_valid() {
        assert_eq!(configured_level(Some("warn")), LevelFilter::Warn);
        assert_eq!(configured_level(Some(" TRACE ")), LevelFilter::Trace);
    }

    #[test]
    fn invalid_or_missing_variable_keeps_the_default_level() {
        let default_level = configured_level(None);
        assert_eq!(configured_level(Some("loud")), default_level);
        assert!(default_level >= LevelFilter::Info);
    }

    #[test]
    fn other_crates_reach_the_file_only_from_warnings() {
        let maximum_level = LevelFilter::Debug;
        assert!(is_logged(
            Level::Debug,
            "pistolame_app::ui_bridge",
            maximum_level
        ));
        assert!(!is_logged(
            Level::Trace,
            "pistolame_app::ui_bridge",
            maximum_level
        ));
        assert!(!is_logged(Level::Info, "winit::event_loop", maximum_level));
        assert!(is_logged(Level::Warn, "winit::event_loop", maximum_level));
    }

    #[test]
    fn continuation_lines_are_indented() {
        let line = format_line(
            &UtcDateTime::now(),
            Level::Warn,
            "pistolame_app",
            "first\nsecond",
        );
        assert!(line.ends_with("WARN  pistolame_app: first\n    second"));
    }
}
