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
fn test_rtc_mmio_layout() -> bangbang_runtime::rtc::RtcMmioLayout {
    bangbang_runtime::rtc::RtcMmioLayout::new(
        bangbang_runtime::memory::GuestAddress::new(0x4000_1000),
        bangbang_runtime::mmio::MmioRegionId::new(3000),
    )
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
        .capture_arm64_core_system_register_state()
        .expect("internal session should capture core system-register state");
    session
        .capture_arm64_simd_fp_state()
        .expect("internal session should capture SIMD/FP state");
    session
        .capture_arm64_virtual_timer_state()
        .expect("internal session should capture virtual-timer state");
    session
        .capture_arm64_pending_interrupt_state()
        .expect("internal session should capture pending-interrupt state");
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
        .capture_arm64_core_system_register_state()
        .expect("owned session should capture core system-register state");
    session
        .capture_arm64_simd_fp_state()
        .expect("owned session should capture SIMD/FP state");
    session
        .capture_arm64_virtual_timer_state()
        .expect("owned session should capture virtual-timer state");
    session
        .capture_arm64_pending_interrupt_state()
        .expect("owned session should capture pending-interrupt state");
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
