//! Diagnostics wiring: "Copier le diagnostic" in Réglages, and the notice after a crash.

use super::*;

/// Shown at the start that follows a crash report. "Erreur interne" rather than "fermé": a panic
/// in a worker thread leaves the window open. Kept to one line: the message banner does not grow.
const PREVIOUS_CRASH_MESSAGE: &str =
    "Erreur interne lors de la dernière session : Réglages › Copier le diagnostic";

/// One second after the startup notices have left (they last `NOTICE_DISPLAY_DURATION`).
const PREVIOUS_CRASH_MESSAGE_DELAY: Duration =
    Duration::from_secs(NOTICE_DISPLAY_DURATION.as_secs() + 1);

impl UiBridge {
    pub(super) fn connect_diagnostics(self: &Rc<Self>, state: &AppState<'_>) {
        let handle = self.clone();
        state.on_copy_diagnostic(move || {
            match crate::diagnostics::copy_diagnostic_to_clipboard() {
                Ok(()) => {
                    log::info!("diagnostic copied to the clipboard");
                    handle.notice(
                        "Diagnostic copié : collez-le dans un ticket ou un message".to_owned(),
                    );
                }
                Err(error) => {
                    handle.message(format!("Impossible de copier le diagnostic : {error}"));
                }
            }
        });
    }

    /// Tells the user once that the previous session had a crash report. Waits for the startup
    /// notices (missing output device, restored session) to leave: a notice replaces the message,
    /// and the report is marked as seen only when the message is really shown.
    pub(crate) fn report_previous_crash(self: &Rc<Self>) {
        let handle = self.clone();
        slint::Timer::single_shot(PREVIOUS_CRASH_MESSAGE_DELAY, move || {
            if crate::diagnostics::take_unseen_crash_report() {
                handle.message(PREVIOUS_CRASH_MESSAGE.to_owned());
            }
        });
    }
}
