pub(crate) const UNSUPPORTED_TARGET_MESSAGE: &str =
    "Hypervisor.framework backend currently targets macOS on Apple Silicon";

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod imp {
    use std::ffi::c_void;
    use std::ptr;

    use bangbang_runtime::BackendError;

    pub type HvReturn = i32;
    pub type HvVmConfig = *mut c_void;
    pub type HvVcpu = u64;
    pub type HvVcpuConfig = *mut c_void;
    pub type HvVcpuExit = c_void;

    pub const HV_SUCCESS: HvReturn = 0;

    #[derive(Debug, Clone, Copy)]
    pub struct CreatedVcpu {
        pub vcpu: HvVcpu,
        pub exit: *mut HvVcpuExit,
    }

    #[link(name = "Hypervisor", kind = "framework")]
    extern "C" {
        pub fn hv_vm_create(config: HvVmConfig) -> HvReturn;
        pub fn hv_vm_destroy() -> HvReturn;
        pub fn hv_vcpu_create(
            vcpu: *mut HvVcpu,
            exit: *mut *mut HvVcpuExit,
            config: HvVcpuConfig,
        ) -> HvReturn;
        pub fn hv_vcpu_destroy(vcpu: HvVcpu) -> HvReturn;
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

    pub fn create_vcpu() -> Result<CreatedVcpu, BackendError> {
        let mut vcpu = 0;
        let mut exit = ptr::null_mut();

        // SAFETY: The output pointers are valid for the duration of the call, and a null
        // configuration requests the default vCPU configuration.
        unsafe {
            check(
                hv_vcpu_create(&mut vcpu, &mut exit, ptr::null_mut()),
                "hv_vcpu_create",
            )?;
        }

        Ok(CreatedVcpu { vcpu, exit })
    }

    pub fn destroy_vcpu(vcpu: HvVcpu) -> Result<(), BackendError> {
        // SAFETY: The caller owns this current-thread vCPU handle and guarantees it has not
        // already been destroyed.
        unsafe { check(hv_vcpu_destroy(vcpu), "hv_vcpu_destroy") }
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
mod imp {
    use std::ffi::c_void;

    use bangbang_runtime::BackendError;

    use super::UNSUPPORTED_TARGET_MESSAGE;

    pub type HvVcpu = u64;
    pub type HvVcpuExit = c_void;

    #[derive(Debug, Clone, Copy)]
    pub struct CreatedVcpu {
        pub vcpu: HvVcpu,
        pub exit: *mut HvVcpuExit,
    }

    pub fn create_vm() -> Result<(), BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn destroy_vm() -> Result<(), BackendError> {
        Ok(())
    }

    pub fn create_vcpu() -> Result<CreatedVcpu, BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn destroy_vcpu(_: HvVcpu) -> Result<(), BackendError> {
        Ok(())
    }
}

pub use imp::*;
