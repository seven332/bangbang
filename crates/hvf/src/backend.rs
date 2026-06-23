use bangbang_runtime::{BackendError, VmBackend};

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
