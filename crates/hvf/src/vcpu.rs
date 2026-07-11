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

/// One ARM interrupt level exposed by Hypervisor.framework.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfInterruptType {
    /// Normal interrupt request.
    Irq,
    /// Fast interrupt request.
    Fiq,
}

impl HvfInterruptType {
    pub(crate) const fn raw(self) -> crate::ffi::HvInterruptType {
        match self {
            Self::Irq => crate::ffi::HV_INTERRUPT_TYPE_IRQ,
            Self::Fiq => crate::ffi::HV_INTERRUPT_TYPE_FIQ,
        }
    }
}

/// Detached CPU-level IRQ/FIQ pending state captured from one arm64 vCPU.
///
/// Hypervisor.framework clears these injection levels after a vCPU run
/// returns. This value is not GIC/device state or a serialized snapshot schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuPendingInterruptState {
    irq_pending: bool,
    fiq_pending: bool,
}

impl HvfArm64VcpuPendingInterruptState {
    pub(crate) const fn new(irq_pending: bool, fiq_pending: bool) -> Self {
        Self {
            irq_pending,
            fiq_pending,
        }
    }

    /// Return whether the CPU IRQ level was pending.
    pub const fn irq_pending(self) -> bool {
        self.irq_pending
    }

    /// Return whether the CPU FIQ level was pending.
    pub const fn fiq_pending(self) -> bool {
        self.fiq_pending
    }
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

/// Detached raw core system-register state captured from one arm64 vCPU.
///
/// This stack and exception-return subset contains `SP_EL0`, `SP_EL1`,
/// `ELR_EL1`, and `SPSR_EL1`. The values are unvalidated observations for later
/// owner-thread orchestration, not a complete or serialized restorable vCPU
/// state. The wider system-register, SIMD/FP, and interrupt inventories remain
/// outside this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuCoreSystemRegisterState {
    sp_el0: u64,
    sp_el1: u64,
    elr_el1: u64,
    spsr_el1: u64,
}

impl HvfArm64VcpuCoreSystemRegisterState {
    pub(crate) const fn new(sp_el0: u64, sp_el1: u64, elr_el1: u64, spsr_el1: u64) -> Self {
        Self {
            sp_el0,
            sp_el1,
            elr_el1,
            spsr_el1,
        }
    }

    /// Return the raw `SP_EL0` value.
    pub const fn sp_el0(self) -> u64 {
        self.sp_el0
    }

    /// Return the raw `SP_EL1` value.
    pub const fn sp_el1(self) -> u64 {
        self.sp_el1
    }

    /// Return the raw `ELR_EL1` value.
    pub const fn elr_el1(self) -> u64 {
        self.elr_el1
    }

    /// Return the raw `SPSR_EL1` value.
    pub const fn spsr_el1(self) -> u64 {
        self.spsr_el1
    }
}

/// Detached raw thread-context register state captured from one arm64 vCPU.
///
/// These software thread-ID values can contain guest TLS or kernel pointers.
/// They are sensitive raw observations for later owner-thread orchestration,
/// not a complete or serialized restorable vCPU state. `TPIDR2_EL0`, wider
/// system registers, and restore validation remain outside this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuThreadContextRegisterState {
    tpidr_el0: u64,
    tpidrro_el0: u64,
    tpidr_el1: u64,
}

impl HvfArm64VcpuThreadContextRegisterState {
    pub(crate) const fn new(tpidr_el0: u64, tpidrro_el0: u64, tpidr_el1: u64) -> Self {
        Self {
            tpidr_el0,
            tpidrro_el0,
            tpidr_el1,
        }
    }

    /// Return the raw `TPIDR_EL0` value.
    pub const fn tpidr_el0(self) -> u64 {
        self.tpidr_el0
    }

    /// Return the raw `TPIDRRO_EL0` value.
    pub const fn tpidrro_el0(self) -> u64 {
        self.tpidrro_el0
    }

    /// Return the raw `TPIDR_EL1` value.
    pub const fn tpidr_el1(self) -> u64 {
        self.tpidr_el1
    }
}

/// Detached raw baseline SIMD/floating-point state captured from one arm64 vCPU.
///
/// This value contains Q0-Q31, FPCR, and FPSR. Each Q register is preserved as
/// 16 uninterpreted bytes. In streaming SVE mode, Hypervisor.framework aliases
/// these Q values to the low 128 bits of the corresponding Z registers, so this
/// is not complete SVE/SME or restorable vCPU state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfArm64VcpuSimdFpState {
    q_registers: [[u8; 16]; 32],
    fpcr: u64,
    fpsr: u64,
}

