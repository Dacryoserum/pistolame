//! Criterion 5 (shared state) and criterion 4 (media keys): one fake transport whose state is
//! pushed to the main window and the mini-player, whatever the source of the command.

use crate::{MainWindow, MiniPlayerWindow};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::{ComponentHandle, Weak};
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, PlatformConfig};
use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::{Arc, Mutex, MutexGuard};

/// What can happen to the transport; sent by buttons, the tray menu and media keys.
#[derive(Clone, Copy, Debug)]
pub enum Command {
    Play,
    Pause,
    Toggle,
    Next,
}

/// Where a command came from, shown in the UI to prove the right path was exercised.
#[derive(Clone, Copy, Debug)]
pub enum CommandSource {
    Window,
    Tray,
    MediaKey,
}

#[derive(Clone, Copy)]
struct TransportState {
    is_playing: bool,
    track_number: u32,
}

/// Cheap to clone and `Send`: every clone drives the same two windows.
#[derive(Clone)]
pub struct Transport {
    main_window: Weak<MainWindow>,
    mini_window: Weak<MiniPlayerWindow>,
    state: Arc<Mutex<TransportState>>,
}

thread_local! {
    /// SMTC objects are not `Send`; they live on the UI thread and are updated from there.
    static MEDIA_CONTROLS: RefCell<Option<MediaControls>> = const { RefCell::new(None) };
}

impl Transport {
    pub fn new(main_window: &MainWindow, mini_window: &MiniPlayerWindow) -> Self {
        let transport = Self {
            main_window: main_window.as_weak(),
            mini_window: mini_window.as_weak(),
            state: Arc::new(Mutex::new(TransportState { is_playing: false, track_number: 1 })),
        };
        transport.dispatch(None, CommandSource::Window);
        transport
    }

    fn lock_state(&self) -> MutexGuard<'_, TransportState> {
        // A poisoned lock only means another thread panicked; the plain-data state is still valid.
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Applies `command` (or only refreshes when `None`) and updates every window. Callable from
    /// any thread.
    pub fn dispatch(&self, command: Option<Command>, source: CommandSource) {
        let state = {
            let mut state = self.lock_state();
            match command {
                Some(Command::Play) => state.is_playing = true,
                Some(Command::Pause) => state.is_playing = false,
                Some(Command::Toggle) => state.is_playing = !state.is_playing,
                Some(Command::Next) => state.track_number += 1,
                None => {}
            }
            *state
        };
        let (main_window, mini_window) = (self.main_window.clone(), self.mini_window.clone());
        let event_text = command.map(|command| format!("{command:?} from {source:?}"));
        let update_result = slint::invoke_from_event_loop(move || {
            let now_playing = format!("Track {}", state.track_number);
            if let Some(main_window) = main_window.upgrade() {
                main_window.set_is_playing(state.is_playing);
                main_window.set_now_playing(now_playing.clone().into());
                if let (Some(text), CommandSource::MediaKey) = (event_text, source) {
                    main_window.set_media_key_text(text.into());
                }
            }
            if let Some(mini_window) = mini_window.upgrade() {
                mini_window.set_is_playing(state.is_playing);
                mini_window.set_now_playing(now_playing.clone().into());
            }
            publish_to_system_media_controls(state, &now_playing);
        });
        if let Err(error) = update_result {
            eprintln!("transport update dropped, event loop gone: {error}");
        }
    }
}

fn publish_to_system_media_controls(state: TransportState, title: &str) {
    MEDIA_CONTROLS.with(|cell| {
        let mut slot = cell.borrow_mut();
        let Some(controls) = slot.as_mut() else { return };
        let playback = if state.is_playing {
            MediaPlayback::Playing { progress: None }
        } else {
            MediaPlayback::Paused { progress: None }
        };
        let metadata = MediaMetadata {
            title: Some(title),
            artist: Some("Spike artist"),
            album: Some("Spike album"),
            ..MediaMetadata::default()
        };
        if let Err(error) = controls.set_metadata(metadata) {
            eprintln!("SMTC set_metadata failed: {error}");
        }
        if let Err(error) = controls.set_playback(playback) {
            eprintln!("SMTC set_playback failed: {error}");
        }
    });
}

fn media_event_to_command(event: &MediaControlEvent) -> Option<Command> {
    match event {
        MediaControlEvent::Play => Some(Command::Play),
        MediaControlEvent::Pause => Some(Command::Pause),
        MediaControlEvent::Toggle => Some(Command::Toggle),
        MediaControlEvent::Next => Some(Command::Next),
        _ => None,
    }
}

/// Reads the HWND from Slint's `window_handle()`. `None` until the native window exists.
fn native_window_handle(main_window: &MainWindow) -> Result<*mut c_void, String> {
    let slint_handle = main_window.window().window_handle();
    let handle = slint_handle.window_handle().map_err(|error| error.to_string())?;
    match handle.as_raw() {
        RawWindowHandle::Win32(win32) => Ok(win32.hwnd.get() as *mut c_void),
        other => Err(format!("not a Win32 handle: {other:?}")),
    }
}

/// Registers the window with SMTC and routes media key events into `transport`.
/// Must run on the UI thread once the window is shown. Returns a status line for the UI.
pub fn install_media_keys(main_window: &MainWindow, transport: Transport) -> String {
    let hwnd = match native_window_handle(main_window) {
        Ok(hwnd) => hwnd,
        Err(error) => return format!("no HWND: {error}"),
    };
    let config = PlatformConfig {
        display_name: "Pistolame spike",
        dbus_name: "pistolame_spike",
        hwnd: Some(hwnd),
    };
    let mut controls = match MediaControls::new(config) {
        Ok(controls) => controls,
        Err(error) => return format!("SMTC init failed: {error}"),
    };
    // The handler runs on a Windows thread-pool thread; `Transport` is `Send` for that reason.
    let attach_result = controls.attach(move |event| {
        match media_event_to_command(&event) {
            Some(command) => transport.dispatch(Some(command), CommandSource::MediaKey),
            None => eprintln!("ignored media event: {event:?}"),
        }
    });
    if let Err(error) = attach_result {
        return format!("SMTC attach failed: {error}");
    }
    MEDIA_CONTROLS.with(|cell| *cell.borrow_mut() = Some(controls));
    "SMTC ready, press a media key".to_string()
}
