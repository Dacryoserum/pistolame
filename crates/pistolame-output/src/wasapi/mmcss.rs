//! RAII guard registering the render thread with the Multimedia Class Scheduler Service.
//!
//! Microsoft's exclusive-mode sample registers its render thread as "Pro Audio"
//! (<https://learn.microsoft.com/en-us/windows/win32/coreaudio/exclusive-mode-streams>).
//! API: <https://learn.microsoft.com/en-us/windows/win32/api/avrt/nf-avrt-avsetmmthreadcharacteristicsw>

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
};
use windows::core::w;

/// Holds the MMCSS registration of the current thread; reverts it when dropped.
pub(crate) struct MmcssRegistration {
    task_handle: HANDLE,
}

impl MmcssRegistration {
    /// Registers the current thread under the "Pro Audio" task. `None` when MMCSS refuses; the
    /// caller counts it and keeps running at normal priority (plan B8).
    pub(crate) fn register_pro_audio_thread() -> Option<MmcssRegistration> {
        // "The first time this function is called, this value must be zero on input."
        let mut task_index: u32 = 0;
        // SAFETY: the task name is a static null-terminated wide string and `task_index` is a
        // valid, writable u32 that outlives the call.
        let registration =
            unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) };
        registration
            .ok()
            .map(|task_handle| MmcssRegistration { task_handle })
    }
}

impl Drop for MmcssRegistration {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful AvSetMmThreadCharacteristicsW on this
        // thread and is reverted exactly once. A failure only means the thread keeps its class
        // until it exits, which happens right after.
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.task_handle) };
    }
}
