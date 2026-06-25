use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use bangbang_runtime::BackendError;

use crate::backend::HvfBackend;

pub struct HvfVcpu<'vm> {
    vcpu: Option<crate::ffi::HvVcpu>,
    exit: *mut crate::ffi::HvVcpuExit,
    _vm: PhantomData<&'vm mut HvfBackend>,
    _not_send_sync: PhantomData<Rc<()>>,
}

impl<'vm> HvfVcpu<'vm> {
    pub(crate) fn new(_: &'vm mut HvfBackend) -> Result<Self, BackendError> {
        let created = crate::ffi::create_vcpu()?;

        Ok(Self {
            vcpu: Some(created.vcpu),
            exit: created.exit,
            _vm: PhantomData,
            _not_send_sync: PhantomData,
        })
    }

    pub fn destroy(&mut self) -> Result<(), BackendError> {
        self.destroy_inner()
    }

    fn destroy_inner(&mut self) -> Result<(), BackendError> {
        if let Some(vcpu) = self.vcpu {
            crate::ffi::destroy_vcpu(vcpu)?;
            self.vcpu = None;
            self.exit = std::ptr::null_mut();
        }
        Ok(())
    }
}

impl Drop for HvfVcpu<'_> {
    fn drop(&mut self) {
        let _ = self.destroy_inner();
    }
}

impl fmt::Debug for HvfVcpu<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfVcpu")
            .field("vcpu", &self.vcpu)
            .field("has_exit", &(!self.exit.is_null()))
            .finish_non_exhaustive()
    }
}
