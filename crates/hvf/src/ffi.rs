pub(crate) const UNSUPPORTED_TARGET_MESSAGE: &str =
    "Hypervisor.framework backend currently targets macOS on Apple Silicon";
pub(crate) const SME_CONFIGURATION_REQUIRES_MACOS_15_2_MESSAGE: &str =
    "Hypervisor.framework SME configuration queries require macOS 15.2 or newer";
pub(crate) const SME_STATE_REQUIRES_MACOS_15_2_MESSAGE: &str =
    "Hypervisor.framework SME state capture requires macOS 15.2 or newer";
pub(crate) const SME_P_REGISTER_REQUIRES_MACOS_15_2_MESSAGE: &str =
    "Hypervisor.framework SME P-register capture requires macOS 15.2 or newer";
pub(crate) const SME_Z_REGISTER_REQUIRES_MACOS_15_2_MESSAGE: &str =
    "Hypervisor.framework SME Z-register capture requires macOS 15.2 or newer";

pub(crate) type HvVcpu = u64;
pub(crate) type HvExitReason = u32;
pub(crate) type HvInterruptType = u32;
pub(crate) type HvMemoryFlags = u64;
pub(crate) type HvReg = u32;
pub(crate) type HvSimdFpReg = u32;
pub(crate) type HvSysReg = u16;

pub(crate) const HV_MEMORY_READ: HvMemoryFlags = 1 << 0;
pub(crate) const HV_MEMORY_WRITE: HvMemoryFlags = 1 << 1;
pub(crate) const HV_MEMORY_EXEC: HvMemoryFlags = 1 << 2;

pub(crate) const HV_EXIT_REASON_CANCELED: HvExitReason = 0;
pub(crate) const HV_EXIT_REASON_EXCEPTION: HvExitReason = 1;
pub(crate) const HV_EXIT_REASON_VTIMER_ACTIVATED: HvExitReason = 2;
pub(crate) const HV_EXIT_REASON_UNKNOWN: HvExitReason = 3;
pub(crate) const HV_INTERRUPT_TYPE_IRQ: HvInterruptType = 0;
pub(crate) const HV_INTERRUPT_TYPE_FIQ: HvInterruptType = 1;
pub(crate) const HV_REG_X0: HvReg = 0;
pub(crate) const HV_REG_X1: HvReg = 1;
pub(crate) const HV_REG_X2: HvReg = 2;
pub(crate) const HV_REG_X3: HvReg = 3;
pub(crate) const HV_REG_PC: HvReg = 31;
pub(crate) const HV_REG_FPCR: HvReg = 32;
pub(crate) const HV_REG_FPSR: HvReg = 33;
pub(crate) const HV_REG_CPSR: HvReg = 34;
pub(crate) const HV_SIMD_FP_REG_Q0: HvSimdFpReg = 0;
pub(crate) const HV_SIMD_FP_REG_Q31: HvSimdFpReg = 31;
pub(crate) const HV_SYS_REG_DBGBVR0_EL1: HvSysReg = 0x8004;
pub(crate) const HV_SYS_REG_DBGBVR15_EL1: HvSysReg = 0x807c;
pub(crate) const HV_SYS_REG_DBGBCR0_EL1: HvSysReg = 0x8005;
pub(crate) const HV_SYS_REG_DBGBCR15_EL1: HvSysReg = 0x807d;
pub(crate) const HV_SYS_REG_DBGWVR0_EL1: HvSysReg = 0x8006;
pub(crate) const HV_SYS_REG_DBGWVR15_EL1: HvSysReg = 0x807e;
pub(crate) const HV_SYS_REG_DBGWCR0_EL1: HvSysReg = 0x8007;
pub(crate) const HV_SYS_REG_DBGWCR15_EL1: HvSysReg = 0x807f;
pub(crate) const HV_SYS_REG_DEBUG_REGISTER_STRIDE: HvSysReg = 8;
pub(crate) const HV_SYS_REG_MDCCINT_EL1: HvSysReg = 0x8010;
pub(crate) const HV_SYS_REG_MDSCR_EL1: HvSysReg = 0x8012;
pub(crate) const HV_SYS_REG_MIDR_EL1: HvSysReg = 0xc000;
pub(crate) const HV_SYS_REG_MPIDR_EL1: HvSysReg = 0xc005;
pub(crate) const HV_SYS_REG_ID_AA64PFR0_EL1: HvSysReg = 0xc020;
pub(crate) const HV_SYS_REG_ID_AA64PFR1_EL1: HvSysReg = 0xc021;
pub(crate) const HV_SYS_REG_ID_AA64ZFR0_EL1: HvSysReg = 0xc024;
pub(crate) const HV_SYS_REG_ID_AA64SMFR0_EL1: HvSysReg = 0xc025;
pub(crate) const HV_SYS_REG_ID_AA64DFR0_EL1: HvSysReg = 0xc028;
pub(crate) const HV_SYS_REG_ID_AA64DFR1_EL1: HvSysReg = 0xc029;
pub(crate) const HV_SYS_REG_ID_AA64ISAR0_EL1: HvSysReg = 0xc030;
pub(crate) const HV_SYS_REG_ID_AA64ISAR1_EL1: HvSysReg = 0xc031;
pub(crate) const HV_SYS_REG_ID_AA64MMFR0_EL1: HvSysReg = 0xc038;
pub(crate) const HV_SYS_REG_ID_AA64MMFR1_EL1: HvSysReg = 0xc039;
pub(crate) const HV_SYS_REG_ID_AA64MMFR2_EL1: HvSysReg = 0xc03a;
pub(crate) const HV_SYS_REG_SCTLR_EL1: HvSysReg = 0xc080;
pub(crate) const HV_SYS_REG_ACTLR_EL1: HvSysReg = 0xc081;
pub(crate) const HV_SYS_REG_CPACR_EL1: HvSysReg = 0xc082;
pub(crate) const HV_SYS_REG_SMPRI_EL1: HvSysReg = 0xc094;
pub(crate) const HV_SYS_REG_SMCR_EL1: HvSysReg = 0xc096;
pub(crate) const HV_SYS_REG_TTBR0_EL1: HvSysReg = 0xc100;
pub(crate) const HV_SYS_REG_TTBR1_EL1: HvSysReg = 0xc101;
pub(crate) const HV_SYS_REG_TCR_EL1: HvSysReg = 0xc102;
pub(crate) const HV_SYS_REG_APIAKEYLO_EL1: HvSysReg = 0xc108;
pub(crate) const HV_SYS_REG_APIAKEYHI_EL1: HvSysReg = 0xc109;
pub(crate) const HV_SYS_REG_APIBKEYLO_EL1: HvSysReg = 0xc10a;
pub(crate) const HV_SYS_REG_APIBKEYHI_EL1: HvSysReg = 0xc10b;
pub(crate) const HV_SYS_REG_APDAKEYLO_EL1: HvSysReg = 0xc110;
pub(crate) const HV_SYS_REG_APDAKEYHI_EL1: HvSysReg = 0xc111;
pub(crate) const HV_SYS_REG_APDBKEYLO_EL1: HvSysReg = 0xc112;
pub(crate) const HV_SYS_REG_APDBKEYHI_EL1: HvSysReg = 0xc113;
pub(crate) const HV_SYS_REG_APGAKEYLO_EL1: HvSysReg = 0xc118;
pub(crate) const HV_SYS_REG_APGAKEYHI_EL1: HvSysReg = 0xc119;
pub(crate) const HV_SYS_REG_SPSR_EL1: HvSysReg = 0xc200;
pub(crate) const HV_SYS_REG_ELR_EL1: HvSysReg = 0xc201;
pub(crate) const HV_SYS_REG_SP_EL0: HvSysReg = 0xc208;
pub(crate) const HV_SYS_REG_AFSR0_EL1: HvSysReg = 0xc288;
pub(crate) const HV_SYS_REG_AFSR1_EL1: HvSysReg = 0xc289;
pub(crate) const HV_SYS_REG_ESR_EL1: HvSysReg = 0xc290;
pub(crate) const HV_SYS_REG_FAR_EL1: HvSysReg = 0xc300;
pub(crate) const HV_SYS_REG_PAR_EL1: HvSysReg = 0xc3a0;
pub(crate) const HV_SYS_REG_MAIR_EL1: HvSysReg = 0xc510;
pub(crate) const HV_SYS_REG_AMAIR_EL1: HvSysReg = 0xc518;
pub(crate) const HV_SYS_REG_VBAR_EL1: HvSysReg = 0xc600;
pub(crate) const HV_SYS_REG_CONTEXTIDR_EL1: HvSysReg = 0xc681;
pub(crate) const HV_SYS_REG_TPIDR_EL1: HvSysReg = 0xc684;
pub(crate) const HV_SYS_REG_SCXTNUM_EL1: HvSysReg = 0xc687;
pub(crate) const HV_SYS_REG_CNTKCTL_EL1: HvSysReg = 0xc708;
pub(crate) const HV_SYS_REG_CSSELR_EL1: HvSysReg = 0xd000;
pub(crate) const HV_SYS_REG_TPIDR_EL0: HvSysReg = 0xde82;
pub(crate) const HV_SYS_REG_TPIDRRO_EL0: HvSysReg = 0xde83;
pub(crate) const HV_SYS_REG_TPIDR2_EL0: HvSysReg = 0xde85;
pub(crate) const HV_SYS_REG_SCXTNUM_EL0: HvSysReg = 0xde87;
pub(crate) const HV_SYS_REG_CNTP_CTL_EL0: HvSysReg = 0xdf11;
pub(crate) const HV_SYS_REG_CNTP_CVAL_EL0: HvSysReg = 0xdf12;
pub(crate) const HV_SYS_REG_CNTP_TVAL_EL0: HvSysReg = 0xdf10;
pub(crate) const HV_SYS_REG_CNTV_CTL_EL0: HvSysReg = 0xdf19;
pub(crate) const HV_SYS_REG_CNTV_CVAL_EL0: HvSysReg = 0xdf1a;
pub(crate) const HV_SYS_REG_SP_EL1: HvSysReg = 0xe208;

