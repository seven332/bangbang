use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use bangbang_runtime::BackendError;
use bangbang_runtime::memory::GuestAddress;
use bangbang_runtime::mmio::{MmioAccessBytes, MmioOperation};

use crate::backend::HvfBackend;
use crate::exit::{HvfResolvedMmioAccess, HvfVcpuExit};
use crate::mmio::HvfMmioCompletionError;

const DESTROYED_VCPU_MESSAGE: &str = "vCPU has already been destroyed";
const NO_VCPU_EXIT_MESSAGE: &str = "vCPU has not exited yet";

/// CPSR/PSTATE value used for the primary arm64 Linux boot vCPU.
pub const ARM64_LINUX_BOOT_CPSR: u64 = 0x3c5;

/// Guest addresses used to initialize the primary arm64 Linux boot vCPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64BootRegisters {
    /// Guest address loaded into PC before the first vCPU run.
    pub kernel_entry: GuestAddress,
    /// Guest address loaded into X0 before the first vCPU run.
    pub fdt_address: GuestAddress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HvfRegister(u32);

impl HvfRegister {
    pub const X0: Self = Self(crate::ffi::HV_REG_X0);
    pub const X1: Self = Self(crate::ffi::HV_REG_X1);
    pub const X2: Self = Self(crate::ffi::HV_REG_X2);
    pub const X3: Self = Self(crate::ffi::HV_REG_X3);
    pub const PC: Self = Self(crate::ffi::HV_REG_PC);
    pub const CPSR: Self = Self(crate::ffi::HV_REG_CPSR);

    pub(crate) const fn general_purpose(value: u8) -> Option<Self> {
        if value <= 30 {
            Some(Self(crate::ffi::HV_REG_X0 + value as u32))
        } else {
            None
        }
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HvfSystemRegister(u16);

impl HvfSystemRegister {
    pub const MPIDR_EL1: Self = Self(crate::ffi::HV_SYS_REG_MPIDR_EL1);
    pub const SPSR_EL1: Self = Self(crate::ffi::HV_SYS_REG_SPSR_EL1);
    pub const ELR_EL1: Self = Self(crate::ffi::HV_SYS_REG_ELR_EL1);
    pub const SP_EL1: Self = Self(crate::ffi::HV_SYS_REG_SP_EL1);

    pub const fn raw(self) -> u16 {
        self.0
    }
}

pub struct HvfVcpu<'vm> {
    owner: HvfVcpuOwner,
    _vm: PhantomData<&'vm mut HvfBackend>,
    _not_send_sync: PhantomData<Rc<()>>,
}

pub(crate) struct HvfVcpuOwner {
    handle: Option<HvfVcpuHandle>,
    _not_send_sync: PhantomData<Rc<()>>,
}

struct HvfVcpuHandle {
    vcpu: crate::ffi::HvVcpu,
    exit: *mut crate::ffi::HvVcpuExit,
    exit_available: bool,
}

impl HvfVcpuOwner {
    pub(crate) fn new() -> Result<Self, BackendError> {
        let created = crate::ffi::create_vcpu()?;

        Ok(Self {
            handle: Some(HvfVcpuHandle {
                vcpu: created.vcpu,
                exit: created.exit,
                exit_available: false,
            }),
            _not_send_sync: PhantomData,
        })
    }

    pub(crate) fn raw_vcpu(&self) -> Result<crate::ffi::HvVcpu, BackendError> {
        Ok(self.handle()?.vcpu)
    }

    pub(crate) fn destroy(&mut self) -> Result<(), BackendError> {
        if let Some(handle) = &self.handle {
            crate::ffi::destroy_vcpu(handle.vcpu)?;
            self.handle = None;
        }
        Ok(())
    }

    pub(crate) fn run_once(&mut self) -> Result<HvfVcpuExit, BackendError> {
        let vcpu = self.prepare_run()?;

        crate::ffi::run_vcpu(vcpu)?;
        self.mark_exit_available()?;
        self.exit_snapshot()
    }

