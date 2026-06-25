use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use bangbang_runtime::BackendError;

use crate::backend::HvfBackend;

pub struct HvfVcpu<'vm> {
    handle: Option<HvfVcpuHandle>,
    _vm: PhantomData<&'vm mut HvfBackend>,
    _not_send_sync: PhantomData<Rc<()>>,
}

struct HvfVcpuHandle {
    vcpu: crate::ffi::HvVcpu,
    exit: *mut crate::ffi::HvVcpuExit,
}

impl<'vm> HvfVcpu<'vm> {
    pub(crate) fn new(_: &'vm mut HvfBackend) -> Result<Self, BackendError> {
        let created = crate::ffi::create_vcpu()?;

        Ok(Self {
            handle: Some(HvfVcpuHandle {
                vcpu: created.vcpu,
                exit: created.exit,
            }),
            _vm: PhantomData,
            _not_send_sync: PhantomData,
        })
    }

    pub fn destroy(&mut self) -> Result<(), BackendError> {
        self.destroy_inner()
    }

    fn destroy_inner(&mut self) -> Result<(), BackendError> {
        if let Some(handle) = self.handle.take() {
            if let Err(err) = crate::ffi::destroy_vcpu(handle.vcpu) {
                self.handle = Some(handle);
                return Err(err);
            }
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
        let (vcpu, has_exit) = match &self.handle {
            Some(handle) => (Some(handle.vcpu), !handle.exit.is_null()),
            None => (None, false),
        };

        f.debug_struct("HvfVcpu")
            .field("vcpu", &vcpu)
            .field("has_exit", &has_exit)
            .finish_non_exhaustive()
    }
}
