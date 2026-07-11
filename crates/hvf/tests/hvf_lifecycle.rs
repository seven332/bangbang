#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static HVF_LIFECYCLE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static NEXT_HVF_TEST_FILE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VTIMER_WRITABLE_CONTROL_MASK: u64 = 0b11;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VTIMER_TEST_OFFSET: u64 = 0x1234_5678_9abc_def0;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VTIMER_TEST_COMPARE_VALUE: u64 = 0xfedc_ba98_7654_3210;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_TEST_CNTKCTL_EL1: u64 = 3;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_TEST_CNTP_CTL_EL0: u64 = 2;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_TEST_CNTP_CVAL_EL0: u64 = 0x1234_5678;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_WRITABLE_CONTROL_MASK: u64 = 0b11;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_ISTATUS_MASK: u64 = 0b100;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_DEFINED_CONTROL_MASK: u64 = 0b111;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_pstate_capture_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64VcpuSmePstate, bangbang_hvf::HvfVcpuRunnerError>,
) -> Result<Option<bangbang_hvf::HvfArm64VcpuSmePstate>, bangbang_hvf::HvfVcpuRunnerError> {
    use bangbang_hvf::HvfVcpuRunnerError;
    use bangbang_runtime::BackendError;

    match result {
        Ok(state) => Ok(Some(state)),
        Err(HvfVcpuRunnerError::Backend(BackendError::Unsupported(message))) => {
            assert_eq!(
                message,
                "Hypervisor.framework SME state capture requires macOS 15.2 or newer"
            );
            Ok(None)
        }
        Err(HvfVcpuRunnerError::Backend(BackendError::Hypervisor(message))) => {
            assert_eq!(
                message,
                "hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_p_register_capture_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64VcpuSmePRegisterState, bangbang_hvf::HvfVcpuRunnerError>,
) -> Result<Option<bangbang_hvf::HvfArm64VcpuSmePRegisterState>, bangbang_hvf::HvfVcpuRunnerError> {
    use bangbang_hvf::{HvfArm64VcpuSmePRegisterCaptureError, HvfVcpuRunnerError};
    use bangbang_runtime::BackendError;

    match result {
        Ok(state) => Ok(Some(state)),
        Err(HvfVcpuRunnerError::SmePRegisterCapture(
            HvfArm64VcpuSmePRegisterCaptureError::StreamingSveModeDisabled,
        )) => Ok(None),
        Err(HvfVcpuRunnerError::SmePRegisterCapture(
            HvfArm64VcpuSmePRegisterCaptureError::Backend(BackendError::Unsupported(message)),
        )) => {
            assert!(
                [
                    "Hypervisor.framework SME state capture requires macOS 15.2 or newer",
                    "Hypervisor.framework SME configuration queries require macOS 15.2 or newer",
                    "Hypervisor.framework SME P-register capture requires macOS 15.2 or newer",
                ]
                .contains(&message),
                "only a documented macOS 15.2 SME availability boundary is accepted"
            );
            Ok(None)
        }
        Err(HvfVcpuRunnerError::SmePRegisterCapture(
            HvfArm64VcpuSmePRegisterCaptureError::Backend(BackendError::Hypervisor(message)),
        )) => {
            assert!(
                [
                    "hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_sme_config_get_max_svl_bytes failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_vcpu_get_sme_p_reg failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                ]
                .contains(&message.as_str()),
                "only a documented HV_UNSUPPORTED SME availability result is accepted"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_z_register_capture_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64VcpuSmeZRegisterState, bangbang_hvf::HvfVcpuRunnerError>,
) -> Result<Option<bangbang_hvf::HvfArm64VcpuSmeZRegisterState>, bangbang_hvf::HvfVcpuRunnerError> {
    use bangbang_hvf::{HvfArm64VcpuSmeZRegisterCaptureError, HvfVcpuRunnerError};
    use bangbang_runtime::BackendError;

    match result {
        Ok(state) => Ok(Some(state)),
        Err(HvfVcpuRunnerError::SmeZRegisterCapture(
            HvfArm64VcpuSmeZRegisterCaptureError::StreamingSveModeDisabled,
        )) => Ok(None),
        Err(HvfVcpuRunnerError::SmeZRegisterCapture(
            HvfArm64VcpuSmeZRegisterCaptureError::Backend(BackendError::Unsupported(message)),
        )) => {
            assert!(
                [
                    "Hypervisor.framework SME state capture requires macOS 15.2 or newer",
                    "Hypervisor.framework SME configuration queries require macOS 15.2 or newer",
                    "Hypervisor.framework SME Z-register capture requires macOS 15.2 or newer",
                ]
                .contains(&message),
                "only a documented macOS 15.2 SME availability boundary is accepted"
            );
            Ok(None)
        }
        Err(HvfVcpuRunnerError::SmeZRegisterCapture(
            HvfArm64VcpuSmeZRegisterCaptureError::Backend(BackendError::Hypervisor(message)),
        )) => {
            assert!(
                [
                    "hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_sme_config_get_max_svl_bytes failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_vcpu_get_sme_z_reg failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                ]
                .contains(&message.as_str()),
                "only a documented HV_UNSUPPORTED SME availability result is accepted"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_za_register_capture_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64VcpuSmeZaRegisterState, bangbang_hvf::HvfVcpuRunnerError>,
) -> Result<Option<bangbang_hvf::HvfArm64VcpuSmeZaRegisterState>, bangbang_hvf::HvfVcpuRunnerError>
{
    use bangbang_hvf::{HvfArm64VcpuSmeZaRegisterCaptureError, HvfVcpuRunnerError};
    use bangbang_runtime::BackendError;

    match result {
        Ok(state) => Ok(Some(state)),
        Err(HvfVcpuRunnerError::SmeZaRegisterCapture(
            HvfArm64VcpuSmeZaRegisterCaptureError::ZaStorageDisabled,
        )) => Ok(None),
        Err(HvfVcpuRunnerError::SmeZaRegisterCapture(
            HvfArm64VcpuSmeZaRegisterCaptureError::Backend(BackendError::Unsupported(message)),
        )) => {
            assert!(
                [
                    "Hypervisor.framework SME state capture requires macOS 15.2 or newer",
                    "Hypervisor.framework SME configuration queries require macOS 15.2 or newer",
                    "Hypervisor.framework SME ZA-register capture requires macOS 15.2 or newer",
                ]
                .contains(&message),
                "only a documented macOS 15.2 SME availability boundary is accepted"
            );
            Ok(None)
        }
        Err(HvfVcpuRunnerError::SmeZaRegisterCapture(
            HvfArm64VcpuSmeZaRegisterCaptureError::Backend(BackendError::Hypervisor(message)),
        )) => {
            assert!(
                [
                    "hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_sme_config_get_max_svl_bytes failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_vcpu_get_sme_za_reg failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                ]
                .contains(&message.as_str()),
                "only a documented HV_UNSUPPORTED SME availability result is accepted"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_zt0_register_capture_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64VcpuSmeZt0RegisterState, bangbang_hvf::HvfVcpuRunnerError>,
) -> Result<Option<bangbang_hvf::HvfArm64VcpuSmeZt0RegisterState>, bangbang_hvf::HvfVcpuRunnerError>
{
    use bangbang_hvf::{HvfArm64VcpuSmeZt0RegisterCaptureError, HvfVcpuRunnerError};
    use bangbang_runtime::BackendError;

    match result {
        Ok(state) => Ok(Some(state)),
        Err(HvfVcpuRunnerError::SmeZt0RegisterCapture(
            HvfArm64VcpuSmeZt0RegisterCaptureError::ZaStorageDisabled,
        )) => Ok(None),
        Err(HvfVcpuRunnerError::SmeZt0RegisterCapture(
            HvfArm64VcpuSmeZt0RegisterCaptureError::Backend(BackendError::Unsupported(message)),
        )) => {
            assert!(
                [
                    "Hypervisor.framework SME state capture requires macOS 15.2 or newer",
                    "Hypervisor.framework SME ZT0-register capture requires macOS 15.2 or newer",
                ]
                .contains(&message),
                "only a documented macOS 15.2 SME availability boundary is accepted"
            );
            Ok(None)
        }
        Err(HvfVcpuRunnerError::SmeZt0RegisterCapture(
            HvfArm64VcpuSmeZt0RegisterCaptureError::Backend(BackendError::Hypervisor(message)),
        )) => {
            assert!(
                [
                    "hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_vcpu_get_sme_zt0_reg failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                ]
                .contains(&message.as_str()),
                "only a documented HV_UNSUPPORTED SME availability result is accepted"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_configuration_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64SmeConfiguration, bangbang_runtime::BackendError>,
) -> Result<Option<bangbang_hvf::HvfArm64SmeConfiguration>, bangbang_runtime::BackendError> {
    use bangbang_runtime::BackendError;

    match result {
        Ok(configuration) => Ok(Some(configuration)),
        Err(BackendError::Hypervisor(message)) => {
            assert_eq!(
                message,
                "hv_sme_config_get_max_svl_bytes failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_GUEST_CODE: [u32; 9] = [
    0xd280_0060, // mov x0, #3
    0xd518_e100, // msr CNTKCTL_EL1, x0
    0xd280_0040, // mov x0, #2
    0xd51b_e220, // msr CNTP_CTL_EL0, x0
    0xd28a_cf00, // mov x0, #0x5678
    0xf2a2_4680, // movk x0, #0x1234, lsl #16
    0xd51b_e240, // msr CNTP_CVAL_EL0, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CORE_SYSTEM_TEST_SP_EL0: u64 = 0x1000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CORE_SYSTEM_TEST_SP_EL1: u64 = 0x2000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CORE_SYSTEM_TEST_ELR_EL1: u64 = 0x3000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CORE_SYSTEM_TEST_SPSR_EL1: u64 = 0x3c5;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CORE_SYSTEM_REGISTER_GUEST_CODE: [u32; 9] = [
    0xd282_0000, // mov x0, #0x1000
    0xd518_4100, // msr SP_EL0, x0
    0xd284_0000, // mov x0, #0x2000
    0x9100_001f, // mov sp, x0
    0xd286_0000, // mov x0, #0x3000
    0xd518_4020, // msr ELR_EL1, x0
    0xd280_78a0, // mov x0, #0x3c5
    0xd518_4000, // msr SPSR_EL1, x0
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_AFSR0_EL1: u64 = 0x1111;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_AFSR1_EL1: u64 = 0x2222;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_ESR_EL1: u64 = 0x9600_0045;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_FAR_EL1: u64 = 0x3333_4444;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_PAR_EL1: u64 = 0x5555_6800;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_VBAR_EL1: u64 = 0x1234_5000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_REGISTER_GUEST_CODE: [u32; 18] = [
    0xd282_2220, // mov x0, #0x1111
    0xd518_5100, // msr AFSR0_EL1, x0
    0xd284_4440, // mov x0, #0x2222
    0xd518_5120, // msr AFSR1_EL1, x0
    0xd280_08a0, // mov x0, #0x45
    0xf2b2_c000, // movk x0, #0x9600, lsl #16
    0xd518_5200, // msr ESR_EL1, x0
    0xd288_8880, // mov x0, #0x4444
    0xf2a6_6660, // movk x0, #0x3333, lsl #16
    0xd518_6000, // msr FAR_EL1, x0
    0xd28d_0000, // mov x0, #0x6800
    0xf2aa_aaa0, // movk x0, #0x5555, lsl #16
    0xd518_7400, // msr PAR_EL1, x0
    0xd28a_0000, // mov x0, #0x5000
    0xf2a2_4680, // movk x0, #0x1234, lsl #16
    0xd518_c000, // msr VBAR_EL1, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXECUTION_CONTROL_TEST_ACTLR_EL1: u64 = 2;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXECUTION_CONTROL_TEST_CPACR_EL1: u64 = 0x0030_0000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXECUTION_CONTROL_GUEST_CODE: [u32; 6] = [
    0xd280_0040, // mov x0, #2
    0xd518_1020, // msr ACTLR_EL1, x0
    0xd2a0_0600, // mov x0, #0x300000
    0xd518_1040, // msr CPACR_EL1, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_TTBR0_EL1: u64 = 0x1234_5000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_TTBR1_EL1: u64 = 0x5678_9000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_TCR_EL1: u64 = 0x10;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_MAIR_EL1: u64 = 0xff44_0400;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_AMAIR_EL1_WRITE: u64 = 0x1122_3344_5566_7788;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_CONTEXTIDR_EL1: u64 = 0xa5a5_5a5a;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_REGISTER_GUEST_CODE: [u32; 24] = [
    0xd538_1000, // mrs x0, SCTLR_EL1
    0xd518_1000, // msr SCTLR_EL1, x0
    0xd503_3fdf, // isb
    0xd28a_0000, // mov x0, #0x5000
    0xf2a2_4680, // movk x0, #0x1234, lsl #16
    0xd518_2000, // msr TTBR0_EL1, x0
    0xd292_0000, // mov x0, #0x9000
    0xf2aa_cf00, // movk x0, #0x5678, lsl #16
    0xd518_2020, // msr TTBR1_EL1, x0
    0xd280_0200, // mov x0, #0x10
    0xd518_2040, // msr TCR_EL1, x0
    0xd280_8000, // mov x0, #0x400
    0xf2bf_e880, // movk x0, #0xff44, lsl #16
    0xd518_a200, // msr MAIR_EL1, x0
    0xd28e_f100, // mov x0, #0x7788
    0xf2aa_acc0, // movk x0, #0x5566, lsl #16
    0xf2c6_6880, // movk x0, #0x3344, lsl #32
    0xf2e2_2440, // movk x0, #0x1122, lsl #48
    0xd518_a300, // msr AMAIR_EL1, x0
    0xd28b_4b40, // mov x0, #0x5a5a
    0xf2b4_b4a0, // movk x0, #0xa5a5, lsl #16
    0xd518_d020, // msr CONTEXTIDR_EL1, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_TEST_APIA_KEY: u128 = (0x2222_u128 << 64) | 0x1111;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_TEST_APIB_KEY: u128 = (0x4444_u128 << 64) | 0x3333;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_TEST_APDA_KEY: u128 = (0x6666_u128 << 64) | 0x5555;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_TEST_APDB_KEY: u128 = (0x8888_u128 << 64) | 0x7777;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_TEST_APGA_KEY: u128 = (0xaaaa_u128 << 64) | 0x9999;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_KEY_GUEST_CODE: [u32; 22] = [
    0xd282_2220, // mov x0, #0x1111
    0xd518_2100, // msr APIAKeyLo_EL1, x0
    0xd284_4440, // mov x0, #0x2222
    0xd518_2120, // msr APIAKeyHi_EL1, x0
    0xd286_6660, // mov x0, #0x3333
    0xd518_2140, // msr APIBKeyLo_EL1, x0
    0xd288_8880, // mov x0, #0x4444
    0xd518_2160, // msr APIBKeyHi_EL1, x0
    0xd28a_aaa0, // mov x0, #0x5555
    0xd518_2200, // msr APDAKeyLo_EL1, x0
    0xd28c_ccc0, // mov x0, #0x6666
    0xd518_2220, // msr APDAKeyHi_EL1, x0
    0xd28e_eee0, // mov x0, #0x7777
    0xd518_2240, // msr APDBKeyLo_EL1, x0
    0xd291_1100, // mov x0, #0x8888
    0xd518_2260, // msr APDBKeyHi_EL1, x0
    0xd293_3320, // mov x0, #0x9999
    0xd518_2300, // msr APGAKeyLo_EL1, x0
    0xd295_5540, // mov x0, #0xaaaa
    0xd518_2320, // msr APGAKeyHi_EL1, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const THREAD_CONTEXT_TEST_TPIDR_EL0: u64 = 0x1111;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const THREAD_CONTEXT_TEST_TPIDRRO_EL0: u64 = 0x2222;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const THREAD_CONTEXT_TEST_TPIDR_EL1: u64 = 0x3333;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const THREAD_CONTEXT_REGISTER_GUEST_CODE: [u32; 7] = [
    0xd282_2220, // mov x0, #0x1111
    0xd51b_d040, // msr TPIDR_EL0, x0
    0xd284_4440, // mov x0, #0x2222
    0xd51b_d060, // msr TPIDRRO_EL0, x0
    0xd286_6660, // mov x0, #0x3333
    0xd518_d080, // msr TPIDR_EL1, x0
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SIMD_FP_TEST_Q0: [u8; 16] = [0x12; 16];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SIMD_FP_TEST_Q31: [u8; 16] = [0x34; 16];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SIMD_FP_TEST_FPCR: u64 = 0x0100_0000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SIMD_FP_TEST_FPSR: u64 = 0x1f;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SIMD_FP_REGISTER_GUEST_CODE: [u32; 10] = [
    0xd2a0_0600, // mov x0, #0x300000
    0xd518_1040, // msr CPACR_EL1, x0
    0xd503_3fdf, // isb
    0x4f00_e640, // movi v0.16b, #0x12
    0x4f01_e69f, // movi v31.16b, #0x34
    0xd2a0_2000, // mov x0, #0x1000000
    0xd51b_4400, // msr FPCR, x0
    0xd280_03e0, // mov x0, #0x1f
    0xd51b_4420, // msr FPSR, x0
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_ICC_TEST_PMR_EL1: u64 = 0xa0;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_ICC_TEST_BPR0_EL1: u64 = 3;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_ICC_TEST_BPR1_EL1: u64 = 4;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_ICC_REGISTER_GUEST_CODE: [u32; 15] = [
    0xd538_cca0, // mrs x0, ICC_SRE_EL1
    0xb240_0000, // orr x0, x0, #1
    0xd518_cca0, // msr ICC_SRE_EL1, x0
    0xd503_3fdf, // isb
    0xd280_1400, // mov x0, #0xa0
    0xd518_4600, // msr ICC_PMR_EL1, x0
    0xd280_0060, // mov x0, #3
    0xd518_c860, // msr ICC_BPR0_EL1, x0
    0xd280_0080, // mov x0, #4
    0xd518_cc60, // msr ICC_BPR1_EL1, x0
    0xd280_0020, // mov x0, #1
    0xd518_ccc0, // msr ICC_IGRPEN0_EL1, x0
    0xd518_cce0, // msr ICC_IGRPEN1_EL1, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn test_rtc_mmio_layout() -> bangbang_runtime::rtc::RtcMmioLayout {
    bangbang_runtime::rtc::RtcMmioLayout::new(
        bangbang_runtime::memory::GuestAddress::new(0x4000_1000),
        bangbang_runtime::mmio::MmioRegionId::new(3000),
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn queries_arm64_sme_configuration_before_vm_creation() {
    use bangbang_hvf::HvfBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let first =
        assert_sme_configuration_supported_or_unavailable(HvfBackend::arm64_sme_configuration())
            .expect("first SME configuration query should succeed or report unsupported");
    let second =
        assert_sme_configuration_supported_or_unavailable(HvfBackend::arm64_sme_configuration())
            .expect("second SME configuration query should succeed or report unsupported");

    assert!(
        first.is_some() == second.is_some(),
        "SME configuration availability should remain stable on one host"
    );
    if let (Some(first), Some(second)) = (first, second) {
        let first_max_svl_bytes = first.max_svl_bytes();
        let second_max_svl_bytes = second.max_svl_bytes();
        assert!(
            first_max_svl_bytes == second_max_svl_bytes,
            "maximum guest-usable SME SVL should remain stable on one host"
        );
        assert!(
            first == second,
            "SME configuration should remain stable on one host"
        );
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn queries_arm64_default_vcpu_cache_configuration_before_vm_creation() {
    use bangbang_hvf::{HvfArm64VcpuCacheConfiguration, HvfBackend};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let first = HvfBackend::arm64_vcpu_cache_configuration()
        .expect("first default vCPU cache configuration query should succeed");
    let second = HvfBackend::arm64_vcpu_cache_configuration()
        .expect("second default vCPU cache configuration query should succeed");

    let values = |configuration: HvfArm64VcpuCacheConfiguration| {
        [
            configuration.ctr_el0(),
            configuration.clidr_el1(),
            configuration.dczid_el0(),
        ]
    };
    assert!(
        values(first) == values(second),
        "default vCPU cache feature accessors should remain stable on one host"
    );
    assert!(
        first == second,
        "default vCPU cache configuration should remain stable on one host"
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn queries_arm64_default_vcpu_cache_geometry_before_vm_creation() {
    use bangbang_hvf::HvfBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let first = HvfBackend::arm64_vcpu_cache_geometry()
        .expect("first default vCPU cache geometry query should succeed");
    let second = HvfBackend::arm64_vcpu_cache_geometry()
        .expect("second default vCPU cache geometry query should succeed");

    assert!(
        first.data_or_unified_ccsidr_el1() == second.data_or_unified_ccsidr_el1()
            && first.instruction_ccsidr_el1() == second.instruction_ccsidr_el1(),
        "default vCPU CCSIDR accessors should remain stable on one host"
    );
    assert!(
        first == second,
        "default vCPU cache geometry should remain stable on one host"
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn creates_and_destroys_hvf_vcpu() {
    use bangbang_hvf::{HvfBackend, HvfRegister, HvfSystemRegister};
    use bangbang_runtime::BackendError;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    {
        let mut vcpu = backend.create_vcpu().expect("vCPU should be created");
        assert_eq!(
            vcpu.exit_snapshot(),
            Err(BackendError::InvalidState("vCPU has not exited yet"))
        );
        vcpu.set_register(HvfRegister::X0, 0x1234)
            .expect("vCPU register should be set");
        assert_eq!(
            vcpu.get_register(HvfRegister::X0)
                .expect("vCPU register should be read"),
            0x1234
        );
        let original_vtimer_mask = vcpu
            .get_vtimer_mask()
            .expect("original vCPU vtimer mask should be read");
        let original_vtimer_offset = vcpu
            .get_vtimer_offset()
            .expect("original vCPU vtimer offset should be read");
        let original_vtimer_control = vcpu
            .get_system_register(HvfSystemRegister::CNTV_CTL_EL0)
            .expect("original vCPU vtimer control should be read");
        let original_vtimer_compare_value = vcpu
            .get_system_register(HvfSystemRegister::CNTV_CVAL_EL0)
            .expect("original vCPU vtimer compare value should be read");
        vcpu.set_vtimer_mask(true)
            .expect("vCPU vtimer mask should be set");
        vcpu.set_system_register(HvfSystemRegister::CNTV_CTL_EL0, 0)
            .expect("vCPU vtimer should be disabled");
        vcpu.set_vtimer_offset(VTIMER_TEST_OFFSET)
            .expect("vCPU vtimer offset should be set");
        vcpu.set_system_register(HvfSystemRegister::CNTV_CVAL_EL0, VTIMER_TEST_COMPARE_VALUE)
            .expect("vCPU vtimer compare value should be set");
        assert!(
            vcpu.get_vtimer_mask()
                .expect("vCPU vtimer mask should be read")
        );
        assert_eq!(
            vcpu.get_vtimer_offset()
                .expect("vCPU vtimer offset should be read"),
            VTIMER_TEST_OFFSET
        );
        assert_eq!(
            vcpu.get_system_register(HvfSystemRegister::CNTV_CTL_EL0)
                .expect("vCPU vtimer control should be read")
                & VTIMER_WRITABLE_CONTROL_MASK,
            0
        );
        assert_eq!(
            vcpu.get_system_register(HvfSystemRegister::CNTV_CVAL_EL0)
                .expect("vCPU vtimer compare value should be read"),
            VTIMER_TEST_COMPARE_VALUE
        );
        vcpu.set_vtimer_offset(original_vtimer_offset)
            .expect("original vCPU vtimer offset should be restored");
        vcpu.set_system_register(
            HvfSystemRegister::CNTV_CVAL_EL0,
            original_vtimer_compare_value,
        )
        .expect("original vCPU vtimer compare value should be restored");
        vcpu.set_system_register(
            HvfSystemRegister::CNTV_CTL_EL0,
            original_vtimer_control & VTIMER_WRITABLE_CONTROL_MASK,
        )
        .expect("original vCPU vtimer control should be restored");
        vcpu.set_vtimer_mask(original_vtimer_mask)
            .expect("original vCPU vtimer mask should be restored");
        vcpu.destroy().expect("vCPU should be destroyed");
        vcpu.destroy()
            .expect("destroyed vCPU should remain destroyed");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn configures_hvf_vcpu_arm64_boot_registers() {
    use bangbang_hvf::{
        ARM64_LINUX_BOOT_CPSR, HvfArm64BootRegisters, HvfBackend, HvfRegister, HvfSystemRegister,
    };
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::GuestAddress;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let registers = HvfArm64BootRegisters {
        kernel_entry: GuestAddress::new(0x8028_0000),
        fdt_address: GuestAddress::new(0x8fe0_0000),
    };

    backend.create_vm().expect("VM should be created");
    {
        let mut vcpu = backend.create_vcpu().expect("vCPU should be created");
        vcpu.configure_arm64_boot_registers(registers)
            .expect("boot registers should be configured");

        assert_eq!(
            vcpu.get_register(HvfRegister::PC)
                .expect("PC should be read"),
            registers.kernel_entry.raw_value()
        );
        assert_eq!(
            vcpu.get_register(HvfRegister::X0)
                .expect("X0 should be read"),
            registers.fdt_address.raw_value()
        );
        assert_eq!(
            vcpu.get_register(HvfRegister::X1)
                .expect("X1 should be read"),
            0
        );
        assert_eq!(
            vcpu.get_register(HvfRegister::X2)
                .expect("X2 should be read"),
            0
        );
        assert_eq!(
            vcpu.get_register(HvfRegister::X3)
                .expect("X3 should be read"),
            0
        );
        assert_eq!(
            vcpu.get_register(HvfRegister::CPSR)
                .expect("CPSR should be read"),
            ARM64_LINUX_BOOT_CPSR
        );
        let _mpidr = vcpu
            .get_system_register(HvfSystemRegister::MPIDR_EL1)
            .expect("MPIDR_EL1 should be read");

        vcpu.destroy().expect("vCPU should be destroyed");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_configured_arm64_general_registers_on_runner_thread() {
    use bangbang_hvf::{ARM64_LINUX_BOOT_CPSR, HvfArm64BootRegisters, HvfBackend};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::GuestAddress;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let registers = HvfArm64BootRegisters {
        kernel_entry: GuestAddress::new(0x8028_0000),
        fdt_address: GuestAddress::new(0x8fe0_0000),
    };

    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(registers)
            .expect("boot registers should be configured");

        let state = runner
            .capture_arm64_general_register_state()
            .expect("general-register state should be captured");
        assert_eq!(state.general_purpose_registers().len(), 31);
        assert_eq!(
            state.general_purpose_register(0),
            Some(registers.fdt_address.raw_value())
        );
        assert_eq!(state.general_purpose_register(1), Some(0));
        assert_eq!(state.general_purpose_register(2), Some(0));
        assert_eq!(state.general_purpose_register(3), Some(0));
        assert_eq!(state.pc(), registers.kernel_entry.raw_value());
        assert_eq!(state.cpsr(), ARM64_LINUX_BOOT_CPSR);

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn restores_arm64_general_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::GuestAddress;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let registers = HvfArm64BootRegisters {
        kernel_entry: GuestAddress::new(0x8028_0000),
        fdt_address: GuestAddress::new(0x8fe0_0000),
    };

    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(registers)
            .expect("boot registers should be configured");

        let before = runner
            .capture_arm64_general_register_state()
            .expect("general-register state should be captured before restore");
        runner
            .restore_arm64_general_register_state(&before)
            .expect("general-register state should be restored");
        let after = runner
            .capture_arm64_general_register_state()
            .expect("general-register state should be recaptured after restore");
        assert_eq!(after, before);

        runner
            .restore_arm64_general_register_state(&before)
            .expect("repeated general-register restore should succeed");
        let repeated = runner
            .capture_arm64_general_register_state()
            .expect("general-register state should be recaptured after repeated restore");
        assert_eq!(repeated, before);

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_guest_written_arm64_core_system_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = CORE_SYSTEM_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("core system-register guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest register writer should exit through HVC")
        else {
            panic!("guest register writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest register writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_core_system_register_state()
            .expect("core system-register state should be captured");
        assert_eq!(state.sp_el0(), CORE_SYSTEM_TEST_SP_EL0);
        assert_eq!(state.sp_el1(), CORE_SYSTEM_TEST_SP_EL1);
        assert_eq!(state.elr_el1(), CORE_SYSTEM_TEST_ELR_EL1);
        assert_eq!(state.spsr_el1(), CORE_SYSTEM_TEST_SPSR_EL1);

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_guest_written_arm64_exception_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = EXCEPTION_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("exception-register guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest exception-register writer should exit through HVC")
        else {
            panic!("guest exception-register writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest exception-register writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_exception_register_state()
            .expect("exception-register state should be captured");
        // Auxiliary fault-status contents are implementation-defined. Current
        // Apple Silicon exposes AFSR0 as read-as-zero/write-ignored and
        // preserves AFSR1, while another host may expose either behavior for
        // either register.
        assert!(matches!(state.afsr0_el1(), 0 | EXCEPTION_TEST_AFSR0_EL1));
        assert!(matches!(state.afsr1_el1(), 0 | EXCEPTION_TEST_AFSR1_EL1));
        assert_eq!(state.esr_el1(), EXCEPTION_TEST_ESR_EL1);
        assert_eq!(state.far_el1(), EXCEPTION_TEST_FAR_EL1);
        assert_eq!(state.par_el1(), EXCEPTION_TEST_PAR_EL1);
        assert_eq!(state.vbar_el1(), EXCEPTION_TEST_VBAR_EL1);

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_guest_written_arm64_execution_controls_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = EXECUTION_CONTROL_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("execution-control guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest execution-control writer should exit through HVC")
        else {
            panic!("guest execution-control writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest execution-control writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_execution_control_register_state()
            .expect("execution-control state should be captured");
        assert_eq!(state.actlr_el1(), EXECUTION_CONTROL_TEST_ACTLR_EL1);
        assert_eq!(state.cpacr_el1(), EXECUTION_CONTROL_TEST_CPACR_EL1);

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_cache_selection_register_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_cache_selection_register_state()
            .expect("first cache-selection state should be captured");
        let second = runner
            .capture_arm64_cache_selection_register_state()
            .expect("second cache-selection state should be captured");

        // Exercise the raw accessor without assuming an architecturally
        // unknown reset value or interpreting it as cache topology.
        let _captured_values = [first.csselr_el1(), second.csselr_el1()];

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_all_implemented_arm64_breakpoint_registers_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_breakpoint_register_state()
            .expect("first breakpoint-register state should be captured");
        let second = runner
            .capture_arm64_breakpoint_register_state()
            .expect("second breakpoint-register state should be captured");

        for state in [&first, &second] {
            let count = state.implemented_breakpoint_count();
            assert!((1..=16).contains(&count));
            assert_eq!(state.breakpoint_value_registers().len(), usize::from(count));
            assert_eq!(
                state.breakpoint_control_registers().len(),
                usize::from(count)
            );
            for index in 0..count {
                assert!(state.breakpoint_value_register(index).is_some());
                assert!(state.breakpoint_control_register(index).is_some());
            }
            if count < 16 {
                assert_eq!(state.breakpoint_value_register(count), None);
                assert_eq!(state.breakpoint_control_register(count), None);
            }
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_all_implemented_arm64_watchpoint_registers_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_watchpoint_register_state()
            .expect("first watchpoint-register state should be captured");
        let second = runner
            .capture_arm64_watchpoint_register_state()
            .expect("second watchpoint-register state should be captured");

        for state in [&first, &second] {
            let count = state.implemented_watchpoint_count();
            assert!((1..=16).contains(&count));
            assert_eq!(state.watchpoint_value_registers().len(), usize::from(count));
            assert_eq!(
                state.watchpoint_control_registers().len(),
                usize::from(count)
            );
            for index in 0..count {
                assert!(state.watchpoint_value_register(index).is_some());
                assert!(state.watchpoint_control_register(index).is_some());
            }
            if count < 16 {
                assert_eq!(state.watchpoint_value_register(count), None);
                assert_eq!(state.watchpoint_control_register(count), None);
            }
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_debug_control_registers_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_debug_control_register_state()
            .expect("first debug-control state should be captured");
        let second = runner
            .capture_arm64_debug_control_register_state()
            .expect("second debug-control state should be captured");

        // Exercise both raw accessors without assuming model-specific values
        // or stability for security-sensitive control/status fields.
        let _captured_values = [
            first.mdccint_el1(),
            first.mdscr_el1(),
            second.mdccint_el1(),
            second.mdscr_el1(),
        ];

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_debug_trap_state_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_debug_trap_state()
            .expect("first debug-trap state should be captured");
        let second = runner
            .capture_arm64_debug_trap_state()
            .expect("second debug-trap state should be captured");

        // Exercise both raw host-policy accessors without assuming or logging
        // default values or treating observation as safe restore policy.
        let _captured_values = [
            first.trap_debug_exceptions(),
            first.trap_debug_reg_accesses(),
            second.trap_debug_exceptions(),
            second.trap_debug_reg_accesses(),
        ];

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_identification_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuIdentificationRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_identification_register_state()
            .expect("first identification-register state should be captured");
        let second = runner
            .capture_arm64_identification_register_state()
            .expect("second identification-register state should be captured");

        let values = |state: HvfArm64VcpuIdentificationRegisterState| {
            [
                state.midr_el1(),
                state.mpidr_el1(),
                state.id_aa64pfr0_el1(),
                state.id_aa64pfr1_el1(),
                state.id_aa64dfr0_el1(),
                state.id_aa64dfr1_el1(),
                state.id_aa64isar0_el1(),
                state.id_aa64isar1_el1(),
                state.id_aa64mmfr0_el1(),
                state.id_aa64mmfr1_el1(),
                state.id_aa64mmfr2_el1(),
            ]
        };
        assert!(
            values(first) == values(second),
            "identification-register accessors should remain stable within one vCPU lifetime"
        );
        assert!(
            first == second,
            "identification-register state should remain stable within one vCPU lifetime"
        );
        assert!(
            first.mpidr_el1()
                == runner
                    .mpidr_el1()
                    .expect("standalone MPIDR owner-thread read should succeed"),
            "captured MPIDR should match the standalone owner-thread getter"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sve_sme_identification_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSveSmeIdentificationRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_sve_sme_identification_register_state()
            .expect("first SVE/SME identification state should be captured");
        let second = runner
            .capture_arm64_sve_sme_identification_register_state()
            .expect("second SVE/SME identification state should be captured");

        let values = |state: HvfArm64VcpuSveSmeIdentificationRegisterState| {
            [state.id_aa64zfr0_el1(), state.id_aa64smfr0_el1()]
        };
        assert!(
            values(first) == values(second),
            "SVE/SME identification accessors should remain stable within one vCPU lifetime"
        );
        assert!(
            first == second,
            "SVE/SME identification state should remain stable within one vCPU lifetime"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_pstate_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first =
            assert_sme_pstate_capture_supported_or_unavailable(runner.capture_arm64_sme_pstate())
                .expect("first SME PSTATE capture should succeed or report unsupported");
        let second =
            assert_sme_pstate_capture_supported_or_unavailable(runner.capture_arm64_sme_pstate())
                .expect("second SME PSTATE capture should succeed or report unsupported");

        assert_eq!(
            first.is_some(),
            second.is_some(),
            "SME availability should remain stable within one vCPU lifetime"
        );
        if let (Some(first), Some(second)) = (first, second) {
            // Exercise both accessors without assuming or logging the flags,
            // entering streaming mode, enabling ZA, or reading SME data.
            let first_values = (
                first.streaming_sve_mode_enabled(),
                first.za_storage_enabled(),
            );
            let second_values = (
                second.streaming_sve_mode_enabled(),
                second.za_storage_enabled(),
            );
            assert!(
                first_values == second_values,
                "SME PSTATE should remain stable on one idle vCPU"
            );
            assert!(
                first == second,
                "SME PSTATE value should remain stable on one idle vCPU"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_p_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSmePRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = assert_sme_p_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_p_register_state(),
        )
        .expect("first SME P-register capture should succeed or report unavailable");
        let second = assert_sme_p_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_p_register_state(),
        )
        .expect("second SME P-register capture should succeed or report unavailable");

        assert!(
            first.is_some() == second.is_some(),
            "SME P-register capture availability should remain stable within one vCPU lifetime"
        );
        if let (Some(first), Some(second)) = (first, second) {
            assert!(
                first.maximum_svl_bytes() == second.maximum_svl_bytes(),
                "SME maximum streaming vector length should remain stable"
            );
            assert!(
                first.predicate_width_bytes() == second.predicate_width_bytes(),
                "SME predicate allocation width should remain stable"
            );
            assert!(
                first.p_register(15).is_some() && first.p_register(16).is_none(),
                "SME P-register capture should contain exactly P0 through P15"
            );
            for register in 0..HvfArm64VcpuSmePRegisterState::REGISTER_COUNT {
                let first_register = first
                    .p_register(register)
                    .expect("first capture should contain every P register");
                let second_register = second
                    .p_register(register)
                    .expect("second capture should contain every P register");
                assert!(
                    first_register.len() == first.predicate_width_bytes(),
                    "first capture should retain the exact predicate width"
                );
                assert!(
                    second_register.len() == second.predicate_width_bytes(),
                    "second capture should retain the exact predicate width"
                );
            }
            assert!(
                first == second,
                "SME P-register state should remain stable on one idle vCPU"
            );
            assert!(
                format!("{first:?}").contains("<redacted>"),
                "SME P-register debug output should remain redacted"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_z_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSmeZRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = assert_sme_z_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_z_register_state(),
        )
        .expect("first SME Z-register capture should succeed or report unavailable");
        let second = assert_sme_z_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_z_register_state(),
        )
        .expect("second SME Z-register capture should succeed or report unavailable");

        assert!(
            first.is_some() == second.is_some(),
            "SME Z-register capture availability should remain stable within one vCPU lifetime"
        );
        if let (Some(first), Some(second)) = (first, second) {
            assert!(
                first.maximum_svl_bytes() == second.maximum_svl_bytes(),
                "SME maximum streaming vector length should remain stable"
            );
            assert!(
                first.z_register(31).is_some() && first.z_register(32).is_none(),
                "SME Z-register capture should contain exactly Z0 through Z31"
            );
            for register in 0..HvfArm64VcpuSmeZRegisterState::REGISTER_COUNT {
                let first_register = first
                    .z_register(register)
                    .expect("first capture should contain every Z register");
                let second_register = second
                    .z_register(register)
                    .expect("second capture should contain every Z register");
                assert!(
                    first_register.len() == first.maximum_svl_bytes(),
                    "first capture should retain the exact maximum width"
                );
                assert!(
                    second_register.len() == second.maximum_svl_bytes(),
                    "second capture should retain the exact maximum width"
                );
            }
            assert!(
                first == second,
                "SME Z-register state should remain stable on one idle vCPU"
            );
            assert!(
                format!("{first:?}").contains("<redacted>"),
                "SME Z-register debug output should remain redacted"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_za_register_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = assert_sme_za_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_za_register_state(),
        )
        .expect("first SME ZA-register capture should succeed or report unavailable");
        let second = assert_sme_za_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_za_register_state(),
        )
        .expect("second SME ZA-register capture should succeed or report unavailable");

        assert!(
            first.is_some() == second.is_some(),
            "SME ZA-register capture availability should remain stable within one vCPU lifetime"
        );
        if let (Some(first), Some(second)) = (first, second) {
            assert!(
                first.maximum_svl_bytes() == second.maximum_svl_bytes(),
                "SME maximum streaming vector length should remain stable"
            );
            let expected_size = first
                .maximum_svl_bytes()
                .checked_mul(first.maximum_svl_bytes())
                .expect("SME maximum streaming vector length should have a square byte size");
            assert!(
                first.len() == expected_size && first.as_bytes().len() == expected_size,
                "first SME ZA capture should retain the exact maximum-SVL square"
            );
            assert!(
                second.len() == expected_size && second.as_bytes().len() == expected_size,
                "second SME ZA capture should retain the exact maximum-SVL square"
            );
            assert!(
                !first.is_empty() && !second.is_empty(),
                "successful SME ZA captures should contain the complete matrix"
            );
            assert!(
                first == second,
                "SME ZA-register state should remain stable on one idle vCPU"
            );
            assert!(
                format!("{first:?}").contains("<redacted>"),
                "SME ZA-register debug output should remain redacted"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_zt0_register_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSmeZt0RegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = assert_sme_zt0_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_zt0_register_state(),
        )
        .expect("first SME ZT0-register capture should succeed or report unavailable");
        let second = assert_sme_zt0_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_zt0_register_state(),
        )
        .expect("second SME ZT0-register capture should succeed or report unavailable");

        assert!(
            first.is_some() == second.is_some(),
            "SME ZT0-register availability should remain stable within one vCPU lifetime"
        );
        if let (Some(first), Some(second)) = (first, second) {
            assert!(
                first.as_bytes().len() == HvfArm64VcpuSmeZt0RegisterState::BYTE_COUNT
                    && second.as_bytes().len() == HvfArm64VcpuSmeZt0RegisterState::BYTE_COUNT,
                "SME ZT0 captures should preserve exactly 64 bytes"
            );
            assert!(
                first == second,
                "SME ZT0-register state should remain stable on one idle vCPU"
            );
            assert!(
                format!("{first:?}")
                    == "HvfArm64VcpuSmeZt0RegisterState { register: \"<redacted>\" }",
                "SME ZT0-register debug output should remain fully redacted"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_system_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSmeSystemRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_sme_system_register_state()
            .expect("first SME system-register state should be captured");
        let second = runner
            .capture_arm64_sme_system_register_state()
            .expect("second SME system-register state should be captured");

        let values = |state: HvfArm64VcpuSmeSystemRegisterState| {
            [state.smcr_el1(), state.smpri_el1(), state.tpidr2_el0()]
        };
        assert!(
            values(first) == values(second),
            "SME system-register accessors should remain stable within one idle vCPU lifetime"
        );
        assert!(
            first == second,
            "SME system-register state should remain stable within one idle vCPU lifetime"
        );
        assert!(
            format!("{first:?}").contains("<redacted>"),
            "SME system-register debug output should remain redacted"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_system_context_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSystemContextRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_system_context_register_state()
            .expect("first system-context register state should be captured");
        let second = runner
            .capture_arm64_system_context_register_state()
            .expect("second system-context register state should be captured");

        let values = |state: HvfArm64VcpuSystemContextRegisterState| {
            [state.scxtnum_el0(), state.scxtnum_el1()]
        };
        assert!(
            values(first) == values(second),
            "system-context register accessors should remain stable within one idle vCPU lifetime"
        );
        assert!(
            first == second,
            "system-context register state should remain stable within one idle vCPU lifetime"
        );
        assert!(
            format!("{first:?}").contains("<redacted>"),
            "system-context register debug output should remain redacted"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_guest_written_arm64_translation_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = TRANSLATION_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("translation-register guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest translation-register writer should exit through HVC")
        else {
            panic!("guest translation-register writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest translation-register writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_translation_register_state()
            .expect("translation-register state should be captured");
        assert_eq!(state.sctlr_el1() & 1, 0);
        assert_eq!(state.ttbr0_el1(), TRANSLATION_TEST_TTBR0_EL1);
        assert_eq!(state.ttbr1_el1(), TRANSLATION_TEST_TTBR1_EL1);
        assert_eq!(state.tcr_el1(), TRANSLATION_TEST_TCR_EL1);
        assert_eq!(state.mair_el1(), TRANSLATION_TEST_MAIR_EL1);
        // AMAIR is implementation-defined. Current Apple Silicon exposes it
        // as read-as-zero/write-ignored, while a future host may preserve the
        // architecturally valid guest write.
        assert!(matches!(
            state.amair_el1(),
            0 | TRANSLATION_TEST_AMAIR_EL1_WRITE
        ));
        assert_eq!(state.contextidr_el1(), TRANSLATION_TEST_CONTEXTIDR_EL1);

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_guest_written_arm64_pointer_authentication_keys_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = POINTER_AUTHENTICATION_KEY_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("pointer-authentication key guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest pointer-authentication key writer should exit through HVC")
        else {
            panic!("guest pointer-authentication key writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest pointer-authentication key writer should exit through HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_pointer_authentication_key_state()
            .expect("pointer-authentication key state should be captured");
        assert!(
            format!("{state:?}")
                == "HvfArm64VcpuPointerAuthenticationKeyState { keys: \"<redacted>\" }",
            "pointer-authentication key Debug output should be fully redacted"
        );
        assert!(
            state.apia_key() == POINTER_AUTHENTICATION_TEST_APIA_KEY,
            "APIA should match the non-secret test key"
        );
        assert!(
            state.apib_key() == POINTER_AUTHENTICATION_TEST_APIB_KEY,
            "APIB should match the non-secret test key"
        );
        assert!(
            state.apda_key() == POINTER_AUTHENTICATION_TEST_APDA_KEY,
            "APDA should match the non-secret test key"
        );
        assert!(
            state.apdb_key() == POINTER_AUTHENTICATION_TEST_APDB_KEY,
            "APDB should match the non-secret test key"
        );
        assert!(
            state.apga_key() == POINTER_AUTHENTICATION_TEST_APGA_KEY,
            "APGA should match the non-secret test key"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_guest_written_arm64_thread_context_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = THREAD_CONTEXT_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("thread-context register guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest thread-context writer should exit through HVC")
        else {
            panic!("guest thread-context writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest thread-context writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_thread_context_register_state()
            .expect("thread-context register state should be captured");
        assert_eq!(state.tpidr_el0(), THREAD_CONTEXT_TEST_TPIDR_EL0);
        assert_eq!(state.tpidrro_el0(), THREAD_CONTEXT_TEST_TPIDRRO_EL0);
        assert_eq!(state.tpidr_el1(), THREAD_CONTEXT_TEST_TPIDR_EL1);

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_guest_written_arm64_simd_fp_state_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = SIMD_FP_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("SIMD/FP guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest SIMD/FP writer should exit through HVC")
        else {
            panic!("guest SIMD/FP writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest SIMD/FP writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_simd_fp_state()
            .expect("SIMD/FP state should be captured");
        assert_eq!(state.q_register(0), Some(SIMD_FP_TEST_Q0));
        assert_eq!(state.q_register(31), Some(SIMD_FP_TEST_Q31));
        assert_eq!(state.fpcr(), SIMD_FP_TEST_FPCR);
        assert_eq!(state.fpsr(), SIMD_FP_TEST_FPSR);

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn creates_hvf_gic_before_vcpu() {
    use bangbang_hvf::{HvfBackend, HvfGicMetadata};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    let metadata = *backend.create_gic().expect("GIC should be created");
    assert_eq!(metadata.msi, None);
    assert_eq!(HvfGicMetadata::FDT_COMPATIBILITY, "arm,gic-v3");
    assert!(metadata.distributor.size > 0);
    assert!(metadata.redistributor.region.size > 0);
    {
        let mut vcpu = backend
            .create_vcpu()
            .expect("vCPU should be created after GIC");
        vcpu.destroy().expect("vCPU should be destroyed");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_opaque_hvf_gic_device_state_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    backend.create_gic().expect("GIC should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");

        let state = runner
            .capture_gic_device_state()
            .expect("opaque GIC device state should be captured");
        assert!(!state.is_empty());
        assert_eq!(state.as_bytes().len(), state.len());

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_guest_written_arm64_gic_icc_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = GIC_ICC_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("GIC ICC register guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    backend.create_gic().expect("GIC should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest GIC ICC writer should exit through HVC")
        else {
            panic!("guest GIC ICC writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest GIC ICC writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_gic_icc_register_state()
            .expect("GIC ICC register state should be captured");
        assert_eq!(state.pmr_el1(), GIC_ICC_TEST_PMR_EL1);
        assert_eq!(state.bpr0_el1(), GIC_ICC_TEST_BPR0_EL1);
        assert_eq!(state.bpr1_el1(), GIC_ICC_TEST_BPR1_EL1);
        assert_eq!(state.sre_el1() & 1, 1);
        assert_eq!(state.igrpen0_el1(), 1);
        assert_eq!(state.igrpen1_el1(), 1);
        let _host_defined_values = (
            state.ap0r0_el1(),
            state.ap1r0_el1(),
            state.rpr_el1(),
            state.ctlr_el1(),
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn rejects_hvf_gic_after_vcpu_creation() {
    use bangbang_hvf::{HvfBackend, HvfGicError};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    {
        let mut vcpu = backend.create_vcpu().expect("vCPU should be created");
        vcpu.destroy().expect("vCPU should be destroyed");
    }
    assert_eq!(
        backend
            .create_gic()
            .expect_err("GIC creation after vCPU creation should fail"),
        HvfGicError::InvalidState("GIC must be created before creating vCPUs")
    );
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn cancels_runner_before_first_run() {
    use bangbang_hvf::{HvfBackend, HvfVcpuExit};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner.cancel().expect("runner should accept cancellation");
        assert_eq!(
            runner.run_once().expect("runner should return an exit"),
            HvfVcpuExit::Canceled
        );
        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_physical_timer_tval_on_idle_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    backend
        .create_gic()
        .expect("GIC should be created before the physical-timer vCPU");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_physical_timer_state()
            .expect("first idle physical-timer state should be captured");
        let second = runner
            .capture_arm64_physical_timer_state()
            .expect("second idle physical-timer state should be captured");

        let _first_tval = first.cntp_tval_el0();
        let _second_tval = second.cntp_tval_el0();

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_guest_written_arm64_physical_timer_state_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = PHYSICAL_TIMER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("physical-timer guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    backend
        .create_gic()
        .expect("GIC should be created before the physical-timer vCPU");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest physical-timer writer should exit through HVC")
        else {
            panic!("guest physical-timer writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest physical-timer writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_physical_timer_state()
            .expect("physical-timer state should be captured");
        assert_eq!(state.cntkctl_el1(), PHYSICAL_TIMER_TEST_CNTKCTL_EL1);
        assert_eq!(
            state.cntp_ctl_el0() & PHYSICAL_TIMER_WRITABLE_CONTROL_MASK,
            PHYSICAL_TIMER_TEST_CNTP_CTL_EL0
        );
        assert_eq!(
            state.cntp_ctl_el0() & !PHYSICAL_TIMER_DEFINED_CONTROL_MASK,
            0
        );
        assert!(matches!(
            state.cntp_ctl_el0() & PHYSICAL_TIMER_ISTATUS_MASK,
            0 | PHYSICAL_TIMER_ISTATUS_MASK
        ));
        assert_eq!(state.cntp_cval_el0(), PHYSICAL_TIMER_TEST_CNTP_CVAL_EL0);

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_runner_arm64_virtual_timer_state() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let original = runner
            .capture_arm64_virtual_timer_state()
            .expect("original runner vtimer state should be captured");

        runner
            .set_vtimer_mask(true)
            .expect("runner vtimer mask should be set");
        runner
            .set_vtimer_control(0)
            .expect("runner vtimer should be disabled");
        runner
            .set_vtimer_offset(VTIMER_TEST_OFFSET)
            .expect("runner vtimer offset should be set");
        runner
            .set_vtimer_compare_value(VTIMER_TEST_COMPARE_VALUE)
            .expect("runner vtimer compare value should be set");

        let captured = runner
            .capture_arm64_virtual_timer_state()
            .expect("runner vtimer state should be captured");
        assert!(captured.masked());
        assert_eq!(captured.offset(), VTIMER_TEST_OFFSET);
        assert_eq!(captured.control() & VTIMER_WRITABLE_CONTROL_MASK, 0);
        assert_eq!(captured.compare_value(), VTIMER_TEST_COMPARE_VALUE);

        runner
            .set_vtimer_offset(original.offset())
            .expect("original runner vtimer offset should be restored");
        runner
            .set_vtimer_compare_value(original.compare_value())
            .expect("original runner vtimer compare value should be restored");
        runner
            .set_vtimer_control(original.control() & VTIMER_WRITABLE_CONTROL_MASK)
            .expect("original runner vtimer control should be restored");
        runner
            .set_vtimer_mask(original.masked())
            .expect("original runner vtimer mask should be restored");

        let restored = runner
            .capture_arm64_virtual_timer_state()
            .expect("restored runner vtimer state should be captured");
        assert_eq!(restored.masked(), original.masked());
        assert_eq!(restored.offset(), original.offset());
        assert_eq!(
            restored.control() & VTIMER_WRITABLE_CONTROL_MASK,
            original.control() & VTIMER_WRITABLE_CONTROL_MASK
        );
        assert_eq!(restored.compare_value(), original.compare_value());

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_runner_arm64_pending_interrupt_state() {
    use bangbang_hvf::{HvfBackend, HvfInterruptType};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");

        runner
            .set_pending_interrupt(HvfInterruptType::Irq, true)
            .expect("runner IRQ pending level should be set");
        runner
            .set_pending_interrupt(HvfInterruptType::Fiq, false)
            .expect("runner FIQ pending level should be cleared");
        let irq_only = runner
            .capture_arm64_pending_interrupt_state()
            .expect("IRQ-only pending state should be captured");
        assert!(irq_only.irq_pending());
        assert!(!irq_only.fiq_pending());

        runner
            .set_pending_interrupt(HvfInterruptType::Irq, false)
            .expect("runner IRQ pending level should be cleared");
        runner
            .set_pending_interrupt(HvfInterruptType::Fiq, true)
            .expect("runner FIQ pending level should be set");
        let fiq_only = runner
            .capture_arm64_pending_interrupt_state()
            .expect("FIQ-only pending state should be captured");
        assert!(!fiq_only.irq_pending());
        assert!(fiq_only.fiq_pending());

        runner
            .set_pending_interrupt(HvfInterruptType::Irq, false)
            .expect("runner IRQ pending level should remain cleared");
        runner
            .set_pending_interrupt(HvfInterruptType::Fiq, false)
            .expect("runner FIQ pending level should be cleared");
        let cleared = runner
            .capture_arm64_pending_interrupt_state()
            .expect("cleared pending state should be captured");
        assert!(!cleared.irq_pending());
        assert!(!cleared.fiq_pending());

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn sets_and_clears_runner_gic_ppi_pending() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    let metadata = *backend.create_gic().expect("GIC should be created");
    let virtual_timer_intid = metadata.timer_interrupts.el1_virtual_timer_intid;
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .set_gic_ppi_pending(virtual_timer_intid)
            .expect("runner GIC PPI pending bit should be set");
        runner
            .clear_gic_ppi_pending(virtual_timer_intid)
            .expect("runner GIC PPI pending bit should be cleared");
        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn maps_guest_memory_and_unmaps_before_destroying_vm() {
    use bangbang_hvf::{HvfBackend, HvfMemoryPermissions};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let memory = GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    backend
        .destroy_vm()
        .expect("VM destruction should unmap guest memory first");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn prepares_internal_hvf_arm64_boot_session() {
    use bangbang_hvf::{ARM64_LINUX_BOOT_CPSR, HvfArm64BootSessionConfig, HvfBackend};
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::{PmemConfigInput, PmemMmioLayout, VIRTIO_PMEM_ALIGNMENT};
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("session-kernel", &image).expect("temp kernel should be created");
    let writable_pmem = TempFile::new_len("session-writable-pmem", VIRTIO_PMEM_ALIGNMENT)
        .expect("temp writable pmem should be created");
    let readonly_pmem = TempFile::new_len("session-readonly-pmem", VIRTIO_PMEM_ALIGNMENT)
        .expect("temp readonly pmem should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");
    controller
        .handle_action(VmmAction::PutPmem(PmemConfigInput::new(
            "pmem0",
            path_text(writable_pmem.path()),
        )))
        .expect("writable pmem config should be stored");
    controller
        .handle_action(VmmAction::PutPmem(
            PmemConfigInput::new("pmem1", path_text(readonly_pmem.path())).with_read_only(true),
        ))
        .expect("readonly pmem config should be stored");
    let mut backend = HvfBackend::new();
    let pmem_mmio_layout =
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500));
    let rtc_mmio_layout = test_rtc_mmio_layout();
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        pmem_mmio_layout,
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        rtc_mmio_layout,
    );

    let mut session = backend
        .prepare_arm64_boot_session(&controller, config.clone())
        .expect("internal HVF arm64 boot session should prepare");

    let mmio_dispatcher = session.mmio_dispatcher();
    let mmio_regions = mmio_dispatcher
        .try_lock()
        .expect("session MMIO dispatcher should lock")
        .regions()
        .to_vec();
    assert_eq!(mmio_regions.len(), 3);
    let first_pmem_region = mmio_regions
        .iter()
        .find(|region| region.id() == pmem_mmio_layout.base_region_id())
        .expect("first pmem MMIO region should be registered");
    assert_eq!(
        first_pmem_region.range().start(),
        pmem_mmio_layout.base_address()
    );
    assert_eq!(
        first_pmem_region.range().size(),
        bangbang_runtime::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE
    );
    let second_pmem_region_id =
        MmioRegionId::new(pmem_mmio_layout.base_region_id().raw_value() + 1);
    let second_pmem_region = mmio_regions
        .iter()
        .find(|region| region.id() == second_pmem_region_id)
        .expect("second pmem MMIO region should be registered");
    assert_eq!(second_pmem_region.id(), second_pmem_region_id);
    assert_eq!(
        second_pmem_region.range().start(),
        pmem_mmio_layout
            .base_address()
            .checked_add(pmem_mmio_layout.address_stride())
            .expect("second pmem MMIO address should fit")
    );
    assert_eq!(
        second_pmem_region.range().size(),
        bangbang_runtime::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE
    );
    let rtc_region = mmio_regions
        .iter()
        .find(|region| region.id() == rtc_mmio_layout.region_id())
        .expect("RTC MMIO region should be registered");
    assert_eq!(rtc_region.range().start(), rtc_mmio_layout.base());
    assert_eq!(
        rtc_region.range().size(),
        bangbang_runtime::rtc::RTC_MMIO_DEVICE_WINDOW_SIZE
    );
    assert!(session.block_interrupt_lines().is_empty());
    assert_eq!(session.pmem_interrupt_lines().len(), 2);
    assert_eq!(session.runtime_resources().pmem_devices.len(), 2);
    assert!(
        !session.runtime_resources().pmem_devices[0]
            .mapping()
            .is_read_only()
    );
    assert!(
        session.runtime_resources().pmem_devices[1]
            .mapping()
            .is_read_only()
    );
    assert!(
        !session.runtime_resources().pmem_devices[0]
            .guest_range()
            .overlaps(session.runtime_resources().layout.ranges()[0])
    );
    assert!(
        !session.runtime_resources().pmem_devices[0]
            .guest_range()
            .overlaps(session.runtime_resources().pmem_devices[1].guest_range())
    );
    assert_eq!(
        session
            .guest_memory()
            .expect("session should expose mapped guest memory")
            .total_size(),
        session.runtime_resources().layout.total_size()
    );
    let mut fdt_magic = [0; 4];
    session
        .guest_memory()
        .expect("session should expose mapped guest memory")
        .read_slice(&mut fdt_magic, session.runtime_resources().fdt.address)
        .expect("mapped guest memory should contain the written FDT");
    assert_eq!(u32::from_be_bytes(fdt_magic), 0xd00d_feed);
    assert_eq!(
        session.boot_registers().kernel_entry,
        session
            .runtime_resources()
            .loaded_boot_source
            .kernel
            .entry_address
    );
    assert_eq!(
        session.boot_registers().fdt_address,
        session.runtime_resources().fdt.address
    );
    let register_state = session
        .capture_arm64_general_register_state()
        .expect("internal session should capture general-register state");
    assert_eq!(
        register_state.general_purpose_register(0),
        Some(session.boot_registers().fdt_address.raw_value())
    );
    assert_eq!(
        register_state.pc(),
        session.boot_registers().kernel_entry.raw_value()
    );
    assert_eq!(register_state.cpsr(), ARM64_LINUX_BOOT_CPSR);
    session
        .restore_arm64_general_register_state(&register_state)
        .expect("internal session should restore general-register state");
    session
        .capture_arm64_core_system_register_state()
        .expect("internal session should capture core system-register state");
    session
        .capture_arm64_exception_register_state()
        .expect("internal session should capture exception-register state");
    session
        .capture_arm64_execution_control_register_state()
        .expect("internal session should capture execution-control state");
    session
        .capture_arm64_cache_selection_register_state()
        .expect("internal session should capture cache-selection state");
    session
        .capture_arm64_breakpoint_register_state()
        .expect("internal session should capture breakpoint-register state");
    session
        .capture_arm64_watchpoint_register_state()
        .expect("internal session should capture watchpoint-register state");
    session
        .capture_arm64_debug_control_register_state()
        .expect("internal session should capture debug-control state");
    session
        .capture_arm64_debug_trap_state()
        .expect("internal session should capture debug-trap state");
    session
        .capture_arm64_identification_register_state()
        .expect("internal session should capture identification-register state");
    session
        .capture_arm64_sve_sme_identification_register_state()
        .expect("internal session should capture SVE/SME identification state");
    let _sme_pstate =
        assert_sme_pstate_capture_supported_or_unavailable(session.capture_arm64_sme_pstate())
            .expect("internal session SME PSTATE capture should succeed or report unsupported");
    let _sme_p_registers = assert_sme_p_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_p_register_state(),
    )
    .expect("internal session SME P-register capture should succeed or report unavailable");
    let _sme_z_registers = assert_sme_z_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_z_register_state(),
    )
    .expect("internal session SME Z-register capture should succeed or report unavailable");
    let _sme_za_register = assert_sme_za_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_za_register_state(),
    )
    .expect("internal session SME ZA-register capture should succeed or report unavailable");
    let _sme_zt0_register = assert_sme_zt0_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_zt0_register_state(),
    )
    .expect("internal session SME ZT0-register capture should succeed or report unavailable");
    session
        .capture_arm64_sme_system_register_state()
        .expect("internal session should capture SME system-register state");
    session
        .capture_arm64_system_context_register_state()
        .expect("internal session should capture system-context register state");
    session
        .capture_arm64_translation_register_state()
        .expect("internal session should capture translation-register state");
    session
        .capture_arm64_pointer_authentication_key_state()
        .expect("internal session should capture pointer-authentication key state");
    session
        .capture_arm64_thread_context_register_state()
        .expect("internal session should capture thread-context register state");
    session
        .capture_arm64_simd_fp_state()
        .expect("internal session should capture SIMD/FP state");
    session
        .capture_arm64_physical_timer_state()
        .expect("internal session should capture physical-timer state");
    session
        .capture_arm64_virtual_timer_state()
        .expect("internal session should capture virtual-timer state");
    session
        .capture_arm64_pending_interrupt_state()
        .expect("internal session should capture pending-interrupt state");
    assert!(
        !session
            .capture_gic_device_state()
            .expect("internal session should capture GIC device state")
            .is_empty()
    );
    session
        .capture_arm64_gic_icc_register_state()
        .expect("internal session should capture GIC ICC register state");
    let run_cancel_handle = session.run_cancel_handle();
    drop(run_cancel_handle);
    let run_loop_control = session.run_loop_control();
    let run_loop_stop_token = run_loop_control.stop_token();
    run_loop_control
        .request_stop()
        .expect("internal HVF boot-session run-loop stop should request vCPU cancellation");
    assert!(run_loop_stop_token.is_stop_requested());
    session
        .shutdown()
        .expect("internal HVF arm64 boot session should shut down");
    drop(session);

    let mut second_session = backend
        .prepare_arm64_boot_session(&controller, config)
        .expect("second internal HVF arm64 boot session should prepare after shutdown");
    assert_eq!(
        second_session
            .guest_memory_mut()
            .expect("second session should expose mutable mapped guest memory")
            .total_size(),
        second_session.runtime_resources().layout.total_size()
    );
    second_session
        .shutdown()
        .expect("second internal HVF arm64 boot session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn prepares_owned_hvf_arm64_boot_session() {
    use bangbang_hvf::{
        ARM64_LINUX_BOOT_CPSR, HvfArm64BootSessionConfig, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel =
        TempFile::new("owned-session-kernel", &image).expect("temp kernel should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");
    let rtc_mmio_layout = test_rtc_mmio_layout();
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        rtc_mmio_layout,
    );

    let mut session = OwnedHvfArm64BootSession::new(&controller, config.clone())
        .expect("owned HVF arm64 boot session should prepare");

    let mmio_dispatcher = session.mmio_dispatcher();
    let mmio_regions = mmio_dispatcher
        .try_lock()
        .expect("owned session MMIO dispatcher should lock")
        .regions()
        .to_vec();
    assert_eq!(mmio_regions.len(), 1);
    assert_eq!(mmio_regions[0].id(), rtc_mmio_layout.region_id());
    assert_eq!(mmio_regions[0].range().start(), rtc_mmio_layout.base());
    assert_eq!(
        mmio_regions[0].range().size(),
        bangbang_runtime::rtc::RTC_MMIO_DEVICE_WINDOW_SIZE
    );
    assert!(session.block_interrupt_lines().is_empty());
    assert_eq!(
        session
            .guest_memory()
            .expect("owned session should expose mapped guest memory")
            .total_size(),
        session.runtime_resources().layout.total_size()
    );
    let mut fdt_magic = [0; 4];
    session
        .guest_memory()
        .expect("owned session should expose mapped guest memory")
        .read_slice(&mut fdt_magic, session.runtime_resources().fdt.address)
        .expect("mapped guest memory should contain the written FDT");
    assert_eq!(u32::from_be_bytes(fdt_magic), 0xd00d_feed);
    assert_eq!(
        session.boot_registers().kernel_entry,
        session
            .runtime_resources()
            .loaded_boot_source
            .kernel
            .entry_address
    );
    assert_eq!(
        session.boot_registers().fdt_address,
        session.runtime_resources().fdt.address
    );
    let register_state = session
        .capture_arm64_general_register_state()
        .expect("owned session should capture general-register state");
    assert_eq!(
        register_state.general_purpose_register(0),
        Some(session.boot_registers().fdt_address.raw_value())
    );
    assert_eq!(
        register_state.pc(),
        session.boot_registers().kernel_entry.raw_value()
    );
    assert_eq!(register_state.cpsr(), ARM64_LINUX_BOOT_CPSR);
    session
        .restore_arm64_general_register_state(&register_state)
        .expect("owned session should restore general-register state");
    session
        .capture_arm64_core_system_register_state()
        .expect("owned session should capture core system-register state");
    session
        .capture_arm64_exception_register_state()
        .expect("owned session should capture exception-register state");
    session
        .capture_arm64_execution_control_register_state()
        .expect("owned session should capture execution-control state");
    session
        .capture_arm64_cache_selection_register_state()
        .expect("owned session should capture cache-selection state");
    session
        .capture_arm64_breakpoint_register_state()
        .expect("owned session should capture breakpoint-register state");
    session
        .capture_arm64_watchpoint_register_state()
        .expect("owned session should capture watchpoint-register state");
    session
        .capture_arm64_debug_control_register_state()
        .expect("owned session should capture debug-control state");
    session
        .capture_arm64_debug_trap_state()
        .expect("owned session should capture debug-trap state");
    session
        .capture_arm64_identification_register_state()
        .expect("owned session should capture identification-register state");
    session
        .capture_arm64_sve_sme_identification_register_state()
        .expect("owned session should capture SVE/SME identification state");
    let _sme_pstate =
        assert_sme_pstate_capture_supported_or_unavailable(session.capture_arm64_sme_pstate())
            .expect("owned session SME PSTATE capture should succeed or report unsupported");
    let _sme_p_registers = assert_sme_p_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_p_register_state(),
    )
    .expect("owned session SME P-register capture should succeed or report unavailable");
    let _sme_z_registers = assert_sme_z_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_z_register_state(),
    )
    .expect("owned session SME Z-register capture should succeed or report unavailable");
    let _sme_za_register = assert_sme_za_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_za_register_state(),
    )
    .expect("owned session SME ZA-register capture should succeed or report unavailable");
    let _sme_zt0_register = assert_sme_zt0_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_zt0_register_state(),
    )
    .expect("owned session SME ZT0-register capture should succeed or report unavailable");
    session
        .capture_arm64_sme_system_register_state()
        .expect("owned session should capture SME system-register state");
    session
        .capture_arm64_system_context_register_state()
        .expect("owned session should capture system-context register state");
    session
        .capture_arm64_translation_register_state()
        .expect("owned session should capture translation-register state");
    session
        .capture_arm64_pointer_authentication_key_state()
        .expect("owned session should capture pointer-authentication key state");
    session
        .capture_arm64_thread_context_register_state()
        .expect("owned session should capture thread-context register state");
    session
        .capture_arm64_simd_fp_state()
        .expect("owned session should capture SIMD/FP state");
    session
        .capture_arm64_physical_timer_state()
        .expect("owned session should capture physical-timer state");
    session
        .capture_arm64_virtual_timer_state()
        .expect("owned session should capture virtual-timer state");
    session
        .capture_arm64_pending_interrupt_state()
        .expect("owned session should capture pending-interrupt state");
    assert!(
        !session
            .capture_gic_device_state()
            .expect("owned session should capture GIC device state")
            .is_empty()
    );
    session
        .capture_arm64_gic_icc_register_state()
        .expect("owned session should capture GIC ICC register state");
    let run_cancel_handle = session.run_cancel_handle();
    drop(run_cancel_handle);
    let run_loop_control = session.run_loop_control();
    let run_loop_stop_token = run_loop_control.stop_token();
    run_loop_control
        .request_stop()
        .expect("owned HVF boot-session run-loop stop should request vCPU cancellation");
    assert!(run_loop_stop_token.is_stop_requested());
    session
        .shutdown()
        .expect("owned HVF arm64 boot session should shut down");
    session
        .shutdown()
        .expect("repeated owned HVF arm64 boot session shutdown should be idempotent");
    drop(session);

    let mut second_session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("second owned HVF arm64 boot session should prepare after shutdown");
    assert_eq!(
        second_session
            .guest_memory_mut()
            .expect("second owned session should expose mutable mapped guest memory")
            .total_size(),
        second_session.runtime_resources().layout.total_size()
    );
    second_session
        .shutdown()
        .expect("second owned HVF arm64 boot session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn owned_hvf_arm64_boot_session_cleans_up_after_prepare_error() {
    use bangbang_hvf::{
        HvfArm64BootSessionConfig, HvfArm64BootSessionError, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::startup::Arm64BootResourceError;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let empty_controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");

    let err = OwnedHvfArm64BootSession::new(&empty_controller, config.clone())
        .expect_err("missing boot source should fail owned HVF session preparation");
    assert!(matches!(
        err,
        HvfArm64BootSessionError::AssembleResources {
            source: Arm64BootResourceError::MissingBootSource
        }
    ));

    let image = arm64_image().expect("test arm64 image should build");
    let kernel =
        TempFile::new("owned-session-retry-kernel", &image).expect("temp kernel should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");

    let mut session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("owned HVF arm64 boot session should prepare after failed preparation");
    session
        .shutdown()
        .expect("owned HVF arm64 boot session should shut down after retry");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn rejects_boot_session_on_existing_hvf_vm_without_destroying_it() {
    use bangbang_hvf::{HvfArm64BootSessionConfig, HvfArm64BootSessionError, HvfBackend};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("existing VM should be created");

    let err = backend
        .prepare_arm64_boot_session(
            &controller,
            HvfArm64BootSessionConfig::new(
                BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
                PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
                NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
                VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
                test_rtc_mmio_layout(),
            ),
        )
        .expect_err("existing VM should be rejected");

    assert!(matches!(
        err,
        HvfArm64BootSessionError::BackendAlreadyInitialized
    ));
    let _metadata = backend
        .create_gic()
        .expect("existing VM should remain available after rejected session");
    backend
        .destroy_vm()
        .expect("existing VM should remain owned by caller");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn host_page_size() -> Result<u64, std::num::TryFromIntError> {
    // SAFETY: `sysconf(_SC_PAGESIZE)` has no pointer arguments and does not
    // require process-local invariants from Rust.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };

    u64::try_from(page_size)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct TempFile {
    path: std::path::PathBuf,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl TempFile {
    fn new(name: &str, bytes: &[u8]) -> std::io::Result<Self> {
        use std::io::Write as _;

        let id = NEXT_HVF_TEST_FILE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("bangbang-hvf-{name}-{}-{}", std::process::id(), id));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(bytes)?;

        Ok(Self { path })
    }

    fn new_len(name: &str, len: u64) -> std::io::Result<Self> {
        let id = NEXT_HVF_TEST_FILE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("bangbang-hvf-{name}-{}-{}", std::process::id(), id));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.set_len(len)?;

        Ok(Self { path })
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn path_text(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn arm64_image() -> Result<Vec<u8>, &'static str> {
    const ARM64_IMAGE_HEADER_SIZE: usize = 64;
    const ARM64_IMAGE_TEXT_OFFSET_OFFSET: usize = 8;
    const ARM64_IMAGE_SIZE_OFFSET: usize = 16;
    const ARM64_IMAGE_MAGIC_OFFSET: usize = 56;
    const ARM64_IMAGE_MAGIC: u32 = 0x644d_5241;

    let mut bytes = vec![0xaa; ARM64_IMAGE_HEADER_SIZE];
    write_u64_le(&mut bytes, ARM64_IMAGE_TEXT_OFFSET_OFFSET, 0)?;
    write_u64_le(
        &mut bytes,
        ARM64_IMAGE_SIZE_OFFSET,
        ARM64_IMAGE_HEADER_SIZE as u64,
    )?;
    write_u32_le(&mut bytes, ARM64_IMAGE_MAGIC_OFFSET, ARM64_IMAGE_MAGIC)?;
    Ok(bytes)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_u64_le(bytes: &mut [u8], offset: usize, value: u64) -> Result<(), &'static str> {
    let end = offset + std::mem::size_of::<u64>();
    let destination = bytes
        .get_mut(offset..end)
        .ok_or("u64 write range should fit test image")?;
    destination.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_u32_le(bytes: &mut [u8], offset: usize, value: u32) -> Result<(), &'static str> {
    let end = offset + std::mem::size_of::<u32>();
    let destination = bytes
        .get_mut(offset..end)
        .ok_or("u32 write range should fit test image")?;
    destination.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[test]
fn requires_macos_apple_silicon() {
    panic!("signed HVF lifecycle tests require macOS Apple Silicon");
}
