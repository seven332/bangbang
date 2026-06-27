#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static HVF_LIFECYCLE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn creates_and_destroys_hvf_vcpu() {
    use bangbang_hvf::{HvfBackend, HvfRegister};
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
fn host_page_size() -> Result<u64, std::num::TryFromIntError> {
    // SAFETY: `sysconf(_SC_PAGESIZE)` has no pointer arguments and does not
    // require process-local invariants from Rust.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };

    u64::try_from(page_size)
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[test]
fn requires_macos_apple_silicon() {
    panic!("signed HVF lifecycle tests require macOS Apple Silicon");
}
