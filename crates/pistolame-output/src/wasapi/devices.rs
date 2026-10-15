//! Render endpoint enumeration through `IMMDeviceEnumerator` (plan B5.2).
//!
//! - EnumAudioEndpoints: <https://learn.microsoft.com/en-us/windows/win32/api/mmdeviceapi/nf-mmdeviceapi-immdeviceenumerator-enumaudioendpoints>
//! - GetDevice: <https://learn.microsoft.com/en-us/windows/win32/api/mmdeviceapi/nf-mmdeviceapi-immdeviceenumerator-getdevice>
//! - IMMDevice::GetId ("the caller is responsible for freeing ... by calling CoTaskMemFree"):
//!   <https://learn.microsoft.com/en-us/windows/win32/api/mmdeviceapi/nf-mmdeviceapi-immdevice-getid>
//! - PKEY_Device_FriendlyName: <https://learn.microsoft.com/en-us/windows/win32/coreaudio/pkey-device-friendlyname>
//! - RegisterEndpointNotificationCallback: <https://learn.microsoft.com/en-us/windows/win32/api/mmdeviceapi/nf-mmdeviceapi-immdeviceenumerator-registerendpointnotificationcallback>

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IAudioClient, IMMDevice, IMMDeviceEnumerator, IMMNotificationClient,
    MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::core::{PCWSTR, PWSTR};

use super::hresult_mapping::{NameWasapiCall, WasapiCallError};
use crate::{OutputDeviceId, OutputDeviceInfo, OutputError};

/// Wrapper over `IMMDeviceEnumerator`. Lives on one thread with COM initialized.
pub(crate) struct DeviceEnumerator {
    enumerator: IMMDeviceEnumerator,
}

impl DeviceEnumerator {
    /// Creates the system device enumerator.
    pub(crate) fn new() -> Result<DeviceEnumerator, WasapiCallError> {
        // SAFETY: COM is initialized on this thread by the caller (ComApartment); the CLSID is
        // the documented MMDeviceEnumerator class.
        let enumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .for_operation("CoCreateInstance(MMDeviceEnumerator)")?;
        Ok(DeviceEnumerator { enumerator })
    }

    /// Every active render endpoint.
    pub(crate) fn list_active_render_devices(
        &self,
    ) -> Result<Vec<OutputDeviceInfo>, WasapiCallError> {
        // SAFETY: valid enumerator; documented flow and state mask.
        let collection = unsafe {
            self.enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
        }
        .for_operation("IMMDeviceEnumerator::EnumAudioEndpoints")?;
        // SAFETY: valid collection returned just above.
        let device_count =
            unsafe { collection.GetCount() }.for_operation("IMMDeviceCollection::GetCount")?;
        let default_device_id = self.default_render_device_id();
        let mut devices = Vec::with_capacity(device_count as usize);
        for device_index in 0..device_count {
            // SAFETY: the index is below the count reported by the same collection.
            let device = unsafe { collection.Item(device_index) }
                .for_operation("IMMDeviceCollection::Item")?;
            devices.push(describe_device(&device, default_device_id.as_deref())?);
        }
        Ok(devices)
    }

    /// The device with this identifier, if it exists and is active. A device that is known but
    /// unplugged or disabled is reported as `DeviceNotFound` (plan B8).
    pub(crate) fn find_active_render_device(
        &self,
        device_id: &OutputDeviceId,
    ) -> Result<(IMMDevice, OutputDeviceInfo), OutputError> {
        let wide_device_id: Vec<u16> = device_id.0.encode_utf16().chain(Some(0)).collect();
        // SAFETY: `wide_device_id` is null-terminated and outlives the call.
        let device = unsafe { self.enumerator.GetDevice(PCWSTR(wide_device_id.as_ptr())) }
            .for_operation("IMMDeviceEnumerator::GetDevice")?;
        // SAFETY: valid device returned just above.
        let state = unsafe { device.GetState() }.for_operation("IMMDevice::GetState")?;
        if state != DEVICE_STATE_ACTIVE {
            return Err(OutputError::DeviceNotFound);
        }
        let default_device_id = self.default_render_device_id();
        let info = describe_device(&device, default_device_id.as_deref())?;
        Ok((device, info))
    }