    pub(crate) fn exit_snapshot(&self) -> Result<HvfVcpuExit, BackendError> {
        let handle = self.handle()?;
        if !handle.exit_available {
            return Err(BackendError::InvalidState(NO_VCPU_EXIT_MESSAGE));
        }

        // SAFETY: `handle` belongs to this live current-thread vCPU, and
        // `exit_available` is only set after HVF has produced exit data.
        let raw_exit = unsafe { crate::ffi::copy_vcpu_exit(handle.exit)? };

        Ok(HvfVcpuExit::from_raw(raw_exit))
    }

    pub(crate) fn get_register(&self, register: HvfRegister) -> Result<u64, BackendError> {
        crate::ffi::get_reg(self.handle()?.vcpu, register.raw())
    }

    pub(crate) fn set_register(
        &mut self,
        register: HvfRegister,
        value: u64,
    ) -> Result<(), BackendError> {
        crate::ffi::set_reg(self.handle()?.vcpu, register.raw(), value)
    }

    pub(crate) fn configure_arm64_boot_registers(
        &mut self,
        registers: HvfArm64BootRegisters,
    ) -> Result<(), BackendError> {
        configure_arm64_boot_registers_with(registers, |register, value| {
            self.set_register(register, value)
        })
    }

    pub(crate) fn mmio_operation(
        &self,
        access: HvfResolvedMmioAccess,
    ) -> Result<MmioOperation, HvfMmioCompletionError> {
        crate::mmio::build_mmio_operation(access, |register| self.get_register(register))
    }

    pub(crate) fn complete_mmio_read(
        &mut self,
        access: HvfResolvedMmioAccess,
        data: MmioAccessBytes,
    ) -> Result<(), HvfMmioCompletionError> {
        crate::mmio::complete_mmio_read(access, data, |register, value| {
            self.set_register(register, value)
        })
    }

    pub(crate) fn get_system_register(
        &self,
        register: HvfSystemRegister,
    ) -> Result<u64, BackendError> {
        crate::ffi::get_sys_reg(self.handle()?.vcpu, register.raw())
    }

    pub(crate) fn set_system_register(
        &mut self,
        register: HvfSystemRegister,
        value: u64,
    ) -> Result<(), BackendError> {
        crate::ffi::set_sys_reg(self.handle()?.vcpu, register.raw(), value)
    }

    fn mark_exit_available(&mut self) -> Result<(), BackendError> {
        self.handle_mut()?.exit_available = true;
        Ok(())
    }

    fn prepare_run(&mut self) -> Result<crate::ffi::HvVcpu, BackendError> {
        let handle = self.handle_mut()?;
        handle.exit_available = false;
        Ok(handle.vcpu)
    }

    fn handle(&self) -> Result<&HvfVcpuHandle, BackendError> {
        self.handle
            .as_ref()
            .ok_or(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
    }

    fn handle_mut(&mut self) -> Result<&mut HvfVcpuHandle, BackendError> {
        self.handle
            .as_mut()
            .ok_or(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
    }
}

impl Drop for HvfVcpuOwner {
    fn drop(&mut self) {
        let _ = self.destroy();
    }
}

impl fmt::Debug for HvfVcpuOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (active, has_exit_pointer, exit_available) = match &self.handle {
            Some(handle) => (true, !handle.exit.is_null(), handle.exit_available),
            None => (false, false, false),
        };

        f.debug_struct("HvfVcpuOwner")
            .field("active", &active)
            .field("has_exit_pointer", &has_exit_pointer)
            .field("exit_available", &exit_available)
            .finish()
    }
}

impl<'vm> HvfVcpu<'vm> {
    pub(crate) fn new() -> Result<Self, BackendError> {
        Ok(Self {
            owner: HvfVcpuOwner::new()?,
            _vm: PhantomData,
            _not_send_sync: PhantomData,
        })
    }

    pub fn destroy(&mut self) -> Result<(), BackendError> {
        self.owner.destroy()
    }

    pub fn exit_snapshot(&self) -> Result<HvfVcpuExit, BackendError> {
        self.owner.exit_snapshot()
    }

    pub fn get_register(&self, register: HvfRegister) -> Result<u64, BackendError> {
        self.owner.get_register(register)
    }

