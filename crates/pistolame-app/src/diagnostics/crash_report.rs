//! Crash reports: one text file per panic, written by the panic hook before the thread dies.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::timestamp::UtcDateTime;

const CRASH_REPORT_PREFIX: &str = "crash-";
const CRASH_REPORT_EXTENSION: &str = ".txt";
/// Reports kept in the folder; older ones are deleted at startup. Enough to see whether a crash
/// repeats, without letting a crash loop fill the disk.
pub(crate) const KEPT_CRASH_REPORTS: usize = 20;

/// What the panic hook knows about a panic, as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrashDetails {
    pub(crate) time: String,
    pub(crate) thread_name: String,
    pub(crate) message: String,
    pub(crate) location: String,
    pub(crate) backtrace: String,
}

impl CrashDetails {
    /// Reads the panic being reported. The backtrace is always captured, whatever
    /// `RUST_BACKTRACE` says: the user never sets it, and a report without it rarely helps.
    pub(crate) fn capture(panic: &std::panic::PanicHookInfo<'_>) -> Self {
        let message = panic
            .payload_as_str()
            .unwrap_or("(message that is not text)")
            .to_owned();
        let location = panic
            .location()
            .map_or_else(|| "(unknown)".to_owned(), |location| location.to_string());
        Self {
            time: UtcDateTime::now().log_text(),
            thread_name: std::thread::current()
                .name()
                .unwrap_or("(unnamed)")
                .to_owned(),
            message,
            location,
            backtrace: std::backtrace::Backtrace::force_capture().to_string(),
        }
    }

    /// One line for the log file, pointing at the full report.
    pub(crate) fn summary_line(&self) -> String {
        format!(
            "panic in thread '{}' at {}: {}",
            self.thread_name, self.location, self.message
        )
    }

    /// The report's text: what a user pastes into an issue.
    pub(crate) fn report_text(&self) -> String {
        format!(
            "Pistolame {version} crash report\n\
             Time: {time}\n\
             System: {os} {arch}\n\
             Thread: {thread}\n\
             Message: {message}\n\
             Location: {location}\n\
             \n\
             Backtrace:\n\
             {backtrace}\n",
            version = env!("CARGO_PKG_VERSION"),
            time = self.time,
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
            thread = self.thread_name,
            message = self.message,
            location = self.location,
            backtrace = self.backtrace,
        )
    }
}

/// Writes the report to `crash-<time>.txt` in `directory` and returns its path. Two panics in
/// the same second (several threads) go one after the other into the same file.
pub(crate) fn write_report(directory: &Path, details: &CrashDetails) -> std::io::Result<PathBuf> {
    fs::create_dir_all(directory)?;
    let path = directory.join(format!(
        "{CRASH_REPORT_PREFIX}{}{CRASH_REPORT_EXTENSION}",
        UtcDateTime::now().file_name_text()
    ));
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(details.report_text().as_bytes())?;
    Ok(path)
}

/// Deletes all but the `kept_reports` most recent reports. Names hold the time, so name order is
/// time order. Best effort: a report that cannot be deleted stays.
pub(crate) fn delete_old_reports(directory: &Path, kept_reports: usize) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut report_paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_crash_report(path))
        .collect();
    report_paths.sort();
    let deleted_count = report_paths.len().saturating_sub(kept_reports);
    for path in &report_paths[..deleted_count] {
        let _ = fs::remove_file(path);
    }
}

fn is_crash_report(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.starts_with(CRASH_REPORT_PREFIX) && name.ends_with(CRASH_REPORT_EXTENSION)
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn fresh_test_directory(test_name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "pistolame-crash-report-test-{}-{test_name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn sample_details() -> CrashDetails {
        CrashDetails {
            time: "2026-11-14 10:32:01Z".to_owned(),
            thread_name: "main".to_owned(),
            message: "already borrowed: BorrowMutError".to_owned(),
            location: "crates/pistolame-app/src/ui_bridge.rs:1090:17".to_owned(),
            backtrace: "   0: pistolame_app::main".to_owned(),
        }
    }

    #[test]
    fn report_names_version_thread_message_and_location() {
        let text = sample_details().report_text();
        assert!(text.starts_with(&format!(
            "Pistolame {} crash report\n",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(text.contains("Thread: main\n"));
        assert!(text.contains("Message: already borrowed: BorrowMutError\n"));
        assert!(text.contains("Location: crates/pistolame-app/src/ui_bridge.rs:1090:17\n"));
        assert!(text.contains("Backtrace:\n   0: pistolame_app::main\n"));
    }

    #[test]
    fn summary_fits_on_one_line() {
        assert_eq!(
            sample_details().summary_line(),
            "panic in thread 'main' at crates/pistolame-app/src/ui_bridge.rs:1090:17: \
             already borrowed: BorrowMutError"
        );
    }

    #[test]
    fn report_is_written_in_the_directory() {
        let directory = fresh_test_directory("write");
        let path = write_report(&directory, &sample_details()).unwrap();
        assert!(is_crash_report(&path));
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("Message: already borrowed")
        );
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn only_the_most_recent_reports_are_kept() {
        let directory = fresh_test_directory("delete-old");
        for hour in 10..15 {
            fs::write(
                directory.join(format!("crash-2026-11-14-{hour}0000.txt")),
                "",
            )
            .unwrap();
        }
        fs::write(directory.join("pistolame.log"), "").unwrap();
        delete_old_reports(&directory, 2);
        let mut remaining: Vec<String> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        remaining.sort();
        assert_eq!(
            remaining,
            [
                "crash-2026-11-14-130000.txt",
                "crash-2026-11-14-140000.txt",
                "pistolame.log"
            ]
        );
        fs::remove_dir_all(&directory).unwrap();
    }
}
