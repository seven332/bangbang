#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn creates_and_destroys_hvf_vcpu() {
    use bangbang_hvf::{HvfBackend, HvfRegister};
    use bangbang_runtime::BackendError;
    use bangbang_runtime::VmBackend;

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

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[test]
fn requires_macos_apple_silicon() {
    panic!("signed HVF lifecycle tests require macOS Apple Silicon");
}
