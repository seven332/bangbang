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
        .unmap_guest_memory()
        .expect("guest memory should be unmapped");
    backend
        .destroy_vm()
        .expect("VM should be destroyed after guest memory unmap");
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
