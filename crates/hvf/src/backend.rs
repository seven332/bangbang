use bangbang_runtime::{BackendError, VmBackend};

use crate::vcpu::HvfVcpu;

#[derive(Debug, Default)]
pub struct HvfBackend {
    vm_created: bool,
}

impl HvfBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_supported_target() -> bool {
        cfg!(all(target_os = "macos", target_arch = "aarch64"))
    }

    pub fn create_vcpu(&mut self) -> Result<HvfVcpu<'_>, BackendError> {
        if !Self::is_supported_target() {
            return Err(BackendError::Unsupported(
                crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
            ));
        }

        if !self.vm_created {
            return Err(BackendError::InvalidState(
                "VM must be created before creating a vCPU",
            ));
        }

        HvfVcpu::new(self)
    }
}

impl VmBackend for HvfBackend {
    fn create_vm(&mut self) -> Result<(), BackendError> {
        if self.vm_created {
            return Ok(());
        }

        crate::ffi::create_vm()?;
        self.vm_created = true;
        Ok(())
    }

    fn destroy_vm(&mut self) -> Result<(), BackendError> {
        if self.vm_created {
            crate::ffi::destroy_vm()?;
            self.vm_created = false;
        }
        Ok(())
    }
}

impl Drop for HvfBackend {
    fn drop(&mut self) {
        if self.vm_created {
            let _ = crate::ffi::destroy_vm();
            self.vm_created = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use bangbang_runtime::{BackendError, VmBackend};

    use super::HvfBackend;

    #[test]
    fn supported_target_matches_compile_target() {
        assert_eq!(
            HvfBackend::is_supported_target(),
            cfg!(all(target_os = "macos", target_arch = "aarch64"))
        );
    }

    #[test]
    fn create_vcpu_before_vm_reports_state_or_target_error() {
        let mut backend = HvfBackend::new();
        let err = backend
            .create_vcpu()
            .expect_err("creating a vCPU before VM creation should fail");

        if HvfBackend::is_supported_target() {
            assert_eq!(
                err,
                BackendError::InvalidState("VM must be created before creating a vCPU")
            );
        } else {
            assert_eq!(
                err,
                BackendError::Unsupported(crate::ffi::UNSUPPORTED_TARGET_MESSAGE)
            );
        }
    }

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    #[test]
    fn unsupported_target_rejects_vm_creation() {
        let mut backend = HvfBackend::new();

        assert_eq!(
            backend.create_vm(),
            Err(BackendError::Unsupported(
                crate::ffi::UNSUPPORTED_TARGET_MESSAGE
            ))
        );
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    #[ignore = "requires a signed Hypervisor.framework entitlement on macOS Apple Silicon"]
    fn creates_and_destroys_hvf_vcpu() {
        if std::env::var("BANGBANG_RUN_HVF_TESTS").as_deref() != Ok("1") {
            eprintln!(
                "sign the test binary with com.apple.security.hypervisor and set \
                 BANGBANG_RUN_HVF_TESTS=1 to run real HVF lifecycle smoke tests"
            );
            return;
        }

        let mut backend = HvfBackend::new();

        backend.create_vm().expect("VM should be created");
        {
            let vcpu = backend.create_vcpu().expect("vCPU should be created");
            vcpu.destroy().expect("vCPU should be destroyed");
        }
        backend.destroy_vm().expect("VM should be destroyed");
    }
}