impl HvfArm64VcpuSimdFpState {
    pub(crate) const fn new(q_registers: [[u8; 16]; 32], fpcr: u64, fpsr: u64) -> Self {
        Self {
            q_registers,
            fpcr,
            fpsr,
        }
    }

    /// Return all raw Q0-Q31 values in architectural order.
    pub const fn q_registers(&self) -> &[[u8; 16]; 32] {
        &self.q_registers
    }

    /// Return one raw Q-register value, or `None` when `index` is outside 0..=31.
    pub fn q_register(&self, index: usize) -> Option<[u8; 16]> {
        self.q_registers.get(index).copied()
    }

    /// Return the raw `FPCR` value.
    pub const fn fpcr(&self) -> u64 {
        self.fpcr
    }

    /// Return the raw `FPSR` value.
    pub const fn fpsr(&self) -> u64 {
        self.fpsr
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
    pub const FPCR: Self = Self(crate::ffi::HV_REG_FPCR);
    pub const FPSR: Self = Self(crate::ffi::HV_REG_FPSR);
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
pub struct HvfSimdFpRegister(u32);

impl HvfSimdFpRegister {
    /// Return the typed Q-register identifier for `index` in 0..=31.
    pub const fn q(index: u8) -> Option<Self> {
        if (index as crate::ffi::HvSimdFpReg) <= crate::ffi::HV_SIMD_FP_REG_Q31 {
            Some(Self(
                crate::ffi::HV_SIMD_FP_REG_Q0 + index as crate::ffi::HvSimdFpReg,
            ))
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
    pub const SP_EL0: Self = Self(crate::ffi::HV_SYS_REG_SP_EL0);
    pub const TPIDR_EL1: Self = Self(crate::ffi::HV_SYS_REG_TPIDR_EL1);
    pub const TPIDR_EL0: Self = Self(crate::ffi::HV_SYS_REG_TPIDR_EL0);
    pub const TPIDRRO_EL0: Self = Self(crate::ffi::HV_SYS_REG_TPIDRRO_EL0);
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

    pub(crate) fn get_simd_fp_register(
        &self,
        register: HvfSimdFpRegister,
    ) -> Result<[u8; 16], BackendError> {
        crate::ffi::get_simd_fp_reg(self.handle()?.vcpu, register.raw())
    }

    pub(crate) fn set_register(
        &mut self,
        register: HvfRegister,
        value: u64,
    ) -> Result<(), BackendError> {
        crate::ffi::set_reg(self.handle()?.vcpu, register.raw(), value)
    }

    pub(crate) fn get_pending_interrupt(
        &self,
        interrupt_type: HvfInterruptType,
    ) -> Result<bool, BackendError> {
        crate::ffi::get_pending_interrupt(self.handle()?.vcpu, interrupt_type.raw())
    }

    pub(crate) fn set_pending_interrupt(
        &mut self,
        interrupt_type: HvfInterruptType,
        pending: bool,
    ) -> Result<(), BackendError> {
        crate::ffi::set_pending_interrupt(self.handle()?.vcpu, interrupt_type.raw(), pending)
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

    /// Read one raw 128-bit Q-register value from this current-thread vCPU.
    pub fn get_simd_fp_register(
        &self,
        register: HvfSimdFpRegister,
    ) -> Result<[u8; 16], BackendError> {
        self.owner.get_simd_fp_register(register)
    }

    pub fn set_register(&mut self, register: HvfRegister, value: u64) -> Result<(), BackendError> {
        self.owner.set_register(register, value)
    }

    /// Read one CPU-level pending interrupt injection on this current-thread vCPU.
    pub fn get_pending_interrupt(
        &self,
        interrupt_type: HvfInterruptType,
    ) -> Result<bool, BackendError> {
        self.owner.get_pending_interrupt(interrupt_type)
    }

    /// Set one CPU-level pending interrupt injection on this current-thread vCPU.
    ///
    /// Hypervisor.framework clears this level after the next vCPU run returns.
    pub fn set_pending_interrupt(
        &mut self,
        interrupt_type: HvfInterruptType,
        pending: bool,
    ) -> Result<(), BackendError> {
        self.owner.set_pending_interrupt(interrupt_type, pending)
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

pub(crate) fn capture_arm64_vcpu_core_system_register_state_with(
    mut get_system_register: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuCoreSystemRegisterState, BackendError> {
    let sp_el0 = get_system_register(HvfSystemRegister::SP_EL0)?;
    let sp_el1 = get_system_register(HvfSystemRegister::SP_EL1)?;
    let elr_el1 = get_system_register(HvfSystemRegister::ELR_EL1)?;
    let spsr_el1 = get_system_register(HvfSystemRegister::SPSR_EL1)?;

    Ok(HvfArm64VcpuCoreSystemRegisterState::new(
        sp_el0, sp_el1, elr_el1, spsr_el1,
    ))
}

pub(crate) fn capture_arm64_vcpu_thread_context_register_state_with(
    mut get_system_register: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuThreadContextRegisterState, BackendError> {
    let tpidr_el0 = get_system_register(HvfSystemRegister::TPIDR_EL0)?;
    let tpidrro_el0 = get_system_register(HvfSystemRegister::TPIDRRO_EL0)?;
    let tpidr_el1 = get_system_register(HvfSystemRegister::TPIDR_EL1)?;

    Ok(HvfArm64VcpuThreadContextRegisterState::new(
        tpidr_el0,
        tpidrro_el0,
        tpidr_el1,
    ))
}

pub(crate) fn capture_arm64_vcpu_pending_interrupt_state_with(
    mut get_pending_interrupt: impl FnMut(HvfInterruptType) -> Result<bool, BackendError>,
) -> Result<HvfArm64VcpuPendingInterruptState, BackendError> {
    let irq_pending = get_pending_interrupt(HvfInterruptType::Irq)?;
    let fiq_pending = get_pending_interrupt(HvfInterruptType::Fiq)?;

    Ok(HvfArm64VcpuPendingInterruptState::new(
        irq_pending,
        fiq_pending,
    ))
}

pub(crate) fn capture_arm64_vcpu_simd_fp_state_with<R: ?Sized>(
    reader: &mut R,
    mut get_simd_fp_register: impl FnMut(&mut R, HvfSimdFpRegister) -> Result<[u8; 16], BackendError>,
    mut get_register: impl FnMut(&mut R, HvfRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuSimdFpState, BackendError> {
    let mut q_registers = [[0; 16]; 32];
    for (index, value) in (0_u8..32).zip(&mut q_registers) {
        let register =
            HvfSimdFpRegister(crate::ffi::HV_SIMD_FP_REG_Q0 + crate::ffi::HvSimdFpReg::from(index));
        *value = get_simd_fp_register(reader, register)?;
    }
    let fpcr = get_register(reader, HvfRegister::FPCR)?;
    let fpsr = get_register(reader, HvfRegister::FPSR)?;

    Ok(HvfArm64VcpuSimdFpState::new(q_registers, fpcr, fpsr))
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
        ARM64_LINUX_BOOT_CPSR, DESTROYED_VCPU_MESSAGE, HvfArm64BootRegisters, HvfInterruptType,
        HvfRegister, HvfSimdFpRegister, HvfSystemRegister, HvfVcpu, HvfVcpuHandle, HvfVcpuOwner,
        NO_VCPU_EXIT_MESSAGE, capture_arm64_vcpu_core_system_register_state_with,
        capture_arm64_vcpu_general_register_state_with,
        capture_arm64_vcpu_pending_interrupt_state_with, capture_arm64_vcpu_simd_fp_state_with,
        capture_arm64_vcpu_thread_context_register_state_with,
        capture_arm64_vcpu_virtual_timer_state_with, configure_arm64_boot_registers_with,
    };
    use crate::exit::{HvfExceptionExit, HvfVcpuExit};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SimdFpRead {
        Q(HvfSimdFpRegister),
        Scalar(HvfRegister),
    }

    fn simd_fp_q_value(register: HvfSimdFpRegister) -> [u8; 16] {
        std::array::from_fn(|index| (register.raw() as u8) ^ (index as u8).wrapping_mul(17))
    }

    fn expected_simd_fp_reads() -> Vec<SimdFpRead> {
        (0_u8..32)
            .map(|index| {
                SimdFpRead::Q(
                    HvfSimdFpRegister::q(index).expect("Q0-Q31 should map to SIMD registers"),
                )
            })
            .chain([
                SimdFpRead::Scalar(HvfRegister::FPCR),
                SimdFpRead::Scalar(HvfRegister::FPSR),
            ])
            .collect()
    }

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
            vcpu.get_pending_interrupt(HvfInterruptType::Irq),
            Err(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
        );
        assert_eq!(
            vcpu.set_pending_interrupt(HvfInterruptType::Fiq, true),
            Err(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
        );
        assert_eq!(
            vcpu.get_simd_fp_register(
                HvfSimdFpRegister::q(0).expect("Q0 should map to a SIMD register")
            ),
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
    fn captures_arm64_core_system_register_state_in_documented_order() {
        let mut reads = Vec::new();

        let state = capture_arm64_vcpu_core_system_register_state_with(|register| {
            reads.push(register);
            Ok(0x1_0000 + u64::from(register.raw()))
        })
        .expect("core system-register capture should succeed");

        assert_eq!(
            reads,
            [
                HvfSystemRegister::SP_EL0,
                HvfSystemRegister::SP_EL1,
                HvfSystemRegister::ELR_EL1,
                HvfSystemRegister::SPSR_EL1,
            ]
        );
        assert_eq!(
            state.sp_el0(),
            0x1_0000 + u64::from(HvfSystemRegister::SP_EL0.raw())
        );
        assert_eq!(
            state.sp_el1(),
            0x1_0000 + u64::from(HvfSystemRegister::SP_EL1.raw())
        );
        assert_eq!(
            state.elr_el1(),
            0x1_0000 + u64::from(HvfSystemRegister::ELR_EL1.raw())
        );
        assert_eq!(
            state.spsr_el1(),
            0x1_0000 + u64::from(HvfSystemRegister::SPSR_EL1.raw())
        );
    }

    #[test]
    fn captures_arm64_pending_interrupt_state_in_irq_then_fiq_order() {
        let mut reads = Vec::new();

        let state = capture_arm64_vcpu_pending_interrupt_state_with(|interrupt_type| {
            reads.push(interrupt_type);
            Ok(interrupt_type == HvfInterruptType::Irq)
        })
        .expect("pending-interrupt capture should succeed");

        assert_eq!(reads, [HvfInterruptType::Irq, HvfInterruptType::Fiq]);
        assert!(state.irq_pending());
        assert!(!state.fiq_pending());
        assert_eq!(
            HvfInterruptType::Irq.raw(),
            crate::ffi::HV_INTERRUPT_TYPE_IRQ
        );
        assert_eq!(
            HvfInterruptType::Fiq.raw(),
            crate::ffi::HV_INTERRUPT_TYPE_FIQ
        );
    }

    #[test]
    fn arm64_pending_interrupt_capture_stops_after_each_error_and_can_retry() {
        for failed_type in [HvfInterruptType::Irq, HvfInterruptType::Fiq] {
            let fail_next = Cell::new(true);
            let reads = RefCell::new(Vec::new());
            let get_pending_interrupt = |interrupt_type| {
                reads.borrow_mut().push(interrupt_type);
                if interrupt_type == failed_type && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake pending interrupt read failed",
                    ))
                } else {
                    Ok(interrupt_type == HvfInterruptType::Fiq)
                }
            };

            assert_eq!(
                capture_arm64_vcpu_pending_interrupt_state_with(&get_pending_interrupt),
                Err(BackendError::InvalidState(
                    "fake pending interrupt read failed"
                ))
            );
            let expected_reads = if failed_type == HvfInterruptType::Irq {
                vec![HvfInterruptType::Irq]
            } else {
                vec![HvfInterruptType::Irq, HvfInterruptType::Fiq]
            };
            assert_eq!(*reads.borrow(), expected_reads);

            reads.borrow_mut().clear();
            let state = capture_arm64_vcpu_pending_interrupt_state_with(&get_pending_interrupt)
                .expect("pending-interrupt capture retry should succeed");
            assert!(!state.irq_pending());
            assert!(state.fiq_pending());
            assert_eq!(
                *reads.borrow(),
                [HvfInterruptType::Irq, HvfInterruptType::Fiq]
            );
        }
    }

    #[test]
    fn arm64_core_system_register_capture_stops_after_each_error_and_can_retry() {
        let registers = [
            HvfSystemRegister::SP_EL0,
            HvfSystemRegister::SP_EL1,
            HvfSystemRegister::ELR_EL1,
            HvfSystemRegister::SPSR_EL1,
        ];

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let fail_next = Cell::new(true);
            let reads = RefCell::new(Vec::new());
            let read_system_register = |register: HvfSystemRegister| {
                reads.borrow_mut().push(register);
                if register == failed_register && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake system register read failed",
                    ))
                } else {
                    Ok(u64::from(register.raw()))
                }
            };

            assert_eq!(
                capture_arm64_vcpu_core_system_register_state_with(&read_system_register),
                Err(BackendError::InvalidState(
                    "fake system register read failed"
                ))
            );
            assert_eq!(*reads.borrow(), registers[..=failed_index]);

            reads.borrow_mut().clear();
            let state = capture_arm64_vcpu_core_system_register_state_with(&read_system_register)
                .expect("core system-register capture retry should succeed");
            assert_eq!(state.sp_el0(), u64::from(HvfSystemRegister::SP_EL0.raw()));
            assert_eq!(state.sp_el1(), u64::from(HvfSystemRegister::SP_EL1.raw()));
            assert_eq!(state.elr_el1(), u64::from(HvfSystemRegister::ELR_EL1.raw()));
            assert_eq!(
                state.spsr_el1(),
                u64::from(HvfSystemRegister::SPSR_EL1.raw())
            );
            assert_eq!(*reads.borrow(), registers);
        }
    }

    #[test]
    fn captures_arm64_thread_context_register_state_in_documented_order() {
        let mut reads = Vec::new();

        let state = capture_arm64_vcpu_thread_context_register_state_with(|register| {
            reads.push(register);
            Ok(0x5_0000 + u64::from(register.raw()))
        })
        .expect("thread-context register capture should succeed");

        assert_eq!(
            reads,
            [
                HvfSystemRegister::TPIDR_EL0,
                HvfSystemRegister::TPIDRRO_EL0,
                HvfSystemRegister::TPIDR_EL1,
            ]
        );
        assert_eq!(
            state.tpidr_el0(),
            0x5_0000 + u64::from(HvfSystemRegister::TPIDR_EL0.raw())
        );
        assert_eq!(
            state.tpidrro_el0(),
            0x5_0000 + u64::from(HvfSystemRegister::TPIDRRO_EL0.raw())
        );
        assert_eq!(
            state.tpidr_el1(),
            0x5_0000 + u64::from(HvfSystemRegister::TPIDR_EL1.raw())
        );
        assert_eq!(
            HvfSystemRegister::TPIDR_EL0.raw(),
            crate::ffi::HV_SYS_REG_TPIDR_EL0
        );
        assert_eq!(
            HvfSystemRegister::TPIDRRO_EL0.raw(),
            crate::ffi::HV_SYS_REG_TPIDRRO_EL0
        );
        assert_eq!(
            HvfSystemRegister::TPIDR_EL1.raw(),
            crate::ffi::HV_SYS_REG_TPIDR_EL1
        );
    }

    #[test]
    fn arm64_thread_context_register_capture_stops_after_each_error_and_can_retry() {
        let registers = [
            HvfSystemRegister::TPIDR_EL0,
            HvfSystemRegister::TPIDRRO_EL0,
            HvfSystemRegister::TPIDR_EL1,
        ];

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let fail_next = Cell::new(true);
            let reads = RefCell::new(Vec::new());
            let read_system_register = |register: HvfSystemRegister| {
                reads.borrow_mut().push(register);
                if register == failed_register && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake thread-context register read failed",
                    ))
                } else {
                    Ok(u64::from(register.raw()))
                }
            };

            assert_eq!(
                capture_arm64_vcpu_thread_context_register_state_with(&read_system_register),
                Err(BackendError::InvalidState(
                    "fake thread-context register read failed"
                ))
            );
            assert_eq!(*reads.borrow(), registers[..=failed_index]);

            reads.borrow_mut().clear();
            let state =
                capture_arm64_vcpu_thread_context_register_state_with(&read_system_register)
                    .expect("thread-context register capture retry should succeed");
            assert_eq!(
                state.tpidr_el0(),
                u64::from(HvfSystemRegister::TPIDR_EL0.raw())
            );
            assert_eq!(
                state.tpidrro_el0(),
                u64::from(HvfSystemRegister::TPIDRRO_EL0.raw())
            );
            assert_eq!(
                state.tpidr_el1(),
                u64::from(HvfSystemRegister::TPIDR_EL1.raw())
            );
            assert_eq!(*reads.borrow(), registers);
        }
    }

    #[test]
    fn captures_arm64_simd_fp_state_in_documented_order() {
        let reads = RefCell::new(Vec::new());
        let mut reader = ();

        let state = capture_arm64_vcpu_simd_fp_state_with(
            &mut reader,
            |_, register| {
                reads.borrow_mut().push(SimdFpRead::Q(register));
                Ok(simd_fp_q_value(register))
            },
            |_, register| {
                reads.borrow_mut().push(SimdFpRead::Scalar(register));
                Ok(0x3_0000 + u64::from(register.raw()))
            },
        )
        .expect("SIMD/FP capture should succeed");

        assert_eq!(*reads.borrow(), expected_simd_fp_reads());
        for index in 0_u8..32 {
            let register =
                HvfSimdFpRegister::q(index).expect("Q0-Q31 should map to SIMD registers");
            assert_eq!(
                state.q_register(usize::from(index)),
                Some(simd_fp_q_value(register))
            );
        }
        assert_eq!(state.q_registers().len(), 32);
        assert_eq!(state.q_register(32), None);
        assert_eq!(state.fpcr(), 0x3_0000 + u64::from(HvfRegister::FPCR.raw()));
        assert_eq!(state.fpsr(), 0x3_0000 + u64::from(HvfRegister::FPSR.raw()));
    }

    #[test]
    fn arm64_simd_fp_capture_stops_after_each_error_and_can_retry() {
        let expected_reads = expected_simd_fp_reads();

        for failed_index in 0..expected_reads.len() {
            let fail_next = Cell::new(true);
            let read_index = Cell::new(0);
            let reads = RefCell::new(Vec::new());
            let mut reader = ();
            let record_read = |read| {
                reads.borrow_mut().push(read);
                let index = read_index.get();
                read_index.set(index + 1);
                if index == failed_index && fail_next.replace(false) {
                    Err(BackendError::InvalidState("fake SIMD/FP read failed"))
                } else {
                    Ok(())
                }
            };

            assert_eq!(
                capture_arm64_vcpu_simd_fp_state_with(
                    &mut reader,
                    |_, register| {
                        record_read(SimdFpRead::Q(register))?;
                        Ok(simd_fp_q_value(register))
                    },
                    |_, register| {
                        record_read(SimdFpRead::Scalar(register))?;
                        Ok(u64::from(register.raw()))
                    },
                ),
                Err(BackendError::InvalidState("fake SIMD/FP read failed"))
            );
            assert_eq!(*reads.borrow(), expected_reads[..=failed_index]);

            reads.borrow_mut().clear();
            read_index.set(0);
            let state = capture_arm64_vcpu_simd_fp_state_with(
                &mut reader,
                |_, register| {
                    record_read(SimdFpRead::Q(register))?;
                    Ok(simd_fp_q_value(register))
                },
                |_, register| {
                    record_read(SimdFpRead::Scalar(register))?;
                    Ok(u64::from(register.raw()))
                },
            )
            .expect("SIMD/FP capture retry should succeed");
            assert_eq!(
                state.q_register(31),
                Some(simd_fp_q_value(
                    HvfSimdFpRegister::q(31).expect("Q31 should map to a SIMD register")
                ))
            );
            assert_eq!(state.fpcr(), u64::from(HvfRegister::FPCR.raw()));
            assert_eq!(state.fpsr(), u64::from(HvfRegister::FPSR.raw()));
            assert_eq!(*reads.borrow(), expected_reads);
        }
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

    #[test]
    fn simd_fp_register_mapping_accepts_only_q0_through_q31() {
        assert_eq!(
            HvfSimdFpRegister::q(0).map(HvfSimdFpRegister::raw),
            Some(crate::ffi::HV_SIMD_FP_REG_Q0)
        );
        assert_eq!(
            HvfSimdFpRegister::q(31).map(HvfSimdFpRegister::raw),
            Some(crate::ffi::HV_SIMD_FP_REG_Q31)
        );
        assert_eq!(HvfSimdFpRegister::q(32), None);
    }
}
