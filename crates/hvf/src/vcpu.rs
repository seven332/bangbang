use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use bangbang_runtime::BackendError;
use bangbang_runtime::memory::GuestAddress;
use bangbang_runtime::mmio::{MmioAccessBytes, MmioDispatchOutcome, MmioDispatcher, MmioOperation};

use crate::backend::HvfBackend;
use crate::exit::{HvfResolvedMmioAccess, HvfVcpuExit};
use crate::gic::{HvfGicError, HvfGicPpiPendingWriter};
use crate::mmio::{HvfMmioCompletionError, HvfMmioDispatchError, HvfMmioRegisterAccess};

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

/// Detached general-register state captured from one arm64 vCPU.
///
/// This is the first read-only architectural subset for later snapshot
/// orchestration. It does not include system, SIMD/FP, timer, interrupt, or
/// device state and is not a serialized snapshot schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfArm64VcpuGeneralRegisterState {
    general_purpose_registers: [u64; 31],
    pc: u64,
    cpsr: u64,
}

impl HvfArm64VcpuGeneralRegisterState {
    /// Return the captured X0 through X30 values in architectural order.
    pub const fn general_purpose_registers(&self) -> &[u64; 31] {
        &self.general_purpose_registers
    }

    /// Return one captured X register, or `None` when `index` is outside X0-X30.
    pub fn general_purpose_register(&self, index: u8) -> Option<u64> {
        self.general_purpose_registers
            .get(usize::from(index))
            .copied()
    }

    /// Return the captured program counter.
    pub const fn pc(&self) -> u64 {
        self.pc
    }

    /// Return the captured CPSR/PSTATE value.
    pub const fn cpsr(&self) -> u64 {
        self.cpsr
    }
}

/// Detached raw virtual-timer state captured from one arm64 vCPU.
///
/// The offset is the Hypervisor.framework value used in its
/// `CNTVCT_EL0 = mach_absolute_time() - offset` relation. `control` is the raw
/// `CNTV_CTL_EL0` observation, including its time-sensitive ISTATUS bit, so raw
/// equality does not imply restore-equivalent timer configuration. This subset
/// does not include pending interrupts, GIC state, or a portable snapshot-time
/// adjustment policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuVirtualTimerState {
    masked: bool,
    offset: u64,
    control: u64,
    compare_value: u64,
}

impl HvfArm64VcpuVirtualTimerState {
    pub(crate) const fn new(masked: bool, offset: u64, control: u64, compare_value: u64) -> Self {
        Self {
            masked,
            offset,
            control,
            compare_value,
        }
    }

    /// Return whether Hypervisor.framework virtual-timer exits are masked.
    pub const fn masked(self) -> bool {
        self.masked
    }

    /// Return the raw Hypervisor.framework virtual-timer offset.
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Return the raw `CNTV_CTL_EL0` value captured from the guest timer.
    ///
    /// ENABLE and IMASK are writable control bits, while ISTATUS is derived
    /// from the timer condition and can change as the virtual count advances.
    pub const fn control(self) -> u64 {
        self.control
    }

    /// Return the raw `CNTV_CVAL_EL0` compare value.
    pub const fn compare_value(self) -> u64 {
        self.compare_value
    }
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
    pub const CNTV_CTL_EL0: Self = Self(crate::ffi::HV_SYS_REG_CNTV_CTL_EL0);
    pub const CNTV_CVAL_EL0: Self = Self(crate::ffi::HV_SYS_REG_CNTV_CVAL_EL0);
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