#[repr(C, align(16))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HvSimdFpValue([u8; 16]);

impl HvSimdFpValue {
    const fn zeroed() -> Self {
        Self([0; 16])
    }

    const fn into_bytes(self) -> [u8; 16] {
        self.0
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HvVcpuSmeState {
    streaming_sve_mode_enabled: bool,
    za_storage_enabled: bool,
}

impl HvVcpuSmeState {
    const fn zeroed() -> Self {
        Self {
            streaming_sve_mode_enabled: false,
            za_storage_enabled: false,
        }
    }

    const fn into_parts(self) -> (bool, bool) {
        (self.streaming_sve_mode_enabled, self.za_storage_enabled)
    }
}

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
    use std::mem;
    use std::ptr;
    use std::ptr::NonNull;

    use bangbang_runtime::BackendError;

    use super::{
        CreatedVcpu, HvInterruptType, HvMemoryFlags, HvReg, HvSimdFpReg, HvSimdFpValue, HvSysReg,
        HvVcpu, HvVcpuExit, HvVcpuSmeState, SME_CONFIGURATION_REQUIRES_MACOS_15_2_MESSAGE,
        SME_P_REGISTER_REQUIRES_MACOS_15_2_MESSAGE, SME_STATE_REQUIRES_MACOS_15_2_MESSAGE,
        SME_Z_REGISTER_REQUIRES_MACOS_15_2_MESSAGE,
    };

    pub type HvReturn = i32;
    pub type HvVmConfig = *mut c_void;
    pub type HvVcpuConfig = *mut c_void;

    pub const HV_SUCCESS: HvReturn = 0;
    const DYNAMIC_SYMBOL_SIZE_MISMATCH_MESSAGE: &str =
        "Hypervisor.framework SME symbol pointer size does not match a function pointer";
    const HV_ERROR: HvReturn = 0xfae94001u32 as HvReturn;
    const HV_BUSY: HvReturn = 0xfae94002u32 as HvReturn;
    const HV_BAD_ARGUMENT: HvReturn = 0xfae94003u32 as HvReturn;
    const HV_NO_RESOURCES: HvReturn = 0xfae94005u32 as HvReturn;
    const HV_NO_DEVICE: HvReturn = 0xfae94006u32 as HvReturn;
    const HV_DENIED: HvReturn = 0xfae94007u32 as HvReturn;
    const HV_FAULT: HvReturn = 0xfae94008u32 as HvReturn;
    const HV_UNSUPPORTED: HvReturn = 0xfae9400fu32 as HvReturn;

    type HvCacheType = u32;
    type HvFeatureReg = u32;
    type HvVcpuConfigCreate = unsafe extern "C" fn() -> HvVcpuConfig;
    type HvVcpuConfigGetCcsidrEl1SysRegValues =
        unsafe extern "C" fn(HvVcpuConfig, HvCacheType, *mut u64) -> HvReturn;
    type HvVcpuConfigGetFeatureReg =
        unsafe extern "C" fn(HvVcpuConfig, HvFeatureReg, *mut u64) -> HvReturn;
    type OsRelease = unsafe extern "C" fn(*mut c_void);
    type HvSmeConfigGetMaxSvlBytes = unsafe extern "C" fn(value: *mut usize) -> HvReturn;
    type HvSmePReg = u32;
    type HvSmeZReg = u32;
    type HvVcpuGetSmeState =
        unsafe extern "C" fn(vcpu: HvVcpu, sme_state: *mut HvVcpuSmeState) -> HvReturn;
    type HvVcpuGetSmePReg = unsafe extern "C" fn(
        vcpu: HvVcpu,
        reg: HvSmePReg,
        value: *mut u8,
        length: usize,
    ) -> HvReturn;
    type HvVcpuGetSmeZReg = unsafe extern "C" fn(
        vcpu: HvVcpu,
        reg: HvSmeZReg,
        value: *mut u8,
        length: usize,
    ) -> HvReturn;

    const HV_CACHE_TYPE_DATA: HvCacheType = 0;
    const HV_CACHE_TYPE_INSTRUCTION: HvCacheType = 1;
    const HV_FEATURE_REG_CTR_EL0: HvFeatureReg = 9;
    const HV_FEATURE_REG_CLIDR_EL1: HvFeatureReg = 10;
    const HV_FEATURE_REG_DCZID_EL0: HvFeatureReg = 11;

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
        pub fn hv_vcpu_config_create() -> HvVcpuConfig;
        pub fn hv_vcpu_config_get_ccsidr_el1_sys_reg_values(
            config: HvVcpuConfig,
            cache_type: HvCacheType,
            values: *mut u64,
        ) -> HvReturn;
        pub fn hv_vcpu_config_get_feature_reg(
            config: HvVcpuConfig,
            feature_reg: HvFeatureReg,
            value: *mut u64,
        ) -> HvReturn;
        pub fn hv_vcpu_create(
            vcpu: *mut HvVcpu,
            exit: *mut *mut HvVcpuExit,
            config: HvVcpuConfig,
        ) -> HvReturn;
        pub fn hv_vcpu_destroy(vcpu: HvVcpu) -> HvReturn;
        pub fn hv_vcpu_get_pending_interrupt(
            vcpu: HvVcpu,
            interrupt_type: HvInterruptType,
            pending: *mut bool,
        ) -> HvReturn;
        pub fn hv_vcpu_set_pending_interrupt(
            vcpu: HvVcpu,
            interrupt_type: HvInterruptType,
            pending: bool,
        ) -> HvReturn;
        pub fn hv_vcpu_get_trap_debug_exceptions(vcpu: HvVcpu, value: *mut bool) -> HvReturn;
        pub fn hv_vcpu_get_trap_debug_reg_accesses(vcpu: HvVcpu, value: *mut bool) -> HvReturn;
        pub fn hv_vcpu_get_reg(vcpu: HvVcpu, reg: HvReg, value: *mut u64) -> HvReturn;
        pub fn hv_vcpu_set_reg(vcpu: HvVcpu, reg: HvReg, value: u64) -> HvReturn;
        pub fn hv_vcpu_get_simd_fp_reg(
            vcpu: HvVcpu,
            reg: HvSimdFpReg,
            value: *mut HvSimdFpValue,
        ) -> HvReturn;
        pub fn hv_vcpu_get_sys_reg(vcpu: HvVcpu, reg: HvSysReg, value: *mut u64) -> HvReturn;
        pub fn hv_vcpu_set_sys_reg(vcpu: HvVcpu, reg: HvSysReg, value: u64) -> HvReturn;
        pub fn hv_vcpu_get_vtimer_mask(vcpu: HvVcpu, vtimer_is_masked: *mut bool) -> HvReturn;
        pub fn hv_vcpu_set_vtimer_mask(vcpu: HvVcpu, vtimer_is_masked: bool) -> HvReturn;
        pub fn hv_vcpu_get_vtimer_offset(vcpu: HvVcpu, vtimer_offset: *mut u64) -> HvReturn;
        pub fn hv_vcpu_set_vtimer_offset(vcpu: HvVcpu, vtimer_offset: u64) -> HvReturn;
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

    #[derive(Debug)]
    struct HvVcpuConfigOwner {
        config: NonNull<c_void>,
        release: OsRelease,
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

    impl HvVcpuConfigOwner {
        fn create_with(
            create: HvVcpuConfigCreate,
            release: OsRelease,
        ) -> Result<Self, BackendError> {
            // SAFETY: The injected function has the SDK's exact
            // `hv_vcpu_config_create` ABI and returns a retained object or null.
            let config = unsafe { create() };
            let config = NonNull::new(config).ok_or(BackendError::Hypervisor(
                "hv_vcpu_config_create returned null".to_string(),
            ))?;

            Ok(Self { config, release })
        }

        fn as_ptr(&self) -> HvVcpuConfig {
            self.config.as_ptr()
        }
    }

    impl Drop for HvVcpuConfigOwner {
        fn drop(&mut self) {
            // SAFETY: `self.config` is the retained object returned by the
            // injected create function, and this guard releases it exactly once.
            unsafe { (self.release)(self.config.as_ptr()) };
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

    fn load_sme_config_max_svl_bytes_getter() -> Result<HvSmeConfigGetMaxSvlBytes, BackendError> {
        // SAFETY: `RTLD_DEFAULT` searches the already loaded process images, and
        // the symbol name is a NUL-terminated static C string.
        let symbol = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                c"hv_sme_config_get_max_svl_bytes".as_ptr(),
            )
        };
        sme_config_max_svl_bytes_getter_from_symbol(symbol)
    }

    fn sme_config_max_svl_bytes_getter_from_symbol(
        symbol: *mut c_void,
    ) -> Result<HvSmeConfigGetMaxSvlBytes, BackendError> {
        if mem::size_of::<HvSmeConfigGetMaxSvlBytes>() != mem::size_of::<*mut c_void>() {
            return Err(BackendError::InvalidState(
                DYNAMIC_SYMBOL_SIZE_MISMATCH_MESSAGE,
            ));
        }

        if symbol.is_null() {
            return Err(BackendError::Unsupported(
                SME_CONFIGURATION_REQUIRES_MACOS_15_2_MESSAGE,
            ));
        }

        // SAFETY: The requested symbol has the SDK's
        // `hv_sme_config_get_max_svl_bytes` signature. Function pointers and
        // dynamic symbol pointers have the same representation on this target,
        // checked above.
        Ok(unsafe { mem::transmute_copy::<*mut c_void, HvSmeConfigGetMaxSvlBytes>(&symbol) })
    }

    fn load_get_sme_state() -> Result<HvVcpuGetSmeState, BackendError> {
        // SAFETY: `RTLD_DEFAULT` searches the already loaded process images, and
        // the symbol name is a NUL-terminated static C string.
        let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"hv_vcpu_get_sme_state".as_ptr()) };
        sme_state_getter_from_symbol(symbol)
    }

