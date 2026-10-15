//! Self-update: checks, download progress and install.

use super::*;

impl UiBridge {
    /// Asks GitHub for a newer release, in the background.
    pub(super) fn check_for_update(&self) {
        self.next_update_check
            .set(std::time::Instant::now() + UPDATE_CHECK_INTERVAL);
        self.is_update_worker_busy.set(true);
        let sender = self.update_sender.clone();
        let spawned = std::thread::Builder::new()
            .name("pistolame-update-check".into())
            .spawn(move || {
                let _ = sender.send(match crate::updater::check_for_update() {
                    Ok(update) => UpdateMessage::Checked(update),
                    Err(_) => UpdateMessage::CheckFailed,
                });
            });
        if spawned.is_err() {
            self.is_update_worker_busy.set(false);
        }
    }

    /// "Mettre à jour": download, verify and install in the background, then relaunch.
    pub(super) fn start_update(&self) {
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        if let Some(reason) = crate::updater::installation_blocker() {
            state.set_update_status(reason.into());
            return;
        }
        let Some(update) = self.available_update.borrow().clone() else {
            return;
        };
        state.set_is_updating(true);
        state.set_update_status("Connexion…".into());
        self.is_update_worker_busy.set(true);
        let work_folder = std::env::temp_dir().join("pistolame-update");
        *self.update_download.borrow_mut() = crate::updater::download_path(&update, &work_folder)
            .map(|path| (path, update.asset.size));
        let sender = self.update_sender.clone();
        let spawned = std::thread::Builder::new()
            .name("pistolame-update-install".into())
            .spawn(move || {
                let result = crate::updater::download_and_verify(&update, &work_folder)
                    .and_then(|archive| crate::updater::install(&archive, &work_folder));
                let _ = sender.send(match result {
                    Ok(installed) => UpdateMessage::Installed(installed),
                    Err(error) => UpdateMessage::InstallFailed(format!("{error:#}")),
                });
            });
        if spawned.is_err() {
            self.is_update_worker_busy.set(false);
            state.set_is_updating(false);
        }
    }

    /// "Téléchargement… 12,3 / 22,4 Mo" while the archive grows, then the verification step.
    pub(super) fn show_update_progress(&self) {
        let Some((path, size)) = self.update_download.borrow().clone() else {
            return;
        };
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let received = std::fs::metadata(&path).map_or(0, |metadata| metadata.len());
        if received == 0 {
            return;
        }
        let megabytes = |bytes: u64| format!("{:.1}", bytes as f64 / 1_000_000.0).replace('.', ",");
        let text = if size > 0 && received >= size {
            "Vérification de la signature et installation…".to_owned()
        } else if size > 0 {
            format!(
                "Téléchargement… {} / {} Mo",
                megabytes(received),
                megabytes(size)
            )
        } else {
            format!("Téléchargement… {} Mo", megabytes(received))
        };
        let state = window.global::<AppState>();
        let status: SharedString = text.into();
        if state.get_update_status() != status {
            state.set_update_status(status);
        }
    }

    pub(super) fn apply_update_message(&self, message: UpdateMessage) {
        if matches!(
            message,
            UpdateMessage::Installed(_) | UpdateMessage::InstallFailed(_)
        ) {
            *self.update_download.borrow_mut() = None;
        }
        self.is_update_worker_busy.set(false);
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let state = window.global::<AppState>();
        match message {
            UpdateMessage::Checked(Some(update)) => {
                state.set_update_check_status(
                    format!(
                        "La version {} est disponible : cliquez sur la pastille verte.",
                        update.version
                    )
                    .into(),
                );
                state.set_update_version(update.version.as_str().into());
                state.set_update_notes(update.notes.as_str().into());
                state.set_update_status("".into());
                state.set_is_update_available(true);
                *self.available_update.borrow_mut() = Some(update);
            }
            // No newer release, or no network: the settings page says so, nothing else does.
            UpdateMessage::Checked(None) => {
                state.set_update_check_status("Pistolame est à jour.".into());
            }
            UpdateMessage::CheckFailed => {
                state.set_update_check_status(
                    "Impossible de joindre GitHub. Nouvel essai plus tard.".into(),
                );
            }
            UpdateMessage::Installed(installed) => {
                state.set_update_status("Redémarrage…".into());
                self.engine.send(EngineCommand::Stop);
                match crate::updater::relaunch(&installed) {
                    Ok(()) => {
                        let _ = slint::quit_event_loop();
                    }
                    Err(error) => {
                        state.set_is_updating(false);
                        state.set_update_status(
                            format!("Mise à jour installée, mais {error:#}. Relancez Pistolame.")
                                .into(),
                        );
                    }
                }
            }
            UpdateMessage::InstallFailed(error) => {
                state.set_is_updating(false);
                state.set_update_status(format!("Échec de la mise à jour : {error}").into());
            }
        }
    }
}