    /// Starts delivering endpoint notifications to `client`. The caller unregisters it with
    /// `unregister_endpoint_notifications` before releasing it.
    pub(crate) fn register_endpoint_notifications(
        &self,
        client: &IMMNotificationClient,
    ) -> Result<(), WasapiCallError> {
        // SAFETY: valid enumerator and client; the client stays alive until it is unregistered.
        unsafe { self.enumerator.RegisterEndpointNotificationCallback(client) }
            .for_operation("IMMDeviceEnumerator::RegisterEndpointNotificationCallback")
    }

    /// Stops the notifications `register_endpoint_notifications` started.
    pub(crate) fn unregister_endpoint_notifications(
        &self,
        client: &IMMNotificationClient,
    ) -> Result<(), WasapiCallError> {
        // SAFETY: valid enumerator; `client` is the registered one, still alive.
        unsafe {
            self.enumerator
                .UnregisterEndpointNotificationCallback(client)
        }
        .for_operation("IMMDeviceEnumerator::UnregisterEndpointNotificationCallback")
    }

    /// Identifier of the system default render device for the console role, if there is one.
    fn default_render_device_id(&self) -> Option<String> {
        // SAFETY: valid enumerator; documented flow and role. It fails with E_NOTFOUND when no
        // render device exists; any failure only means no device is marked as default.
        let device = unsafe { self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole) };
        device
            .ok()
            .and_then(|device| device_id_string(&device).ok())
    }
}

/// Activates a fresh `IAudioClient` on the device. The `Initialize` alignment retry and the
/// shared-mode candidate loop need a new one per attempt (`Initialize` remarks).
pub(crate) fn activate_audio_client(device: &IMMDevice) -> Result<IAudioClient, WasapiCallError> {
    // SAFETY: valid device; no activation parameters, as documented for IAudioClient.
    unsafe { device.Activate::<IAudioClient>(CLSCTX_ALL, None) }
        .for_operation("IMMDevice::Activate")
}

fn describe_device(
    device: &IMMDevice,
    default_device_id: Option<&str>,
) -> Result<OutputDeviceInfo, WasapiCallError> {
    let id = device_id_string(device)?;
    let display_name = device_friendly_name(device)?;
    Ok(OutputDeviceInfo {
        is_system_default: default_device_id == Some(id.as_str()),
        id: OutputDeviceId(id),
        display_name,
    })
}

fn device_id_string(device: &IMMDevice) -> Result<String, WasapiCallError> {
    // SAFETY: valid device.
    let id_pointer = unsafe { device.GetId() }.for_operation("IMMDevice::GetId")?;
    Ok(take_com_allocated_string(id_pointer))
}

fn device_friendly_name(device: &IMMDevice) -> Result<String, WasapiCallError> {
    // SAFETY: valid device; read-only access as documented.
    let property_store = unsafe { device.OpenPropertyStore(STGM_READ) }
        .for_operation("IMMDevice::OpenPropertyStore")?;
    // SAFETY: valid store; the key is a documented static PROPERTYKEY.
    let mut value = unsafe { property_store.GetValue(&PKEY_Device_FriendlyName) }
        .for_operation("IPropertyStore::GetValue")?;
    // SAFETY: `value` is an initialized PROPVARIANT returned by GetValue.
    let name =
        unsafe { PropVariantToStringAlloc(&value) }.for_operation("PropVariantToStringAlloc");
    // SAFETY: frees whatever GetValue allocated inside `value`, exactly once.
    let _ = unsafe { PropVariantClear(&mut value) };
    Ok(take_com_allocated_string(name?))
}

/// Copies a COM-allocated wide string and frees it with `CoTaskMemFree`, as GetId and
/// PropVariantToStringAlloc require. Invalid UTF-16 is replaced, never dropped.
fn take_com_allocated_string(wide_string: PWSTR) -> String {
    // SAFETY: `wide_string` is a non-null, null-terminated string returned by a successful COM
    // call; it is read once, then freed exactly once with the documented allocator.
    unsafe {
        let text = String::from_utf16_lossy(wide_string.as_wide());
        CoTaskMemFree(Some(wide_string.0 as *const _));
        text
    }
}
