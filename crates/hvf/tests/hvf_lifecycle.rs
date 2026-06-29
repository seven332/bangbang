#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static HVF_LIFECYCLE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static NEXT_HVF_TEST_FILE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
#[test]
fn prepares_internal_hvf_arm64_boot_session() {
    use bangbang_hvf::{HvfArm64BootSessionConfig, HvfBackend};
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("session-kernel", &image).expect("temp kernel should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");
    let mut backend = HvfBackend::new();
    let config = HvfArm64BootSessionConfig::new(BlockMmioLayout::new(
        GuestAddress::new(0x4000_0000),
        MmioRegionId::new(1),
    ));

    let mut session = backend
        .prepare_arm64_boot_session(&controller, config)
        .expect("internal HVF arm64 boot session should prepare");

    assert!(session.block_interrupt_lines().is_empty());
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
    session
        .shutdown()
        .expect("internal HVF arm64 boot session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn rejects_boot_session_on_existing_hvf_vm_without_destroying_it() {
    use bangbang_hvf::{HvfArm64BootSessionConfig, HvfArm64BootSessionError, HvfBackend};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("existing VM should be created");

    let err = backend
        .prepare_arm64_boot_session(
            &controller,
            HvfArm64BootSessionConfig::new(BlockMmioLayout::new(
                GuestAddress::new(0x4000_0000),
                MmioRegionId::new(1),
            )),
        )
        .expect_err("existing VM should be rejected");

    assert!(matches!(
        err,
        HvfArm64BootSessionError::BackendAlreadyInitialized
    ));
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

    fn path(&self) -> &std::path::Path {
        &self.path
    }
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
