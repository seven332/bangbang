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
    const HV_ERROR: HvReturn = 0xfae94001u32 as HvReturn;
    const HV_BUSY: HvReturn = 0xfae94002u32 as HvReturn;
    const HV_BAD_ARGUMENT: HvReturn = 0xfae94003u32 as HvReturn;
    const HV_NO_RESOURCES: HvReturn = 0xfae94005u32 as HvReturn;
    const HV_NO_DEVICE: HvReturn = 0xfae94006u32 as HvReturn;
    const HV_DENIED: HvReturn = 0xfae94007u32 as HvReturn;
    const HV_FAULT: HvReturn = 0xfae94008u32 as HvReturn;
    const HV_UNSUPPORTED: HvReturn = 0xfae9400fu32 as HvReturn;

    #[derive(Debug)]
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

    fn hv_return_name(code: HvReturn) -> Option<&'static str> {
        match code {
            HV_ERROR => Some("HV_ERROR"),
            HV_BUSY => Some("HV_BUSY"),
            HV_BAD_ARGUMENT => Some("HV_BAD_ARGUMENT"),
            HV_NO_RESOURCES => Some("HV_NO_RESOURCES"),
            HV_NO_DEVICE => Some("HV_NO_DEVICE"),
            HV_DENIED => Some("HV_DENIED"),
            HV_FAULT => Some("HV_FAULT"),
            HV_UNSUPPORTED => Some("HV_UNSUPPORTED"),
            _ => None,
        }
    }

    fn format_hv_return(code: HvReturn) -> String {
        let code = code as u32;

        match hv_return_name(code as HvReturn) {
            Some(name) => format!("{name} (hv_return_t=0x{code:08x})"),
            None => format!("hv_return_t=0x{code:08x}"),
        }
    }

    pub fn check(code: HvReturn, operation: &'static str) -> Result<(), BackendError> {
        if code == HV_SUCCESS {
            Ok(())
        } else {
            Err(BackendError::Hypervisor(format!(
                "{operation} failed with {}",
                format_hv_return(code)
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

    #[cfg(test)]
    mod tests {
        use super::{check, HV_DENIED, HV_UNSUPPORTED};

        #[test]
        fn check_displays_named_hv_return() {
            let err = check(HV_DENIED, "hv_vm_create").expect_err("HV_DENIED should fail");

            assert_eq!(
                err.to_string(),
                "hypervisor error: hv_vm_create failed with HV_DENIED (hv_return_t=0xfae94007)"
            );
        }

        #[test]
        fn check_displays_unsupported_hv_return() {
            let err =
                check(HV_UNSUPPORTED, "hv_vm_create").expect_err("HV_UNSUPPORTED should fail");

            assert_eq!(
                err.to_string(),
                "hypervisor error: hv_vm_create failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)"
            );
        }
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
mod imp {
    use std::ffi::c_void;

    use bangbang_runtime::BackendError;

    use super::UNSUPPORTED_TARGET_MESSAGE;

    pub type HvVcpu = u64;
    pub type HvVcpuExit = c_void;

    #[derive(Debug)]
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
