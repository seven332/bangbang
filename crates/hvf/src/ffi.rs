pub(crate) const UNSUPPORTED_TARGET_MESSAGE: &str =
    "Hypervisor.framework backend currently targets macOS on Apple Silicon";

pub(crate) type HvVcpu = u64;
pub(crate) type HvExitReason = u32;
pub(crate) type HvMemoryFlags = u64;
pub(crate) type HvReg = u32;
pub(crate) type HvSysReg = u16;

pub(crate) const HV_MEMORY_READ: HvMemoryFlags = 1 << 0;
pub(crate) const HV_MEMORY_WRITE: HvMemoryFlags = 1 << 1;
pub(crate) const HV_MEMORY_EXEC: HvMemoryFlags = 1 << 2;

pub(crate) const HV_EXIT_REASON_CANCELED: HvExitReason = 0;
pub(crate) const HV_EXIT_REASON_EXCEPTION: HvExitReason = 1;
pub(crate) const HV_EXIT_REASON_VTIMER_ACTIVATED: HvExitReason = 2;
pub(crate) const HV_EXIT_REASON_UNKNOWN: HvExitReason = 3;
pub(crate) const HV_REG_X0: HvReg = 0;
pub(crate) const HV_REG_X1: HvReg = 1;
pub(crate) const HV_REG_X2: HvReg = 2;
pub(crate) const HV_REG_X3: HvReg = 3;
pub(crate) const HV_REG_PC: HvReg = 31;
pub(crate) const HV_REG_CPSR: HvReg = 34;
pub(crate) const HV_SYS_REG_MPIDR_EL1: HvSysReg = 0xc005;
pub(crate) const HV_SYS_REG_SPSR_EL1: HvSysReg = 0xc200;
pub(crate) const HV_SYS_REG_ELR_EL1: HvSysReg = 0xc201;
pub(crate) const HV_SYS_REG_SP_EL1: HvSysReg = 0xe208;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HvVcpuExitException {
    pub(crate) syndrome: u64,
    pub(crate) virtual_address: u64,
    pub(crate) physical_address: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HvVcpuExit {
    pub(crate) reason: HvExitReason,
    pub(crate) exception: HvVcpuExitException,
}

#[derive(Debug)]
pub(crate) struct CreatedVcpu {
    pub(crate) vcpu: HvVcpu,
    pub(crate) exit: *mut HvVcpuExit,
}

/// # Safety
///
/// `exit` must point to initialized `HvVcpuExit` data belonging to a live
/// current-thread vCPU whose latest `hv_vcpu_run` call has returned, or to
/// test-owned memory with the same layout.
pub(crate) unsafe fn copy_vcpu_exit(
    exit: *const HvVcpuExit,
) -> Result<HvVcpuExit, bangbang_runtime::BackendError> {
    if exit.is_null() {
        return Err(bangbang_runtime::BackendError::Hypervisor(
            "hv_vcpu_exit_t pointer is null".to_string(),
        ));
    }

    // SAFETY: The caller guarantees `exit` is valid for a read of `HvVcpuExit`.
    unsafe { Ok(*exit) }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod imp {
    use std::ffi::c_void;
    use std::ptr;
    use std::ptr::NonNull;

    use bangbang_runtime::BackendError;

    use super::{CreatedVcpu, HvMemoryFlags, HvReg, HvSysReg, HvVcpu, HvVcpuExit};

    pub type HvReturn = i32;
    pub type HvVmConfig = *mut c_void;
    pub type HvVcpuConfig = *mut c_void;

    pub const HV_SUCCESS: HvReturn = 0;
    const HV_ERROR: HvReturn = 0xfae94001u32 as HvReturn;
    const HV_BUSY: HvReturn = 0xfae94002u32 as HvReturn;
    const HV_BAD_ARGUMENT: HvReturn = 0xfae94003u32 as HvReturn;
    const HV_NO_RESOURCES: HvReturn = 0xfae94005u32 as HvReturn;
    const HV_NO_DEVICE: HvReturn = 0xfae94006u32 as HvReturn;
    const HV_DENIED: HvReturn = 0xfae94007u32 as HvReturn;
    const HV_FAULT: HvReturn = 0xfae94008u32 as HvReturn;
    const HV_UNSUPPORTED: HvReturn = 0xfae9400fu32 as HvReturn;

    #[link(name = "Hypervisor", kind = "framework")]
    unsafe extern "C" {
        pub fn hv_vm_config_create() -> HvVmConfig;
        pub fn hv_vm_config_get_max_ipa_size(ipa_bit_length: *mut u32) -> HvReturn;
        pub fn hv_vm_config_set_ipa_size(config: HvVmConfig, ipa_bit_length: u32) -> HvReturn;
        pub fn hv_vm_create(config: HvVmConfig) -> HvReturn;
        pub fn hv_vm_destroy() -> HvReturn;
        pub fn hv_vm_map(
            addr: *mut c_void,
            ipa: u64,
            size: usize,
            flags: HvMemoryFlags,
        ) -> HvReturn;
        pub fn hv_vm_unmap(ipa: u64, size: usize) -> HvReturn;
        pub fn hv_vcpu_create(
            vcpu: *mut HvVcpu,
            exit: *mut *mut HvVcpuExit,
            config: HvVcpuConfig,
        ) -> HvReturn;
        pub fn hv_vcpu_destroy(vcpu: HvVcpu) -> HvReturn;
        pub fn hv_vcpu_get_reg(vcpu: HvVcpu, reg: HvReg, value: *mut u64) -> HvReturn;
        pub fn hv_vcpu_set_reg(vcpu: HvVcpu, reg: HvReg, value: u64) -> HvReturn;
        pub fn hv_vcpu_get_sys_reg(vcpu: HvVcpu, reg: HvSysReg, value: *mut u64) -> HvReturn;
        pub fn hv_vcpu_set_sys_reg(vcpu: HvVcpu, reg: HvSysReg, value: u64) -> HvReturn;
        pub fn hv_vcpu_get_vtimer_mask(vcpu: HvVcpu, vtimer_is_masked: *mut bool) -> HvReturn;
        pub fn hv_vcpu_set_vtimer_mask(vcpu: HvVcpu, vtimer_is_masked: bool) -> HvReturn;
        pub fn hv_vcpu_run(vcpu: HvVcpu) -> HvReturn;
        pub fn hv_vcpus_exit(vcpus: *mut HvVcpu, vcpu_count: u32) -> HvReturn;
    }

    unsafe extern "C" {
        fn os_release(object: *mut c_void);
    }

    #[derive(Debug)]
    struct HvVmConfigOwner {
        config: NonNull<c_void>,
    }

    impl HvVmConfigOwner {
        fn with_max_ipa_size() -> Result<Self, BackendError> {
            // SAFETY: Creates a retained Hypervisor.framework VM configuration object.
            let config = unsafe { hv_vm_config_create() };
            let config = NonNull::new(config).ok_or(BackendError::Hypervisor(
                "hv_vm_config_create returned null".to_string(),
            ))?;
            let owner = Self { config };
            let mut max_ipa_size = 0;

            // SAFETY: `max_ipa_size` is a valid out-pointer for the duration of the call.
            unsafe {
                check(
                    hv_vm_config_get_max_ipa_size(&mut max_ipa_size),
                    "hv_vm_config_get_max_ipa_size",
                )?;
            }

            // SAFETY: `owner.config` owns a live VM configuration, and the requested IPA size
            // is the framework-reported maximum for the current host.
            unsafe {
                check(
                    hv_vm_config_set_ipa_size(owner.config.as_ptr(), max_ipa_size),
                    "hv_vm_config_set_ipa_size",
                )?;
            }

            Ok(owner)
        }

        fn as_ptr(&self) -> HvVmConfig {
            self.config.as_ptr()
        }
    }

    impl Drop for HvVmConfigOwner {
        fn drop(&mut self) {
            // SAFETY: `self.config` is a retained OS object returned by `hv_vm_config_create`.
            unsafe { os_release(self.config.as_ptr()) };
        }
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
        let config = HvVmConfigOwner::with_max_ipa_size()?;

        // SAFETY: `config` owns a live VM configuration for the duration of the call.
        unsafe { check(hv_vm_create(config.as_ptr()), "hv_vm_create") }
    }

    pub fn destroy_vm() -> Result<(), BackendError> {
        // SAFETY: Destroys the process-local VM after vCPUs have been destroyed.
        unsafe { check(hv_vm_destroy(), "hv_vm_destroy") }
    }

    pub fn map_memory(
        host_address: NonNull<c_void>,
        guest_address: u64,
        size: usize,
        flags: HvMemoryFlags,
    ) -> Result<(), BackendError> {
        // SAFETY: The caller validates page alignment, region size, and VM lifecycle before
        // mapping this userspace address into the process-local HVF VM.
        unsafe {
            check(
                hv_vm_map(host_address.as_ptr(), guest_address, size, flags),
                "hv_vm_map",
            )
        }
    }

    pub fn unmap_memory(guest_address: u64, size: usize) -> Result<(), BackendError> {
        // SAFETY: The caller owns a previously mapped guest physical range for the live
        // process-local HVF VM and guarantees the range is page aligned.
        unsafe { check(hv_vm_unmap(guest_address, size), "hv_vm_unmap") }
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

    pub fn run_vcpu(vcpu: HvVcpu) -> Result<(), BackendError> {
        // SAFETY: The caller owns this current-thread vCPU handle.
        unsafe { check(hv_vcpu_run(vcpu), "hv_vcpu_run") }
    }

    pub fn exit_vcpus(vcpus: &mut [HvVcpu]) -> Result<(), BackendError> {
        let vcpu_count = u32::try_from(vcpus.len())
            .map_err(|_| BackendError::InvalidState("too many vCPUs to exit"))?;

        // SAFETY: `vcpus.as_mut_ptr()` is valid for `vcpu_count` elements for the duration of
        // the call, and HVF only uses the ids to request asynchronous exits.
        unsafe {
            check(
                hv_vcpus_exit(vcpus.as_mut_ptr(), vcpu_count),
                "hv_vcpus_exit",
            )
        }
    }

    pub fn get_reg(vcpu: HvVcpu, reg: HvReg) -> Result<u64, BackendError> {
        let mut value = 0;

        // SAFETY: The caller owns this current-thread vCPU handle, and `value` is a valid
        // out-pointer for the duration of the call.
        unsafe { check(hv_vcpu_get_reg(vcpu, reg, &mut value), "hv_vcpu_get_reg")? };

        Ok(value)
    }

    pub fn set_reg(vcpu: HvVcpu, reg: HvReg, value: u64) -> Result<(), BackendError> {
        // SAFETY: The caller owns this current-thread vCPU handle.
        unsafe { check(hv_vcpu_set_reg(vcpu, reg, value), "hv_vcpu_set_reg") }
    }

    pub fn get_sys_reg(vcpu: HvVcpu, reg: HvSysReg) -> Result<u64, BackendError> {
        let mut value = 0;

        // SAFETY: The caller owns this current-thread vCPU handle, and `value` is a valid
        // out-pointer for the duration of the call.
        unsafe {
            check(
                hv_vcpu_get_sys_reg(vcpu, reg, &mut value),
                "hv_vcpu_get_sys_reg",
            )?
        };

        Ok(value)
    }

    pub fn set_sys_reg(vcpu: HvVcpu, reg: HvSysReg, value: u64) -> Result<(), BackendError> {
        // SAFETY: The caller owns this current-thread vCPU handle.
        unsafe { check(hv_vcpu_set_sys_reg(vcpu, reg, value), "hv_vcpu_set_sys_reg") }
    }

    pub fn get_vtimer_mask(vcpu: HvVcpu) -> Result<bool, BackendError> {
        let mut value = false;

        // SAFETY: The caller owns this current-thread vCPU handle, and `value` is a valid
        // out-pointer for the duration of the call.
        unsafe {
            check(
                hv_vcpu_get_vtimer_mask(vcpu, &mut value),
                "hv_vcpu_get_vtimer_mask",
            )?
        };

        Ok(value)
    }

    pub fn set_vtimer_mask(vcpu: HvVcpu, masked: bool) -> Result<(), BackendError> {
        // SAFETY: The caller owns this current-thread vCPU handle.
        unsafe {
            check(
                hv_vcpu_set_vtimer_mask(vcpu, masked),
                "hv_vcpu_set_vtimer_mask",
            )
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{HV_BAD_ARGUMENT, HV_DENIED, HV_UNSUPPORTED, check};

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

        #[test]
        fn check_displays_register_operation_hv_return() {
            let err =
                check(HV_BAD_ARGUMENT, "hv_vcpu_get_reg").expect_err("HV_BAD_ARGUMENT should fail");

            assert_eq!(
                err.to_string(),
                "hypervisor error: hv_vcpu_get_reg failed with HV_BAD_ARGUMENT (hv_return_t=0xfae94003)"
            );
        }
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
mod imp {
    use std::ffi::c_void;
    use std::ptr::NonNull;

    use bangbang_runtime::BackendError;

    use super::{CreatedVcpu, HvMemoryFlags, HvReg, HvSysReg, HvVcpu, UNSUPPORTED_TARGET_MESSAGE};

    pub fn create_vm() -> Result<(), BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn destroy_vm() -> Result<(), BackendError> {
        Ok(())
    }

    pub fn map_memory(
        _: NonNull<c_void>,
        _: u64,
        _: usize,
        _: HvMemoryFlags,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn unmap_memory(_: u64, _: usize) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn create_vcpu() -> Result<CreatedVcpu, BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn destroy_vcpu(_: HvVcpu) -> Result<(), BackendError> {
        Ok(())
    }

    pub fn run_vcpu(_: HvVcpu) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn exit_vcpus(_: &mut [HvVcpu]) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn get_reg(_: HvVcpu, _: HvReg) -> Result<u64, BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn set_reg(_: HvVcpu, _: HvReg, _: u64) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn get_sys_reg(_: HvVcpu, _: HvSysReg) -> Result<u64, BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn set_sys_reg(_: HvVcpu, _: HvSysReg, _: u64) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn get_vtimer_mask(_: HvVcpu) -> Result<bool, BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn set_vtimer_mask(_: HvVcpu, _: bool) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }
}

pub(crate) use imp::*;

#[cfg(test)]
mod tests {
    use std::mem::{align_of, offset_of, size_of};

    use super::{HvVcpuExit, HvVcpuExitException};

    #[test]
    fn vcpu_exit_layout_matches_hvf_sdk() {
        assert_eq!(size_of::<HvVcpuExit>(), 32);
        assert_eq!(align_of::<HvVcpuExit>(), 8);
        assert_eq!(offset_of!(HvVcpuExit, reason), 0);
        assert_eq!(offset_of!(HvVcpuExit, exception), 8);
        assert_eq!(offset_of!(HvVcpuExit, exception.syndrome), 8);
        assert_eq!(offset_of!(HvVcpuExit, exception.virtual_address), 16);
        assert_eq!(offset_of!(HvVcpuExit, exception.physical_address), 24);
    }

    #[test]
    fn vcpu_exit_exception_layout_matches_hvf_sdk() {
        assert_eq!(size_of::<HvVcpuExitException>(), 24);
        assert_eq!(align_of::<HvVcpuExitException>(), 8);
    }
}
