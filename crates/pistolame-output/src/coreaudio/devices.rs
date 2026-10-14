//! Output devices and their properties: listing, lookup by UID, sample rates, channels, hog
//! owner, latency. Reference: `AudioHardware.h` (CoreAudio framework).

use objc2_core_audio::{
    AudioObjectID, kAudioDevicePropertyAvailableNominalSampleRates,
    kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyDeviceIsAlive,
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyHogMode, kAudioDevicePropertyLatency,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertySafetyOffset,
    kAudioDevicePropertyStreams, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject, kAudioStreamPropertyVirtualFormat,
};
use objc2_core_audio_types::{AudioStreamBasicDescription, AudioValueRange};

use super::properties::{
    property_address, read_property, read_property_array, read_string_property, write_property,
};
use crate::{OutputDeviceId, OutputDeviceInfo, OutputError};

/// The audio system object, root of the device list.
pub(crate) const SYSTEM_OBJECT: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

/// Value of `kAudioDevicePropertyHogMode` when no process holds the device ("-1 if the device
/// is not currently in hog mode", `AudioHardware.h`).
pub(crate) const HOG_MODE_FREE: i32 = -1;

/// Every device that has at least one output stream, with the system default flagged.
pub(crate) fn list_output_devices() -> Result<Vec<OutputDeviceInfo>, OutputError> {
    let device_ids = all_device_ids()?;
    let default_output = default_output_device()?;
    Ok(device_ids
        .into_iter()
        .filter_map(|device_id| output_device_info(device_id, default_output))
        .collect())
}

/// Finds an output device by its UID (`OutputDeviceId`).
pub(crate) fn find_output_device(
    device_id: &OutputDeviceId,
) -> Result<(AudioObjectID, OutputDeviceInfo), OutputError> {
    let device_ids = all_device_ids()?;
    let default_output = default_output_device()?;
    device_ids
        .into_iter()
        .filter_map(|object_id| {
            output_device_info(object_id, default_output).map(|info| (object_id, info))
        })
        .find(|(_, info)| info.id == *device_id)
        .ok_or(OutputError::DeviceNotFound)
}

/// Every audio device the system knows, inputs included.
fn all_device_ids() -> Result<Vec<AudioObjectID>, OutputError> {
    read_property_array(
        SYSTEM_OBJECT,
        property_address(
            kAudioHardwarePropertyDevices,
            kAudioObjectPropertyScopeGlobal,
        ),
        "kAudioHardwarePropertyDevices",
    )
}

/// The device's description if it has output streams; `None` for input-only devices and for
/// devices whose properties cannot be read, so one faulty device never hides the others.
fn output_device_info(
    object_id: AudioObjectID,
    default_output: AudioObjectID,
) -> Option<OutputDeviceInfo> {
    let has_output = output_stream_ids(object_id).is_ok_and(|streams| !streams.is_empty());
    if !has_output {
        return None;
    }
    device_info(object_id, default_output).ok()
}

fn default_output_device() -> Result<AudioObjectID, OutputError> {
    read_property(
        SYSTEM_OBJECT,
        property_address(
            kAudioHardwarePropertyDefaultOutputDevice,
            kAudioObjectPropertyScopeGlobal,
        ),
        "kAudioHardwarePropertyDefaultOutputDevice",
    )
}

fn device_info(
    object_id: AudioObjectID,
    default_output: AudioObjectID,
) -> Result<OutputDeviceInfo, OutputError> {
    let uid = read_string_property(
        object_id,
        property_address(
            kAudioDevicePropertyDeviceUID,
            kAudioObjectPropertyScopeGlobal,
        ),
        "kAudioDevicePropertyDeviceUID",
    )?;
    let display_name = read_string_property(
        object_id,
        property_address(kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal),
        "kAudioObjectPropertyName",
    )?;
    Ok(OutputDeviceInfo {
        id: OutputDeviceId(uid),
        display_name,
        is_system_default: object_id == default_output,
    })
}

/// The device's output streams.
fn output_stream_ids(device: AudioObjectID) -> Result<Vec<AudioObjectID>, OutputError> {
    read_property_array(
        device,
        property_address(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput),
        "kAudioDevicePropertyStreams",
    )
}

/// The current virtual format of every output stream.
pub(crate) fn output_stream_formats(
    device: AudioObjectID,
) -> Result<Vec<AudioStreamBasicDescription>, OutputError> {
    output_stream_ids(device)?
        .into_iter()
        .map(|stream| {
            read_property(
                stream,
                property_address(
                    kAudioStreamPropertyVirtualFormat,
                    kAudioObjectPropertyScopeGlobal,
                ),
                "kAudioStreamPropertyVirtualFormat",
            )
        })
        .collect()
}

