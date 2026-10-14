//! Watches the render endpoints through `IMMNotificationClient` and calls the engine's listener
//! when the default render device changes or a device is added, removed, enabled or disabled
//! (`AudioOutput::watch_device_changes`).
//!
//! - IMMNotificationClient: <https://learn.microsoft.com/en-us/windows/win32/api/mmdeviceapi/nn-mmdeviceapi-immnotificationclient>
//!   The callbacks run on a thread of the audio service client; they must not block, so they
//!   only call the engine's listener, which posts a wake-up.
//! - OnDefaultDeviceChanged: <https://learn.microsoft.com/en-us/windows/win32/api/mmdeviceapi/nf-mmdeviceapi-immnotificationclient-ondefaultdevicechanged>
//!   It is called once per role; only `eRender` / `eConsole` matters, the role
//!   `DeviceEnumerator::default_render_device_id` reads.
//!
//! The enumerator and the client are COM objects bound to the thread that created them, so a
//! small dedicated thread registers the client, waits until the backend is dropped, then
//! unregisters it ("the client must call UnregisterEndpointNotificationCallback ... before
//! releasing the IMMNotificationClient", RegisterEndpointNotificationCallback remarks).

use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, bounded};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    DEVICE_STATE, EDataFlow, ERole, IMMNotificationClient, IMMNotificationClient_Impl, eConsole,
    eRender,
};
use windows::core::{PCWSTR, implement};

use super::com_apartment::ComApartment;
use super::devices::DeviceEnumerator;
use crate::DeviceChangeListener;

/// Name of the thread that holds the registration.
const WATCH_THREAD_NAME: &str = "pistolame-wasapi-endpoints";

/// Receives the endpoint notifications.
#[implement(IMMNotificationClient)]
struct EndpointChangeClient {
    on_change: DeviceChangeListener,
}

impl IMMNotificationClient_Impl for EndpointChangeClient_Impl {
    fn OnDeviceStateChanged(
        &self,
        _device_id: &PCWSTR,
        _new_state: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        (self.on_change)();
        Ok(())
    }

    fn OnDeviceAdded(&self, _device_id: &PCWSTR) -> windows::core::Result<()> {
        (self.on_change)();
        Ok(())
    }

    fn OnDeviceRemoved(&self, _device_id: &PCWSTR) -> windows::core::Result<()> {
        (self.on_change)();
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _default_device_id: &PCWSTR,
    ) -> windows::core::Result<()> {
        if flow == eRender && role == eConsole {
            (self.on_change)();
        }
        Ok(())
    }

    /// Property changes (volume, format, names) do not change which device to open.
    fn OnPropertyValueChanged(
        &self,
        _device_id: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

/// The running registration; unregistered when dropped.
#[derive(Debug)]
pub(crate) struct EndpointWatch {
    /// Dropping it wakes the watch thread, which unregisters and ends.
    stop_sender: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl EndpointWatch {
    /// Registers `on_change` on a dedicated thread. `None` when COM, the enumerator or the
    /// registration failed: the engine then re-reads the default at each open.
    pub(crate) fn start(on_change: DeviceChangeListener) -> Option<EndpointWatch> {
        let (stop_sender, stop_receiver) = bounded::<()>(0);
        let (ready_sender, ready_receiver) = bounded::<bool>(1);
        let thread = std::thread::Builder::new()
            .name(WATCH_THREAD_NAME.to_owned())
            .spawn(move || watch_until_stopped(on_change, &ready_sender, &stop_receiver))
            .ok()?;
        let watch = EndpointWatch {
            stop_sender: Some(stop_sender),
            thread: Some(thread),
        };
        // A failed start drops `watch`, which joins the thread (it has already returned).
        ready_receiver.recv().unwrap_or(false).then_some(watch)
    }
}

impl Drop for EndpointWatch {
    fn drop(&mut self) {
        drop(self.stop_sender.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The watch thread: registers, reports whether it worked, blocks until the backend is
/// dropped, unregisters. COM objects are declared after the apartment, so they are released
/// before it ends.
fn watch_until_stopped(
    on_change: DeviceChangeListener,
    ready_sender: &Sender<bool>,
    stop_receiver: &Receiver<()>,
) {
    let Ok(_com_apartment) = ComApartment::initialize_multithreaded() else {
        let _ = ready_sender.send(false);
        return;
    };
    let Ok(enumerator) = DeviceEnumerator::new() else {
        let _ = ready_sender.send(false);
        return;
    };
    let client: IMMNotificationClient = EndpointChangeClient { on_change }.into();
    if enumerator.register_endpoint_notifications(&client).is_err() {
        let _ = ready_sender.send(false);
        return;
    }
    let _ = ready_sender.send(true);
    // Returns when the sender is dropped (nothing is ever sent).
    let _ = stop_receiver.recv();
    let _ = enumerator.unregister_endpoint_notifications(&client);
}