    pub(crate) fn dispatch_mmio_access(
        &mut self,
        access: HvfResolvedMmioAccess,
        dispatcher: &mut MmioDispatcher,
    ) -> Result<MmioDispatchOutcome, HvfMmioDispatchError> {
        crate::mmio::dispatch_mmio_access(access, dispatcher, self)
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

    pub(crate) fn get_vtimer_mask(&self) -> Result<bool, BackendError> {
        crate::ffi::get_vtimer_mask(self.handle()?.vcpu)
    }

    pub(crate) fn set_vtimer_mask(&mut self, masked: bool) -> Result<(), BackendError> {
        crate::ffi::set_vtimer_mask(self.handle()?.vcpu, masked)
    }

    pub(crate) fn get_vtimer_offset(&self) -> Result<u64, BackendError> {
        crate::ffi::get_vtimer_offset(self.handle()?.vcpu)
    }

    pub(crate) fn set_vtimer_offset(&mut self, offset: u64) -> Result<(), BackendError> {
        crate::ffi::set_vtimer_offset(self.handle()?.vcpu, offset)
    }

    pub(crate) fn set_gic_ppi_pending(
        &mut self,
        writer: &HvfGicPpiPendingWriter,
        intid: u32,
        pending: bool,
    ) -> Result<(), HvfGicError> {
        writer.set_pending(self.handle()?.vcpu, intid, pending)
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

impl HvfMmioRegisterAccess for HvfVcpuOwner {
    fn read_register(&mut self, register: HvfRegister) -> Result<u64, BackendError> {
        self.get_register(register)
    }

    fn write_register(&mut self, register: HvfRegister, value: u64) -> Result<(), BackendError> {
        self.set_register(register, value)
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

    /// Dispatch one resolved HVF MMIO access through runtime handlers and complete read data.
    pub fn dispatch_mmio_access(
        &mut self,
        access: HvfResolvedMmioAccess,
        dispatcher: &mut MmioDispatcher,
    ) -> Result<MmioDispatchOutcome, HvfMmioDispatchError> {
        self.owner.dispatch_mmio_access(access, dispatcher)
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

    /// Read whether HVF's ARM virtual timer exit is masked for this current-thread vCPU.
    pub fn get_vtimer_mask(&self) -> Result<bool, BackendError> {
        self.owner.get_vtimer_mask()
    }

    /// Set whether HVF should suppress ARM virtual timer activated exits for this vCPU.
    pub fn set_vtimer_mask(&mut self, masked: bool) -> Result<(), BackendError> {
        self.owner.set_vtimer_mask(masked)
    }

    /// Read the raw HVF virtual-timer offset for this current-thread vCPU.
    pub fn get_vtimer_offset(&self) -> Result<u64, BackendError> {
        self.owner.get_vtimer_offset()
    }

    /// Set the raw HVF virtual-timer offset for this current-thread vCPU.
    pub fn set_vtimer_offset(&mut self, offset: u64) -> Result<(), BackendError> {
        self.owner.set_vtimer_offset(offset)
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

pub(crate) fn capture_arm64_vcpu_general_register_state_with(
    mut get_register: impl FnMut(HvfRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuGeneralRegisterState, BackendError> {
    let mut general_purpose_registers = [0; 31];
    for (index, value) in (0_u8..31).zip(&mut general_purpose_registers) {
        let register = HvfRegister::general_purpose(index).ok_or(BackendError::InvalidState(
            "arm64 general register index is outside X0-X30",
        ))?;
        *value = get_register(register)?;
    }

    let pc = get_register(HvfRegister::PC)?;
    let cpsr = get_register(HvfRegister::CPSR)?;

    Ok(HvfArm64VcpuGeneralRegisterState {
        general_purpose_registers,
        pc,
        cpsr,
    })
}

#[cfg(test)]
pub(crate) fn capture_arm64_vcpu_virtual_timer_state_with(
    get_mask: impl FnOnce() -> Result<bool, BackendError>,
    get_offset: impl FnOnce() -> Result<u64, BackendError>,
    get_control: impl FnOnce() -> Result<u64, BackendError>,
    get_compare_value: impl FnOnce() -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuVirtualTimerState, BackendError> {
    let masked = get_mask()?;
    let offset = get_offset()?;
    let control = get_control()?;
    let compare_value = get_compare_value()?;

    Ok(HvfArm64VcpuVirtualTimerState::new(
        masked,
        offset,
        control,
        compare_value,
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::marker::PhantomData;
    use std::mem::ManuallyDrop;
    use std::ptr;
    use std::rc::Rc;

    use bangbang_runtime::BackendError;
    use bangbang_runtime::memory::GuestAddress;

    use super::{
        ARM64_LINUX_BOOT_CPSR, DESTROYED_VCPU_MESSAGE, HvfArm64BootRegisters, HvfRegister,
        HvfSystemRegister, HvfVcpu, HvfVcpuHandle, HvfVcpuOwner, NO_VCPU_EXIT_MESSAGE,
        capture_arm64_vcpu_general_register_state_with,
        capture_arm64_vcpu_virtual_timer_state_with, configure_arm64_boot_registers_with,
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
        assert_eq!(
            vcpu.get_vtimer_mask(),
            Err(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
        );
        assert_eq!(
            vcpu.set_vtimer_mask(false),
            Err(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
        );
        assert_eq!(
            vcpu.get_vtimer_offset(),
            Err(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
        );
        assert_eq!(
            vcpu.set_vtimer_offset(0),
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
    fn captures_arm64_general_register_state_in_architectural_order() {
        let mut reads = Vec::new();

        let state = capture_arm64_vcpu_general_register_state_with(|register| {
            reads.push(register);
            Ok(0x1000 + u64::from(register.raw()))
        })
        .expect("general-register capture should succeed");

        let expected_reads = (0_u8..31)
            .map(|index| {
                HvfRegister::general_purpose(index).expect("X0-X30 should map to registers")
            })
            .chain([HvfRegister::PC, HvfRegister::CPSR])
            .collect::<Vec<_>>();
        assert_eq!(reads, expected_reads);
        assert_eq!(state.general_purpose_registers().len(), 31);
        assert_eq!(state.general_purpose_register(0), Some(0x1000));
        assert_eq!(state.general_purpose_register(30), Some(0x101e));
        assert_eq!(state.general_purpose_register(31), None);
        assert_eq!(state.pc(), 0x1000 + u64::from(HvfRegister::PC.raw()));
        assert_eq!(state.cpsr(), 0x1000 + u64::from(HvfRegister::CPSR.raw()));
    }

    #[test]
    fn arm64_general_register_capture_stops_after_read_error_and_can_retry() {
        let fail_next_x2 = Cell::new(true);
        let reads = RefCell::new(Vec::new());
        let read_register = |register: HvfRegister| {
            reads.borrow_mut().push(register);
            if register == HvfRegister::X2 && fail_next_x2.replace(false) {
                Err(BackendError::InvalidState("fake register read failed"))
            } else {
                Ok(u64::from(register.raw()))
            }
        };

        assert_eq!(
            capture_arm64_vcpu_general_register_state_with(&read_register),
            Err(BackendError::InvalidState("fake register read failed"))
        );
        assert_eq!(
            *reads.borrow(),
            vec![HvfRegister::X0, HvfRegister::X1, HvfRegister::X2]
        );

        reads.borrow_mut().clear();
        let state = capture_arm64_vcpu_general_register_state_with(&read_register)
            .expect("general-register capture retry should succeed");
        assert_eq!(state.general_purpose_register(2), Some(2));
        assert_eq!(reads.borrow().len(), 33);
    }

    #[test]
    fn captures_arm64_virtual_timer_state_in_documented_order() {
        let reads = RefCell::new(Vec::new());

        let state = capture_arm64_vcpu_virtual_timer_state_with(
            || {
                reads.borrow_mut().push("mask");
                Ok(true)
            },
            || {
                reads.borrow_mut().push("offset");
                Ok(0x1234_5678_9abc_def0)
            },
            || {
                reads.borrow_mut().push("control");
                Ok(0b101)
            },
            || {
                reads.borrow_mut().push("compare");
                Ok(0xfedc_ba98_7654_3210)
            },
        )
        .expect("virtual-timer capture should succeed");

        assert_eq!(*reads.borrow(), ["mask", "offset", "control", "compare"]);
        assert!(state.masked());
        assert_eq!(state.offset(), 0x1234_5678_9abc_def0);
        assert_eq!(state.control(), 0b101);
        assert_eq!(state.compare_value(), 0xfedc_ba98_7654_3210);
    }

    #[test]
    fn arm64_virtual_timer_capture_returns_no_state_after_any_read_error() {
        let offset_called = Cell::new(false);
        assert_eq!(
            capture_arm64_vcpu_virtual_timer_state_with(
                || Err(BackendError::InvalidState("fake mask read failed")),
                || {
                    offset_called.set(true);
                    Ok(1)
                },
                || Ok(2),
                || Ok(3),
            ),
            Err(BackendError::InvalidState("fake mask read failed"))
        );
        assert!(!offset_called.get());

        let control_called = Cell::new(false);
        assert_eq!(
            capture_arm64_vcpu_virtual_timer_state_with(
                || Ok(false),
                || Err(BackendError::InvalidState("fake offset read failed")),
                || {
                    control_called.set(true);
                    Ok(2)
                },
                || Ok(3),
            ),
            Err(BackendError::InvalidState("fake offset read failed"))
        );
        assert!(!control_called.get());

        let compare_called = Cell::new(false);
        assert_eq!(
            capture_arm64_vcpu_virtual_timer_state_with(
                || Ok(false),
                || Ok(1),
                || Err(BackendError::InvalidState("fake control read failed")),
                || {
                    compare_called.set(true);
                    Ok(3)
                },
            ),
            Err(BackendError::InvalidState("fake control read failed"))
        );
        assert!(!compare_called.get());

        assert_eq!(
            capture_arm64_vcpu_virtual_timer_state_with(
                || Ok(false),
                || Ok(7),
                || Ok(3),
                || Err(BackendError::InvalidState("fake compare read failed")),
            ),
            Err(BackendError::InvalidState("fake compare read failed"))
        );
        assert_eq!(
            capture_arm64_vcpu_virtual_timer_state_with(
                || Ok(false),
                || Ok(7),
                || Ok(3),
                || Ok(11),
            ),
            Ok(super::HvfArm64VcpuVirtualTimerState {
                masked: false,
                offset: 7,
                control: 3,
                compare_value: 11,
            })
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
