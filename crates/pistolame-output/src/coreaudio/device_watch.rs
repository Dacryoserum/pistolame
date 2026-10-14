//! Watches the audio system for a new default output device or an output device added or
//! removed, and calls the engine's listener (`AudioOutput::watch_device_changes`).
//!
//! Reference: `AudioHardware.h` (CoreAudio framework): `kAudioHardwarePropertyDefaultOutputDevice`,
//! `kAudioHardwarePropertyDevices`, `AudioObjectAddPropertyListener`. System notifications are
//! delivered on the process's main run loop ("In 10.6 and later, the HAL will use the process's
//! run loop (as defined by CFRunLoopGetMain())", `kAudioHardwarePropertyRunLoop` in
//! `AudioHardwareDeprecated.h`), which the app's UI runs. They never reach the real-time thread.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;

use objc2_core_audio::{
    AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectPropertySelector, AudioObjectRemovePropertyListener,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyScopeGlobal,
};

use super::devices::SYSTEM_OBJECT;
use super::properties::{NO_ERROR, property_address};
use crate::DeviceChangeListener;

/// The system properties whose change means the device to open may have changed.
const WATCHED_SYSTEM_PROPERTIES: [AudioObjectPropertySelector; 2] = [
    kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyDevices,
];

/// The registered system listeners; removed when dropped.
pub(crate) struct SystemDeviceWatch {
    /// Deliberately leaked once registered (one per backend, a few bytes): CoreAudio documents
    /// no guarantee that a notification in flight has finished when
    /// `AudioObjectRemovePropertyListener` returns, so freeing it could leave a dangling pointer
    /// (same rule as the stream's device listener).
    listener: &'static DeviceChangeListener,
    registered_properties: Vec<AudioObjectPropertyAddress>,
}

impl SystemDeviceWatch {
    /// Registers `on_change` for every watched property. `None` when the default-output
    /// property could not be watched: the engine then re-reads the default at each open.
    pub(crate) fn start(on_change: DeviceChangeListener) -> Option<SystemDeviceWatch> {
        let listener: &'static DeviceChangeListener = Box::leak(Box::new(on_change));
        let mut registered_properties = Vec::new();
        for selector in WATCHED_SYSTEM_PROPERTIES {
            let address = property_address(selector, kAudioObjectPropertyScopeGlobal);
            // SAFETY: the system object and the address are valid; the client data is the
            // leaked listener, which outlives every notification.
            let status = unsafe {
                AudioObjectAddPropertyListener(
                    SYSTEM_OBJECT,
                    NonNull::from(&address),
                    Some(system_device_listener),
                    client_data(listener),
                )
            };
            if status == NO_ERROR {
                registered_properties.push(address);
            }
        }
        let watch = SystemDeviceWatch {
            listener,
            registered_properties,
        };
        let is_default_watched = watch
            .registered_properties
            .iter()
            .any(|address| address.mSelector == kAudioHardwarePropertyDefaultOutputDevice);
        // Dropping a partial watch removes what was registered.
        is_default_watched.then_some(watch)
    }
}

impl Drop for SystemDeviceWatch {
    fn drop(&mut self) {
        for address in &self.registered_properties {
            // SAFETY: same arguments as the registration; the listener stays leaked.
            let _ = unsafe {
                AudioObjectRemovePropertyListener(
                    SYSTEM_OBJECT,
                    NonNull::from(address),
                    Some(system_device_listener),
                    client_data(self.listener),
                )
            };
        }
    }
}

impl std::fmt::Debug for SystemDeviceWatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SystemDeviceWatch")
            .field("watched_property_count", &self.registered_properties.len())
            .finish_non_exhaustive()
    }
}

fn client_data(listener: &'static DeviceChangeListener) -> *mut c_void {
    std::ptr::from_ref(listener).cast_mut().cast::<c_void>()
}

/// System property listener: forwards the change to the engine's listener, which only posts a
/// wake-up. A panic must not unwind into CoreAudio.
unsafe extern "C-unwind" fn system_device_listener(
    _object: AudioObjectID,
    _address_count: u32,
    _addresses: NonNull<AudioObjectPropertyAddress>,
    client_data: *mut c_void,
) -> i32 {
    // SAFETY: `client_data` is the leaked `DeviceChangeListener` registered with this listener;
    // it is never freed.
    let Some(listener) = (unsafe { client_data.cast::<DeviceChangeListener>().as_ref() }) else {
        return NO_ERROR;
    };
    let _ = catch_unwind(AssertUnwindSafe(listener));
    NO_ERROR
}
