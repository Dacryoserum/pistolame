//! Typed helpers over `AudioObjectGetPropertyData` / `AudioObjectSetPropertyData`. Every
//! CoreAudio property access of the backend goes through here, so the `unsafe` FFI calls and
//! their size checks exist once (DRY).
//! Reference: <https://developer.apple.com/documentation/coreaudio/1422524-audioobjectgetpropertydata>

use std::ffi::c_void;
use std::mem::{MaybeUninit, size_of};
use std::ptr::{NonNull, null};

use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectPropertyScope, AudioObjectPropertySelector,
    AudioObjectSetPropertyData, kAudioObjectPropertyElementMain,
};
use objc2_core_audio_types::{AudioStreamBasicDescription, AudioValueRange};
use objc2_core_foundation::{CFRetained, CFString};

use crate::OutputError;

/// `noErr`: the OSStatus every CoreAudio call returns on success.
pub(crate) const NO_ERROR: i32 = 0;

/// The address of a property on the main element.
pub(crate) fn property_address(
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Maps an OSStatus to `Ok` or to a named platform error.
pub(crate) fn check_status(status: i32, operation: &'static str) -> Result<(), OutputError> {
    if status == NO_ERROR {
        Ok(())
    } else {
        Err(OutputError::Platform {
            operation,
            platform_code: status,
        })
    }
}

/// Size of `T` as the `u32` CoreAudio expects. The types used here are a few bytes long.
fn byte_size_of<T>() -> u32 {
    u32::try_from(size_of::<T>()).unwrap_or(u32::MAX)
}

/// A plain C type CoreAudio reads or writes as raw bytes.
///
/// # Safety
/// Implementors must be `#[repr(C)]` or primitive types for which every bit pattern CoreAudio
/// writes, including all-zero bytes, is a valid value (integers, floats, structs of them, raw
/// pointers).
pub(crate) unsafe trait PlainCoreAudioData: Copy {
    /// The all-zero value, used to initialize buffers before CoreAudio fills them.
    fn zeroed() -> Self {
        // SAFETY: the trait contract guarantees all-zero bytes are a valid value.
        unsafe { MaybeUninit::<Self>::zeroed().assume_init() }
    }
}

// SAFETY: primitive integers and floats accept every bit pattern.
unsafe impl PlainCoreAudioData for u32 {}
// SAFETY: as above.
unsafe impl PlainCoreAudioData for i32 {}
// SAFETY: as above.
unsafe impl PlainCoreAudioData for f64 {}
// SAFETY: a raw pointer accepts every bit pattern; it is only dereferenced after a null check.
unsafe impl PlainCoreAudioData for *const CFString {}
// SAFETY: `#[repr(C)]` struct of two `f64`.
unsafe impl PlainCoreAudioData for AudioValueRange {}
// SAFETY: `#[repr(C)]` struct of integers and one `f64`.
unsafe impl PlainCoreAudioData for AudioStreamBasicDescription {}

/// Reads a fixed-size property of the plain C type CoreAudio documents for the selector.
pub(crate) fn read_property<T: PlainCoreAudioData>(
    object_id: AudioObjectID,
    address: AudioObjectPropertyAddress,
    operation: &'static str,
) -> Result<T, OutputError> {
    let mut value = T::zeroed();
    let mut data_size = byte_size_of::<T>();
    // SAFETY: the address and size pointers refer to live locals; `value` has room for exactly
    // `data_size` bytes, the size CoreAudio documents for this selector.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object_id,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut data_size),
            NonNull::from(&mut value).cast::<c_void>(),
        )
    };
    check_status(status, operation)?;
    if data_size != byte_size_of::<T>() {
        return Err(OutputError::Platform {
            operation,
            platform_code: NO_ERROR,
        });
    }
    Ok(value)
}

/// Reads a variable-length array property (device lists, stream lists, rate ranges).
pub(crate) fn read_property_array<T: PlainCoreAudioData>(
    object_id: AudioObjectID,
    address: AudioObjectPropertyAddress,
    operation: &'static str,
) -> Result<Vec<T>, OutputError> {
    let mut data_size: u32 = 0;
    // SAFETY: both pointers refer to live locals; no qualifier is passed.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object_id,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut data_size),
        )
    };
    check_status(status, operation)?;
    let element_count = data_size as usize / size_of::<T>();
    if element_count == 0 {
        return Ok(Vec::new());
    }
    let mut values = vec![T::zeroed(); element_count];
    let mut written_size = data_size;
    // SAFETY: `values` holds `element_count` initialized elements, i.e. at least `written_size`
    // bytes; CoreAudio writes at most `written_size` bytes and reports how many it wrote.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object_id,
            NonNull::from(&address),
            0,
            null(),
            NonNull::from(&mut written_size),
            NonNull::from(values.as_mut_slice()).cast::<c_void>(),
        )
    };
    check_status(status, operation)?;
    values.truncate(written_size as usize / size_of::<T>());
    Ok(values)
}

/// Writes a fixed-size property.
pub(crate) fn write_property<T: PlainCoreAudioData>(
    object_id: AudioObjectID,
    address: AudioObjectPropertyAddress,
    value: &T,
    operation: &'static str,
) -> Result<(), OutputError> {
    // SAFETY: the address and the value refer to live data of the documented type and size.
    let status = unsafe {
        AudioObjectSetPropertyData(
            object_id,
            NonNull::from(&address),
            0,
            null(),
            byte_size_of::<T>(),
            NonNull::from(value).cast::<c_void>(),
        )
    };
    check_status(status, operation)
}

/// Reads a `CFStringRef` property (names, UIDs). CoreAudio returns a retained string that the
/// caller owns; `CFRetained` releases it.
pub(crate) fn read_string_property(
    object_id: AudioObjectID,
    address: AudioObjectPropertyAddress,
    operation: &'static str,
) -> Result<String, OutputError> {
    let string_pointer: *const CFString = read_property(object_id, address, operation)?;
    let Some(string_pointer) = NonNull::new(string_pointer.cast_mut()) else {
        return Ok(String::new());
    };
    // SAFETY: CoreAudio documents string properties as returned with +1 retain count ("the
    // caller is responsible for releasing"), so taking ownership balances it.
    let string = unsafe { CFRetained::from_raw(string_pointer) };
    Ok(string.to_string())
}
