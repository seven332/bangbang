#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod imp {
    use std::ffi::c_void;
    use std::ptr;

    use bangbang_runtime::BackendError;

    pub type HvReturn = i32;
    pub type HvVmConfig = *mut c_void;

    pub const HV_SUCCESS: HvReturn = 0;

    #[link(name = "Hypervisor", kind = "framework")]
    extern "C" {
        pub fn hv_vm_create(config: HvVmConfig) -> HvReturn;
        pub fn hv_vm_destroy() -> HvReturn;
    }

    pub fn check(code: HvReturn, operation: &'static str) -> Result<(), BackendError> {
        if code == HV_SUCCESS {
            Ok(())
        } else {
            Err(BackendError::Hypervisor(format!(
                "{operation} failed with hv_return_t=0x{code:08x}"
            )))
        }
    }

    pub fn create_vm() -> Result<(), BackendError> {
        // SAFETY: Passing null requests the default VM configuration per Hypervisor.framework.
        unsafe { check(hv_vm_create(ptr::null_mut()), "hv_vm_create") }
    }

    pub fn destroy_vm() -> Result<(), BackendError> {
        // SAFETY: Destroys the process-local VM after vCPUs have been destroyed.
        unsafe { check(hv_vm_destroy(), "hv_vm_destroy") }
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
mod imp {
    use bangbang_runtime::BackendError;

    pub fn create_vm() -> Result<(), BackendError> {
        Err(BackendError::Unsupported(
            "Hypervisor.framework backend currently targets macOS on Apple Silicon",
        ))
    }

    pub fn destroy_vm() -> Result<(), BackendError> {
        Ok(())
    }
}

pub use imp::*;