    fn sme_state_getter_from_symbol(
        symbol: *mut c_void,
    ) -> Result<HvVcpuGetSmeState, BackendError> {
        if mem::size_of::<HvVcpuGetSmeState>() != mem::size_of::<*mut c_void>() {
            return Err(BackendError::InvalidState(
                DYNAMIC_SYMBOL_SIZE_MISMATCH_MESSAGE,
            ));
        }

        if symbol.is_null() {
            return Err(BackendError::Unsupported(
                SME_STATE_REQUIRES_MACOS_15_2_MESSAGE,
            ));
        }

        // SAFETY: The requested symbol has the SDK's `hv_vcpu_get_sme_state`
        // signature. Function pointers and dynamic symbol pointers have the same
        // representation on this target, checked above.
        Ok(unsafe { mem::transmute_copy::<*mut c_void, HvVcpuGetSmeState>(&symbol) })
    }

    fn load_get_sme_p_reg() -> Result<HvVcpuGetSmePReg, BackendError> {
        // SAFETY: `RTLD_DEFAULT` searches the already loaded process images, and
        // the symbol name is a NUL-terminated static C string.
        let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"hv_vcpu_get_sme_p_reg".as_ptr()) };
        sme_p_reg_getter_from_symbol(symbol)
    }

    fn sme_p_reg_getter_from_symbol(symbol: *mut c_void) -> Result<HvVcpuGetSmePReg, BackendError> {
        if mem::size_of::<HvVcpuGetSmePReg>() != mem::size_of::<*mut c_void>() {
            return Err(BackendError::Hypervisor(
                DYNAMIC_SYMBOL_SIZE_MISMATCH_MESSAGE.to_string(),
            ));
        }

        if symbol.is_null() {
            return Err(BackendError::Unsupported(
                SME_P_REGISTER_REQUIRES_MACOS_15_2_MESSAGE,
            ));
        }

        // SAFETY: The requested symbol has the SDK's `hv_vcpu_get_sme_p_reg`
        // signature. Function pointers and dynamic symbol pointers have the same
        // representation on this target, checked above.
        Ok(unsafe { mem::transmute_copy::<*mut c_void, HvVcpuGetSmePReg>(&symbol) })
    }

    fn load_get_sme_z_reg() -> Result<HvVcpuGetSmeZReg, BackendError> {
        // SAFETY: `RTLD_DEFAULT` searches the already loaded process images, and
        // the symbol name is a NUL-terminated static C string.
        let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"hv_vcpu_get_sme_z_reg".as_ptr()) };
        sme_z_reg_getter_from_symbol(symbol)
    }

    fn sme_z_reg_getter_from_symbol(symbol: *mut c_void) -> Result<HvVcpuGetSmeZReg, BackendError> {
        if mem::size_of::<HvVcpuGetSmeZReg>() != mem::size_of::<*mut c_void>() {
            return Err(BackendError::Hypervisor(
                DYNAMIC_SYMBOL_SIZE_MISMATCH_MESSAGE.to_string(),
            ));
        }

        if symbol.is_null() {
            return Err(BackendError::Unsupported(
                SME_Z_REGISTER_REQUIRES_MACOS_15_2_MESSAGE,
            ));
        }

        // SAFETY: The requested symbol has the SDK's `hv_vcpu_get_sme_z_reg`
        // signature. Function pointers and dynamic symbol pointers have the same
        // representation on this target, checked above.
        Ok(unsafe { mem::transmute_copy::<*mut c_void, HvVcpuGetSmeZReg>(&symbol) })
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

    fn get_vcpu_config_feature_reg_with(
        config: &HvVcpuConfigOwner,
        get_feature_reg: HvVcpuConfigGetFeatureReg,
        feature_reg: HvFeatureReg,
    ) -> Result<u64, BackendError> {
        let mut value = 0;

        // SAFETY: `config` owns a live configuration object, the injected
        // function has the SDK's exact getter ABI, and `value` is a valid output
        // pointer for the duration of the call.
        unsafe {
            check(
                get_feature_reg(config.as_ptr(), feature_reg, &mut value),
                "hv_vcpu_config_get_feature_reg",
            )?;
        }

        Ok(value)
    }

    fn get_arm64_vcpu_cache_feature_registers_with(
        create: HvVcpuConfigCreate,
        get_feature_reg: HvVcpuConfigGetFeatureReg,
        release: OsRelease,
    ) -> Result<[u64; 3], BackendError> {
        let config = HvVcpuConfigOwner::create_with(create, release)?;
        let ctr_el0 =
            get_vcpu_config_feature_reg_with(&config, get_feature_reg, HV_FEATURE_REG_CTR_EL0)?;
        let clidr_el1 =
            get_vcpu_config_feature_reg_with(&config, get_feature_reg, HV_FEATURE_REG_CLIDR_EL1)?;
        let dczid_el0 =
            get_vcpu_config_feature_reg_with(&config, get_feature_reg, HV_FEATURE_REG_DCZID_EL0)?;

        Ok([ctr_el0, clidr_el1, dczid_el0])
    }

    pub fn get_arm64_vcpu_cache_feature_registers() -> Result<[u64; 3], BackendError> {
        get_arm64_vcpu_cache_feature_registers_with(
            hv_vcpu_config_create,
            hv_vcpu_config_get_feature_reg,
            os_release,
        )
    }

    fn get_vcpu_config_ccsidr_el1_values_with(
        config: &HvVcpuConfigOwner,
        get_ccsidr_el1_sys_reg_values: HvVcpuConfigGetCcsidrEl1SysRegValues,
        cache_type: HvCacheType,
    ) -> Result<[u64; 8], BackendError> {
        let mut values = [0; 8];

        // SAFETY: `config` owns a live configuration object, the injected
        // function has the SDK's exact getter ABI, and `values` supplies the
        // required writable eight-element `u64` buffer for the duration of the
        // call.
        unsafe {
            check(
                get_ccsidr_el1_sys_reg_values(config.as_ptr(), cache_type, values.as_mut_ptr()),
                "hv_vcpu_config_get_ccsidr_el1_sys_reg_values",
            )?;
        }

        Ok(values)
    }

    fn get_arm64_vcpu_cache_geometry_with(
        create: HvVcpuConfigCreate,
        get_ccsidr_el1_sys_reg_values: HvVcpuConfigGetCcsidrEl1SysRegValues,
        release: OsRelease,
    ) -> Result<[[u64; 8]; 2], BackendError> {
        let config = HvVcpuConfigOwner::create_with(create, release)?;
        let data_or_unified = get_vcpu_config_ccsidr_el1_values_with(
            &config,
            get_ccsidr_el1_sys_reg_values,
            HV_CACHE_TYPE_DATA,
        )?;
        let instruction = get_vcpu_config_ccsidr_el1_values_with(
            &config,
            get_ccsidr_el1_sys_reg_values,
            HV_CACHE_TYPE_INSTRUCTION,
        )?;

        Ok([data_or_unified, instruction])
    }

    pub fn get_arm64_vcpu_cache_geometry() -> Result<[[u64; 8]; 2], BackendError> {
        get_arm64_vcpu_cache_geometry_with(
            hv_vcpu_config_create,
            hv_vcpu_config_get_ccsidr_el1_sys_reg_values,
            os_release,
        )
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

    pub fn get_pending_interrupt(
        vcpu: HvVcpu,
        interrupt_type: HvInterruptType,
    ) -> Result<bool, BackendError> {
        let mut pending = false;

        // SAFETY: The caller owns this current-thread vCPU handle, and `pending` is a valid
        // out-pointer for the duration of the call.
        unsafe {
            check(
                hv_vcpu_get_pending_interrupt(vcpu, interrupt_type, &mut pending),
                "hv_vcpu_get_pending_interrupt",
            )?;
        }

        Ok(pending)
    }

    pub fn set_pending_interrupt(
        vcpu: HvVcpu,
        interrupt_type: HvInterruptType,
        pending: bool,
    ) -> Result<(), BackendError> {
        // SAFETY: The caller owns this current-thread vCPU handle.
        unsafe {
            check(
                hv_vcpu_set_pending_interrupt(vcpu, interrupt_type, pending),
                "hv_vcpu_set_pending_interrupt",
            )
        }
    }

    pub fn get_trap_debug_exceptions(vcpu: HvVcpu) -> Result<bool, BackendError> {
        let mut value = false;

        // SAFETY: The caller owns this current-thread vCPU handle, and `value` is a valid
        // out-pointer for the duration of the call.
        unsafe {
            check(
                hv_vcpu_get_trap_debug_exceptions(vcpu, &mut value),
                "hv_vcpu_get_trap_debug_exceptions",
            )?;
        }

        Ok(value)
    }

    pub fn get_trap_debug_reg_accesses(vcpu: HvVcpu) -> Result<bool, BackendError> {
        let mut value = false;

        // SAFETY: The caller owns this current-thread vCPU handle, and `value` is a valid
        // out-pointer for the duration of the call.
        unsafe {
            check(
                hv_vcpu_get_trap_debug_reg_accesses(vcpu, &mut value),
                "hv_vcpu_get_trap_debug_reg_accesses",
            )?;
        }

        Ok(value)
    }

    pub fn get_sme_state(vcpu: HvVcpu) -> Result<(bool, bool), BackendError> {
        let get_sme_state = load_get_sme_state()?;
        let mut value = HvVcpuSmeState::zeroed();

        // SAFETY: The caller owns this current-thread vCPU handle, the dynamically
        // resolved function has the SDK's exact C ABI, and `value` is a valid
        // `hv_vcpu_sme_state_t` out-pointer for the duration of the call.
        unsafe {
            check(get_sme_state(vcpu, &mut value), "hv_vcpu_get_sme_state")?;
        }

        Ok(value.into_parts())
    }

    fn get_sme_p_reg_with(
        get_sme_p_reg: HvVcpuGetSmePReg,
        vcpu: HvVcpu,
        reg: HvSmePReg,
        value: &mut [u8],
    ) -> Result<(), BackendError> {
        // SAFETY: The caller owns the current-thread vCPU handle, the dynamically
        // resolved function has the SDK's exact C ABI, and `value` is a live
        // writable byte slice whose pointer and full length remain valid for the
        // duration of the call.
        unsafe {
            check(
                get_sme_p_reg(vcpu, reg, value.as_mut_ptr(), value.len()),
                "hv_vcpu_get_sme_p_reg",
            )
        }
    }

    pub fn get_sme_p_reg(
        vcpu: HvVcpu,
        reg: HvSmePReg,
        value: &mut [u8],
    ) -> Result<(), BackendError> {
        get_sme_p_reg_with(load_get_sme_p_reg()?, vcpu, reg, value)
    }

    fn get_sme_z_reg_with(
        get_sme_z_reg: HvVcpuGetSmeZReg,
        vcpu: HvVcpu,
        reg: HvSmeZReg,
        value: &mut [u8],
    ) -> Result<(), BackendError> {
        // SAFETY: The caller owns the current-thread vCPU handle, the dynamically
        // resolved function has the SDK's exact C ABI, and `value` is a live
        // writable byte slice whose pointer and full length remain valid for the
        // duration of the call.
        unsafe {
            check(
                get_sme_z_reg(vcpu, reg, value.as_mut_ptr(), value.len()),
                "hv_vcpu_get_sme_z_reg",
            )
        }
    }

    pub fn get_sme_z_reg(
        vcpu: HvVcpu,
        reg: HvSmeZReg,
        value: &mut [u8],
    ) -> Result<(), BackendError> {
        get_sme_z_reg_with(load_get_sme_z_reg()?, vcpu, reg, value)
    }

    fn get_sme_config_max_svl_bytes_with(
        get_max_svl_bytes: HvSmeConfigGetMaxSvlBytes,
    ) -> Result<usize, BackendError> {
        let mut value = 0;

        // SAFETY: The dynamically resolved function has the SDK's exact C ABI,
        // and `value` is a valid `size_t` out-pointer for the duration of the
        // call.
        unsafe {
            check(
                get_max_svl_bytes(&mut value),
                "hv_sme_config_get_max_svl_bytes",
            )?;
        }

        Ok(value)
    }

    pub fn get_sme_config_max_svl_bytes() -> Result<usize, BackendError> {
        get_sme_config_max_svl_bytes_with(load_sme_config_max_svl_bytes_getter()?)
    }

    pub fn get_simd_fp_reg(vcpu: HvVcpu, reg: HvSimdFpReg) -> Result<[u8; 16], BackendError> {
        let mut value = HvSimdFpValue::zeroed();

        // SAFETY: The caller owns this current-thread vCPU handle, and `value` is a valid,
        // 16-byte-aligned out-pointer for the duration of the call.
        unsafe {
            check(
                hv_vcpu_get_simd_fp_reg(vcpu, reg, &mut value),
                "hv_vcpu_get_simd_fp_reg",
            )?
        };

        Ok(value.into_bytes())
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

    pub fn get_vtimer_offset(vcpu: HvVcpu) -> Result<u64, BackendError> {
        let mut value = 0;

        // SAFETY: The caller owns this current-thread vCPU handle, and `value` is a valid
        // out-pointer for the duration of the call.
        unsafe {
            check(
                hv_vcpu_get_vtimer_offset(vcpu, &mut value),
                "hv_vcpu_get_vtimer_offset",
            )?
        };

        Ok(value)
    }

    pub fn set_vtimer_offset(vcpu: HvVcpu, offset: u64) -> Result<(), BackendError> {
        // SAFETY: The caller owns this current-thread vCPU handle.
        unsafe {
            check(
                hv_vcpu_set_vtimer_offset(vcpu, offset),
                "hv_vcpu_set_vtimer_offset",
            )
        }
    }

    #[cfg(test)]
    mod tests {
        use std::ffi::c_void;
        use std::panic;
        use std::ptr;
        use std::sync::Mutex;

        use bangbang_runtime::BackendError;

        use super::{
            HV_BAD_ARGUMENT, HV_CACHE_TYPE_DATA, HV_CACHE_TYPE_INSTRUCTION, HV_DENIED,
            HV_FEATURE_REG_CLIDR_EL1, HV_FEATURE_REG_CTR_EL0, HV_FEATURE_REG_DCZID_EL0, HV_SUCCESS,
            HV_UNSUPPORTED, HvVcpuConfig, HvVcpuConfigOwner, check,
            get_arm64_vcpu_cache_feature_registers_with, get_arm64_vcpu_cache_geometry_with,
            get_sme_config_max_svl_bytes_with, get_sme_p_reg_with, get_sme_z_reg_with,
            sme_config_max_svl_bytes_getter_from_symbol, sme_p_reg_getter_from_symbol,
            sme_state_getter_from_symbol, sme_z_reg_getter_from_symbol,
        };
        use crate::ffi::{
            SME_CONFIGURATION_REQUIRES_MACOS_15_2_MESSAGE,
            SME_P_REGISTER_REQUIRES_MACOS_15_2_MESSAGE, SME_STATE_REQUIRES_MACOS_15_2_MESSAGE,
            SME_Z_REGISTER_REQUIRES_MACOS_15_2_MESSAGE,
        };

        const TEST_MAX_SVL_BYTES: usize = usize::MAX - 0x1234;
        const TEST_SME_P_BYTES: [u8; 5] = [0xff, 0, 0x13, 0x57, 0x9b];
        const TEST_SME_P_VCPU: u64 = 0xfedc_ba98_7654_3210;
        const TEST_SME_P_REG: u32 = 15;
        const TEST_SME_Z_BYTES: [u8; 7] = [0, 1, 0xff, 0x23, 0x45, 0x67, 0x89];
        const TEST_SME_Z_VCPU: u64 = 0x0123_4567_89ab_cdef;
        const TEST_SME_Z_REG: u32 = 31;
        const TEST_CACHE_FEATURE_VALUES: [u64; 3] = [0, u64::MAX, 0x0123_4567_89ab_cdef];
        const TEST_CACHE_GEOMETRY: [[u64; 8]; 2] = [
            [
                0,
                1,
                u64::MAX,
                0x0123_4567_89ab_cdef,
                0xfedc_ba98_7654_3210,
                0x1111_2222_3333_4444,
                0xaaaa_bbbb_cccc_dddd,
                0x5555_6666_7777_8888,
            ],
            [
                0x8000_0000_0000_0000,
                0x7fff_ffff_ffff_ffff,
                2,
                3,
                4,
                5,
                6,
                7,
            ],
        ];

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum TestVcpuConfigCall {
            Create,
            Get(u32),
            GetCcsidr(u32),
            Release,
        }

        #[derive(Debug)]
        struct TestVcpuConfigState {
            calls: Vec<TestVcpuConfigCall>,
            fail_on_get: Option<usize>,
            get_count: usize,
            config_pointer_matches: bool,
        }

        impl TestVcpuConfigState {
            const fn new() -> Self {
                Self {
                    calls: Vec::new(),
                    fail_on_get: None,
                    get_count: 0,
                    config_pointer_matches: true,
                }
            }
        }

        static TEST_VCPU_CONFIG_LOCK: Mutex<()> = Mutex::new(());
        static TEST_VCPU_CONFIG_STATE: Mutex<TestVcpuConfigState> =
            Mutex::new(TestVcpuConfigState::new());

        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        struct TestSmePCall {
            vcpu: u64,
            reg: u32,
            length: usize,
        }

        static TEST_SME_P_LOCK: Mutex<()> = Mutex::new(());
        static TEST_SME_P_CALL: Mutex<Option<TestSmePCall>> = Mutex::new(None);

        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        struct TestSmeZCall {
            vcpu: u64,
            reg: u32,
            length: usize,
        }

        static TEST_SME_Z_LOCK: Mutex<()> = Mutex::new(());
        static TEST_SME_Z_CALL: Mutex<Option<TestSmeZCall>> = Mutex::new(None);

        fn test_vcpu_config_pointer() -> HvVcpuConfig {
            std::ptr::NonNull::<u8>::dangling()
                .cast::<c_void>()
                .as_ptr()
        }

        fn reset_test_vcpu_config_state(fail_on_get: Option<usize>) {
            *lock_test_vcpu_config_state() = TestVcpuConfigState {
                fail_on_get,
                ..TestVcpuConfigState::new()
            };
        }

        fn lock_test_vcpu_config_state() -> std::sync::MutexGuard<'static, TestVcpuConfigState> {
            TEST_VCPU_CONFIG_STATE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        unsafe extern "C" fn test_vcpu_config_create() -> HvVcpuConfig {
            lock_test_vcpu_config_state()
                .calls
                .push(TestVcpuConfigCall::Create);
            test_vcpu_config_pointer()
        }

        unsafe extern "C" fn test_vcpu_config_create_null() -> HvVcpuConfig {
            lock_test_vcpu_config_state()
                .calls
                .push(TestVcpuConfigCall::Create);
            ptr::null_mut()
        }

        unsafe extern "C" fn test_vcpu_config_get_feature_reg(
            config: HvVcpuConfig,
            feature_reg: u32,
            value: *mut u64,
        ) -> super::HvReturn {
            let mut state = lock_test_vcpu_config_state();
            state.config_pointer_matches &= config == test_vcpu_config_pointer();
            state.calls.push(TestVcpuConfigCall::Get(feature_reg));
            let get_index = state.get_count;
            state.get_count += 1;
            if state.fail_on_get == Some(get_index) {
                return HV_BAD_ARGUMENT;
            }
            let register_value = match feature_reg {
                HV_FEATURE_REG_CTR_EL0 => TEST_CACHE_FEATURE_VALUES[0],
                HV_FEATURE_REG_CLIDR_EL1 => TEST_CACHE_FEATURE_VALUES[1],
                HV_FEATURE_REG_DCZID_EL0 => TEST_CACHE_FEATURE_VALUES[2],
                _ => return HV_BAD_ARGUMENT,
            };
            drop(state);

            // SAFETY: The FFI test helper is called only through the production
            // helper, which supplies a live `u64` output pointer.
            unsafe { *value = register_value };
            HV_SUCCESS
        }

        unsafe extern "C" fn test_vcpu_config_get_ccsidr_el1_sys_reg_values(
            config: HvVcpuConfig,
            cache_type: u32,
            values: *mut u64,
        ) -> super::HvReturn {
            let mut state = lock_test_vcpu_config_state();
            state.config_pointer_matches &= config == test_vcpu_config_pointer();
            state.calls.push(TestVcpuConfigCall::GetCcsidr(cache_type));
            let get_index = state.get_count;
            state.get_count += 1;
            if state.fail_on_get == Some(get_index) {
                return HV_BAD_ARGUMENT;
            }
            let cache_values = match cache_type {
                HV_CACHE_TYPE_DATA => &TEST_CACHE_GEOMETRY[0],
                HV_CACHE_TYPE_INSTRUCTION => &TEST_CACHE_GEOMETRY[1],
                _ => return HV_BAD_ARGUMENT,
            };
            drop(state);

            // SAFETY: The FFI test helper is called only through the production
            // helper, which supplies a live eight-element `u64` output buffer.
            unsafe { ptr::copy_nonoverlapping(cache_values.as_ptr(), values, cache_values.len()) };
            HV_SUCCESS
        }

        unsafe extern "C" fn test_vcpu_config_release(config: *mut c_void) {
            let mut state = lock_test_vcpu_config_state();
            state.config_pointer_matches &= config == test_vcpu_config_pointer();
            state.calls.push(TestVcpuConfigCall::Release);
        }

        fn test_vcpu_config_calls() -> (Vec<TestVcpuConfigCall>, bool) {
            let state = lock_test_vcpu_config_state();
            (state.calls.clone(), state.config_pointer_matches)
        }

        unsafe extern "C" fn test_get_max_svl_bytes(value: *mut usize) -> super::HvReturn {
            // SAFETY: The FFI test helper is called only through
            // `get_sme_config_max_svl_bytes_with`, which supplies a live `usize`
            // out-pointer.
            unsafe { *value = TEST_MAX_SVL_BYTES };
            HV_SUCCESS
        }

        unsafe extern "C" fn test_get_max_svl_bytes_unsupported(_: *mut usize) -> super::HvReturn {
            HV_UNSUPPORTED
        }

        fn lock_test_sme_p_call() -> std::sync::MutexGuard<'static, Option<TestSmePCall>> {
            TEST_SME_P_CALL
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        unsafe extern "C" fn test_get_sme_p_reg(
            vcpu: u64,
            reg: u32,
            value: *mut u8,
            length: usize,
        ) -> super::HvReturn {
            *lock_test_sme_p_call() = Some(TestSmePCall { vcpu, reg, length });
            if length != TEST_SME_P_BYTES.len() {
                return HV_BAD_ARGUMENT;
            }

            // SAFETY: The FFI test helper is called only through the production
            // helper, which supplies a live output slice of `length` bytes.
            unsafe {
                ptr::copy_nonoverlapping(TEST_SME_P_BYTES.as_ptr(), value, length);
            }
            HV_SUCCESS
        }

        unsafe extern "C" fn test_get_sme_p_reg_unsupported(
            _: u64,
            _: u32,
            _: *mut u8,
            _: usize,
        ) -> super::HvReturn {
            HV_UNSUPPORTED
        }

        fn lock_test_sme_z_call() -> std::sync::MutexGuard<'static, Option<TestSmeZCall>> {
            TEST_SME_Z_CALL
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        unsafe extern "C" fn test_get_sme_z_reg(
            vcpu: u64,
            reg: u32,
            value: *mut u8,
            length: usize,
        ) -> super::HvReturn {
            *lock_test_sme_z_call() = Some(TestSmeZCall { vcpu, reg, length });
            if length != TEST_SME_Z_BYTES.len() {
                return HV_BAD_ARGUMENT;
            }

            // SAFETY: The FFI test helper is called only through the production
            // helper, which supplies a live output slice of `length` bytes.
            unsafe {
                ptr::copy_nonoverlapping(TEST_SME_Z_BYTES.as_ptr(), value, length);
            }
            HV_SUCCESS
        }

        unsafe extern "C" fn test_get_sme_z_reg_unsupported(
            _: u64,
            _: u32,
            _: *mut u8,
            _: usize,
        ) -> super::HvReturn {
            HV_UNSUPPORTED
        }

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

        #[test]
        fn check_displays_debug_trap_operation_hv_return() {
            let err = check(HV_BAD_ARGUMENT, "hv_vcpu_get_trap_debug_exceptions")
                .expect_err("HV_BAD_ARGUMENT should fail");

            assert_eq!(
                err.to_string(),
                "hypervisor error: hv_vcpu_get_trap_debug_exceptions failed with HV_BAD_ARGUMENT (hv_return_t=0xfae94003)"
            );
        }

        #[test]
        fn check_displays_sme_state_operation_hv_return() {
            let err = check(HV_UNSUPPORTED, "hv_vcpu_get_sme_state")
                .expect_err("HV_UNSUPPORTED should fail");

            assert_eq!(
                err.to_string(),
                "hypervisor error: hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)"
            );
        }

        #[test]
        fn missing_sme_state_symbol_reports_macos_boundary() {
            assert_eq!(
                sme_state_getter_from_symbol(ptr::null_mut()),
                Err(BackendError::Unsupported(
                    SME_STATE_REQUIRES_MACOS_15_2_MESSAGE
                ))
            );
        }

        #[test]
        fn missing_sme_p_register_symbol_reports_macos_boundary() {
            assert_eq!(
                sme_p_reg_getter_from_symbol(ptr::null_mut()),
                Err(BackendError::Unsupported(
                    SME_P_REGISTER_REQUIRES_MACOS_15_2_MESSAGE
                ))
            );
        }

        #[test]
        fn sme_p_register_getter_preserves_id_length_and_bytes() {
            let _lock = TEST_SME_P_LOCK
                .lock()
                .expect("test SME P-register lock should not be poisoned");
            *lock_test_sme_p_call() = None;
            let symbol = test_get_sme_p_reg as *const () as *mut c_void;
            let getter = sme_p_reg_getter_from_symbol(symbol)
                .expect("present SME P-register symbol should resolve");
            let mut value = [0; TEST_SME_P_BYTES.len()];

            assert_eq!(
                get_sme_p_reg_with(getter, TEST_SME_P_VCPU, TEST_SME_P_REG, &mut value),
                Ok(())
            );
            assert_eq!(value, TEST_SME_P_BYTES);
            assert_eq!(
                *lock_test_sme_p_call(),
                Some(TestSmePCall {
                    vcpu: TEST_SME_P_VCPU,
                    reg: TEST_SME_P_REG,
                    length: TEST_SME_P_BYTES.len(),
                })
            );
        }

        #[test]
        fn sme_p_register_getter_preserves_unsupported_return() {
            let mut value = [0; TEST_SME_P_BYTES.len()];
            let err = get_sme_p_reg_with(
                test_get_sme_p_reg_unsupported,
                TEST_SME_P_VCPU,
                TEST_SME_P_REG,
                &mut value,
            )
            .expect_err("HV_UNSUPPORTED should fail");

            assert_eq!(
                err.to_string(),
                "hypervisor error: hv_vcpu_get_sme_p_reg failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)"
            );
        }

        #[test]
        fn missing_sme_z_register_symbol_reports_macos_boundary() {
            assert_eq!(
                sme_z_reg_getter_from_symbol(ptr::null_mut()),
                Err(BackendError::Unsupported(
                    SME_Z_REGISTER_REQUIRES_MACOS_15_2_MESSAGE
                ))
            );
        }

        #[test]
        fn sme_z_register_getter_preserves_id_length_and_bytes() {
            let _lock = TEST_SME_Z_LOCK
                .lock()
                .expect("test SME Z-register lock should not be poisoned");
            *lock_test_sme_z_call() = None;
            let symbol = test_get_sme_z_reg as *const () as *mut c_void;
            let getter = sme_z_reg_getter_from_symbol(symbol)
                .expect("present SME Z-register symbol should resolve");
            let mut value = [0; TEST_SME_Z_BYTES.len()];

            assert_eq!(
                get_sme_z_reg_with(getter, TEST_SME_Z_VCPU, TEST_SME_Z_REG, &mut value),
                Ok(())
            );
            assert_eq!(value, TEST_SME_Z_BYTES);
            assert_eq!(
                *lock_test_sme_z_call(),
                Some(TestSmeZCall {
                    vcpu: TEST_SME_Z_VCPU,
                    reg: TEST_SME_Z_REG,
                    length: TEST_SME_Z_BYTES.len(),
                })
            );
        }

        #[test]
        fn sme_z_register_getter_preserves_unsupported_return() {
            let mut value = [0; TEST_SME_Z_BYTES.len()];
            let err = get_sme_z_reg_with(
                test_get_sme_z_reg_unsupported,
                TEST_SME_Z_VCPU,
                TEST_SME_Z_REG,
                &mut value,
            )
            .expect_err("HV_UNSUPPORTED should fail");

            assert_eq!(
                err.to_string(),
                "hypervisor error: hv_vcpu_get_sme_z_reg failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)"
            );
        }

        #[test]
        fn missing_sme_configuration_symbol_reports_macos_boundary() {
            assert_eq!(
                sme_config_max_svl_bytes_getter_from_symbol(ptr::null_mut()),
                Err(BackendError::Unsupported(
                    SME_CONFIGURATION_REQUIRES_MACOS_15_2_MESSAGE
                ))
            );
        }

        #[test]
        fn sme_configuration_getter_preserves_size_t_value() {
            let symbol = test_get_max_svl_bytes as *const () as *mut c_void;
            let getter = sme_config_max_svl_bytes_getter_from_symbol(symbol)
                .expect("present SME configuration symbol should resolve");

            assert_eq!(
                get_sme_config_max_svl_bytes_with(getter),
                Ok(TEST_MAX_SVL_BYTES)
            );
        }

        #[test]
        fn sme_configuration_getter_preserves_unsupported_return() {
            let err = get_sme_config_max_svl_bytes_with(test_get_max_svl_bytes_unsupported)
                .expect_err("HV_UNSUPPORTED should fail");

            assert_eq!(
                err.to_string(),
                "hypervisor error: hv_sme_config_get_max_svl_bytes failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)"
            );
        }

        #[test]
        fn vcpu_cache_feature_ids_match_hvf_sdk() {
            assert_eq!(HV_FEATURE_REG_CTR_EL0, 9);
            assert_eq!(HV_FEATURE_REG_CLIDR_EL1, 10);
            assert_eq!(HV_FEATURE_REG_DCZID_EL0, 11);
        }

        #[test]
        fn vcpu_cache_types_match_hvf_sdk() {
            assert_eq!(HV_CACHE_TYPE_DATA, 0);
            assert_eq!(HV_CACHE_TYPE_INSTRUCTION, 1);
        }

        #[test]
        fn null_vcpu_configuration_stops_without_getter_or_release() {
            let _lock = TEST_VCPU_CONFIG_LOCK
                .lock()
                .expect("test vCPU config lock should not be poisoned");
            reset_test_vcpu_config_state(None);

            assert_eq!(
                get_arm64_vcpu_cache_feature_registers_with(
                    test_vcpu_config_create_null,
                    test_vcpu_config_get_feature_reg,
                    test_vcpu_config_release,
                ),
                Err(BackendError::Hypervisor(
                    "hv_vcpu_config_create returned null".to_string()
                ))
            );
            assert_eq!(
                test_vcpu_config_calls(),
                (vec![TestVcpuConfigCall::Create], true)
            );
        }

        #[test]
        fn vcpu_cache_features_preserve_values_order_and_release() {
            let _lock = TEST_VCPU_CONFIG_LOCK
                .lock()
                .expect("test vCPU config lock should not be poisoned");
            reset_test_vcpu_config_state(None);

            assert_eq!(
                get_arm64_vcpu_cache_feature_registers_with(
                    test_vcpu_config_create,
                    test_vcpu_config_get_feature_reg,
                    test_vcpu_config_release,
                ),
                Ok(TEST_CACHE_FEATURE_VALUES)
            );
            assert_eq!(
                test_vcpu_config_calls(),
                (
                    vec![
                        TestVcpuConfigCall::Create,
                        TestVcpuConfigCall::Get(HV_FEATURE_REG_CTR_EL0),
                        TestVcpuConfigCall::Get(HV_FEATURE_REG_CLIDR_EL1),
                        TestVcpuConfigCall::Get(HV_FEATURE_REG_DCZID_EL0),
                        TestVcpuConfigCall::Release,
                    ],
                    true,
                )
            );
        }

        #[test]
        fn every_vcpu_cache_feature_failure_stops_and_releases() {
            let _lock = TEST_VCPU_CONFIG_LOCK
                .lock()
                .expect("test vCPU config lock should not be poisoned");
            let registers = [
                HV_FEATURE_REG_CTR_EL0,
                HV_FEATURE_REG_CLIDR_EL1,
                HV_FEATURE_REG_DCZID_EL0,
            ];

            for fail_on_get in 0..registers.len() {
                reset_test_vcpu_config_state(Some(fail_on_get));
                let err = get_arm64_vcpu_cache_feature_registers_with(
                    test_vcpu_config_create,
                    test_vcpu_config_get_feature_reg,
                    test_vcpu_config_release,
                )
                .expect_err("configured feature getter should fail");
                assert_eq!(
                    err.to_string(),
                    "hypervisor error: hv_vcpu_config_get_feature_reg failed with HV_BAD_ARGUMENT (hv_return_t=0xfae94003)"
                );

                let mut expected_calls = vec![TestVcpuConfigCall::Create];
                expected_calls.extend(
                    registers
                        .iter()
                        .take(fail_on_get + 1)
                        .copied()
                        .map(TestVcpuConfigCall::Get),
                );
                expected_calls.push(TestVcpuConfigCall::Release);
                assert_eq!(test_vcpu_config_calls(), (expected_calls, true));
            }
        }

        #[test]
        fn null_vcpu_configuration_stops_without_ccsidr_getter_or_release() {
            let _lock = TEST_VCPU_CONFIG_LOCK
                .lock()
                .expect("test vCPU config lock should not be poisoned");
            reset_test_vcpu_config_state(None);

            assert_eq!(
                get_arm64_vcpu_cache_geometry_with(
                    test_vcpu_config_create_null,
                    test_vcpu_config_get_ccsidr_el1_sys_reg_values,
                    test_vcpu_config_release,
                ),
                Err(BackendError::Hypervisor(
                    "hv_vcpu_config_create returned null".to_string()
                ))
            );
            assert_eq!(
                test_vcpu_config_calls(),
                (vec![TestVcpuConfigCall::Create], true)
            );
        }

        #[test]
        fn vcpu_cache_geometry_preserves_values_order_and_release() {
            let _lock = TEST_VCPU_CONFIG_LOCK
                .lock()
                .expect("test vCPU config lock should not be poisoned");
            reset_test_vcpu_config_state(None);

            assert_eq!(
                get_arm64_vcpu_cache_geometry_with(
                    test_vcpu_config_create,
                    test_vcpu_config_get_ccsidr_el1_sys_reg_values,
                    test_vcpu_config_release,
                ),
                Ok(TEST_CACHE_GEOMETRY)
            );
            assert_eq!(
                test_vcpu_config_calls(),
                (
                    vec![
                        TestVcpuConfigCall::Create,
                        TestVcpuConfigCall::GetCcsidr(HV_CACHE_TYPE_DATA),
                        TestVcpuConfigCall::GetCcsidr(HV_CACHE_TYPE_INSTRUCTION),
                        TestVcpuConfigCall::Release,
                    ],
                    true,
                )
            );
        }

        #[test]
        fn every_vcpu_cache_geometry_failure_stops_and_releases() {
            let _lock = TEST_VCPU_CONFIG_LOCK
                .lock()
                .expect("test vCPU config lock should not be poisoned");
            let cache_types = [HV_CACHE_TYPE_DATA, HV_CACHE_TYPE_INSTRUCTION];

            for fail_on_get in 0..cache_types.len() {
                reset_test_vcpu_config_state(Some(fail_on_get));
                let err = get_arm64_vcpu_cache_geometry_with(
                    test_vcpu_config_create,
                    test_vcpu_config_get_ccsidr_el1_sys_reg_values,
                    test_vcpu_config_release,
                )
                .expect_err("configured CCSIDR getter should fail");
                assert_eq!(
                    err.to_string(),
                    "hypervisor error: hv_vcpu_config_get_ccsidr_el1_sys_reg_values failed with HV_BAD_ARGUMENT (hv_return_t=0xfae94003)"
                );

                let mut expected_calls = vec![TestVcpuConfigCall::Create];
                expected_calls.extend(
                    cache_types
                        .iter()
                        .take(fail_on_get + 1)
                        .copied()
                        .map(TestVcpuConfigCall::GetCcsidr),
                );
                expected_calls.push(TestVcpuConfigCall::Release);
                assert_eq!(test_vcpu_config_calls(), (expected_calls, true));
            }
        }

        #[test]
        fn vcpu_configuration_guard_releases_during_unwind() {
            let _lock = TEST_VCPU_CONFIG_LOCK
                .lock()
                .expect("test vCPU config lock should not be poisoned");
            reset_test_vcpu_config_state(None);

            let unwind = panic::catch_unwind(|| {
                let _config = HvVcpuConfigOwner::create_with(
                    test_vcpu_config_create,
                    test_vcpu_config_release,
                )
                .expect("test vCPU config should be created");
                panic!("exercise retained vCPU configuration unwind cleanup");
            });

            assert!(unwind.is_err());
            assert_eq!(
                test_vcpu_config_calls(),
                (
                    vec![TestVcpuConfigCall::Create, TestVcpuConfigCall::Release],
                    true,
                )
            );
        }
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
mod imp {
    use std::ffi::c_void;
    use std::ptr::NonNull;

    use bangbang_runtime::BackendError;

    use super::{
        CreatedVcpu, HvInterruptType, HvMemoryFlags, HvReg, HvSimdFpReg, HvSysReg, HvVcpu,
        UNSUPPORTED_TARGET_MESSAGE,
    };

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

    pub fn get_arm64_vcpu_cache_feature_registers() -> Result<[u64; 3], BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn get_arm64_vcpu_cache_geometry() -> Result<[[u64; 8]; 2], BackendError> {
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

    pub fn get_pending_interrupt(_: HvVcpu, _: HvInterruptType) -> Result<bool, BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn set_pending_interrupt(
        _: HvVcpu,
        _: HvInterruptType,
        _: bool,
    ) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn get_trap_debug_exceptions(_: HvVcpu) -> Result<bool, BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn get_trap_debug_reg_accesses(_: HvVcpu) -> Result<bool, BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn get_sme_state(_: HvVcpu) -> Result<(bool, bool), BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn get_sme_p_reg(_: HvVcpu, _: u32, _: &mut [u8]) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn get_sme_z_reg(_: HvVcpu, _: u32, _: &mut [u8]) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn get_sme_config_max_svl_bytes() -> Result<usize, BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn get_simd_fp_reg(_: HvVcpu, _: HvSimdFpReg) -> Result<[u8; 16], BackendError> {
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

    pub fn get_vtimer_offset(_: HvVcpu) -> Result<u64, BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }

    pub fn set_vtimer_offset(_: HvVcpu, _: u64) -> Result<(), BackendError> {
        Err(BackendError::Unsupported(UNSUPPORTED_TARGET_MESSAGE))
    }
}

pub(crate) use imp::*;

#[cfg(test)]
mod tests {
    use std::mem::{align_of, offset_of, size_of};

    use super::{
        HV_INTERRUPT_TYPE_FIQ, HV_INTERRUPT_TYPE_IRQ, HV_SIMD_FP_REG_Q0, HV_SIMD_FP_REG_Q31,
        HvSimdFpValue, HvVcpuExit, HvVcpuExitException, HvVcpuSmeState,
    };

    #[test]
    fn simd_fp_value_layout_matches_hvf_sdk() {
        assert_eq!(size_of::<HvSimdFpValue>(), 16);
        assert_eq!(align_of::<HvSimdFpValue>(), 16);
        assert_eq!(HV_SIMD_FP_REG_Q0, 0);
        assert_eq!(HV_SIMD_FP_REG_Q31, 31);
    }

    #[test]
    fn sme_state_layout_matches_hvf_sdk() {
        assert_eq!(size_of::<HvVcpuSmeState>(), 2);
        assert_eq!(align_of::<HvVcpuSmeState>(), 1);
        assert_eq!(offset_of!(HvVcpuSmeState, streaming_sve_mode_enabled), 0);
        assert_eq!(offset_of!(HvVcpuSmeState, za_storage_enabled), 1);
    }

    #[test]
    fn interrupt_type_values_match_hvf_sdk() {
        assert_eq!(HV_INTERRUPT_TYPE_IRQ, 0);
        assert_eq!(HV_INTERRUPT_TYPE_FIQ, 1);
    }

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
