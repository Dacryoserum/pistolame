//! Runs Nyquist's analysis (`pistolame_analysis`) off the UI thread, one file at a time.
//!
//! The analysis decodes the whole file and runs FFT passes on several cores. It uses a pool
//! that leaves one core free, so the real-time audio thread is never starved, and it only ever
//! analyzes the most recent request: skipping through tracks does not queue stale work.

use std::path::PathBuf;

use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::authenticity::AuthenticityView;

/// The outcome for one file: the view, or why the file could not be analyzed.
pub(crate) type AnalysisOutcome = (PathBuf, Result<AuthenticityView, String>);

/// Owns the analysis thread; dropping it ends the thread after the current file.
pub(crate) struct AnalysisWorker {
    requests: Sender<PathBuf>,
    results: Receiver<AnalysisOutcome>,
}

impl AnalysisWorker {
    /// Starts the analysis thread. `None` if the system refuses a new thread (the page then
    /// says the analysis is unavailable).
    pub(crate) fn start() -> Option<AnalysisWorker> {
        let (requests, pending) = unbounded::<PathBuf>();
        let (finished, results) = unbounded();
        let threads = std::thread::available_parallelism()
            .map_or(1, |count| count.get().saturating_sub(1).max(1));
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|index| format!("pistolame-analysis-{index}"))
            .build()
            .ok()?;
        std::thread::Builder::new()
            .name("pistolame-analysis".into())
            .spawn(move || {
                while let Ok(first) = pending.recv() {
                    // Only the newest request matters.
                    let path = pending.try_iter().last().unwrap_or(first);
                    let outcome = pool.install(|| {
                        pistolame_analysis::analysis::analyze(&path)
                            .map(|result| crate::authenticity::view_of(&result))
                    });
                    if finished.send((path, outcome)).is_err() {
                        return;
                    }
                }
            })
            .ok()?;
        Some(AnalysisWorker { requests, results })
    }

    /// Asks for `path` to be analyzed.
    pub(crate) fn request(&self, path: PathBuf) {
        // The thread lives as long as the worker; a send only fails at exit.
        let _ = self.requests.send(path);
    }

    /// The analyses finished since the last call.
    pub(crate) fn take_results(&self) -> Vec<AnalysisOutcome> {
        self.results.try_iter().collect()
    }
}
