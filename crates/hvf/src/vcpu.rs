use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use bangbang_runtime::BackendError;

use crate::backend::HvfBackend;
use crate::exit::HvfVcpuExit;

const DESTROYED_VCPU_MESSAGE: &str = "vCPU has already been destroyed";
const NO_VCPU_EXIT_MESSAGE: &str = "vCPU has not exited yet";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HvfRegister(u32);

impl HvfRegister {
    pub const X0: Self = Self(crate::ffi::HV_REG_X0);
    pub const X1: Self = Self(crate::ffi::HV_REG_X1);
    pub const X2: Self = Self(crate::ffi::HV_REG_X2);
    pub const X3: Self = Self(crate::ffi::HV_REG_X3);
    pub const PC: Self = Self(crate::ffi::HV_REG_PC);
    pub const CPSR: Self = Self(crate::ffi::HV_REG_CPSR);

    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HvfSystemRegister(u16);

impl HvfSystemRegister {
    pub const SPSR_EL1: Self = Self(crate::ffi::HV_SYS_REG_SPSR_EL1);
    pub const ELR_EL1: Self = Self(crate::ffi::HV_SYS_REG_ELR_EL1);
    pub const SP_EL1: Self = Self(crate::ffi::HV_SYS_REG_SP_EL1);

    pub const fn raw(self) -> u16 {
        self.0
    }
}

pub struct HvfVcpu<'vm> {
    handle: Option<HvfVcpuHandle>,
    _vm: PhantomData<&'vm mut HvfBackend>,
    _not_send_sync: PhantomData<Rc<()>>,
}

struct HvfVcpuHandle {
    vcpu: crate::ffi::HvVcpu,
    exit: *mut crate::ffi::HvVcpuExit,
    exit_available: bool,
}

impl<'vm> HvfVcpu<'vm> {
    pub(crate) fn new(_: &'vm mut HvfBackend) -> Result<Self, BackendError> {
        let created = crate::ffi::create_vcpu()?;

        Ok(Self {
            handle: Some(HvfVcpuHandle {
                vcpu: created.vcpu,
                exit: created.exit,
                exit_available: false,
            }),
            _vm: PhantomData,
            _not_send_sync: PhantomData,
        })
    }

    pub fn destroy(&mut self) -> Result<(), BackendError> {
        self.destroy_inner()
    }

    pub fn exit_snapshot(&self) -> Result<HvfVcpuExit, BackendError> {
        let handle = self.handle()?;
        if !handle.exit_available {
            return Err(BackendError::InvalidState(NO_VCPU_EXIT_MESSAGE));
        }

        // SAFETY: `handle` belongs to this live current-thread vCPU, and
        // `exit_available` is only set after HVF has produced exit data.
        let raw_exit = unsafe { crate::ffi::copy_vcpu_exit(handle.exit)? };

        Ok(HvfVcpuExit::from_raw(raw_exit))
    }

    pub fn get_register(&self, register: HvfRegister) -> Result<u64, BackendError> {
        crate::ffi::get_reg(self.handle()?.vcpu, register.raw())
    }

    pub fn set_register(&self, register: HvfRegister, value: u64) -> Result<(), BackendError> {
        crate::ffi::set_reg(self.handle()?.vcpu, register.raw(), value)
    }

    pub fn get_system_register(&self, register: HvfSystemRegister) -> Result<u64, BackendError> {
        crate::ffi::get_sys_reg(self.handle()?.vcpu, register.raw())
    }

    pub fn set_system_register(
        &self,
        register: HvfSystemRegister,
        value: u64,
    ) -> Result<(), BackendError> {
        crate::ffi::set_sys_reg(self.handle()?.vcpu, register.raw(), value)
    }

