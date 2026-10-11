//! RAII guard for COM initialization on the current thread (plan B5.3).
//!
//! `CoInitializeEx`: "each successful call to CoInitialize or CoInitializeEx, including any
//! call that returns S_FALSE, must be balanced by a corresponding call to CoUninitialize"
//! <https://learn.microsoft.com/en-us/windows/win32/api/combaseapi/nf-combaseapi-coinitializeex>

use std::marker::PhantomData;

use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

use crate::OutputError;

/// Keeps COM initialized on this thread while alive. Not `Send`: COM initialization belongs to
/// the thread that made it.
pub(crate) struct ComApartment {
    should_uninitialize: bool,
    _bound_to_this_thread: PhantomData<*const ()>,
}

impl ComApartment {
    /// Joins the multithreaded apartment. If the thread already joined a single-threaded
    /// apartment (`RPC_E_CHANGED_MODE`), COM is usable as is and the guard leaves it alone.
    pub(crate) fn initialize_multithreaded() -> Result<ComApartment, OutputError> {
        // SAFETY: plain FFI call with no pointer argument (the reserved parameter is None).
        let hresult = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hresult == RPC_E_CHANGED_MODE {
            return Ok(ComApartment {
                should_uninitialize: false,
                _bound_to_this_thread: PhantomData,
            });
        }
        if hresult.is_err() {
            return Err(OutputError::Platform {
                operation: "CoInitializeEx",
                platform_code: hresult.0,
            });
        }
        Ok(ComApartment {
            should_uninitialize: true,
            _bound_to_this_thread: PhantomData,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.should_uninitialize {
            // SAFETY: balances the successful CoInitializeEx made by this guard on this same
            // thread (the guard is not Send); every COM object created under it was declared
            // after it and has already been dropped.
            unsafe { CoUninitialize() };
        }
    }
}
