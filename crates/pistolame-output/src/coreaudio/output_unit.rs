//! The AUHAL output unit (`kAudioUnitSubType_HALOutput`) bound to one device: creation,
//! client format, render callback, start, stop and disposal (plan coreaudio.md §0 item 3).
//! Reference: Apple Technical Note TN2091, "Device input using the HAL Output Audio Unit", and
//! `AudioUnitProperties.h`.

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{NonNull, null_mut};

use objc2_audio_toolbox::{
    AURenderCallbackStruct, AudioComponentDescription, AudioComponentFindNext,
    AudioComponentInstanceDispose, AudioComponentInstanceNew, AudioOutputUnitStart,
    AudioOutputUnitStop, AudioUnit, AudioUnitGetProperty, AudioUnitInitialize,
    AudioUnitSetProperty, AudioUnitUninitialize, kAudioOutputUnitProperty_CurrentDevice,
    kAudioUnitManufacturer_Apple, kAudioUnitProperty_Latency,
    kAudioUnitProperty_MaximumFramesPerSlice, kAudioUnitProperty_SetRenderCallback,
    kAudioUnitProperty_StreamFormat, kAudioUnitScope_Global, kAudioUnitScope_Input,
    kAudioUnitSubType_HALOutput, kAudioUnitType_Output,
};
use objc2_core_audio::AudioObjectID;
use objc2_core_audio_types::{
    AudioStreamBasicDescription, kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked,
    kAudioFormatLinearPCM,
};

use super::properties::check_status;
use crate::OutputError;
use crate::float_conversion::FLOAT32_SAMPLE_BYTES;

/// The output element of an output unit ("bus 0").
const OUTPUT_ELEMENT: u32 = 0;
/// OSStatus returned when no AUHAL component exists (never expected on macOS).
const COMPONENT_NOT_FOUND: i32 = -1;

/// The client format every stream feeds the unit: interleaved, packed float32 at the render
/// rate (the backend converts the integer render bytes exactly, see `float_conversion`).
pub(crate) fn float32_client_format(
    sample_rate_hz: u32,
    channel_count: u8,
) -> AudioStreamBasicDescription {
    let bytes_per_frame = (FLOAT32_SAMPLE_BYTES as u32) * u32::from(channel_count);
    AudioStreamBasicDescription {
        mSampleRate: f64::from(sample_rate_hz),
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
        mBytesPerPacket: bytes_per_frame,
        mFramesPerPacket: 1,
        mBytesPerFrame: bytes_per_frame,
        mChannelsPerFrame: u32::from(channel_count),
        mBitsPerChannel: (FLOAT32_SAMPLE_BYTES * 8) as u32,
        mReserved: 0,
    }
}

/// An AUHAL instance. Uninitialized and disposed on drop; disposing it ends every callback.
#[derive(Debug)]
pub(crate) struct OutputUnit {
    unit: AudioUnit,
    is_initialized: bool,
}

// SAFETY: an AudioUnit handle may be used from any thread; the stream only calls it from the
// engine thread, one call at a time.
unsafe impl Send for OutputUnit {}

impl OutputUnit {
    /// Creates an AUHAL instance that plays to `device`.
    pub(crate) fn new(device: AudioObjectID) -> Result<OutputUnit, OutputError> {
        let description = AudioComponentDescription {
            componentType: kAudioUnitType_Output,
            componentSubType: kAudioUnitSubType_HALOutput,
            componentManufacturer: kAudioUnitManufacturer_Apple,
            componentFlags: 0,
            componentFlagsMask: 0,
        };
        // SAFETY: the description is a live local; a null component starts the search.
        let component = unsafe { AudioComponentFindNext(null_mut(), NonNull::from(&description)) };
        if component.is_null() {
            return Err(OutputError::Platform {
                operation: "AudioComponentFindNext(HALOutput)",
                platform_code: COMPONENT_NOT_FOUND,
            });
        }
        let mut unit: AudioUnit = null_mut();
        // SAFETY: `component` was just found; `unit` is a live local that receives the instance.
        let status = unsafe { AudioComponentInstanceNew(component, NonNull::from(&mut unit)) };
        check_status(status, "AudioComponentInstanceNew")?;
        let output_unit = OutputUnit {
            unit,
            is_initialized: false,
        };
        output_unit.set_property(
            kAudioOutputUnitProperty_CurrentDevice,
            kAudioUnitScope_Global,
            &device,
            "kAudioOutputUnitProperty_CurrentDevice",
        )?;
        Ok(output_unit)
    }