    pub fn set_register(&mut self, register: HvfRegister, value: u64) -> Result<(), BackendError> {
        self.owner.set_register(register, value)
    }

    /// Configure the primary arm64 Linux boot-register state on this current-thread vCPU.
    pub fn configure_arm64_boot_registers(
        &mut self,
        registers: HvfArm64BootRegisters,
    ) -> Result<(), BackendError> {
        self.owner.configure_arm64_boot_registers(registers)
    }

    /// Build the runtime MMIO operation represented by a resolved HVF exit.
    pub fn mmio_operation(
        &self,
        access: HvfResolvedMmioAccess,
    ) -> Result<MmioOperation, HvfMmioCompletionError> {
        self.owner.mmio_operation(access)
    }

    /// Complete an HVF MMIO read exit by writing the runtime read data into the trapped GPR.
    pub fn complete_mmio_read(
        &mut self,
        access: HvfResolvedMmioAccess,
        data: MmioAccessBytes,
    ) -> Result<(), HvfMmioCompletionError> {
        self.owner.complete_mmio_read(access, data)
    }

    pub fn get_system_register(&self, register: HvfSystemRegister) -> Result<u64, BackendError> {
        self.owner.get_system_register(register)
    }

    pub fn set_system_register(
        &mut self,
        register: HvfSystemRegister,
        value: u64,
    ) -> Result<(), BackendError> {
        self.owner.set_system_register(register, value)
    }
}

impl fmt::Debug for HvfVcpu<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfVcpu")
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

fn configure_arm64_boot_registers_with(
    registers: HvfArm64BootRegisters,
    mut set_register: impl FnMut(HvfRegister, u64) -> Result<(), BackendError>,
) -> Result<(), BackendError> {
    for (register, value) in [
        (HvfRegister::PC, registers.kernel_entry.raw_value()),
        (HvfRegister::X0, registers.fdt_address.raw_value()),
        (HvfRegister::X1, 0),
        (HvfRegister::X2, 0),
        (HvfRegister::X3, 0),
        (HvfRegister::CPSR, ARM64_LINUX_BOOT_CPSR),
    ] {
        set_register(register, value)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::marker::PhantomData;
    use std::mem::ManuallyDrop;
    use std::ptr;
    use std::rc::Rc;

    use bangbang_runtime::BackendError;
    use bangbang_runtime::memory::GuestAddress;

    use super::{
        ARM64_LINUX_BOOT_CPSR, DESTROYED_VCPU_MESSAGE, HvfArm64BootRegisters, HvfRegister,
        HvfSystemRegister, HvfVcpu, HvfVcpuHandle, HvfVcpuOwner, NO_VCPU_EXIT_MESSAGE,
        configure_arm64_boot_registers_with,
    };
    use crate::exit::{HvfExceptionExit, HvfVcpuExit};

    fn fake_vcpu_owner(exit: *mut crate::ffi::HvVcpuExit, exit_available: bool) -> HvfVcpuOwner {
        HvfVcpuOwner {
            handle: Some(HvfVcpuHandle {
                vcpu: 7,
                exit,
                exit_available,
            }),
            _not_send_sync: PhantomData::<Rc<()>>,
        }
    }

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

    fn boot_registers() -> HvfArm64BootRegisters {
        HvfArm64BootRegisters {
            kernel_entry: GuestAddress::new(0x8028_0000),
            fdt_address: GuestAddress::new(0x8fe0_0000),
        }
    }

    fn fake_vcpu(
        exit: *mut crate::ffi::HvVcpuExit,
        exit_available: bool,
    ) -> ManuallyDrop<HvfVcpu<'static>> {
        ManuallyDrop::new(HvfVcpu {
            owner: fake_vcpu_owner(exit, exit_available),
            _vm: PhantomData,
            _not_send_sync: PhantomData::<Rc<()>>,
        })
    }

    #[test]
    fn exit_snapshot_copies_raw_exit_data() {
        let mut exit = raw_exit(crate::ffi::HV_EXIT_REASON_EXCEPTION);
        let vcpu = fake_vcpu(ptr::addr_of_mut!(exit), true);

        assert_eq!(
            vcpu.exit_snapshot(),
            Ok(HvfVcpuExit::Exception(HvfExceptionExit {
                syndrome: 0xabc,
                virtual_address: 0xdef,
                physical_address: 0x123,
            }))
        );
    }

    #[test]
    fn exit_snapshot_rejects_null_exit_pointer() {
        let vcpu = fake_vcpu(ptr::null_mut(), true);

        let err = vcpu
            .exit_snapshot()
            .expect_err("null exit pointer should fail");

        assert_eq!(
            err,
            BackendError::Hypervisor("hv_vcpu_exit_t pointer is null".to_string())
        );
    }

    #[test]
    fn exit_snapshot_rejects_unavailable_exit() {
        let mut exit = raw_exit(crate::ffi::HV_EXIT_REASON_EXCEPTION);
        let vcpu = fake_vcpu(ptr::addr_of_mut!(exit), false);

        assert_eq!(
            vcpu.exit_snapshot(),
            Err(BackendError::InvalidState(NO_VCPU_EXIT_MESSAGE))
        );
    }

    #[test]
    fn prepare_run_clears_stale_exit_snapshot() {
        let mut exit = raw_exit(crate::ffi::HV_EXIT_REASON_EXCEPTION);
        let mut owner = ManuallyDrop::new(fake_vcpu_owner(ptr::addr_of_mut!(exit), true));

        assert_eq!(owner.prepare_run(), Ok(7));
        assert_eq!(
            owner.exit_snapshot(),
            Err(BackendError::InvalidState(NO_VCPU_EXIT_MESSAGE))
        );
    }

    #[test]
    fn exit_snapshot_rejects_destroyed_vcpu() {
        let vcpu = HvfVcpu {
            owner: HvfVcpuOwner {
                handle: None,
                _not_send_sync: PhantomData::<Rc<()>>,
            },
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
        let mut vcpu = HvfVcpu {
            owner: HvfVcpuOwner {
                handle: None,
                _not_send_sync: PhantomData::<Rc<()>>,
            },
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
        assert_eq!(
            vcpu.configure_arm64_boot_registers(boot_registers()),
            Err(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
        );
    }

    #[test]
    fn arm64_boot_register_setup_writes_linux_boot_state() {
        let mut writes = Vec::new();

        configure_arm64_boot_registers_with(boot_registers(), |register, value| {
            writes.push((register, value));
            Ok(())
        })
        .expect("boot register setup should succeed");

        assert_eq!(
            writes,
            vec![
                (HvfRegister::PC, 0x8028_0000),
                (HvfRegister::X0, 0x8fe0_0000),
                (HvfRegister::X1, 0),
                (HvfRegister::X2, 0),
                (HvfRegister::X3, 0),
                (HvfRegister::CPSR, ARM64_LINUX_BOOT_CPSR),
            ]
        );
    }

    #[test]
    fn arm64_boot_register_setup_stops_after_register_error() {
        let mut writes = Vec::new();

        let result = configure_arm64_boot_registers_with(boot_registers(), |register, value| {
            writes.push((register, value));
            if register == HvfRegister::X0 {
                Err(BackendError::InvalidState("fake register write failed"))
            } else {
                Ok(())
            }
        });

        assert_eq!(
            result,
            Err(BackendError::InvalidState("fake register write failed"))
        );
        assert_eq!(
            writes,
            vec![
                (HvfRegister::PC, 0x8028_0000),
                (HvfRegister::X0, 0x8fe0_0000),
            ]
        );
    }

    #[test]
    fn general_purpose_register_mapping_excludes_pc() {
        assert_eq!(
            HvfRegister::general_purpose(0).map(HvfRegister::raw),
            Some(crate::ffi::HV_REG_X0)
        );
        assert_eq!(
            HvfRegister::general_purpose(30).map(HvfRegister::raw),
            Some(crate::ffi::HV_REG_X0 + 30)
        );
        assert_eq!(HvfRegister::general_purpose(31), None);
        assert_ne!(crate::ffi::HV_REG_X0 + 30, HvfRegister::PC.raw());
    }
}
