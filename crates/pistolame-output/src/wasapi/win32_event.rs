//! Owned Win32 auto-reset event, used for the WASAPI buffer event and the control event.
//! <https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-createeventw>

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{CreateEventW, SetEvent};

use super::hresult_mapping::{NameWasapiCall, WasapiCallError};

/// An unnamed auto-reset event, closed when dropped.
pub(crate) struct AutoResetEvent {
    handle: HANDLE,
}

// SAFETY: an event handle is a kernel object reference valid in every thread of the process;
// `SetEvent` and the wait functions are thread-safe, and the handle is closed only in `drop`,
// when no other reference to the wrapper exists.
unsafe impl Send for AutoResetEvent {}
// SAFETY: as above; shared access only calls `SetEvent` or passes the handle to a wait.
unsafe impl Sync for AutoResetEvent {}

impl AutoResetEvent {
    /// Creates an unsignaled auto-reset event.
    pub(crate) fn new() -> Result<AutoResetEvent, WasapiCallError> {
        // SAFETY: no security attributes and no name, both documented as optional.
        let handle =
            unsafe { CreateEventW(None, false, false, None) }.for_operation("CreateEventW")?;
        Ok(AutoResetEvent { handle })
    }

    /// The raw handle, for `SetEventHandle` and the waits. Valid while `self` is alive.
    pub(crate) fn raw_handle(&self) -> HANDLE {
        self.handle
    }

    /// Signals the event. Thread-safe, needs no COM. Does not allocate on success.
    pub(crate) fn signal(&self) -> Result<(), WasapiCallError> {
        // SAFETY: the handle is a live event owned by `self`.
        unsafe { SetEvent(self.handle) }.for_operation("SetEvent")
    }
}

impl Drop for AutoResetEvent {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by CreateEventW and is closed exactly once, here.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}