    fn handle(&self) -> Result<&HvfVcpuHandle, BackendError> {
        self.handle
            .as_ref()
            .ok_or(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
    }

    fn destroy_inner(&mut self) -> Result<(), BackendError> {
        if let Some(handle) = &self.handle {
            crate::ffi::destroy_vcpu(handle.vcpu)?;
            self.handle = None;
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
        let (vcpu, has_exit_pointer, exit_available) = match &self.handle {
            Some(handle) => (
                Some(handle.vcpu),
                !handle.exit.is_null(),
                handle.exit_available,
            ),
            None => (None, false, false),
        };

        f.debug_struct("HvfVcpu")
            .field("vcpu", &vcpu)
            .field("has_exit_pointer", &has_exit_pointer)
            .field("exit_available", &exit_available)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::marker::PhantomData;
    use std::ptr;
    use std::rc::Rc;

    use bangbang_runtime::BackendError;

    use super::{
        HvfRegister, HvfSystemRegister, HvfVcpu, HvfVcpuHandle, DESTROYED_VCPU_MESSAGE,
        NO_VCPU_EXIT_MESSAGE,
    };
    use crate::exit::{HvfExceptionExit, HvfVcpuExit};

    fn raw_exit(reason: u32) -> crate::ffi::HvVcpuExit {
        crate::ffi::HvVcpuExit {
            reason,
            exception: crate::ffi::HvVcpuExitException {
                syndrome: 0xabc,
                virtual_address: 0xdef,
                physical_address: 0x123,
            },
        }
    }

    fn fake_vcpu(exit: *mut crate::ffi::HvVcpuExit, exit_available: bool) -> HvfVcpu<'static> {
        HvfVcpu {
            handle: Some(HvfVcpuHandle {
                vcpu: 7,
                exit,
                exit_available,
            }),
            _vm: PhantomData,
            _not_send_sync: PhantomData::<Rc<()>>,
        }
    }

    #[test]
    fn exit_snapshot_copies_raw_exit_data() {
        let mut exit = raw_exit(crate::ffi::HV_EXIT_REASON_EXCEPTION);
        let mut vcpu = fake_vcpu(ptr::addr_of_mut!(exit), true);

        assert_eq!(
            vcpu.exit_snapshot(),
            Ok(HvfVcpuExit::Exception(HvfExceptionExit {
                syndrome: 0xabc,
                virtual_address: 0xdef,
                physical_address: 0x123,
            }))
        );

        vcpu.handle = None;
    }

    #[test]
    fn exit_snapshot_rejects_null_exit_pointer() {
        let mut vcpu = fake_vcpu(ptr::null_mut(), true);

        let err = vcpu
            .exit_snapshot()
            .expect_err("null exit pointer should fail");

        assert_eq!(
            err,
            BackendError::Hypervisor("hv_vcpu_exit_t pointer is null".to_string())
        );

        vcpu.handle = None;
    }

    #[test]
    fn exit_snapshot_rejects_unavailable_exit() {
        let mut exit = raw_exit(crate::ffi::HV_EXIT_REASON_EXCEPTION);
        let mut vcpu = fake_vcpu(ptr::addr_of_mut!(exit), false);

        assert_eq!(
            vcpu.exit_snapshot(),
            Err(BackendError::InvalidState(NO_VCPU_EXIT_MESSAGE))
        );

        vcpu.handle = None;
    }

    #[test]
    fn exit_snapshot_rejects_destroyed_vcpu() {
        let vcpu = HvfVcpu {
            handle: None,
            _vm: PhantomData,
            _not_send_sync: PhantomData::<Rc<()>>,
        };

        assert_eq!(
            vcpu.exit_snapshot(),
            Err(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
        );
    }

    #[test]
    fn register_access_rejects_destroyed_vcpu() {
        let vcpu = HvfVcpu {
            handle: None,
            _vm: PhantomData,
            _not_send_sync: PhantomData::<Rc<()>>,
        };

        assert_eq!(
            vcpu.get_register(HvfRegister::X0),
            Err(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
        );
        assert_eq!(
            vcpu.set_register(HvfRegister::X0, 0),
            Err(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
        );
        assert_eq!(
            vcpu.get_system_register(HvfSystemRegister::SP_EL1),
            Err(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
        );
        assert_eq!(
            vcpu.set_system_register(HvfSystemRegister::SP_EL1, 0),
            Err(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
        );
    }
}