    /// Sets the client format, the largest slice, the render callback, then initializes.
    ///
    /// # Safety
    /// `callback.inputProcRefCon` must stay valid for as long as the unit may call
    /// `callback.inputProc`, i.e. until this `OutputUnit` is dropped.
    pub(crate) unsafe fn configure(
        &mut self,
        client_format: &AudioStreamBasicDescription,
        maximum_frames_per_slice: u32,
        callback: &AURenderCallbackStruct,
    ) -> Result<(), OutputError> {
        self.set_property(
            kAudioUnitProperty_StreamFormat,
            kAudioUnitScope_Input,
            client_format,
            "kAudioUnitProperty_StreamFormat",
        )?;
        self.set_property(
            kAudioUnitProperty_MaximumFramesPerSlice,
            kAudioUnitScope_Global,
            &maximum_frames_per_slice,
            "kAudioUnitProperty_MaximumFramesPerSlice",
        )?;
        self.set_property(
            kAudioUnitProperty_SetRenderCallback,
            kAudioUnitScope_Input,
            callback,
            "kAudioUnitProperty_SetRenderCallback",
        )?;
        // SAFETY: valid unit, fully configured above.
        let status = unsafe { AudioUnitInitialize(self.unit) };
        check_status(status, "AudioUnitInitialize")?;
        self.is_initialized = true;
        Ok(())
    }

    /// The unit's own processing latency in seconds (format conversion, resampling).
    pub(crate) fn latency_seconds(&self) -> Result<f64, OutputError> {
        let mut latency_seconds: f64 = 0.0;
        let mut data_size = size_of::<f64>() as u32;
        // SAFETY: both out pointers refer to live locals of the documented type and size.
        let status = unsafe {
            AudioUnitGetProperty(
                self.unit,
                kAudioUnitProperty_Latency,
                kAudioUnitScope_Global,
                OUTPUT_ELEMENT,
                NonNull::from(&mut latency_seconds).cast::<c_void>(),
                NonNull::from(&mut data_size),
            )
        };
        check_status(status, "kAudioUnitProperty_Latency")?;
        Ok(latency_seconds)
    }

    /// Starts the IO: the render callback begins to be called.
    pub(crate) fn start(&self) -> Result<(), OutputError> {
        // SAFETY: valid, initialized unit.
        let status = unsafe { AudioOutputUnitStart(self.unit) };
        check_status(status, "AudioOutputUnitStart")
    }

    /// Stops the IO. Callbacks may still be in flight when it returns; disposal ends them.
    pub(crate) fn stop(&self) -> Result<(), OutputError> {
        // SAFETY: valid unit; stopping a stopped unit is harmless.
        let status = unsafe { AudioOutputUnitStop(self.unit) };
        check_status(status, "AudioOutputUnitStop")
    }

    fn set_property<T>(
        &self,
        property_id: u32,
        scope: u32,
        value: &T,
        operation: &'static str,
    ) -> Result<(), OutputError> {
        // SAFETY: `value` is a live value of the type the property documents, of exactly
        // `size_of::<T>()` bytes; the unit copies it before returning.
        let status = unsafe {
            AudioUnitSetProperty(
                self.unit,
                property_id,
                scope,
                OUTPUT_ELEMENT,
                (value as *const T).cast::<c_void>(),
                size_of::<T>() as u32,
            )
        };
        check_status(status, operation)
    }
}

impl Drop for OutputUnit {
    fn drop(&mut self) {
        if self.is_initialized {
            // SAFETY: valid unit, initialized by `configure`.
            let _ = unsafe { AudioUnitUninitialize(self.unit) };
        }
        // SAFETY: the unit was created by `AudioComponentInstanceNew` and is disposed once.
        let _ = unsafe { AudioComponentInstanceDispose(self.unit) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_format_is_interleaved_packed_float32() {
        let format = float32_client_format(96_000, 2);
        assert_eq!(format.mSampleRate, 96_000.0);
        assert_eq!(format.mFormatID, kAudioFormatLinearPCM);
        assert_eq!(
            format.mFormatFlags,
            kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked
        );
        assert_eq!(
            (
                format.mBytesPerFrame,
                format.mChannelsPerFrame,
                format.mBitsPerChannel
            ),
            (8, 2, 32)
        );
    }
}