/// Total number of output channels across the device's output streams.
pub(crate) fn output_channel_count(device: AudioObjectID) -> Result<u32, OutputError> {
    Ok(output_stream_formats(device)?
        .iter()
        .map(|format| format.mChannelsPerFrame)
        .sum())
}

/// Sample-rate ranges the device can be set to (`kAudioDevicePropertyAvailableNominalSampleRates`).
pub(crate) fn available_nominal_sample_rates(
    device: AudioObjectID,
) -> Result<Vec<AudioValueRange>, OutputError> {
    read_property_array(
        device,
        property_address(
            kAudioDevicePropertyAvailableNominalSampleRates,
            kAudioObjectPropertyScopeGlobal,
        ),
        "kAudioDevicePropertyAvailableNominalSampleRates",
    )
}

/// The device's current sample rate in hertz.
pub(crate) fn nominal_sample_rate(device: AudioObjectID) -> Result<f64, OutputError> {
    read_property(
        device,
        property_address(
            kAudioDevicePropertyNominalSampleRate,
            kAudioObjectPropertyScopeGlobal,
        ),
        "kAudioDevicePropertyNominalSampleRate",
    )
}

/// Asks the device to run at `sample_rate_hz`. Asynchronous: read it back before relying on it.
pub(crate) fn request_nominal_sample_rate(
    device: AudioObjectID,
    sample_rate_hz: f64,
) -> Result<(), OutputError> {
    write_property(
        device,
        property_address(
            kAudioDevicePropertyNominalSampleRate,
            kAudioObjectPropertyScopeGlobal,
        ),
        &sample_rate_hz,
        "set kAudioDevicePropertyNominalSampleRate",
    )
}

/// The pid of the process holding the device in hog mode, or `HOG_MODE_FREE`.
pub(crate) fn hog_mode_owner(device: AudioObjectID) -> Result<i32, OutputError> {
    read_property(
        device,
        property_address(kAudioDevicePropertyHogMode, kAudioObjectPropertyScopeGlobal),
        "kAudioDevicePropertyHogMode",
    )
}

/// Writes the hog-mode owner. Documented as toggling in practice, so callers read it back.
pub(crate) fn write_hog_mode_owner(device: AudioObjectID, owner: i32) -> Result<(), OutputError> {
    write_property(
        device,
        property_address(kAudioDevicePropertyHogMode, kAudioObjectPropertyScopeGlobal),
        &owner,
        "set kAudioDevicePropertyHogMode",
    )
}

/// Whether the device still exists and works (`kAudioDevicePropertyDeviceIsAlive`).
pub(crate) fn is_device_alive(device: AudioObjectID) -> bool {
    read_property::<u32>(
        device,
        property_address(
            kAudioDevicePropertyDeviceIsAlive,
            kAudioObjectPropertyScopeGlobal,
        ),
        "kAudioDevicePropertyDeviceIsAlive",
    )
    .map(|is_alive| is_alive != 0)
    .unwrap_or(false)
}

/// Frames of output latency between the IO cycle and the speaker: device latency, safety
/// offset and one IO buffer, all in device frames (`AudioHardware.h` property docs).
pub(crate) fn output_presentation_latency_frames(
    device: AudioObjectID,
) -> Result<u32, OutputError> {
    let output_property = |selector, operation| {
        read_property::<u32>(
            device,
            property_address(selector, kAudioObjectPropertyScopeOutput),
            operation,
        )
    };
    let latency = output_property(kAudioDevicePropertyLatency, "kAudioDevicePropertyLatency")?;
    let safety_offset = output_property(
        kAudioDevicePropertySafetyOffset,
        "kAudioDevicePropertySafetyOffset",
    )?;
    Ok(latency
        .saturating_add(safety_offset)
        .saturating_add(io_buffer_frame_count(device)?))
}

/// Frames per IO cycle (`kAudioDevicePropertyBufferFrameSize`).
pub(crate) fn io_buffer_frame_count(device: AudioObjectID) -> Result<u32, OutputError> {
    read_property(
        device,
        property_address(
            kAudioDevicePropertyBufferFrameSize,
            kAudioObjectPropertyScopeGlobal,
        ),
        "kAudioDevicePropertyBufferFrameSize",
    )
}
