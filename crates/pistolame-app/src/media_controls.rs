//! Media keys and the system "Now Playing" panel (audit 2026-10-13, §4 item 11): Windows SMTC,
//! macOS MPNowPlayingInfoCenter / MPRemoteCommandCenter, Linux MPRIS, all through `souvlaki`
//! (validated by `prototypes/spike-slint-backend`).
//!
//! The system calls back on its own threads; events only go into a channel that the UI tick
//! drains, so no system thread ever touches the UI. Everything here is best effort: when the
//! platform refuses, the app works the same without media keys.

use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};

use crate::AppWindow;

/// Name shown by the system next to the controls.
const DISPLAY_NAME: &str = "Pistolame";
/// MPRIS bus name suffix (Linux): `org.mpris.MediaPlayer2.pistolame`.
const DBUS_NAME: &str = "pistolame";
/// Step of the "seek" keys that give no amount (some keyboards, headsets).
const DEFAULT_SEEK_STEP: Duration = Duration::from_secs(10);

/// What a media key or the system panel asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MediaCommand {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Stop,
    /// Jump to this position in the current track.
    SeekTo(Duration),
    /// Move by this amount, forward or backward.
    SeekBy {
        is_forward: bool,
        amount: Duration,
    },
}

/// What the system shows about the transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransportStatus {
    Playing,
    Paused,
    Stopped,
}

/// The app's link with the system media controls. Lives on the UI thread.
pub(crate) struct SystemMediaControls {
    controls: Option<MediaControls>,
    has_tried_to_install: bool,
    sender: Sender<MediaCommand>,
    commands: Receiver<MediaCommand>,
}

impl SystemMediaControls {
    pub(crate) fn new() -> SystemMediaControls {
        let (sender, commands) = unbounded();
        SystemMediaControls {
            controls: None,
            has_tried_to_install: false,
            sender,
            commands,
        }
    }

    /// Registers with the system, once. Call it after the window is shown (Windows needs the
    /// native window). A failure leaves the app without media keys, silently. Returns whether
    /// the controls were installed by this call (the caller then publishes the current track).
    pub(crate) fn install_once(&mut self, window: &AppWindow) -> bool {
        if self.has_tried_to_install {
            return false;
        }
        self.has_tried_to_install = true;
        let config = PlatformConfig {
            display_name: DISPLAY_NAME,
            dbus_name: DBUS_NAME,
            hwnd: native_window_handle(window),
        };
        let Ok(mut controls) = MediaControls::new(config) else {
            return false;
        };
        let sender = self.sender.clone();
        let attached = controls.attach(move |event| {
            if let Some(command) = command_for(event) {
                // The receiver lives as long as the app; a send can only fail at exit.
                let _ = sender.send(command);
            }
        });
        if attached.is_err() {
            return false;
        }
        self.controls = Some(controls);
        true
    }

    /// The commands received since the last call, oldest first.
    pub(crate) fn take_commands(&self) -> Vec<MediaCommand> {
        self.commands.try_iter().collect()
    }

    /// Shows the current track in the system panel.
    pub(crate) fn publish_track(
        &mut self,
        title: &str,
        artist: &str,
        album: &str,
        duration: Duration,
    ) {
        if let Some(controls) = self.controls.as_mut() {
            let _ = controls.set_metadata(MediaMetadata {
                title: Some(title),
                artist: Some(artist),
                album: Some(album),
                duration: Some(duration),
                cover_url: None,
            });
        }
    }

    /// Shows whether the app plays, and where in the track.
    pub(crate) fn publish_status(&mut self, status: TransportStatus, position: Duration) {
        if let Some(controls) = self.controls.as_mut() {
            let progress = Some(MediaPosition(position));
            let _ = controls.set_playback(match status {
                TransportStatus::Playing => MediaPlayback::Playing { progress },
                TransportStatus::Paused => MediaPlayback::Paused { progress },
                TransportStatus::Stopped => MediaPlayback::Stopped,
            });
        }
    }
}

/// The app command for a system event; `None` for events the app does not handle (volume:
/// the level belongs to the DAC or to the in-app shared-mode slider).
fn command_for(event: MediaControlEvent) -> Option<MediaCommand> {
    Some(match event {
        MediaControlEvent::Play => MediaCommand::Play,
        MediaControlEvent::Pause => MediaCommand::Pause,
        MediaControlEvent::Toggle => MediaCommand::Toggle,
        MediaControlEvent::Next => MediaCommand::Next,
        MediaControlEvent::Previous => MediaCommand::Previous,
        MediaControlEvent::Stop => MediaCommand::Stop,
        MediaControlEvent::SetPosition(MediaPosition(position)) => MediaCommand::SeekTo(position),
        MediaControlEvent::Seek(direction) => MediaCommand::SeekBy {
            is_forward: direction == SeekDirection::Forward,
            amount: DEFAULT_SEEK_STEP,
        },
        MediaControlEvent::SeekBy(direction, amount) => MediaCommand::SeekBy {
            is_forward: direction == SeekDirection::Forward,
            amount,
        },
        MediaControlEvent::SetVolume(_)
        | MediaControlEvent::OpenUri(_)
        | MediaControlEvent::Raise
        | MediaControlEvent::Quit => return None,
    })
}

/// The HWND Windows SMTC attaches to. Other systems need no window.
#[cfg(windows)]
fn native_window_handle(window: &AppWindow) -> Option<*mut std::ffi::c_void> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::ComponentHandle;
    let slint_handle = window.window().window_handle();
    let handle = slint_handle.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(win32) => Some(win32.hwnd.get() as *mut std::ffi::c_void),
        _ => None,
    }
}

#[cfg(not(windows))]
fn native_window_handle(_window: &AppWindow) -> Option<*mut std::ffi::c_void> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_events_map_to_commands() {
        assert_eq!(
            command_for(MediaControlEvent::Toggle),
            Some(MediaCommand::Toggle)
        );
        assert_eq!(
            command_for(MediaControlEvent::Previous),
            Some(MediaCommand::Previous)
        );
        assert_eq!(
            command_for(MediaControlEvent::SetPosition(MediaPosition(
                Duration::from_secs(42)
            ))),
            Some(MediaCommand::SeekTo(Duration::from_secs(42)))
        );
    }

    #[test]
    fn seek_without_amount_uses_the_default_step() {
        assert_eq!(
            command_for(MediaControlEvent::Seek(SeekDirection::Backward)),
            Some(MediaCommand::SeekBy {
                is_forward: false,
                amount: DEFAULT_SEEK_STEP
            })
        );
    }

    #[test]
    fn volume_and_window_events_are_ignored() {
        assert_eq!(command_for(MediaControlEvent::SetVolume(0.5)), None);
        assert_eq!(command_for(MediaControlEvent::Quit), None);
    }
}
