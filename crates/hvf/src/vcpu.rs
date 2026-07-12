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
const ARM64_SME_P_REGISTER_COUNT: usize = 16;
const ARM64_SME_Z_REGISTER_COUNT: usize = 32;

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
/// returns. The complete typed value can be reapplied through an owner-thread
/// primitive before a run, but the two writes are nontransactional. This value
/// is not GIC/device state, interrupt delivery policy, automatic per-run
/// reassertion, or a serialized snapshot schema.
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

/// Failure while restoring one CPU-level arm64 pending-interrupt field.
///
/// Hypervisor.framework writes IRQ and FIQ levels separately and provides no
/// batch transaction. Interrupt types before [`Self::failed_interrupt_type`]
/// have already been written when this error is returned. Callers must retry
/// the complete typed state or discard the vCPU before execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfArm64VcpuPendingInterruptRestoreError {
    failed_interrupt_type: HvfInterruptType,
    completed_writes: usize,
    source: BackendError,
}

impl HvfArm64VcpuPendingInterruptRestoreError {
    const fn new(
        failed_interrupt_type: HvfInterruptType,
        completed_writes: usize,
        source: BackendError,
    ) -> Self {
        Self {
            failed_interrupt_type,
            completed_writes,
            source,
        }
    }

    /// Return the pending-interrupt type whose setter failed.
    pub const fn failed_interrupt_type(&self) -> HvfInterruptType {
        self.failed_interrupt_type
    }

    /// Return the number of pending levels written before the failure.
    pub const fn completed_writes(&self) -> usize {
        self.completed_writes
    }
}

impl fmt::Display for HvfArm64VcpuPendingInterruptRestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let interrupt_name = match self.failed_interrupt_type {
            HvfInterruptType::Irq => "IRQ",
            HvfInterruptType::Fiq => "FIQ",
        };
        write!(
            f,
            "failed to restore arm64 {interrupt_name} pending interrupt after {} successful writes: {}",
            self.completed_writes, self.source
        )
    }
}

impl std::error::Error for HvfArm64VcpuPendingInterruptRestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Detached general-register state captured from one arm64 vCPU.
///
/// This is the first captured and owner-thread-restorable architectural subset
/// for later snapshot orchestration. It does not include system, SIMD/FP,
/// timer, interrupt, or device state and is not a serialized snapshot schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfArm64VcpuGeneralRegisterState {
    general_purpose_registers: [u64; 31],
    pc: u64,
    cpsr: u64,
}

impl HvfArm64VcpuGeneralRegisterState {
    pub(crate) const fn new(general_purpose_registers: [u64; 31], pc: u64, cpsr: u64) -> Self {
        Self {
            general_purpose_registers,
            pc,
            cpsr,
        }
    }

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

/// Failure while restoring one field of arm64 general-register state.
///
/// Hypervisor.framework writes one register at a time and provides no batch
/// transaction. Registers before [`Self::failed_register`] have already been
/// written when this error is returned. Callers must retry the complete state
/// or discard the vCPU before execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfArm64VcpuGeneralRegisterRestoreError {
    failed_register: HvfRegister,
    completed_writes: usize,
    source: BackendError,
}

impl HvfArm64VcpuGeneralRegisterRestoreError {
    const fn new(
        failed_register: HvfRegister,
        completed_writes: usize,
        source: BackendError,
    ) -> Self {
        Self {
            failed_register,
            completed_writes,
            source,
        }
    }

    /// Return the register whose setter failed.
    pub const fn failed_register(&self) -> HvfRegister {
        self.failed_register
    }

    /// Return the number of target registers written before the failure.
    pub const fn completed_writes(&self) -> usize {
        self.completed_writes
    }
}

impl fmt::Display for HvfArm64VcpuGeneralRegisterRestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to restore arm64 register id {} after {} successful writes: {}",
            self.failed_register.raw(),
            self.completed_writes,
            self.source
        )
    }
}

impl std::error::Error for HvfArm64VcpuGeneralRegisterRestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Failure while restoring one field of arm64 system-register state.
///
/// Hypervisor.framework writes one system register at a time and provides no
/// batch transaction. Registers before [`Self::failed_register`] have already
/// been written when this error is returned. Callers must retry the complete
/// typed state or discard the vCPU before execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfArm64VcpuSystemRegisterRestoreError {
    failed_register: HvfSystemRegister,
    completed_writes: usize,
    source: BackendError,
}

impl HvfArm64VcpuSystemRegisterRestoreError {
    const fn new(
        failed_register: HvfSystemRegister,
        completed_writes: usize,
        source: BackendError,
    ) -> Self {
        Self {
            failed_register,
            completed_writes,
            source,
        }
    }

    /// Return the system register whose setter failed.
    pub const fn failed_register(&self) -> HvfSystemRegister {
        self.failed_register
    }

    /// Return the number of target system registers written before failure.
    pub const fn completed_writes(&self) -> usize {
        self.completed_writes
    }
}

impl fmt::Display for HvfArm64VcpuSystemRegisterRestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to restore arm64 system register id {} after {} successful writes: {}",
            self.failed_register.raw(),
            self.completed_writes,
            self.source
        )
    }
}

impl std::error::Error for HvfArm64VcpuSystemRegisterRestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Register space containing a failed baseline arm64 SIMD/FP state write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64VcpuSimdFpRestoreRegister {
    /// One Q0-Q31 register written through Hypervisor.framework's SIMD/FP API.
    SimdFp(HvfSimdFpRegister),
    /// `FPCR` or `FPSR` written through Hypervisor.framework's scalar API.
    Scalar(HvfRegister),
}

/// Failure while restoring one field of baseline arm64 SIMD/FP state.
///
/// Hypervisor.framework writes one register at a time and provides no batch
/// transaction. Registers before [`Self::failed_register`] have already been
/// written when this error is returned. Callers must retry the complete typed
/// state or discard the vCPU before execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfArm64VcpuSimdFpRestoreError {
    failed_register: HvfArm64VcpuSimdFpRestoreRegister,
    completed_writes: usize,
    source: BackendError,
}

impl HvfArm64VcpuSimdFpRestoreError {
    const fn new(
        failed_register: HvfArm64VcpuSimdFpRestoreRegister,
        completed_writes: usize,
        source: BackendError,
    ) -> Self {
        Self {
            failed_register,
            completed_writes,
            source,
        }
    }

    /// Return the typed register whose setter failed.
    pub const fn failed_register(&self) -> HvfArm64VcpuSimdFpRestoreRegister {
        self.failed_register
    }

    /// Return the number of target registers written before the failure.
    pub const fn completed_writes(&self) -> usize {
        self.completed_writes
    }
}

impl fmt::Display for HvfArm64VcpuSimdFpRestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (register_space, register_id) = match self.failed_register {
            HvfArm64VcpuSimdFpRestoreRegister::SimdFp(register) => ("SIMD/FP", register.raw()),
            HvfArm64VcpuSimdFpRestoreRegister::Scalar(register) => ("scalar", register.raw()),
        };
        write!(
            f,
            "failed to restore arm64 {register_space} register id {register_id} after {} successful writes: {}",
            self.completed_writes, self.source
        )
    }
}

impl std::error::Error for HvfArm64VcpuSimdFpRestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Detached raw core system-register state captured from one arm64 vCPU.
///
/// This stack and exception-return subset contains `SP_EL0`, `SP_EL1`,
/// `ELR_EL1`, and `SPSR_EL1`. The values are unvalidated observations that can
/// be reapplied through the owner-thread runner primitive, not a complete or
/// serialized restorable vCPU state. The wider system-register, SIMD/FP, and
/// interrupt inventories remain outside this value.
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

/// Detached raw EL1 exception-register state captured from one arm64 vCPU.
///
/// This value contains both auxiliary fault status registers, the exception
/// syndrome, fault address, address-translation result, and exception vector
/// base. AFSR contents are implementation-defined, and the report fields are
/// not validated as one coherent exception. FAR, PAR, and VBAR can expose
/// sensitive guest addresses. The complete typed value can be reapplied by an
/// owner-thread runner primitive, but it still omits vector-table memory,
/// feature and semantic validation, persistence, schema, and wider restore
/// ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuExceptionRegisterState {
    afsr0_el1: u64,
    afsr1_el1: u64,
    esr_el1: u64,
    far_el1: u64,
    par_el1: u64,
    vbar_el1: u64,
}

impl HvfArm64VcpuExceptionRegisterState {
    pub(crate) const fn new(
        afsr0_el1: u64,
        afsr1_el1: u64,
        esr_el1: u64,
        far_el1: u64,
        par_el1: u64,
        vbar_el1: u64,
    ) -> Self {
        Self {
            afsr0_el1,
            afsr1_el1,
            esr_el1,
            far_el1,
            par_el1,
            vbar_el1,
        }
    }

    /// Return the raw `AFSR0_EL1` value.
    pub const fn afsr0_el1(self) -> u64 {
        self.afsr0_el1
    }

    /// Return the raw `AFSR1_EL1` value.
    pub const fn afsr1_el1(self) -> u64 {
        self.afsr1_el1
    }

    /// Return the raw `ESR_EL1` value.
    pub const fn esr_el1(self) -> u64 {
        self.esr_el1
    }

    /// Return the raw `FAR_EL1` value.
    pub const fn far_el1(self) -> u64 {
        self.far_el1
    }

    /// Return the raw `PAR_EL1` value.
    pub const fn par_el1(self) -> u64 {
        self.par_el1
    }

    /// Return the raw `VBAR_EL1` value.
    pub const fn vbar_el1(self) -> u64 {
        self.vbar_el1
    }
}

/// Detached raw EL1 execution-control state captured from one arm64 vCPU.
///
/// Hypervisor.framework exposes only `ACTLR_EL1.EnTSO`, and that register is
/// available on macOS 15 and newer. `CPACR_EL1` includes baseline FP/SIMD
/// access control plus optional architecture feature controls. The complete
/// typed value can be reapplied through an owner-thread primitive, but it is
/// not feature- or destination-validated and does not define writable-bit
/// policy, wider feature-state ordering, or guest-visible ISB transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuExecutionControlRegisterState {
    actlr_el1: u64,
    cpacr_el1: u64,
}

impl HvfArm64VcpuExecutionControlRegisterState {
    pub(crate) const fn new(actlr_el1: u64, cpacr_el1: u64) -> Self {
        Self {
            actlr_el1,
            cpacr_el1,
        }
    }

    /// Return the raw `ACTLR_EL1` value exposed by Hypervisor.framework.
    pub const fn actlr_el1(self) -> u64 {
        self.actlr_el1
    }

    /// Return the raw `CPACR_EL1` value.
    pub const fn cpacr_el1(self) -> u64 {
        self.cpacr_el1
    }
}

/// Detached raw EL1 cache-size selection state captured from one arm64 vCPU.
///
/// `CSSELR_EL1` selects the cache level and type observed by a subsequent
/// `CCSIDR_EL1` read; it is not cache topology itself. Reset and unsupported
/// selector encodings can be architecturally unknown. This complete typed value
/// can be reapplied through an owner-thread primitive, but does not validate the
/// selector, capture an atomic cache feature/size manifest, issue synchronization
/// or maintenance, or provide a portable restore policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuCacheSelectionRegisterState {
    csselr_el1: u64,
}

impl HvfArm64VcpuCacheSelectionRegisterState {
    pub(crate) const fn new(csselr_el1: u64) -> Self {
        Self { csselr_el1 }
    }

    /// Return the raw `CSSELR_EL1` value.
    pub const fn csselr_el1(self) -> u64 {
        self.csselr_el1
    }
}

/// Detached raw EL1 hardware-breakpoint state captured from one arm64 vCPU.
///
/// The implemented count is derived from `ID_AA64DFR0_EL1.BRPs`, and only
/// that many `DBGBVR<n>_EL1` / `DBGBCR<n>_EL1` pairs are exposed. Breakpoint
/// values can contain guest virtual addresses, Context IDs, or VMIDs, and the
/// controls can enable sensitive debug behavior. Treat this observation as
/// confidential guest state. It is not feature-validated, serialized, or safe
/// to restore, and capture does not write the registers, enable debugging, or
/// change Hypervisor.framework debug-register trap policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuBreakpointRegisterState {
    implemented_breakpoint_count: u8,
    breakpoint_value_registers: [u64; 16],
    breakpoint_control_registers: [u64; 16],
}

impl HvfArm64VcpuBreakpointRegisterState {
    pub(crate) const fn new(
        implemented_breakpoint_count: u8,
        breakpoint_value_registers: [u64; 16],
        breakpoint_control_registers: [u64; 16],
    ) -> Self {
        Self {
            implemented_breakpoint_count,
            breakpoint_value_registers,
            breakpoint_control_registers,
        }
    }

    /// Return the number of implemented hardware-breakpoint register pairs.
    pub const fn implemented_breakpoint_count(&self) -> u8 {
        self.implemented_breakpoint_count
    }

    /// Return the implemented `DBGBVR<n>_EL1` values in ascending slot order.
    pub fn breakpoint_value_registers(&self) -> &[u64] {
        self.breakpoint_value_registers
            .get(..usize::from(self.implemented_breakpoint_count))
            .unwrap_or_default()
    }

    /// Return the implemented `DBGBCR<n>_EL1` values in ascending slot order.
    pub fn breakpoint_control_registers(&self) -> &[u64] {
        self.breakpoint_control_registers
            .get(..usize::from(self.implemented_breakpoint_count))
            .unwrap_or_default()
    }

    /// Return one raw `DBGBVR<n>_EL1` value when `index` is implemented.
    pub fn breakpoint_value_register(&self, index: u8) -> Option<u64> {
        self.breakpoint_value_registers()
            .get(usize::from(index))
            .copied()
    }

    /// Return one raw `DBGBCR<n>_EL1` value when `index` is implemented.
    pub fn breakpoint_control_register(&self, index: u8) -> Option<u64> {
        self.breakpoint_control_registers()
            .get(usize::from(index))
            .copied()
    }
}

/// Detached raw EL1 hardware-watchpoint state captured from one arm64 vCPU.
///
/// The implemented count is derived from `ID_AA64DFR0_EL1.WRPs`, and only
/// that many `DBGWVR<n>_EL1` / `DBGWCR<n>_EL1` pairs are exposed. Watchpoint
/// values can contain guest data virtual addresses, and the controls can
/// describe sensitive debug matching and enablement. Treat this observation
/// as confidential guest state. It is not feature-validated, serialized, or
/// safe to restore, and capture does not write the registers, enable debugging,
/// or change Hypervisor.framework debug-register trap policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuWatchpointRegisterState {
    implemented_watchpoint_count: u8,
    watchpoint_value_registers: [u64; 16],
    watchpoint_control_registers: [u64; 16],
}

impl HvfArm64VcpuWatchpointRegisterState {
    pub(crate) const fn new(
        implemented_watchpoint_count: u8,
        watchpoint_value_registers: [u64; 16],
        watchpoint_control_registers: [u64; 16],
    ) -> Self {
        Self {
            implemented_watchpoint_count,
            watchpoint_value_registers,
            watchpoint_control_registers,
        }
    }

    /// Return the number of implemented hardware-watchpoint register pairs.
    pub const fn implemented_watchpoint_count(&self) -> u8 {
        self.implemented_watchpoint_count
    }

    /// Return the implemented `DBGWVR<n>_EL1` values in ascending slot order.
    pub fn watchpoint_value_registers(&self) -> &[u64] {
        self.watchpoint_value_registers
            .get(..usize::from(self.implemented_watchpoint_count))
            .unwrap_or_default()
    }

    /// Return the implemented `DBGWCR<n>_EL1` values in ascending slot order.
    pub fn watchpoint_control_registers(&self) -> &[u64] {
        self.watchpoint_control_registers
            .get(..usize::from(self.implemented_watchpoint_count))
            .unwrap_or_default()
    }

    /// Return one raw `DBGWVR<n>_EL1` value when `index` is implemented.
    pub fn watchpoint_value_register(&self, index: u8) -> Option<u64> {
        self.watchpoint_value_registers()
            .get(usize::from(index))
            .copied()
    }

    /// Return one raw `DBGWCR<n>_EL1` value when `index` is implemented.
    pub fn watchpoint_control_register(&self, index: u8) -> Option<u64> {
        self.watchpoint_control_registers()
            .get(usize::from(index))
            .copied()
    }
}

/// Detached raw EL1 debug-control state captured from one arm64 vCPU.
///
/// `MDCCINT_EL1` and `MDSCR_EL1` control security-sensitive self-hosted debug
/// behavior. The complete typed value can be reapplied through an owner-thread
/// primitive, but the two writes are nontransactional. This value does not
/// include the separately captured breakpoint/watchpoint comparators or
/// Hypervisor.framework debug-trap policy and defines no feature/writable-bit
/// or destination validation, wider debug ordering, persistence, snapshot
/// schema, or safe complete restore policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuDebugControlRegisterState {
    mdccint_el1: u64,
    mdscr_el1: u64,
}

impl HvfArm64VcpuDebugControlRegisterState {
    pub(crate) const fn new(mdccint_el1: u64, mdscr_el1: u64) -> Self {
        Self {
            mdccint_el1,
            mdscr_el1,
        }
    }

    /// Return the raw `MDCCINT_EL1` value.
    pub const fn mdccint_el1(self) -> u64 {
        self.mdccint_el1
    }

    /// Return the raw `MDSCR_EL1` value.
    pub const fn mdscr_el1(self) -> u64 {
        self.mdscr_el1
    }
}

/// Detached Hypervisor.framework debug-trap policy captured from one arm64 vCPU.
///
/// These booleans mirror host `MDCR_EL2.TDE`- and `MDCR_EL2.TDA`-equivalent
/// policy, not guest EL1 debug-register contents. The complete typed value can
/// be reapplied through an owner-thread primitive, but the two writes are
/// nontransactional. This value does not define feature or destination policy,
/// wider debug-state ordering, persistence, a snapshot schema, or public
/// snapshot-load behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuDebugTrapState {
    trap_debug_exceptions: bool,
    trap_debug_reg_accesses: bool,
}

impl HvfArm64VcpuDebugTrapState {
    pub(crate) const fn new(trap_debug_exceptions: bool, trap_debug_reg_accesses: bool) -> Self {
        Self {
            trap_debug_exceptions,
            trap_debug_reg_accesses,
        }
    }

    /// Return whether guest debug exceptions trap to the host.
    pub const fn trap_debug_exceptions(self) -> bool {
        self.trap_debug_exceptions
    }

    /// Return whether guest debug-register accesses trap to the host.
    pub const fn trap_debug_reg_accesses(self) -> bool {
        self.trap_debug_reg_accesses
    }
}

/// One host debug-trap policy operation in arm64 restore order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfArm64VcpuDebugTrapRestoreOperation {
    /// Set whether guest debug exceptions trap to the host.
    DebugExceptions,
    /// Set whether guest debug-register accesses trap to the host.
    DebugRegisterAccesses,
}

/// Failure while restoring one arm64 host debug-trap policy field.
///
/// Hypervisor.framework writes the two fields separately and provides no batch
/// transaction. Operations before [`Self::failed_operation`] have already been
/// applied when this error is returned. Callers must retry the complete typed
/// state or discard the vCPU before execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfArm64VcpuDebugTrapRestoreError {
    failed_operation: HvfArm64VcpuDebugTrapRestoreOperation,
    completed_writes: usize,
    source: BackendError,
}

impl HvfArm64VcpuDebugTrapRestoreError {
    const fn new(
        failed_operation: HvfArm64VcpuDebugTrapRestoreOperation,
        completed_writes: usize,
        source: BackendError,
    ) -> Self {
        Self {
            failed_operation,
            completed_writes,
            source,
        }
    }

    /// Return the debug-trap policy operation whose setter failed.
    pub const fn failed_operation(&self) -> HvfArm64VcpuDebugTrapRestoreOperation {
        self.failed_operation
    }

    /// Return the number of policy fields written before the failure.
    pub const fn completed_writes(&self) -> usize {
        self.completed_writes
    }
}

impl fmt::Display for HvfArm64VcpuDebugTrapRestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let operation_name = match self.failed_operation {
            HvfArm64VcpuDebugTrapRestoreOperation::DebugExceptions => "debug-exception trap policy",
            HvfArm64VcpuDebugTrapRestoreOperation::DebugRegisterAccesses => {
                "debug-register-access trap policy"
            }
        };
        write!(
            f,
            "failed to restore arm64 {operation_name} after {} successful writes: {}",
            self.completed_writes, self.source
        )
    }
}

impl std::error::Error for HvfArm64VcpuDebugTrapRestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Detached arm64 processor identification state captured from one vCPU.
///
/// These guest-visible MIDR, MPIDR, and baseline `ID_AA64*` values describe
/// the virtual CPU and Hypervisor.framework feature model. They are raw inputs
/// for later compatibility checks, not physical-host identity, mutable guest
/// state, or a destination compatibility decision. Optional SVE/SME
/// identification metadata is captured separately; newer identification
/// registers, persistence, and a serialized schema remain outside this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuIdentificationRegisterState {
    midr_el1: u64,
    mpidr_el1: u64,
    id_aa64pfr0_el1: u64,
    id_aa64pfr1_el1: u64,
    id_aa64dfr0_el1: u64,
    id_aa64dfr1_el1: u64,
    id_aa64isar0_el1: u64,
    id_aa64isar1_el1: u64,
    id_aa64mmfr0_el1: u64,
    id_aa64mmfr1_el1: u64,
    id_aa64mmfr2_el1: u64,
}

impl HvfArm64VcpuIdentificationRegisterState {
    pub(crate) const fn new(values: [u64; 11]) -> Self {
        let [
            midr_el1,
            mpidr_el1,
            id_aa64pfr0_el1,
            id_aa64pfr1_el1,
            id_aa64dfr0_el1,
            id_aa64dfr1_el1,
            id_aa64isar0_el1,
            id_aa64isar1_el1,
            id_aa64mmfr0_el1,
            id_aa64mmfr1_el1,
            id_aa64mmfr2_el1,
        ] = values;
        Self {
            midr_el1,
            mpidr_el1,
            id_aa64pfr0_el1,
            id_aa64pfr1_el1,
            id_aa64dfr0_el1,
            id_aa64dfr1_el1,
            id_aa64isar0_el1,
            id_aa64isar1_el1,
            id_aa64mmfr0_el1,
            id_aa64mmfr1_el1,
            id_aa64mmfr2_el1,
        }
    }

    /// Return the raw guest-visible `MIDR_EL1` value.
    pub const fn midr_el1(self) -> u64 {
        self.midr_el1
    }

    /// Return the raw guest-visible `MPIDR_EL1` value.
    pub const fn mpidr_el1(self) -> u64 {
        self.mpidr_el1
    }

    /// Return the raw `ID_AA64PFR0_EL1` value.
    pub const fn id_aa64pfr0_el1(self) -> u64 {
        self.id_aa64pfr0_el1
    }

    /// Return the raw `ID_AA64PFR1_EL1` value.
    pub const fn id_aa64pfr1_el1(self) -> u64 {
        self.id_aa64pfr1_el1
    }

    /// Return the raw `ID_AA64DFR0_EL1` value.
    pub const fn id_aa64dfr0_el1(self) -> u64 {
        self.id_aa64dfr0_el1
    }

    /// Return the raw `ID_AA64DFR1_EL1` value.
    pub const fn id_aa64dfr1_el1(self) -> u64 {
        self.id_aa64dfr1_el1
    }

    /// Return the raw `ID_AA64ISAR0_EL1` value.
    pub const fn id_aa64isar0_el1(self) -> u64 {
        self.id_aa64isar0_el1
    }

    /// Return the raw `ID_AA64ISAR1_EL1` value.
    pub const fn id_aa64isar1_el1(self) -> u64 {
        self.id_aa64isar1_el1
    }

    /// Return the raw `ID_AA64MMFR0_EL1` value.
    pub const fn id_aa64mmfr0_el1(self) -> u64 {
        self.id_aa64mmfr0_el1
    }

    /// Return the raw `ID_AA64MMFR1_EL1` value.
    pub const fn id_aa64mmfr1_el1(self) -> u64 {
        self.id_aa64mmfr1_el1
    }

    /// Return the raw `ID_AA64MMFR2_EL1` value.
    pub const fn id_aa64mmfr2_el1(self) -> u64 {
        self.id_aa64mmfr2_el1
    }
}

/// Detached optional SVE/SME identification metadata captured from one arm64 vCPU.
///
/// Hypervisor.framework exposes these guest-visible `ID_AA64ZFR0_EL1` and
/// `ID_AA64SMFR0_EL1` values on macOS 15.2 and newer. They describe the virtual
/// CPU feature model and are raw inputs for later compatibility checks, not
/// mutable guest execution state, complete SVE/SME state, or a destination
/// compatibility decision. Feature masks, persistence, snapshot schema, and
/// restore behavior remain outside this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuSveSmeIdentificationRegisterState {
    id_aa64zfr0_el1: u64,
    id_aa64smfr0_el1: u64,
}

impl HvfArm64VcpuSveSmeIdentificationRegisterState {
    pub(crate) const fn new(id_aa64zfr0_el1: u64, id_aa64smfr0_el1: u64) -> Self {
        Self {
            id_aa64zfr0_el1,
            id_aa64smfr0_el1,
        }
    }

    /// Return the raw `ID_AA64ZFR0_EL1` value.
    pub const fn id_aa64zfr0_el1(self) -> u64 {
        self.id_aa64zfr0_el1
    }

    /// Return the raw `ID_AA64SMFR0_EL1` value.
    pub const fn id_aa64smfr0_el1(self) -> u64 {
        self.id_aa64smfr0_el1
    }
}

/// Detached SME PSTATE captured from one arm64 vCPU.
///
/// Hypervisor.framework exposes these `PSTATE.SM` streaming-SVE-mode and
/// `PSTATE.ZA` storage-enable flags on macOS 15.2 and newer when SME is
/// supported. They are mutable guest execution controls, not SVE/SME
/// identification metadata or the conditionally present Z, P, ZA, and ZT0
/// register contents. In streaming mode, baseline Q registers alias the low
/// 128 bits of Z registers. This getter-only value defines no transition,
/// persistence, snapshot-schema, or restore-ordering policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuSmePstate {
    streaming_sve_mode_enabled: bool,
    za_storage_enabled: bool,
}

impl HvfArm64VcpuSmePstate {
    pub(crate) const fn new(streaming_sve_mode_enabled: bool, za_storage_enabled: bool) -> Self {
        Self {
            streaming_sve_mode_enabled,
            za_storage_enabled,
        }
    }

    /// Return whether streaming SVE mode (`PSTATE.SM`) is enabled.
    pub const fn streaming_sve_mode_enabled(self) -> bool {
        self.streaming_sve_mode_enabled
    }

    /// Return whether ZA storage (`PSTATE.ZA`) is enabled.
    pub const fn za_storage_enabled(self) -> bool {
        self.za_storage_enabled
    }
}

/// Error while capturing streaming SVE P-register contents from one arm64 vCPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfArm64VcpuSmePRegisterCaptureError {
    /// Hypervisor.framework or compile-target failure.
    Backend(BackendError),
    /// `PSTATE.SM` was disabled, so the SDK forbids P-register reads.
    StreamingSveModeDisabled,
    /// Hypervisor.framework reported a zero maximum streaming vector length.
    ZeroMaximumSvl,
    /// Maximum SVL could not produce an exact one-eighth predicate width.
    MaximumSvlNotDivisibleByEight {
        /// Maximum streaming vector length returned by Hypervisor.framework.
        maximum_svl_bytes: usize,
    },
    /// The complete 16-register byte count overflowed `usize`.
    CaptureSizeOverflow {
        /// Maximum streaming vector length returned by Hypervisor.framework.
        maximum_svl_bytes: usize,
    },
    /// The complete private capture buffer could not be allocated.
    AllocationFailed {
        /// Requested allocation size in bytes.
        size: usize,
    },
}

impl fmt::Display for HvfArm64VcpuSmePRegisterCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(source) => write!(f, "{source}"),
            Self::StreamingSveModeDisabled => {
                f.write_str("cannot capture SME P registers while streaming SVE mode is disabled")
            }
            Self::ZeroMaximumSvl => {
                f.write_str("Hypervisor.framework reported a zero maximum streaming vector length")
            }
            Self::MaximumSvlNotDivisibleByEight { maximum_svl_bytes } => write!(
                f,
                "maximum SVL {maximum_svl_bytes} bytes is not divisible by 8 for SME P-register capture"
            ),
            Self::CaptureSizeOverflow { maximum_svl_bytes } => write!(
                f,
                "SME P-register capture size overflows for maximum SVL {maximum_svl_bytes} bytes"
            ),
            Self::AllocationFailed { size } => {
                write!(
                    f,
                    "failed to allocate {size} bytes for SME P-register capture"
                )
            }
        }
    }
}

impl std::error::Error for HvfArm64VcpuSmePRegisterCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(source) => Some(source),
            Self::StreamingSveModeDisabled
            | Self::ZeroMaximumSvl
            | Self::MaximumSvlNotDivisibleByEight { .. }
            | Self::CaptureSizeOverflow { .. }
            | Self::AllocationFailed { .. } => None,
        }
    }
}

impl From<BackendError> for HvfArm64VcpuSmePRegisterCaptureError {
    fn from(source: BackendError) -> Self {
        Self::Backend(source)
    }
}

/// Detached streaming SVE P0-P15 contents captured from one arm64 vCPU.
///
/// Every register contains exactly one eighth of the maximum streaming vector
/// length in bytes reported by Hypervisor.framework. That allocation width is
/// not an effective-SVL interpretation. These predicate bytes are sensitive
/// guest execution state, so `Debug` redacts them. Z registers, ZA, ZT0,
/// setters, persistence, encryption, snapshot schema, and restore ordering
/// remain outside this getter-only value.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfArm64VcpuSmePRegisterState {
    maximum_svl_bytes: usize,
    bytes: Box<[u8]>,
}

impl HvfArm64VcpuSmePRegisterState {
    /// Number of architectural P registers captured by this value.
    pub const REGISTER_COUNT: usize = ARM64_SME_P_REGISTER_COUNT;

    fn new(maximum_svl_bytes: usize, bytes: Vec<u8>) -> Self {
        debug_assert_eq!(
            bytes.len(),
            Self::REGISTER_COUNT * (maximum_svl_bytes / 8),
            "SME P-register capture buffer must preserve every register"
        );
        Self {
            maximum_svl_bytes,
            bytes: bytes.into_boxed_slice(),
        }
    }

    /// Return the maximum streaming vector length reported by HVF.
    pub const fn maximum_svl_bytes(&self) -> usize {
        self.maximum_svl_bytes
    }

    /// Return the exact per-register predicate allocation width.
    pub const fn predicate_width_bytes(&self) -> usize {
        self.maximum_svl_bytes / 8
    }

    /// Return one maximum-width raw P-register byte slice.
    pub fn p_register(&self, index: usize) -> Option<&[u8]> {
        if index >= Self::REGISTER_COUNT {
            return None;
        }
        let width = self.predicate_width_bytes();
        let start = index.checked_mul(width)?;
        let end = start.checked_add(width)?;
        self.bytes.get(start..end)
    }
}

impl fmt::Debug for HvfArm64VcpuSmePRegisterState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64VcpuSmePRegisterState")
            .field("registers", &"<redacted>")
            .finish()
    }
}

/// Error while capturing streaming SVE Z-register contents from one arm64 vCPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfArm64VcpuSmeZRegisterCaptureError {
    /// Hypervisor.framework or compile-target failure.
    Backend(BackendError),
    /// `PSTATE.SM` was disabled, so the SDK forbids Z-register reads.
    StreamingSveModeDisabled,
    /// Hypervisor.framework reported a zero maximum streaming vector length.
    ZeroMaximumSvl,
    /// The complete 32-register byte count overflowed `usize`.
    CaptureSizeOverflow {
        /// Maximum streaming vector length returned by Hypervisor.framework.
        maximum_svl_bytes: usize,
    },
    /// The complete private capture buffer could not be allocated.
    AllocationFailed {
        /// Requested allocation size in bytes.
        size: usize,
    },
}

impl fmt::Display for HvfArm64VcpuSmeZRegisterCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(source) => write!(f, "{source}"),
            Self::StreamingSveModeDisabled => {
                f.write_str("cannot capture SME Z registers while streaming SVE mode is disabled")
            }
            Self::ZeroMaximumSvl => {
                f.write_str("Hypervisor.framework reported a zero maximum streaming vector length")
            }
            Self::CaptureSizeOverflow { maximum_svl_bytes } => write!(
                f,
                "SME Z-register capture size overflows for maximum SVL {maximum_svl_bytes} bytes"
            ),
            Self::AllocationFailed { size } => {
                write!(
                    f,
                    "failed to allocate {size} bytes for SME Z-register capture"
                )
            }
        }
    }
}

impl std::error::Error for HvfArm64VcpuSmeZRegisterCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(source) => Some(source),
            Self::StreamingSveModeDisabled
            | Self::ZeroMaximumSvl
            | Self::CaptureSizeOverflow { .. }
            | Self::AllocationFailed { .. } => None,
        }
    }
}

impl From<BackendError> for HvfArm64VcpuSmeZRegisterCaptureError {
    fn from(source: BackendError) -> Self {
        Self::Backend(source)
    }
}

/// Detached streaming SVE Z0-Z31 contents captured from one arm64 vCPU.
///
/// Every register contains exactly the maximum streaming vector length in
/// bytes reported by Hypervisor.framework. That allocation width is not an
/// effective-SVL interpretation. These bytes are sensitive guest execution and
/// potentially cryptographic state, so `Debug` redacts them. Baseline Q
/// registers alias the low 128 bits while streaming mode is active. P
/// predicates, ZA, ZT0, setters, persistence, encryption, snapshot schema, and
/// restore ordering remain outside this getter-only value.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfArm64VcpuSmeZRegisterState {
    maximum_svl_bytes: usize,
    bytes: Box<[u8]>,
}

impl HvfArm64VcpuSmeZRegisterState {
    /// Number of architectural Z registers captured by this value.
    pub const REGISTER_COUNT: usize = ARM64_SME_Z_REGISTER_COUNT;

    fn new(maximum_svl_bytes: usize, bytes: Vec<u8>) -> Self {
        debug_assert_eq!(
            bytes.len(),
            Self::REGISTER_COUNT * maximum_svl_bytes,
            "SME Z-register capture buffer must preserve every register"
        );
        Self {
            maximum_svl_bytes,
            bytes: bytes.into_boxed_slice(),
        }
    }

    /// Return the exact per-register allocation width reported by HVF.
    pub const fn maximum_svl_bytes(&self) -> usize {
        self.maximum_svl_bytes
    }

    /// Return one maximum-width raw Z-register byte slice.
    pub fn z_register(&self, index: usize) -> Option<&[u8]> {
        if index >= Self::REGISTER_COUNT {
            return None;
        }
        let start = index.checked_mul(self.maximum_svl_bytes)?;
        let end = start.checked_add(self.maximum_svl_bytes)?;
        self.bytes.get(start..end)
    }
}

impl fmt::Debug for HvfArm64VcpuSmeZRegisterState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64VcpuSmeZRegisterState")
            .field("registers", &"<redacted>")
            .finish()
    }
}

/// Error while capturing the SME ZA matrix contents from one arm64 vCPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfArm64VcpuSmeZaRegisterCaptureError {
    /// Hypervisor.framework or compile-target failure.
    Backend(BackendError),
    /// `PSTATE.ZA` was disabled, so the SDK forbids a ZA-register read.
    ZaStorageDisabled,
    /// Hypervisor.framework reported a zero maximum streaming vector length.
    ZeroMaximumSvl,
    /// The maximum-SVL square byte count overflowed `usize`.
    CaptureSizeOverflow {
        /// Maximum streaming vector length returned by Hypervisor.framework.
        maximum_svl_bytes: usize,
    },
    /// The complete private capture buffer could not be allocated.
    AllocationFailed {
        /// Requested allocation size in bytes.
        size: usize,
    },
}

impl fmt::Display for HvfArm64VcpuSmeZaRegisterCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(source) => write!(f, "{source}"),
            Self::ZaStorageDisabled => {
                f.write_str("cannot capture the SME ZA register while ZA storage is disabled")
            }
            Self::ZeroMaximumSvl => {
                f.write_str("Hypervisor.framework reported a zero maximum streaming vector length")
            }
            Self::CaptureSizeOverflow { maximum_svl_bytes } => write!(
                f,
                "SME ZA-register capture size overflows for maximum SVL {maximum_svl_bytes} bytes"
            ),
            Self::AllocationFailed { size } => {
                write!(
                    f,
                    "failed to allocate {size} bytes for SME ZA-register capture"
                )
            }
        }
    }
}

impl std::error::Error for HvfArm64VcpuSmeZaRegisterCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(source) => Some(source),
            Self::ZaStorageDisabled
            | Self::ZeroMaximumSvl
            | Self::CaptureSizeOverflow { .. }
            | Self::AllocationFailed { .. } => None,
        }
    }
}

impl From<BackendError> for HvfArm64VcpuSmeZaRegisterCaptureError {
    fn from(source: BackendError) -> Self {
        Self::Backend(source)
    }
}

/// Detached raw SME ZA matrix contents captured from one arm64 vCPU.
///
/// Hypervisor.framework requires exactly the square of its maximum streaming
/// vector length in bytes. That allocation size is not an effective-SVL, row,
/// tile, or active-lane interpretation. ZA bytes are sensitive guest execution
/// and potentially cryptographic state, so `Debug` redacts them. Z, P, ZT0,
/// setters, persistence, encryption, snapshot schema, and restore ordering
/// remain outside this getter-only value.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfArm64VcpuSmeZaRegisterState {
    maximum_svl_bytes: usize,
    bytes: Box<[u8]>,
}

impl HvfArm64VcpuSmeZaRegisterState {
    fn new(maximum_svl_bytes: usize, bytes: Vec<u8>) -> Self {
        debug_assert_eq!(
            bytes.len(),
            maximum_svl_bytes * maximum_svl_bytes,
            "SME ZA-register capture buffer must preserve the complete matrix"
        );
        Self {
            maximum_svl_bytes,
            bytes: bytes.into_boxed_slice(),
        }
    }

    /// Return the maximum streaming vector length reported by HVF.
    pub const fn maximum_svl_bytes(&self) -> usize {
        self.maximum_svl_bytes
    }

    /// Return the complete raw ZA-register bytes without layout interpretation.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Return the complete ZA-register capture size in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Return whether the complete ZA-register capture contains no bytes.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl fmt::Debug for HvfArm64VcpuSmeZaRegisterState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64VcpuSmeZaRegisterState")
            .field("register", &"<redacted>")
            .finish()
    }
}

/// Error while capturing the SME2 ZT0 register from one arm64 vCPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfArm64VcpuSmeZt0RegisterCaptureError {
    /// Hypervisor.framework or compile-target failure.
    Backend(BackendError),
    /// `PSTATE.ZA` was disabled, so the SDK forbids a ZT0-register read.
    ZaStorageDisabled,
}

impl fmt::Display for HvfArm64VcpuSmeZt0RegisterCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(source) => write!(f, "{source}"),
            Self::ZaStorageDisabled => {
                f.write_str("cannot capture the SME ZT0 register while ZA storage is disabled")
            }
        }
    }
}

impl std::error::Error for HvfArm64VcpuSmeZt0RegisterCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(source) => Some(source),
            Self::ZaStorageDisabled => None,
        }
    }
}

impl From<BackendError> for HvfArm64VcpuSmeZt0RegisterCaptureError {
    fn from(source: BackendError) -> Self {
        Self::Backend(source)
    }
}

/// Detached raw SME2 ZT0-register contents captured from one arm64 vCPU.
///
/// Hypervisor.framework exposes ZT0 as a fixed 64-byte register that requires
/// `PSTATE.ZA` but not `PSTATE.SM`; its size is independent of maximum or
/// effective SVL. These bytes are sensitive guest execution and potentially
/// cryptographic state, so `Debug` redacts them. Z, P, ZA, setters, lane
/// interpretation, persistence, encryption, snapshot schema, and restore
/// ordering remain outside this getter-only value.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfArm64VcpuSmeZt0RegisterState {
    bytes: [u8; 64],
}

impl HvfArm64VcpuSmeZt0RegisterState {
    /// Fixed ZT0-register size defined by the Hypervisor.framework SDK.
    pub const BYTE_COUNT: usize = 64;

    const fn new(bytes: [u8; Self::BYTE_COUNT]) -> Self {
        Self { bytes }
    }

    /// Return the complete fixed-size raw ZT0-register bytes.
    pub const fn as_bytes(&self) -> &[u8; Self::BYTE_COUNT] {
        &self.bytes
    }
}

impl fmt::Debug for HvfArm64VcpuSmeZt0RegisterState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64VcpuSmeZt0RegisterState")
            .field("register", &"<redacted>")
            .finish()
    }
}

/// Detached raw SME system-register state captured from one arm64 vCPU.
///
/// Hypervisor.framework exposes `SMCR_EL1`, `SMPRI_EL1`, and `TPIDR2_EL0` on
/// macOS 15.2 and newer. `TPIDR2_EL0` can contain sensitive guest thread
/// context, so `Debug` redacts every raw value. These mutable observations are
/// separate from SVE/SME identification metadata, `PSTATE.SM`/`PSTATE.ZA`, the
/// maximum streaming vector length, and Z/P/ZA/ZT0 contents. This getter-only
/// value defines no feature validation, persistence, snapshot schema, or safe
/// restore ordering.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuSmeSystemRegisterState {
    smcr_el1: u64,
    smpri_el1: u64,
    tpidr2_el0: u64,
}

impl HvfArm64VcpuSmeSystemRegisterState {
    pub(crate) const fn new(smcr_el1: u64, smpri_el1: u64, tpidr2_el0: u64) -> Self {
        Self {
            smcr_el1,
            smpri_el1,
            tpidr2_el0,
        }
    }

    /// Return the raw `SMCR_EL1` value.
    pub const fn smcr_el1(self) -> u64 {
        self.smcr_el1
    }

    /// Return the raw `SMPRI_EL1` value.
    pub const fn smpri_el1(self) -> u64 {
        self.smpri_el1
    }

    /// Return the raw `TPIDR2_EL0` value.
    pub const fn tpidr2_el0(self) -> u64 {
        self.tpidr2_el0
    }
}

impl fmt::Debug for HvfArm64VcpuSmeSystemRegisterState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64VcpuSmeSystemRegisterState")
            .field("registers", &"<redacted>")
            .finish()
    }
}

/// Detached raw system-context register state captured from one arm64 vCPU.
///
/// Hypervisor.framework exposes `SCXTNUM_EL0` and `SCXTNUM_EL1` on macOS 15.2
/// and newer. These guest software context numbers can identify execution
/// contexts, so `Debug` redacts both raw values. They are separate from TPIDR
/// thread context, `CONTEXTIDR_EL1`, and processor feature metadata. This
/// complete typed value can be reapplied through an owner-thread primitive,
/// but defines no interpretation, feature or destination validation,
/// persistence, snapshot schema, rollback, or safe wider-context ordering.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuSystemContextRegisterState {
    scxtnum_el0: u64,
    scxtnum_el1: u64,
}

impl HvfArm64VcpuSystemContextRegisterState {
    pub(crate) const fn new(scxtnum_el0: u64, scxtnum_el1: u64) -> Self {
        Self {
            scxtnum_el0,
            scxtnum_el1,
        }
    }

    /// Return the raw `SCXTNUM_EL0` value.
    pub const fn scxtnum_el0(self) -> u64 {
        self.scxtnum_el0
    }

    /// Return the raw `SCXTNUM_EL1` value.
    pub const fn scxtnum_el1(self) -> u64 {
        self.scxtnum_el1
    }
}

impl fmt::Debug for HvfArm64VcpuSystemContextRegisterState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64VcpuSystemContextRegisterState")
            .field("registers", &"<redacted>")
            .finish()
    }
}

/// Detached raw EL1 translation-register state captured from one arm64 vCPU.
///
/// This value contains `SCTLR_EL1`, both translation table bases, `TCR_EL1`,
/// `MAIR_EL1`, `AMAIR_EL1`, and `CONTEXTIDR_EL1`. Table bases can expose guest
/// physical addresses, and context values can expose guest identifiers. The
/// complete typed value can be reapplied through an owner-thread primitive,
/// but it remains sensitive, unvalidated raw state rather than a complete or
/// serialized restorable vCPU. Table-memory persistence, destination and
/// writable-bit validation, dependency ordering, MMU transitions, barriers,
/// TLB/cache maintenance, and schema remain outside it. Optional
/// `SCXTNUM_EL0`/`SCXTNUM_EL1` context is captured separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuTranslationRegisterState {
    sctlr_el1: u64,
    ttbr0_el1: u64,
    ttbr1_el1: u64,
    tcr_el1: u64,
    mair_el1: u64,
    amair_el1: u64,
    contextidr_el1: u64,
}

impl HvfArm64VcpuTranslationRegisterState {
    pub(crate) const fn new(
        sctlr_el1: u64,
        ttbr0_el1: u64,
        ttbr1_el1: u64,
        tcr_el1: u64,
        mair_el1: u64,
        amair_el1: u64,
        contextidr_el1: u64,
    ) -> Self {
        Self {
            sctlr_el1,
            ttbr0_el1,
            ttbr1_el1,
            tcr_el1,
            mair_el1,
            amair_el1,
            contextidr_el1,
        }
    }

    /// Return the raw `SCTLR_EL1` value.
    pub const fn sctlr_el1(self) -> u64 {
        self.sctlr_el1
    }

    /// Return the raw `TTBR0_EL1` value.
    pub const fn ttbr0_el1(self) -> u64 {
        self.ttbr0_el1
    }

    /// Return the raw `TTBR1_EL1` value.
    pub const fn ttbr1_el1(self) -> u64 {
        self.ttbr1_el1
    }

    /// Return the raw `TCR_EL1` value.
    pub const fn tcr_el1(self) -> u64 {
        self.tcr_el1
    }

    /// Return the raw `MAIR_EL1` value.
    pub const fn mair_el1(self) -> u64 {
        self.mair_el1
    }

    /// Return the raw `AMAIR_EL1` value.
    pub const fn amair_el1(self) -> u64 {
        self.amair_el1
    }

    /// Return the raw `CONTEXTIDR_EL1` value.
    pub const fn contextidr_el1(self) -> u64 {
        self.contextidr_el1
    }
}

/// Detached EL1 pointer-authentication keys captured from one arm64 vCPU.
///
/// The five 128-bit keys are cryptographic secrets. `Debug` redacts every key,
/// but the named accessors intentionally expose raw values to trusted internal
/// snapshot orchestration. The complete typed value can be reapplied through an
/// owner-thread primitive, but this value has no feature or destination
/// validation, zeroization, persistence protection, SCTLR enable ordering,
/// rollback, or serialized schema policy.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfArm64VcpuPointerAuthenticationKeyState {
    keys: [u128; 5],
}

impl HvfArm64VcpuPointerAuthenticationKeyState {
    pub(crate) const fn new(halves: [u64; 10]) -> Self {
        Self {
            keys: [
                pointer_authentication_key(halves[0], halves[1]),
                pointer_authentication_key(halves[2], halves[3]),
                pointer_authentication_key(halves[4], halves[5]),
                pointer_authentication_key(halves[6], halves[7]),
                pointer_authentication_key(halves[8], halves[9]),
            ],
        }
    }

    /// Return the raw 128-bit instruction A key.
    pub const fn apia_key(&self) -> u128 {
        self.keys[0]
    }

    /// Return the raw 128-bit instruction B key.
    pub const fn apib_key(&self) -> u128 {
        self.keys[1]
    }

    /// Return the raw 128-bit data A key.
    pub const fn apda_key(&self) -> u128 {
        self.keys[2]
    }

    /// Return the raw 128-bit data B key.
    pub const fn apdb_key(&self) -> u128 {
        self.keys[3]
    }

    /// Return the raw 128-bit generic key.
    pub const fn apga_key(&self) -> u128 {
        self.keys[4]
    }
}

impl fmt::Debug for HvfArm64VcpuPointerAuthenticationKeyState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64VcpuPointerAuthenticationKeyState")
            .field("keys", &"<redacted>")
            .finish()
    }
}

const fn pointer_authentication_key(low: u64, high: u64) -> u128 {
    (low as u128) | ((high as u128) << 64)
}

const fn pointer_authentication_key_halves(key: u128) -> (u64, u64) {
    (key as u64, (key >> 64) as u64)
}

/// Detached raw thread-context register state captured from one arm64 vCPU.
///
/// These software thread-ID values can contain guest TLS or kernel pointers.
/// The complete typed value can be reapplied through an owner-thread primitive,
/// but it is not address- or destination-validated and is not a complete or
/// serialized restorable vCPU state. `TPIDR2_EL0` is captured separately with
/// SME system registers, while `SCXTNUM_EL0`/`SCXTNUM_EL1` and
/// `CONTEXTIDR_EL1` remain separate. Persistence, schema, and wider context
/// ordering stay outside this value.
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
/// these Q values to the low 128 bits of the corresponding Z registers. The
/// complete typed value can be reapplied through an owner-thread primitive, but
/// that raw operation defines no streaming-mode or Q/Z ordering, feature or
/// destination validation, writable-bit policy, persistence, rollback, schema,
/// or complete SVE/SME restore.
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

/// Detached raw physical-timer state captured from one arm64 vCPU.
///
/// Hypervisor.framework exposes the CNTP registers on macOS 15 and newer only
/// when the VM creates a GIC before its vCPU. `CNTP_CTL_EL0` includes derived
/// ISTATUS, while `CNTP_CVAL_EL0` is an absolute comparator against a continuing
/// physical count. `CNTP_TVAL_EL0` is the architecturally signed 32-bit relative
/// view of that comparator, returned here as the raw Hypervisor.framework `u64`.
/// It changes as time advances between the separately timed CVAL and TVAL reads.
/// These raw values have no portable elapsed-time adjustment, writable-bit,
/// interrupt-delivery, or restore policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64VcpuPhysicalTimerState {
    cntkctl_el1: u64,
    cntp_ctl_el0: u64,
    cntp_cval_el0: u64,
    cntp_tval_el0: u64,
}

impl HvfArm64VcpuPhysicalTimerState {
    pub(crate) const fn new(
        cntkctl_el1: u64,
        cntp_ctl_el0: u64,
        cntp_cval_el0: u64,
        cntp_tval_el0: u64,
    ) -> Self {
        Self {
            cntkctl_el1,
            cntp_ctl_el0,
            cntp_cval_el0,
            cntp_tval_el0,
        }
    }

    /// Return the raw `CNTKCTL_EL1` value.
    pub const fn cntkctl_el1(self) -> u64 {
        self.cntkctl_el1
    }

    /// Return the raw `CNTP_CTL_EL0` value, including derived ISTATUS.
    pub const fn cntp_ctl_el0(self) -> u64 {
        self.cntp_ctl_el0
    }

    /// Return the raw absolute `CNTP_CVAL_EL0` compare value.
    pub const fn cntp_cval_el0(self) -> u64 {
        self.cntp_cval_el0
    }

    /// Return the raw `CNTP_TVAL_EL0` relative timer value.
    pub const fn cntp_tval_el0(self) -> u64 {
        self.cntp_tval_el0
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
    pub const MDCCINT_EL1: Self = Self(crate::ffi::HV_SYS_REG_MDCCINT_EL1);
    pub const MDSCR_EL1: Self = Self(crate::ffi::HV_SYS_REG_MDSCR_EL1);
    pub const MIDR_EL1: Self = Self(crate::ffi::HV_SYS_REG_MIDR_EL1);
    pub const MPIDR_EL1: Self = Self(crate::ffi::HV_SYS_REG_MPIDR_EL1);
    pub const ID_AA64PFR0_EL1: Self = Self(crate::ffi::HV_SYS_REG_ID_AA64PFR0_EL1);
    pub const ID_AA64PFR1_EL1: Self = Self(crate::ffi::HV_SYS_REG_ID_AA64PFR1_EL1);
    pub const ID_AA64ZFR0_EL1: Self = Self(crate::ffi::HV_SYS_REG_ID_AA64ZFR0_EL1);
    pub const ID_AA64SMFR0_EL1: Self = Self(crate::ffi::HV_SYS_REG_ID_AA64SMFR0_EL1);
    pub const ID_AA64DFR0_EL1: Self = Self(crate::ffi::HV_SYS_REG_ID_AA64DFR0_EL1);
    pub const ID_AA64DFR1_EL1: Self = Self(crate::ffi::HV_SYS_REG_ID_AA64DFR1_EL1);
    pub const ID_AA64ISAR0_EL1: Self = Self(crate::ffi::HV_SYS_REG_ID_AA64ISAR0_EL1);
    pub const ID_AA64ISAR1_EL1: Self = Self(crate::ffi::HV_SYS_REG_ID_AA64ISAR1_EL1);
    pub const ID_AA64MMFR0_EL1: Self = Self(crate::ffi::HV_SYS_REG_ID_AA64MMFR0_EL1);
    pub const ID_AA64MMFR1_EL1: Self = Self(crate::ffi::HV_SYS_REG_ID_AA64MMFR1_EL1);
    pub const ID_AA64MMFR2_EL1: Self = Self(crate::ffi::HV_SYS_REG_ID_AA64MMFR2_EL1);
    pub const SCTLR_EL1: Self = Self(crate::ffi::HV_SYS_REG_SCTLR_EL1);
    pub const ACTLR_EL1: Self = Self(crate::ffi::HV_SYS_REG_ACTLR_EL1);
    pub const CPACR_EL1: Self = Self(crate::ffi::HV_SYS_REG_CPACR_EL1);
    pub const SMPRI_EL1: Self = Self(crate::ffi::HV_SYS_REG_SMPRI_EL1);
    pub const SMCR_EL1: Self = Self(crate::ffi::HV_SYS_REG_SMCR_EL1);
    pub const TTBR0_EL1: Self = Self(crate::ffi::HV_SYS_REG_TTBR0_EL1);
    pub const TTBR1_EL1: Self = Self(crate::ffi::HV_SYS_REG_TTBR1_EL1);
    pub const TCR_EL1: Self = Self(crate::ffi::HV_SYS_REG_TCR_EL1);
    pub const APIAKEYLO_EL1: Self = Self(crate::ffi::HV_SYS_REG_APIAKEYLO_EL1);
    pub const APIAKEYHI_EL1: Self = Self(crate::ffi::HV_SYS_REG_APIAKEYHI_EL1);
    pub const APIBKEYLO_EL1: Self = Self(crate::ffi::HV_SYS_REG_APIBKEYLO_EL1);
    pub const APIBKEYHI_EL1: Self = Self(crate::ffi::HV_SYS_REG_APIBKEYHI_EL1);
    pub const APDAKEYLO_EL1: Self = Self(crate::ffi::HV_SYS_REG_APDAKEYLO_EL1);
    pub const APDAKEYHI_EL1: Self = Self(crate::ffi::HV_SYS_REG_APDAKEYHI_EL1);
    pub const APDBKEYLO_EL1: Self = Self(crate::ffi::HV_SYS_REG_APDBKEYLO_EL1);
    pub const APDBKEYHI_EL1: Self = Self(crate::ffi::HV_SYS_REG_APDBKEYHI_EL1);
    pub const APGAKEYLO_EL1: Self = Self(crate::ffi::HV_SYS_REG_APGAKEYLO_EL1);
    pub const APGAKEYHI_EL1: Self = Self(crate::ffi::HV_SYS_REG_APGAKEYHI_EL1);
    pub const SPSR_EL1: Self = Self(crate::ffi::HV_SYS_REG_SPSR_EL1);
    pub const ELR_EL1: Self = Self(crate::ffi::HV_SYS_REG_ELR_EL1);
    pub const SP_EL0: Self = Self(crate::ffi::HV_SYS_REG_SP_EL0);
    pub const AFSR0_EL1: Self = Self(crate::ffi::HV_SYS_REG_AFSR0_EL1);
    pub const AFSR1_EL1: Self = Self(crate::ffi::HV_SYS_REG_AFSR1_EL1);
    pub const ESR_EL1: Self = Self(crate::ffi::HV_SYS_REG_ESR_EL1);
    pub const FAR_EL1: Self = Self(crate::ffi::HV_SYS_REG_FAR_EL1);
    pub const PAR_EL1: Self = Self(crate::ffi::HV_SYS_REG_PAR_EL1);
    pub const MAIR_EL1: Self = Self(crate::ffi::HV_SYS_REG_MAIR_EL1);
    pub const AMAIR_EL1: Self = Self(crate::ffi::HV_SYS_REG_AMAIR_EL1);
    pub const VBAR_EL1: Self = Self(crate::ffi::HV_SYS_REG_VBAR_EL1);
    pub const CONTEXTIDR_EL1: Self = Self(crate::ffi::HV_SYS_REG_CONTEXTIDR_EL1);
    pub const TPIDR_EL1: Self = Self(crate::ffi::HV_SYS_REG_TPIDR_EL1);
    pub const SCXTNUM_EL1: Self = Self(crate::ffi::HV_SYS_REG_SCXTNUM_EL1);
    pub const CNTKCTL_EL1: Self = Self(crate::ffi::HV_SYS_REG_CNTKCTL_EL1);
    pub const CSSELR_EL1: Self = Self(crate::ffi::HV_SYS_REG_CSSELR_EL1);
    pub const TPIDR_EL0: Self = Self(crate::ffi::HV_SYS_REG_TPIDR_EL0);
    pub const TPIDRRO_EL0: Self = Self(crate::ffi::HV_SYS_REG_TPIDRRO_EL0);
    pub const TPIDR2_EL0: Self = Self(crate::ffi::HV_SYS_REG_TPIDR2_EL0);
    pub const SCXTNUM_EL0: Self = Self(crate::ffi::HV_SYS_REG_SCXTNUM_EL0);
    pub const CNTP_CTL_EL0: Self = Self(crate::ffi::HV_SYS_REG_CNTP_CTL_EL0);
    pub const CNTP_CVAL_EL0: Self = Self(crate::ffi::HV_SYS_REG_CNTP_CVAL_EL0);
    pub const CNTP_TVAL_EL0: Self = Self(crate::ffi::HV_SYS_REG_CNTP_TVAL_EL0);
    pub const CNTV_CTL_EL0: Self = Self(crate::ffi::HV_SYS_REG_CNTV_CTL_EL0);
    pub const CNTV_CVAL_EL0: Self = Self(crate::ffi::HV_SYS_REG_CNTV_CVAL_EL0);
    pub const SP_EL1: Self = Self(crate::ffi::HV_SYS_REG_SP_EL1);

    /// Return the typed `DBGBVR<n>_EL1` identifier for `index` in 0..=15.
    pub const fn debug_breakpoint_value(index: u8) -> Option<Self> {
        let raw = crate::ffi::HV_SYS_REG_DBGBVR0_EL1
            + index as u16 * crate::ffi::HV_SYS_REG_DEBUG_REGISTER_STRIDE;
        if raw <= crate::ffi::HV_SYS_REG_DBGBVR15_EL1 {
            Some(Self(raw))
        } else {
            None
        }
    }

    /// Return the typed `DBGBCR<n>_EL1` identifier for `index` in 0..=15.
    pub const fn debug_breakpoint_control(index: u8) -> Option<Self> {
        let raw = crate::ffi::HV_SYS_REG_DBGBCR0_EL1
            + index as u16 * crate::ffi::HV_SYS_REG_DEBUG_REGISTER_STRIDE;
        if raw <= crate::ffi::HV_SYS_REG_DBGBCR15_EL1 {
            Some(Self(raw))
        } else {
            None
        }
    }

    /// Return the typed `DBGWVR<n>_EL1` identifier for `index` in 0..=15.
    pub const fn debug_watchpoint_value(index: u8) -> Option<Self> {
        let raw = crate::ffi::HV_SYS_REG_DBGWVR0_EL1
            + index as u16 * crate::ffi::HV_SYS_REG_DEBUG_REGISTER_STRIDE;
        if raw <= crate::ffi::HV_SYS_REG_DBGWVR15_EL1 {
            Some(Self(raw))
        } else {
            None
        }
    }

    /// Return the typed `DBGWCR<n>_EL1` identifier for `index` in 0..=15.
    pub const fn debug_watchpoint_control(index: u8) -> Option<Self> {
        let raw = crate::ffi::HV_SYS_REG_DBGWCR0_EL1
            + index as u16 * crate::ffi::HV_SYS_REG_DEBUG_REGISTER_STRIDE;
        if raw <= crate::ffi::HV_SYS_REG_DBGWCR15_EL1 {
            Some(Self(raw))
        } else {
            None
        }
    }

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

    pub(crate) fn set_simd_fp_register(
        &mut self,
        register: HvfSimdFpRegister,
        value: [u8; 16],
    ) -> Result<(), BackendError> {
        crate::ffi::set_simd_fp_reg(self.handle()?.vcpu, register.raw(), value)
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

    pub(crate) fn get_trap_debug_exceptions(&self) -> Result<bool, BackendError> {
        crate::ffi::get_trap_debug_exceptions(self.handle()?.vcpu)
    }

    pub(crate) fn set_trap_debug_exceptions(&mut self, value: bool) -> Result<(), BackendError> {
        crate::ffi::set_trap_debug_exceptions(self.handle()?.vcpu, value)
    }

    pub(crate) fn get_trap_debug_reg_accesses(&self) -> Result<bool, BackendError> {
        crate::ffi::get_trap_debug_reg_accesses(self.handle()?.vcpu)
    }

    pub(crate) fn set_trap_debug_reg_accesses(&mut self, value: bool) -> Result<(), BackendError> {
        crate::ffi::set_trap_debug_reg_accesses(self.handle()?.vcpu, value)
    }

    pub(crate) fn get_sme_pstate(&self) -> Result<(bool, bool), BackendError> {
        crate::ffi::get_sme_state(self.handle()?.vcpu)
    }

    pub(crate) fn get_sme_maximum_svl_bytes(&self) -> Result<usize, BackendError> {
        self.handle()?;
        crate::ffi::get_sme_config_max_svl_bytes()
    }

    pub(crate) fn get_sme_p_register(
        &self,
        register: u32,
        value: &mut [u8],
    ) -> Result<(), BackendError> {
        crate::ffi::get_sme_p_reg(self.handle()?.vcpu, register, value)
    }

    pub(crate) fn get_sme_z_register(
        &self,
        register: u32,
        value: &mut [u8],
    ) -> Result<(), BackendError> {
        crate::ffi::get_sme_z_reg(self.handle()?.vcpu, register, value)
    }

    pub(crate) fn get_sme_za_register(&self, value: &mut [u8]) -> Result<(), BackendError> {
        crate::ffi::get_sme_za_reg(self.handle()?.vcpu, value)
    }

    pub(crate) fn get_sme_zt0_register(&self) -> Result<[u8; 64], BackendError> {
        crate::ffi::get_sme_zt0_reg(self.handle()?.vcpu)
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

    /// Write one raw 128-bit Q-register value on this current-thread vCPU.
    ///
    /// In streaming SVE mode, this also changes the low 128 bits of the aliased
    /// Z register. The caller is responsible for wider-state ordering.
    pub fn set_simd_fp_register(
        &mut self,
        register: HvfSimdFpRegister,
        value: [u8; 16],
    ) -> Result<(), BackendError> {
        self.owner.set_simd_fp_register(register, value)
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

pub(crate) fn restore_arm64_vcpu_general_register_state_with(
    state: &HvfArm64VcpuGeneralRegisterState,
    mut set_register: impl FnMut(HvfRegister, u64) -> Result<(), BackendError>,
) -> Result<(), HvfArm64VcpuGeneralRegisterRestoreError> {
    let mut completed_writes = 0;
    let mut write_register = |register, value| {
        set_register(register, value).map_err(|source| {
            HvfArm64VcpuGeneralRegisterRestoreError::new(register, completed_writes, source)
        })?;
        completed_writes += 1;
        Ok(())
    };

    for (index, value) in (0_u8..31).zip(state.general_purpose_registers.iter().copied()) {
        let register = HvfRegister(crate::ffi::HV_REG_X0 + u32::from(index));
        write_register(register, value)?;
    }
    write_register(HvfRegister::PC, state.pc)?;
    write_register(HvfRegister::CPSR, state.cpsr)?;

    Ok(())
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

pub(crate) fn restore_arm64_vcpu_core_system_register_state_with(
    state: &HvfArm64VcpuCoreSystemRegisterState,
    mut set_system_register: impl FnMut(HvfSystemRegister, u64) -> Result<(), BackendError>,
) -> Result<(), HvfArm64VcpuSystemRegisterRestoreError> {
    let mut completed_writes = 0;
    let mut write_system_register = |register, value| {
        set_system_register(register, value).map_err(|source| {
            HvfArm64VcpuSystemRegisterRestoreError::new(register, completed_writes, source)
        })?;
        completed_writes += 1;
        Ok(())
    };

    for (register, value) in [
        (HvfSystemRegister::SP_EL0, state.sp_el0()),
        (HvfSystemRegister::SP_EL1, state.sp_el1()),
        (HvfSystemRegister::ELR_EL1, state.elr_el1()),
        (HvfSystemRegister::SPSR_EL1, state.spsr_el1()),
    ] {
        write_system_register(register, value)?;
    }

    Ok(())
}

pub(crate) fn capture_arm64_vcpu_exception_register_state_with(
    mut get_system_register: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuExceptionRegisterState, BackendError> {
    let afsr0_el1 = get_system_register(HvfSystemRegister::AFSR0_EL1)?;
    let afsr1_el1 = get_system_register(HvfSystemRegister::AFSR1_EL1)?;
    let esr_el1 = get_system_register(HvfSystemRegister::ESR_EL1)?;
    let far_el1 = get_system_register(HvfSystemRegister::FAR_EL1)?;
    let par_el1 = get_system_register(HvfSystemRegister::PAR_EL1)?;
    let vbar_el1 = get_system_register(HvfSystemRegister::VBAR_EL1)?;

    Ok(HvfArm64VcpuExceptionRegisterState::new(
        afsr0_el1, afsr1_el1, esr_el1, far_el1, par_el1, vbar_el1,
    ))
}

pub(crate) fn restore_arm64_vcpu_exception_register_state_with(
    state: &HvfArm64VcpuExceptionRegisterState,
    mut set_system_register: impl FnMut(HvfSystemRegister, u64) -> Result<(), BackendError>,
) -> Result<(), HvfArm64VcpuSystemRegisterRestoreError> {
    let mut completed_writes = 0;
    let mut write_system_register = |register, value| {
        set_system_register(register, value).map_err(|source| {
            HvfArm64VcpuSystemRegisterRestoreError::new(register, completed_writes, source)
        })?;
        completed_writes += 1;
        Ok(())
    };

    for (register, value) in [
        (HvfSystemRegister::AFSR0_EL1, state.afsr0_el1()),
        (HvfSystemRegister::AFSR1_EL1, state.afsr1_el1()),
        (HvfSystemRegister::ESR_EL1, state.esr_el1()),
        (HvfSystemRegister::FAR_EL1, state.far_el1()),
        (HvfSystemRegister::PAR_EL1, state.par_el1()),
        (HvfSystemRegister::VBAR_EL1, state.vbar_el1()),
    ] {
        write_system_register(register, value)?;
    }

    Ok(())
}

pub(crate) fn capture_arm64_vcpu_execution_control_register_state_with(
    mut get_system_register: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuExecutionControlRegisterState, BackendError> {
    let actlr_el1 = get_system_register(HvfSystemRegister::ACTLR_EL1)?;
    let cpacr_el1 = get_system_register(HvfSystemRegister::CPACR_EL1)?;

    Ok(HvfArm64VcpuExecutionControlRegisterState::new(
        actlr_el1, cpacr_el1,
    ))
}

pub(crate) fn restore_arm64_vcpu_execution_control_register_state_with(
    state: &HvfArm64VcpuExecutionControlRegisterState,
    mut set_system_register: impl FnMut(HvfSystemRegister, u64) -> Result<(), BackendError>,
) -> Result<(), HvfArm64VcpuSystemRegisterRestoreError> {
    let mut completed_writes = 0;
    let mut write_system_register = |register, value| {
        set_system_register(register, value).map_err(|source| {
            HvfArm64VcpuSystemRegisterRestoreError::new(register, completed_writes, source)
        })?;
        completed_writes += 1;
        Ok(())
    };

    for (register, value) in [
        (HvfSystemRegister::ACTLR_EL1, state.actlr_el1()),
        (HvfSystemRegister::CPACR_EL1, state.cpacr_el1()),
    ] {
        write_system_register(register, value)?;
    }

    Ok(())
}

pub(crate) fn capture_arm64_vcpu_cache_selection_register_state_with(
    mut get_system_register: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuCacheSelectionRegisterState, BackendError> {
    let csselr_el1 = get_system_register(HvfSystemRegister::CSSELR_EL1)?;

    Ok(HvfArm64VcpuCacheSelectionRegisterState::new(csselr_el1))
}

pub(crate) fn restore_arm64_vcpu_cache_selection_register_state_with(
    state: &HvfArm64VcpuCacheSelectionRegisterState,
    mut set_system_register: impl FnMut(HvfSystemRegister, u64) -> Result<(), BackendError>,
) -> Result<(), HvfArm64VcpuSystemRegisterRestoreError> {
    set_system_register(HvfSystemRegister::CSSELR_EL1, state.csselr_el1()).map_err(|source| {
        HvfArm64VcpuSystemRegisterRestoreError::new(HvfSystemRegister::CSSELR_EL1, 0, source)
    })
}

pub(crate) fn capture_arm64_vcpu_breakpoint_register_state_with(
    mut get_system_register: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuBreakpointRegisterState, BackendError> {
    const BRPS_SHIFT: u32 = 12;
    const BRPS_MASK: u64 = 0xf;
    const INVALID_BREAKPOINT_INDEX_MESSAGE: &str =
        "ID_AA64DFR0_EL1 reported an invalid breakpoint register index";

    let id_aa64dfr0_el1 = get_system_register(HvfSystemRegister::ID_AA64DFR0_EL1)?;
    let implemented_breakpoint_count = ((id_aa64dfr0_el1 >> BRPS_SHIFT) & BRPS_MASK) as u8 + 1;
    let mut breakpoint_value_registers = [0; 16];
    let mut breakpoint_control_registers = [0; 16];

    for index in 0..implemented_breakpoint_count {
        let value_register = HvfSystemRegister::debug_breakpoint_value(index)
            .ok_or(BackendError::InvalidState(INVALID_BREAKPOINT_INDEX_MESSAGE))?;
        let value = get_system_register(value_register)?;
        let value_slot = breakpoint_value_registers
            .get_mut(usize::from(index))
            .ok_or(BackendError::InvalidState(INVALID_BREAKPOINT_INDEX_MESSAGE))?;
        *value_slot = value;

        let control_register = HvfSystemRegister::debug_breakpoint_control(index)
            .ok_or(BackendError::InvalidState(INVALID_BREAKPOINT_INDEX_MESSAGE))?;
        let control = get_system_register(control_register)?;
        let control_slot = breakpoint_control_registers
            .get_mut(usize::from(index))
            .ok_or(BackendError::InvalidState(INVALID_BREAKPOINT_INDEX_MESSAGE))?;
        *control_slot = control;
    }

    Ok(HvfArm64VcpuBreakpointRegisterState::new(
        implemented_breakpoint_count,
        breakpoint_value_registers,
        breakpoint_control_registers,
    ))
}

pub(crate) fn capture_arm64_vcpu_watchpoint_register_state_with(
    mut get_system_register: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuWatchpointRegisterState, BackendError> {
    const WRPS_SHIFT: u32 = 20;
    const WRPS_MASK: u64 = 0xf;
    const INVALID_WATCHPOINT_INDEX_MESSAGE: &str =
        "ID_AA64DFR0_EL1 reported an invalid watchpoint register index";

    let id_aa64dfr0_el1 = get_system_register(HvfSystemRegister::ID_AA64DFR0_EL1)?;
    let implemented_watchpoint_count = ((id_aa64dfr0_el1 >> WRPS_SHIFT) & WRPS_MASK) as u8 + 1;
    let mut watchpoint_value_registers = [0; 16];
    let mut watchpoint_control_registers = [0; 16];

    for index in 0..implemented_watchpoint_count {
        let value_register = HvfSystemRegister::debug_watchpoint_value(index)
            .ok_or(BackendError::InvalidState(INVALID_WATCHPOINT_INDEX_MESSAGE))?;
        let value = get_system_register(value_register)?;
        let value_slot = watchpoint_value_registers
            .get_mut(usize::from(index))
            .ok_or(BackendError::InvalidState(INVALID_WATCHPOINT_INDEX_MESSAGE))?;
        *value_slot = value;

        let control_register = HvfSystemRegister::debug_watchpoint_control(index)
            .ok_or(BackendError::InvalidState(INVALID_WATCHPOINT_INDEX_MESSAGE))?;
        let control = get_system_register(control_register)?;
        let control_slot = watchpoint_control_registers
            .get_mut(usize::from(index))
            .ok_or(BackendError::InvalidState(INVALID_WATCHPOINT_INDEX_MESSAGE))?;
        *control_slot = control;
    }

    Ok(HvfArm64VcpuWatchpointRegisterState::new(
        implemented_watchpoint_count,
        watchpoint_value_registers,
        watchpoint_control_registers,
    ))
}

pub(crate) fn capture_arm64_vcpu_debug_control_register_state_with(
    mut get_system_register: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuDebugControlRegisterState, BackendError> {
    let mdccint_el1 = get_system_register(HvfSystemRegister::MDCCINT_EL1)?;
    let mdscr_el1 = get_system_register(HvfSystemRegister::MDSCR_EL1)?;

    Ok(HvfArm64VcpuDebugControlRegisterState::new(
        mdccint_el1,
        mdscr_el1,
    ))
}

pub(crate) fn restore_arm64_vcpu_debug_control_register_state_with(
    state: &HvfArm64VcpuDebugControlRegisterState,
    mut set_system_register: impl FnMut(HvfSystemRegister, u64) -> Result<(), BackendError>,
) -> Result<(), HvfArm64VcpuSystemRegisterRestoreError> {
    let mut completed_writes = 0;
    let mut write_system_register = |register, value| {
        set_system_register(register, value).map_err(|source| {
            HvfArm64VcpuSystemRegisterRestoreError::new(register, completed_writes, source)
        })?;
        completed_writes += 1;
        Ok(())
    };

    for (register, value) in [
        (HvfSystemRegister::MDCCINT_EL1, state.mdccint_el1()),
        (HvfSystemRegister::MDSCR_EL1, state.mdscr_el1()),
    ] {
        write_system_register(register, value)?;
    }

    Ok(())
}

pub(crate) fn capture_arm64_vcpu_debug_trap_state_with<R: ?Sized>(
    reader: &mut R,
    get_trap_debug_exceptions: impl FnOnce(&mut R) -> Result<bool, BackendError>,
    get_trap_debug_reg_accesses: impl FnOnce(&mut R) -> Result<bool, BackendError>,
) -> Result<HvfArm64VcpuDebugTrapState, BackendError> {
    let trap_debug_exceptions = get_trap_debug_exceptions(reader)?;
    let trap_debug_reg_accesses = get_trap_debug_reg_accesses(reader)?;

    Ok(HvfArm64VcpuDebugTrapState::new(
        trap_debug_exceptions,
        trap_debug_reg_accesses,
    ))
}

pub(crate) fn restore_arm64_vcpu_debug_trap_state_with<W: ?Sized>(
    state: &HvfArm64VcpuDebugTrapState,
    writer: &mut W,
    set_trap_debug_exceptions: impl FnOnce(&mut W, bool) -> Result<(), BackendError>,
    set_trap_debug_reg_accesses: impl FnOnce(&mut W, bool) -> Result<(), BackendError>,
) -> Result<(), HvfArm64VcpuDebugTrapRestoreError> {
    set_trap_debug_exceptions(writer, state.trap_debug_exceptions()).map_err(|source| {
        HvfArm64VcpuDebugTrapRestoreError::new(
            HvfArm64VcpuDebugTrapRestoreOperation::DebugExceptions,
            0,
            source,
        )
    })?;
    set_trap_debug_reg_accesses(writer, state.trap_debug_reg_accesses()).map_err(|source| {
        HvfArm64VcpuDebugTrapRestoreError::new(
            HvfArm64VcpuDebugTrapRestoreOperation::DebugRegisterAccesses,
            1,
            source,
        )
    })?;

    Ok(())
}

pub(crate) fn capture_arm64_vcpu_identification_register_state_with(
    mut get_system_register: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuIdentificationRegisterState, BackendError> {
    let values = [
        get_system_register(HvfSystemRegister::MIDR_EL1)?,
        get_system_register(HvfSystemRegister::MPIDR_EL1)?,
        get_system_register(HvfSystemRegister::ID_AA64PFR0_EL1)?,
        get_system_register(HvfSystemRegister::ID_AA64PFR1_EL1)?,
        get_system_register(HvfSystemRegister::ID_AA64DFR0_EL1)?,
        get_system_register(HvfSystemRegister::ID_AA64DFR1_EL1)?,
        get_system_register(HvfSystemRegister::ID_AA64ISAR0_EL1)?,
        get_system_register(HvfSystemRegister::ID_AA64ISAR1_EL1)?,
        get_system_register(HvfSystemRegister::ID_AA64MMFR0_EL1)?,
        get_system_register(HvfSystemRegister::ID_AA64MMFR1_EL1)?,
        get_system_register(HvfSystemRegister::ID_AA64MMFR2_EL1)?,
    ];

    Ok(HvfArm64VcpuIdentificationRegisterState::new(values))
}

pub(crate) fn capture_arm64_vcpu_sve_sme_identification_register_state_with(
    mut get_system_register: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuSveSmeIdentificationRegisterState, BackendError> {
    let id_aa64zfr0_el1 = get_system_register(HvfSystemRegister::ID_AA64ZFR0_EL1)?;
    let id_aa64smfr0_el1 = get_system_register(HvfSystemRegister::ID_AA64SMFR0_EL1)?;

    Ok(HvfArm64VcpuSveSmeIdentificationRegisterState::new(
        id_aa64zfr0_el1,
        id_aa64smfr0_el1,
    ))
}

pub(crate) fn capture_arm64_vcpu_sme_pstate_with(
    get_sme_pstate: impl FnOnce() -> Result<(bool, bool), BackendError>,
) -> Result<HvfArm64VcpuSmePstate, BackendError> {
    let (streaming_sve_mode_enabled, za_storage_enabled) = get_sme_pstate()?;

    Ok(HvfArm64VcpuSmePstate::new(
        streaming_sve_mode_enabled,
        za_storage_enabled,
    ))
}

fn allocate_arm64_vcpu_sme_p_register_bytes(
    size: usize,
) -> Result<Vec<u8>, HvfArm64VcpuSmePRegisterCaptureError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| HvfArm64VcpuSmePRegisterCaptureError::AllocationFailed { size })?;
    bytes.resize(size, 0);
    Ok(bytes)
}

pub(crate) fn capture_arm64_vcpu_sme_p_register_state_with<R: ?Sized>(
    reader: &mut R,
    get_sme_pstate: impl FnOnce(&mut R) -> Result<(bool, bool), BackendError>,
    get_maximum_svl_bytes: impl FnOnce(&mut R) -> Result<usize, BackendError>,
    allocate: impl FnOnce(usize) -> Result<Vec<u8>, HvfArm64VcpuSmePRegisterCaptureError>,
    mut get_sme_p_register: impl FnMut(&mut R, u32, &mut [u8]) -> Result<(), BackendError>,
) -> Result<HvfArm64VcpuSmePRegisterState, HvfArm64VcpuSmePRegisterCaptureError> {
    let (streaming_sve_mode_enabled, _) = get_sme_pstate(reader)?;
    if !streaming_sve_mode_enabled {
        return Err(HvfArm64VcpuSmePRegisterCaptureError::StreamingSveModeDisabled);
    }

    let maximum_svl_bytes = get_maximum_svl_bytes(reader)?;
    if maximum_svl_bytes == 0 {
        return Err(HvfArm64VcpuSmePRegisterCaptureError::ZeroMaximumSvl);
    }
    if maximum_svl_bytes % 8 != 0 {
        return Err(
            HvfArm64VcpuSmePRegisterCaptureError::MaximumSvlNotDivisibleByEight {
                maximum_svl_bytes,
            },
        );
    }
    let predicate_width_bytes = maximum_svl_bytes / 8;
    let capture_size = predicate_width_bytes
        .checked_mul(ARM64_SME_P_REGISTER_COUNT)
        .ok_or(HvfArm64VcpuSmePRegisterCaptureError::CaptureSizeOverflow { maximum_svl_bytes })?;
    let mut bytes = allocate(capture_size)?;
    if bytes.len() != capture_size {
        return Err(HvfArm64VcpuSmePRegisterCaptureError::AllocationFailed { size: capture_size });
    }

    for (register, value) in (0_u32..).zip(bytes.chunks_exact_mut(predicate_width_bytes)) {
        get_sme_p_register(reader, register, value)?;
    }

    Ok(HvfArm64VcpuSmePRegisterState::new(maximum_svl_bytes, bytes))
}

pub(crate) fn capture_arm64_vcpu_sme_p_register_state<R: ?Sized>(
    reader: &mut R,
    get_sme_pstate: impl FnOnce(&mut R) -> Result<(bool, bool), BackendError>,
    get_maximum_svl_bytes: impl FnOnce(&mut R) -> Result<usize, BackendError>,
    get_sme_p_register: impl FnMut(&mut R, u32, &mut [u8]) -> Result<(), BackendError>,
) -> Result<HvfArm64VcpuSmePRegisterState, HvfArm64VcpuSmePRegisterCaptureError> {
    capture_arm64_vcpu_sme_p_register_state_with(
        reader,
        get_sme_pstate,
        get_maximum_svl_bytes,
        allocate_arm64_vcpu_sme_p_register_bytes,
        get_sme_p_register,
    )
}

fn allocate_arm64_vcpu_sme_z_register_bytes(
    size: usize,
) -> Result<Vec<u8>, HvfArm64VcpuSmeZRegisterCaptureError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| HvfArm64VcpuSmeZRegisterCaptureError::AllocationFailed { size })?;
    bytes.resize(size, 0);
    Ok(bytes)
}

pub(crate) fn capture_arm64_vcpu_sme_z_register_state_with<R: ?Sized>(
    reader: &mut R,
    get_sme_pstate: impl FnOnce(&mut R) -> Result<(bool, bool), BackendError>,
    get_maximum_svl_bytes: impl FnOnce(&mut R) -> Result<usize, BackendError>,
    allocate: impl FnOnce(usize) -> Result<Vec<u8>, HvfArm64VcpuSmeZRegisterCaptureError>,
    mut get_sme_z_register: impl FnMut(&mut R, u32, &mut [u8]) -> Result<(), BackendError>,
) -> Result<HvfArm64VcpuSmeZRegisterState, HvfArm64VcpuSmeZRegisterCaptureError> {
    let (streaming_sve_mode_enabled, _) = get_sme_pstate(reader)?;
    if !streaming_sve_mode_enabled {
        return Err(HvfArm64VcpuSmeZRegisterCaptureError::StreamingSveModeDisabled);
    }

    let maximum_svl_bytes = get_maximum_svl_bytes(reader)?;
    if maximum_svl_bytes == 0 {
        return Err(HvfArm64VcpuSmeZRegisterCaptureError::ZeroMaximumSvl);
    }
    let capture_size = maximum_svl_bytes
        .checked_mul(ARM64_SME_Z_REGISTER_COUNT)
        .ok_or(HvfArm64VcpuSmeZRegisterCaptureError::CaptureSizeOverflow { maximum_svl_bytes })?;
    let mut bytes = allocate(capture_size)?;
    if bytes.len() != capture_size {
        return Err(HvfArm64VcpuSmeZRegisterCaptureError::AllocationFailed { size: capture_size });
    }

    for (register, value) in (0_u32..).zip(bytes.chunks_exact_mut(maximum_svl_bytes)) {
        get_sme_z_register(reader, register, value)?;
    }

    Ok(HvfArm64VcpuSmeZRegisterState::new(maximum_svl_bytes, bytes))
}

pub(crate) fn capture_arm64_vcpu_sme_z_register_state<R: ?Sized>(
    reader: &mut R,
    get_sme_pstate: impl FnOnce(&mut R) -> Result<(bool, bool), BackendError>,
    get_maximum_svl_bytes: impl FnOnce(&mut R) -> Result<usize, BackendError>,
    get_sme_z_register: impl FnMut(&mut R, u32, &mut [u8]) -> Result<(), BackendError>,
) -> Result<HvfArm64VcpuSmeZRegisterState, HvfArm64VcpuSmeZRegisterCaptureError> {
    capture_arm64_vcpu_sme_z_register_state_with(
        reader,
        get_sme_pstate,
        get_maximum_svl_bytes,
        allocate_arm64_vcpu_sme_z_register_bytes,
        get_sme_z_register,
    )
}

fn allocate_arm64_vcpu_sme_za_register_bytes(
    size: usize,
) -> Result<Vec<u8>, HvfArm64VcpuSmeZaRegisterCaptureError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| HvfArm64VcpuSmeZaRegisterCaptureError::AllocationFailed { size })?;
    bytes.resize(size, 0);
    Ok(bytes)
}

pub(crate) fn capture_arm64_vcpu_sme_za_register_state_with<R: ?Sized>(
    reader: &mut R,
    get_sme_pstate: impl FnOnce(&mut R) -> Result<(bool, bool), BackendError>,
    get_maximum_svl_bytes: impl FnOnce(&mut R) -> Result<usize, BackendError>,
    allocate: impl FnOnce(usize) -> Result<Vec<u8>, HvfArm64VcpuSmeZaRegisterCaptureError>,
    get_sme_za_register: impl FnOnce(&mut R, &mut [u8]) -> Result<(), BackendError>,
) -> Result<HvfArm64VcpuSmeZaRegisterState, HvfArm64VcpuSmeZaRegisterCaptureError> {
    let (_, za_storage_enabled) = get_sme_pstate(reader)?;
    if !za_storage_enabled {
        return Err(HvfArm64VcpuSmeZaRegisterCaptureError::ZaStorageDisabled);
    }

    let maximum_svl_bytes = get_maximum_svl_bytes(reader)?;
    if maximum_svl_bytes == 0 {
        return Err(HvfArm64VcpuSmeZaRegisterCaptureError::ZeroMaximumSvl);
    }
    let capture_size = maximum_svl_bytes
        .checked_mul(maximum_svl_bytes)
        .ok_or(HvfArm64VcpuSmeZaRegisterCaptureError::CaptureSizeOverflow { maximum_svl_bytes })?;
    let mut bytes = allocate(capture_size)?;
    if bytes.len() != capture_size {
        return Err(HvfArm64VcpuSmeZaRegisterCaptureError::AllocationFailed { size: capture_size });
    }

    get_sme_za_register(reader, &mut bytes)?;

    Ok(HvfArm64VcpuSmeZaRegisterState::new(
        maximum_svl_bytes,
        bytes,
    ))
}

pub(crate) fn capture_arm64_vcpu_sme_za_register_state<R: ?Sized>(
    reader: &mut R,
    get_sme_pstate: impl FnOnce(&mut R) -> Result<(bool, bool), BackendError>,
    get_maximum_svl_bytes: impl FnOnce(&mut R) -> Result<usize, BackendError>,
    get_sme_za_register: impl FnOnce(&mut R, &mut [u8]) -> Result<(), BackendError>,
) -> Result<HvfArm64VcpuSmeZaRegisterState, HvfArm64VcpuSmeZaRegisterCaptureError> {
    capture_arm64_vcpu_sme_za_register_state_with(
        reader,
        get_sme_pstate,
        get_maximum_svl_bytes,
        allocate_arm64_vcpu_sme_za_register_bytes,
        get_sme_za_register,
    )
}

pub(crate) fn capture_arm64_vcpu_sme_zt0_register_state<R: ?Sized>(
    reader: &mut R,
    get_sme_pstate: impl FnOnce(&mut R) -> Result<(bool, bool), BackendError>,
    get_sme_zt0_register: impl FnOnce(&mut R) -> Result<[u8; 64], BackendError>,
) -> Result<HvfArm64VcpuSmeZt0RegisterState, HvfArm64VcpuSmeZt0RegisterCaptureError> {
    let (_, za_storage_enabled) = get_sme_pstate(reader)?;
    if !za_storage_enabled {
        return Err(HvfArm64VcpuSmeZt0RegisterCaptureError::ZaStorageDisabled);
    }

    let bytes = get_sme_zt0_register(reader)?;
    Ok(HvfArm64VcpuSmeZt0RegisterState::new(bytes))
}

pub(crate) fn capture_arm64_vcpu_sme_system_register_state_with(
    mut get_system_register: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuSmeSystemRegisterState, BackendError> {
    let smcr_el1 = get_system_register(HvfSystemRegister::SMCR_EL1)?;
    let smpri_el1 = get_system_register(HvfSystemRegister::SMPRI_EL1)?;
    let tpidr2_el0 = get_system_register(HvfSystemRegister::TPIDR2_EL0)?;

    Ok(HvfArm64VcpuSmeSystemRegisterState::new(
        smcr_el1, smpri_el1, tpidr2_el0,
    ))
}

pub(crate) fn capture_arm64_vcpu_system_context_register_state_with(
    mut get_system_register: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuSystemContextRegisterState, BackendError> {
    let scxtnum_el0 = get_system_register(HvfSystemRegister::SCXTNUM_EL0)?;
    let scxtnum_el1 = get_system_register(HvfSystemRegister::SCXTNUM_EL1)?;

    Ok(HvfArm64VcpuSystemContextRegisterState::new(
        scxtnum_el0,
        scxtnum_el1,
    ))
}

pub(crate) fn restore_arm64_vcpu_system_context_register_state_with(
    state: &HvfArm64VcpuSystemContextRegisterState,
    mut set_system_register: impl FnMut(HvfSystemRegister, u64) -> Result<(), BackendError>,
) -> Result<(), HvfArm64VcpuSystemRegisterRestoreError> {
    let mut completed_writes = 0;
    let mut write_system_register = |register, value| {
        set_system_register(register, value).map_err(|source| {
            HvfArm64VcpuSystemRegisterRestoreError::new(register, completed_writes, source)
        })?;
        completed_writes += 1;
        Ok(())
    };

    for (register, value) in [
        (HvfSystemRegister::SCXTNUM_EL0, state.scxtnum_el0()),
        (HvfSystemRegister::SCXTNUM_EL1, state.scxtnum_el1()),
    ] {
        write_system_register(register, value)?;
    }

    Ok(())
}

pub(crate) fn capture_arm64_vcpu_translation_register_state_with(
    mut get_system_register: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuTranslationRegisterState, BackendError> {
    let sctlr_el1 = get_system_register(HvfSystemRegister::SCTLR_EL1)?;
    let ttbr0_el1 = get_system_register(HvfSystemRegister::TTBR0_EL1)?;
    let ttbr1_el1 = get_system_register(HvfSystemRegister::TTBR1_EL1)?;
    let tcr_el1 = get_system_register(HvfSystemRegister::TCR_EL1)?;
    let mair_el1 = get_system_register(HvfSystemRegister::MAIR_EL1)?;
    let amair_el1 = get_system_register(HvfSystemRegister::AMAIR_EL1)?;
    let contextidr_el1 = get_system_register(HvfSystemRegister::CONTEXTIDR_EL1)?;

    Ok(HvfArm64VcpuTranslationRegisterState::new(
        sctlr_el1,
        ttbr0_el1,
        ttbr1_el1,
        tcr_el1,
        mair_el1,
        amair_el1,
        contextidr_el1,
    ))
}

pub(crate) fn restore_arm64_vcpu_translation_register_state_with(
    state: &HvfArm64VcpuTranslationRegisterState,
    mut set_system_register: impl FnMut(HvfSystemRegister, u64) -> Result<(), BackendError>,
) -> Result<(), HvfArm64VcpuSystemRegisterRestoreError> {
    let mut completed_writes = 0;
    let mut write_system_register = |register, value| {
        set_system_register(register, value).map_err(|source| {
            HvfArm64VcpuSystemRegisterRestoreError::new(register, completed_writes, source)
        })?;
        completed_writes += 1;
        Ok(())
    };

    for (register, value) in [
        (HvfSystemRegister::SCTLR_EL1, state.sctlr_el1()),
        (HvfSystemRegister::TTBR0_EL1, state.ttbr0_el1()),
        (HvfSystemRegister::TTBR1_EL1, state.ttbr1_el1()),
        (HvfSystemRegister::TCR_EL1, state.tcr_el1()),
        (HvfSystemRegister::MAIR_EL1, state.mair_el1()),
        (HvfSystemRegister::AMAIR_EL1, state.amair_el1()),
        (HvfSystemRegister::CONTEXTIDR_EL1, state.contextidr_el1()),
    ] {
        write_system_register(register, value)?;
    }

    Ok(())
}

pub(crate) fn capture_arm64_vcpu_pointer_authentication_key_state_with(
    mut get_system_register: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuPointerAuthenticationKeyState, BackendError> {
    let halves = [
        get_system_register(HvfSystemRegister::APIAKEYLO_EL1)?,
        get_system_register(HvfSystemRegister::APIAKEYHI_EL1)?,
        get_system_register(HvfSystemRegister::APIBKEYLO_EL1)?,
        get_system_register(HvfSystemRegister::APIBKEYHI_EL1)?,
        get_system_register(HvfSystemRegister::APDAKEYLO_EL1)?,
        get_system_register(HvfSystemRegister::APDAKEYHI_EL1)?,
        get_system_register(HvfSystemRegister::APDBKEYLO_EL1)?,
        get_system_register(HvfSystemRegister::APDBKEYHI_EL1)?,
        get_system_register(HvfSystemRegister::APGAKEYLO_EL1)?,
        get_system_register(HvfSystemRegister::APGAKEYHI_EL1)?,
    ];

    Ok(HvfArm64VcpuPointerAuthenticationKeyState::new(halves))
}

pub(crate) fn restore_arm64_vcpu_pointer_authentication_key_state_with(
    state: &HvfArm64VcpuPointerAuthenticationKeyState,
    mut set_system_register: impl FnMut(HvfSystemRegister, u64) -> Result<(), BackendError>,
) -> Result<(), HvfArm64VcpuSystemRegisterRestoreError> {
    let (apia_low, apia_high) = pointer_authentication_key_halves(state.apia_key());
    let (apib_low, apib_high) = pointer_authentication_key_halves(state.apib_key());
    let (apda_low, apda_high) = pointer_authentication_key_halves(state.apda_key());
    let (apdb_low, apdb_high) = pointer_authentication_key_halves(state.apdb_key());
    let (apga_low, apga_high) = pointer_authentication_key_halves(state.apga_key());
    let mut completed_writes = 0;
    let mut write_system_register = |register, value| {
        set_system_register(register, value).map_err(|source| {
            HvfArm64VcpuSystemRegisterRestoreError::new(register, completed_writes, source)
        })?;
        completed_writes += 1;
        Ok(())
    };

    for (register, value) in [
        (HvfSystemRegister::APIAKEYLO_EL1, apia_low),
        (HvfSystemRegister::APIAKEYHI_EL1, apia_high),
        (HvfSystemRegister::APIBKEYLO_EL1, apib_low),
        (HvfSystemRegister::APIBKEYHI_EL1, apib_high),
        (HvfSystemRegister::APDAKEYLO_EL1, apda_low),
        (HvfSystemRegister::APDAKEYHI_EL1, apda_high),
        (HvfSystemRegister::APDBKEYLO_EL1, apdb_low),
        (HvfSystemRegister::APDBKEYHI_EL1, apdb_high),
        (HvfSystemRegister::APGAKEYLO_EL1, apga_low),
        (HvfSystemRegister::APGAKEYHI_EL1, apga_high),
    ] {
        write_system_register(register, value)?;
    }

    Ok(())
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

pub(crate) fn restore_arm64_vcpu_thread_context_register_state_with(
    state: &HvfArm64VcpuThreadContextRegisterState,
    mut set_system_register: impl FnMut(HvfSystemRegister, u64) -> Result<(), BackendError>,
) -> Result<(), HvfArm64VcpuSystemRegisterRestoreError> {
    let mut completed_writes = 0;
    let mut write_system_register = |register, value| {
        set_system_register(register, value).map_err(|source| {
            HvfArm64VcpuSystemRegisterRestoreError::new(register, completed_writes, source)
        })?;
        completed_writes += 1;
        Ok(())
    };

    for (register, value) in [
        (HvfSystemRegister::TPIDR_EL0, state.tpidr_el0()),
        (HvfSystemRegister::TPIDRRO_EL0, state.tpidrro_el0()),
        (HvfSystemRegister::TPIDR_EL1, state.tpidr_el1()),
    ] {
        write_system_register(register, value)?;
    }

    Ok(())
}

pub(crate) fn capture_arm64_vcpu_physical_timer_state_with(
    mut get_system_register: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<HvfArm64VcpuPhysicalTimerState, BackendError> {
    let cntkctl_el1 = get_system_register(HvfSystemRegister::CNTKCTL_EL1)?;
    let cntp_ctl_el0 = get_system_register(HvfSystemRegister::CNTP_CTL_EL0)?;
    let cntp_cval_el0 = get_system_register(HvfSystemRegister::CNTP_CVAL_EL0)?;
    let cntp_tval_el0 = get_system_register(HvfSystemRegister::CNTP_TVAL_EL0)?;

    Ok(HvfArm64VcpuPhysicalTimerState::new(
        cntkctl_el1,
        cntp_ctl_el0,
        cntp_cval_el0,
        cntp_tval_el0,
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

pub(crate) fn restore_arm64_vcpu_pending_interrupt_state_with(
    state: &HvfArm64VcpuPendingInterruptState,
    mut set_pending_interrupt: impl FnMut(HvfInterruptType, bool) -> Result<(), BackendError>,
) -> Result<(), HvfArm64VcpuPendingInterruptRestoreError> {
    for (completed_writes, (interrupt_type, pending)) in [
        (HvfInterruptType::Irq, state.irq_pending()),
        (HvfInterruptType::Fiq, state.fiq_pending()),
    ]
    .into_iter()
    .enumerate()
    {
        set_pending_interrupt(interrupt_type, pending).map_err(|source| {
            HvfArm64VcpuPendingInterruptRestoreError::new(interrupt_type, completed_writes, source)
        })?;
    }

    Ok(())
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

pub(crate) fn restore_arm64_vcpu_simd_fp_state_with<W: ?Sized>(
    state: &HvfArm64VcpuSimdFpState,
    writer: &mut W,
    mut set_simd_fp_register: impl FnMut(
        &mut W,
        HvfSimdFpRegister,
        [u8; 16],
    ) -> Result<(), BackendError>,
    mut set_register: impl FnMut(&mut W, HvfRegister, u64) -> Result<(), BackendError>,
) -> Result<(), HvfArm64VcpuSimdFpRestoreError> {
    let mut completed_writes = 0;
    for (index, value) in (0_u8..32).zip(state.q_registers()) {
        let register =
            HvfSimdFpRegister(crate::ffi::HV_SIMD_FP_REG_Q0 + crate::ffi::HvSimdFpReg::from(index));
        set_simd_fp_register(writer, register, *value).map_err(|source| {
            HvfArm64VcpuSimdFpRestoreError::new(
                HvfArm64VcpuSimdFpRestoreRegister::SimdFp(register),
                completed_writes,
                source,
            )
        })?;
        completed_writes += 1;
    }

    for (register, value) in [
        (HvfRegister::FPCR, state.fpcr()),
        (HvfRegister::FPSR, state.fpsr()),
    ] {
        set_register(writer, register, value).map_err(|source| {
            HvfArm64VcpuSimdFpRestoreError::new(
                HvfArm64VcpuSimdFpRestoreRegister::Scalar(register),
                completed_writes,
                source,
            )
        })?;
        completed_writes += 1;
    }

    Ok(())
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
        ARM64_LINUX_BOOT_CPSR, DESTROYED_VCPU_MESSAGE, HvfArm64BootRegisters,
        HvfArm64VcpuDebugTrapRestoreOperation, HvfArm64VcpuSimdFpRestoreRegister,
        HvfArm64VcpuSmePRegisterCaptureError, HvfArm64VcpuSmePRegisterState,
        HvfArm64VcpuSmeZRegisterCaptureError, HvfArm64VcpuSmeZRegisterState,
        HvfArm64VcpuSmeZaRegisterCaptureError, HvfArm64VcpuSmeZaRegisterState,
        HvfArm64VcpuSmeZt0RegisterCaptureError, HvfArm64VcpuSmeZt0RegisterState, HvfInterruptType,
        HvfRegister, HvfSimdFpRegister, HvfSystemRegister, HvfVcpu, HvfVcpuHandle, HvfVcpuOwner,
        NO_VCPU_EXIT_MESSAGE, capture_arm64_vcpu_breakpoint_register_state_with,
        capture_arm64_vcpu_cache_selection_register_state_with,
        capture_arm64_vcpu_core_system_register_state_with,
        capture_arm64_vcpu_debug_control_register_state_with,
        capture_arm64_vcpu_debug_trap_state_with, capture_arm64_vcpu_exception_register_state_with,
        capture_arm64_vcpu_execution_control_register_state_with,
        capture_arm64_vcpu_general_register_state_with,
        capture_arm64_vcpu_identification_register_state_with,
        capture_arm64_vcpu_pending_interrupt_state_with,
        capture_arm64_vcpu_physical_timer_state_with,
        capture_arm64_vcpu_pointer_authentication_key_state_with,
        capture_arm64_vcpu_simd_fp_state_with, capture_arm64_vcpu_sme_p_register_state_with,
        capture_arm64_vcpu_sme_pstate_with, capture_arm64_vcpu_sme_system_register_state_with,
        capture_arm64_vcpu_sme_z_register_state_with,
        capture_arm64_vcpu_sme_za_register_state_with, capture_arm64_vcpu_sme_zt0_register_state,
        capture_arm64_vcpu_sve_sme_identification_register_state_with,
        capture_arm64_vcpu_system_context_register_state_with,
        capture_arm64_vcpu_thread_context_register_state_with,
        capture_arm64_vcpu_translation_register_state_with,
        capture_arm64_vcpu_virtual_timer_state_with,
        capture_arm64_vcpu_watchpoint_register_state_with, configure_arm64_boot_registers_with,
        restore_arm64_vcpu_cache_selection_register_state_with,
        restore_arm64_vcpu_core_system_register_state_with,
        restore_arm64_vcpu_debug_control_register_state_with,
        restore_arm64_vcpu_debug_trap_state_with, restore_arm64_vcpu_exception_register_state_with,
        restore_arm64_vcpu_execution_control_register_state_with,
        restore_arm64_vcpu_general_register_state_with,
        restore_arm64_vcpu_pending_interrupt_state_with,
        restore_arm64_vcpu_pointer_authentication_key_state_with,
        restore_arm64_vcpu_simd_fp_state_with,
        restore_arm64_vcpu_system_context_register_state_with,
        restore_arm64_vcpu_thread_context_register_state_with,
        restore_arm64_vcpu_translation_register_state_with,
    };
    use crate::exit::{HvfExceptionExit, HvfVcpuExit};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SimdFpRead {
        Q(HvfSimdFpRegister),
        Scalar(HvfRegister),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SimdFpWrite {
        Q(HvfSimdFpRegister, [u8; 16]),
        Scalar(HvfRegister, u64),
    }

    impl SimdFpWrite {
        const fn register(self) -> HvfArm64VcpuSimdFpRestoreRegister {
            match self {
                Self::Q(register, _) => HvfArm64VcpuSimdFpRestoreRegister::SimdFp(register),
                Self::Scalar(register, _) => HvfArm64VcpuSimdFpRestoreRegister::Scalar(register),
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum DebugTrapRead {
        DebugExceptions,
        DebugRegisterAccesses,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum DebugTrapWrite {
        DebugExceptions(bool),
        DebugRegisterAccesses(bool),
    }

    impl DebugTrapWrite {
        const fn operation(self) -> HvfArm64VcpuDebugTrapRestoreOperation {
            match self {
                Self::DebugExceptions(_) => HvfArm64VcpuDebugTrapRestoreOperation::DebugExceptions,
                Self::DebugRegisterAccesses(_) => {
                    HvfArm64VcpuDebugTrapRestoreOperation::DebugRegisterAccesses
                }
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SmePCaptureCall {
        Pstate,
        MaximumSvl,
        P { register: u32, length: usize },
    }

    struct SmePTestReader {
        pstate_result: Result<(bool, bool), BackendError>,
        maximum_svl_result: Result<usize, BackendError>,
        fail_once_on_register: Option<u32>,
        calls: Vec<SmePCaptureCall>,
    }

    impl SmePTestReader {
        fn active(maximum_svl_bytes: usize) -> Self {
            Self {
                pstate_result: Ok((true, false)),
                maximum_svl_result: Ok(maximum_svl_bytes),
                fail_once_on_register: None,
                calls: Vec::new(),
            }
        }
    }

    fn sme_p_test_byte(register: u32, offset: usize) -> u8 {
        register.to_le_bytes()[0].wrapping_mul(11) ^ offset.to_le_bytes()[0]
    }

    fn capture_sme_p_test_reader(
        reader: &mut SmePTestReader,
        allocate: impl FnOnce(usize) -> Result<Vec<u8>, HvfArm64VcpuSmePRegisterCaptureError>,
    ) -> Result<HvfArm64VcpuSmePRegisterState, HvfArm64VcpuSmePRegisterCaptureError> {
        capture_arm64_vcpu_sme_p_register_state_with(
            reader,
            |reader| {
                reader.calls.push(SmePCaptureCall::Pstate);
                reader.pstate_result.clone()
            },
            |reader| {
                reader.calls.push(SmePCaptureCall::MaximumSvl);
                reader.maximum_svl_result.clone()
            },
            allocate,
            |reader, register, value| {
                reader.calls.push(SmePCaptureCall::P {
                    register,
                    length: value.len(),
                });
                if reader.fail_once_on_register == Some(register) {
                    reader.fail_once_on_register = None;
                    return Err(BackendError::InvalidState(
                        "fake SME P-register read failed",
                    ));
                }
                for (offset, byte) in value.iter_mut().enumerate() {
                    *byte = sme_p_test_byte(register, offset);
                }
                Ok(())
            },
        )
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SmeZCaptureCall {
        Pstate,
        MaximumSvl,
        Z { register: u32, length: usize },
    }

    struct SmeZTestReader {
        pstate_result: Result<(bool, bool), BackendError>,
        maximum_svl_result: Result<usize, BackendError>,
        fail_once_on_register: Option<u32>,
        calls: Vec<SmeZCaptureCall>,
    }

    impl SmeZTestReader {
        fn active(maximum_svl_bytes: usize) -> Self {
            Self {
                pstate_result: Ok((true, false)),
                maximum_svl_result: Ok(maximum_svl_bytes),
                fail_once_on_register: None,
                calls: Vec::new(),
            }
        }
    }

    fn sme_z_test_byte(register: u32, offset: usize) -> u8 {
        (register as u8).wrapping_mul(7).wrapping_add(offset as u8)
    }

    fn capture_sme_z_test_reader(
        reader: &mut SmeZTestReader,
        allocate: impl FnOnce(usize) -> Result<Vec<u8>, HvfArm64VcpuSmeZRegisterCaptureError>,
    ) -> Result<HvfArm64VcpuSmeZRegisterState, HvfArm64VcpuSmeZRegisterCaptureError> {
        capture_arm64_vcpu_sme_z_register_state_with(
            reader,
            |reader| {
                reader.calls.push(SmeZCaptureCall::Pstate);
                reader.pstate_result.clone()
            },
            |reader| {
                reader.calls.push(SmeZCaptureCall::MaximumSvl);
                reader.maximum_svl_result.clone()
            },
            allocate,
            |reader, register, value| {
                reader.calls.push(SmeZCaptureCall::Z {
                    register,
                    length: value.len(),
                });
                if reader.fail_once_on_register == Some(register) {
                    reader.fail_once_on_register = None;
                    return Err(BackendError::InvalidState(
                        "fake SME Z-register read failed",
                    ));
                }
                for (offset, byte) in value.iter_mut().enumerate() {
                    *byte = sme_z_test_byte(register, offset);
                }
                Ok(())
            },
        )
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SmeZaCaptureCall {
        Pstate,
        MaximumSvl,
        Za { length: usize },
    }

    struct SmeZaTestReader {
        pstate_result: Result<(bool, bool), BackendError>,
        maximum_svl_result: Result<usize, BackendError>,
        fail_once: bool,
        calls: Vec<SmeZaCaptureCall>,
    }

    impl SmeZaTestReader {
        fn active(maximum_svl_bytes: usize) -> Self {
            Self {
                pstate_result: Ok((false, true)),
                maximum_svl_result: Ok(maximum_svl_bytes),
                fail_once: false,
                calls: Vec::new(),
            }
        }
    }

    fn sme_za_test_byte(offset: usize) -> u8 {
        offset.to_le_bytes()[0].wrapping_mul(19) ^ 0xa5
    }

    fn capture_sme_za_test_reader(
        reader: &mut SmeZaTestReader,
        allocate: impl FnOnce(usize) -> Result<Vec<u8>, HvfArm64VcpuSmeZaRegisterCaptureError>,
    ) -> Result<HvfArm64VcpuSmeZaRegisterState, HvfArm64VcpuSmeZaRegisterCaptureError> {
        capture_arm64_vcpu_sme_za_register_state_with(
            reader,
            |reader| {
                reader.calls.push(SmeZaCaptureCall::Pstate);
                reader.pstate_result.clone()
            },
            |reader| {
                reader.calls.push(SmeZaCaptureCall::MaximumSvl);
                reader.maximum_svl_result.clone()
            },
            allocate,
            |reader, value| {
                reader.calls.push(SmeZaCaptureCall::Za {
                    length: value.len(),
                });
                if reader.fail_once {
                    reader.fail_once = false;
                    return Err(BackendError::InvalidState(
                        "fake SME ZA-register read failed",
                    ));
                }
                for (offset, byte) in value.iter_mut().enumerate() {
                    *byte = sme_za_test_byte(offset);
                }
                Ok(())
            },
        )
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SmeZt0CaptureCall {
        Pstate,
        Zt0,
    }

    struct SmeZt0TestReader {
        pstate_result: Result<(bool, bool), BackendError>,
        fail_once: bool,
        calls: Vec<SmeZt0CaptureCall>,
    }

    fn sme_zt0_test_byte(offset: usize) -> u8 {
        offset.to_le_bytes()[0].wrapping_mul(31) ^ 0x6d
    }

    fn capture_sme_zt0_test_reader(
        reader: &mut SmeZt0TestReader,
    ) -> Result<HvfArm64VcpuSmeZt0RegisterState, HvfArm64VcpuSmeZt0RegisterCaptureError> {
        capture_arm64_vcpu_sme_zt0_register_state(
            reader,
            |reader| {
                reader.calls.push(SmeZt0CaptureCall::Pstate);
                reader.pstate_result.clone()
            },
            |reader| {
                reader.calls.push(SmeZt0CaptureCall::Zt0);
                if reader.fail_once {
                    reader.fail_once = false;
                    return Err(BackendError::InvalidState(
                        "fake SME ZT0-register read failed",
                    ));
                }
                Ok(std::array::from_fn(sme_zt0_test_byte))
            },
        )
    }

    fn identification_registers() -> [HvfSystemRegister; 11] {
        [
            HvfSystemRegister::MIDR_EL1,
            HvfSystemRegister::MPIDR_EL1,
            HvfSystemRegister::ID_AA64PFR0_EL1,
            HvfSystemRegister::ID_AA64PFR1_EL1,
            HvfSystemRegister::ID_AA64DFR0_EL1,
            HvfSystemRegister::ID_AA64DFR1_EL1,
            HvfSystemRegister::ID_AA64ISAR0_EL1,
            HvfSystemRegister::ID_AA64ISAR1_EL1,
            HvfSystemRegister::ID_AA64MMFR0_EL1,
            HvfSystemRegister::ID_AA64MMFR1_EL1,
            HvfSystemRegister::ID_AA64MMFR2_EL1,
        ]
    }

    fn sve_sme_identification_registers() -> [HvfSystemRegister; 2] {
        [
            HvfSystemRegister::ID_AA64ZFR0_EL1,
            HvfSystemRegister::ID_AA64SMFR0_EL1,
        ]
    }

    fn sme_system_registers() -> [HvfSystemRegister; 3] {
        [
            HvfSystemRegister::SMCR_EL1,
            HvfSystemRegister::SMPRI_EL1,
            HvfSystemRegister::TPIDR2_EL0,
        ]
    }

    fn system_context_registers() -> [HvfSystemRegister; 2] {
        [
            HvfSystemRegister::SCXTNUM_EL0,
            HvfSystemRegister::SCXTNUM_EL1,
        ]
    }

    fn system_context_restore_test_state() -> super::HvfArm64VcpuSystemContextRegisterState {
        super::HvfArm64VcpuSystemContextRegisterState::new(
            0x0123_4567_89ab_cdef,
            0xfedc_ba98_7654_3210,
        )
    }

    fn system_context_restore_test_entries() -> [(HvfSystemRegister, u64); 2] {
        let state = system_context_restore_test_state();
        [
            (HvfSystemRegister::SCXTNUM_EL0, state.scxtnum_el0()),
            (HvfSystemRegister::SCXTNUM_EL1, state.scxtnum_el1()),
        ]
    }

    fn identification_test_value(register: HvfSystemRegister) -> u64 {
        0x1d00_0000_0000_0000 | u64::from(register.raw())
    }

    const POINTER_AUTHENTICATION_TEST_HALVES: [u64; 10] = [
        0x0123_4567_89ab_cdef,
        0xfedc_ba98_7654_3210,
        0x0f1e_2d3c_4b5a_6978,
        0x8796_a5b4_c3d2_e1f0,
        0x1357_9bdf_2468_ace0,
        0x0eca_8642_fdb9_7531,
        0x1122_3344_5566_7788,
        0x8877_6655_4433_2211,
        0xa5a5_5a5a_c3c3_3c3c,
        0x3c3c_c3c3_5a5a_a5a5,
    ];

    fn pointer_authentication_key_registers() -> [HvfSystemRegister; 10] {
        [
            HvfSystemRegister::APIAKEYLO_EL1,
            HvfSystemRegister::APIAKEYHI_EL1,
            HvfSystemRegister::APIBKEYLO_EL1,
            HvfSystemRegister::APIBKEYHI_EL1,
            HvfSystemRegister::APDAKEYLO_EL1,
            HvfSystemRegister::APDAKEYHI_EL1,
            HvfSystemRegister::APDBKEYLO_EL1,
            HvfSystemRegister::APDBKEYHI_EL1,
            HvfSystemRegister::APGAKEYLO_EL1,
            HvfSystemRegister::APGAKEYHI_EL1,
        ]
    }

    fn pointer_authentication_test_key(index: usize) -> u128 {
        super::pointer_authentication_key(
            POINTER_AUTHENTICATION_TEST_HALVES[index * 2],
            POINTER_AUTHENTICATION_TEST_HALVES[index * 2 + 1],
        )
    }

    fn pointer_authentication_restore_test_state()
    -> super::HvfArm64VcpuPointerAuthenticationKeyState {
        super::HvfArm64VcpuPointerAuthenticationKeyState::new(POINTER_AUTHENTICATION_TEST_HALVES)
    }

    fn pointer_authentication_restore_test_entries()
    -> [(HvfSystemRegister, u64); POINTER_AUTHENTICATION_TEST_HALVES.len()] {
        let registers = pointer_authentication_key_registers();
        std::array::from_fn(|index| (registers[index], POINTER_AUTHENTICATION_TEST_HALVES[index]))
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

    fn simd_fp_restore_test_state() -> super::HvfArm64VcpuSimdFpState {
        let mut reader = ();
        capture_arm64_vcpu_simd_fp_state_with(
            &mut reader,
            |_, register| Ok(simd_fp_q_value(register)),
            |_, register| Ok(0x3_0000 + u64::from(register.raw())),
        )
        .expect("SIMD/FP restore test state should be captured")
    }

    fn expected_simd_fp_writes(state: &super::HvfArm64VcpuSimdFpState) -> Vec<SimdFpWrite> {
        (0_u8..32)
            .zip(state.q_registers())
            .map(|(index, value)| {
                SimdFpWrite::Q(
                    HvfSimdFpRegister::q(index).expect("Q0-Q31 should map to SIMD registers"),
                    *value,
                )
            })
            .chain([
                SimdFpWrite::Scalar(HvfRegister::FPCR, state.fpcr()),
                SimdFpWrite::Scalar(HvfRegister::FPSR, state.fpsr()),
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
            vcpu.owner.get_trap_debug_exceptions(),
            Err(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
        );
        assert_eq!(
            vcpu.owner.set_trap_debug_exceptions(true),
            Err(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
        );
        assert_eq!(
            vcpu.owner.get_trap_debug_reg_accesses(),
            Err(BackendError::InvalidState(DESTROYED_VCPU_MESSAGE))
        );
        assert_eq!(
            vcpu.owner.set_trap_debug_reg_accesses(false),
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

    fn general_register_restore_test_state() -> super::HvfArm64VcpuGeneralRegisterState {
        capture_arm64_vcpu_general_register_state_with(|register| {
            Ok(0xa500_0000_0000_0000 | u64::from(register.raw()))
        })
        .expect("general-register test state should be captured")
    }

    fn general_register_restore_test_entries(
        state: &super::HvfArm64VcpuGeneralRegisterState,
    ) -> Vec<(HvfRegister, u64)> {
        (0_u8..31)
            .zip(state.general_purpose_registers().iter().copied())
            .map(|(index, value)| {
                (
                    HvfRegister::general_purpose(index).expect("X0-X30 should map to registers"),
                    value,
                )
            })
            .chain([
                (HvfRegister::PC, state.pc()),
                (HvfRegister::CPSR, state.cpsr()),
            ])
            .collect()
    }

    #[test]
    fn restores_arm64_general_register_state_in_architectural_order() {
        let state = general_register_restore_test_state();
        let expected = general_register_restore_test_entries(&state);
        let mut writes = Vec::new();

        restore_arm64_vcpu_general_register_state_with(&state, |register, value| {
            writes.push((register, value));
            Ok(())
        })
        .expect("general-register restore should succeed");

        assert_eq!(writes, expected);
    }

    #[test]
    fn every_arm64_general_register_restore_failure_stops_and_can_retry() {
        use std::error::Error as _;

        let state = general_register_restore_test_state();
        let expected = general_register_restore_test_entries(&state);

        for (failed_index, (failed_register, _)) in expected.iter().copied().enumerate() {
            let fail_once = Cell::new(true);
            let writes = RefCell::new(Vec::new());
            let write_register = |register, value| {
                writes.borrow_mut().push((register, value));
                if register == failed_register && fail_once.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake general-register restore failed",
                    ))
                } else {
                    Ok(())
                }
            };

            let error = restore_arm64_vcpu_general_register_state_with(&state, &write_register)
                .expect_err("injected general-register write should fail");
            assert_eq!(error.failed_register(), failed_register);
            assert_eq!(error.completed_writes(), failed_index);
            assert_eq!(
                error.source().map(ToString::to_string),
                Some("invalid backend state: fake general-register restore failed".to_string())
            );
            assert_eq!(*writes.borrow(), expected[..=failed_index]);
            assert_eq!(
                error.to_string(),
                format!(
                    "failed to restore arm64 register id {} after {failed_index} successful writes: invalid backend state: fake general-register restore failed",
                    failed_register.raw()
                )
            );

            writes.borrow_mut().clear();
            restore_arm64_vcpu_general_register_state_with(&state, &write_register)
                .expect("complete general-register restore retry should succeed");
            assert_eq!(*writes.borrow(), expected);
        }
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

    fn core_system_register_restore_test_state() -> super::HvfArm64VcpuCoreSystemRegisterState {
        capture_arm64_vcpu_core_system_register_state_with(|register| {
            Ok(0xb600_0000_0000_0000 | u64::from(register.raw()))
        })
        .expect("core system-register test state should be captured")
    }

    fn core_system_register_restore_test_entries(
        state: super::HvfArm64VcpuCoreSystemRegisterState,
    ) -> [(HvfSystemRegister, u64); 4] {
        [
            (HvfSystemRegister::SP_EL0, state.sp_el0()),
            (HvfSystemRegister::SP_EL1, state.sp_el1()),
            (HvfSystemRegister::ELR_EL1, state.elr_el1()),
            (HvfSystemRegister::SPSR_EL1, state.spsr_el1()),
        ]
    }

    #[test]
    fn restores_arm64_core_system_register_state_in_capture_order() {
        let state = core_system_register_restore_test_state();
        let expected = core_system_register_restore_test_entries(state);
        let mut writes = Vec::new();

        restore_arm64_vcpu_core_system_register_state_with(&state, |register, value| {
            writes.push((register, value));
            Ok(())
        })
        .expect("core system-register restore should succeed");

        assert_eq!(writes, expected);
    }

    #[test]
    fn every_arm64_core_system_register_restore_failure_stops_and_can_retry() {
        use std::error::Error as _;

        let state = core_system_register_restore_test_state();
        let expected = core_system_register_restore_test_entries(state);

        for (failed_index, (failed_register, _)) in expected.iter().copied().enumerate() {
            let fail_once = Cell::new(true);
            let writes = RefCell::new(Vec::new());
            let write_system_register = |register, value| {
                writes.borrow_mut().push((register, value));
                if register == failed_register && fail_once.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake core system-register restore failed",
                    ))
                } else {
                    Ok(())
                }
            };

            let error =
                restore_arm64_vcpu_core_system_register_state_with(&state, &write_system_register)
                    .expect_err("injected system-register write should fail");
            assert_eq!(error.failed_register(), failed_register);
            assert_eq!(error.completed_writes(), failed_index);
            assert_eq!(
                error.source().map(ToString::to_string),
                Some("invalid backend state: fake core system-register restore failed".to_string())
            );
            assert_eq!(*writes.borrow(), expected[..=failed_index]);
            assert_eq!(
                error.to_string(),
                format!(
                    "failed to restore arm64 system register id {} after {failed_index} successful writes: invalid backend state: fake core system-register restore failed",
                    failed_register.raw()
                )
            );

            writes.borrow_mut().clear();
            restore_arm64_vcpu_core_system_register_state_with(&state, &write_system_register)
                .expect("complete core system-register restore retry should succeed");
            assert_eq!(*writes.borrow(), expected);
        }
    }

    #[test]
    fn captures_arm64_exception_register_state_in_documented_order() {
        let mut reads = Vec::new();

        let state = capture_arm64_vcpu_exception_register_state_with(|register| {
            reads.push(register);
            Ok(0x5a5a_0000_0000_0000 | u64::from(register.raw()))
        })
        .expect("exception-register capture should succeed");

        assert_eq!(
            reads,
            [
                HvfSystemRegister::AFSR0_EL1,
                HvfSystemRegister::AFSR1_EL1,
                HvfSystemRegister::ESR_EL1,
                HvfSystemRegister::FAR_EL1,
                HvfSystemRegister::PAR_EL1,
                HvfSystemRegister::VBAR_EL1,
            ]
        );
        assert_eq!(
            state.afsr0_el1(),
            0x5a5a_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_AFSR0_EL1)
        );
        assert_eq!(
            state.afsr1_el1(),
            0x5a5a_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_AFSR1_EL1)
        );
        assert_eq!(
            state.esr_el1(),
            0x5a5a_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_ESR_EL1)
        );
        assert_eq!(
            state.far_el1(),
            0x5a5a_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_FAR_EL1)
        );
        assert_eq!(
            state.par_el1(),
            0x5a5a_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_PAR_EL1)
        );
        assert_eq!(
            state.vbar_el1(),
            0x5a5a_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_VBAR_EL1)
        );
        assert_eq!(HvfSystemRegister::AFSR0_EL1.raw(), 0xc288);
        assert_eq!(HvfSystemRegister::AFSR1_EL1.raw(), 0xc289);
        assert_eq!(HvfSystemRegister::ESR_EL1.raw(), 0xc290);
        assert_eq!(HvfSystemRegister::FAR_EL1.raw(), 0xc300);
        assert_eq!(HvfSystemRegister::PAR_EL1.raw(), 0xc3a0);
        assert_eq!(HvfSystemRegister::VBAR_EL1.raw(), 0xc600);
    }

    fn exception_register_restore_test_state() -> super::HvfArm64VcpuExceptionRegisterState {
        capture_arm64_vcpu_exception_register_state_with(|register| {
            Ok(0xc700_0000_0000_0000 | u64::from(register.raw()))
        })
        .expect("exception-register test state should be captured")
    }

    fn exception_register_restore_test_entries(
        state: super::HvfArm64VcpuExceptionRegisterState,
    ) -> [(HvfSystemRegister, u64); 6] {
        [
            (HvfSystemRegister::AFSR0_EL1, state.afsr0_el1()),
            (HvfSystemRegister::AFSR1_EL1, state.afsr1_el1()),
            (HvfSystemRegister::ESR_EL1, state.esr_el1()),
            (HvfSystemRegister::FAR_EL1, state.far_el1()),
            (HvfSystemRegister::PAR_EL1, state.par_el1()),
            (HvfSystemRegister::VBAR_EL1, state.vbar_el1()),
        ]
    }

    #[test]
    fn restores_arm64_exception_register_state_in_capture_order() {
        let state = exception_register_restore_test_state();
        let expected = exception_register_restore_test_entries(state);
        let mut writes = Vec::new();

        restore_arm64_vcpu_exception_register_state_with(&state, |register, value| {
            writes.push((register, value));
            Ok(())
        })
        .expect("exception-register restore should succeed");

        assert_eq!(writes, expected);
    }

    #[test]
    fn every_arm64_exception_register_restore_failure_stops_and_can_retry() {
        use std::error::Error as _;

        let state = exception_register_restore_test_state();
        let expected = exception_register_restore_test_entries(state);

        for (failed_index, (failed_register, _)) in expected.iter().copied().enumerate() {
            let fail_once = Cell::new(true);
            let writes = RefCell::new(Vec::new());
            let write_system_register = |register, value| {
                writes.borrow_mut().push((register, value));
                if register == failed_register && fail_once.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake exception-register restore failed",
                    ))
                } else {
                    Ok(())
                }
            };

            let error =
                restore_arm64_vcpu_exception_register_state_with(&state, &write_system_register)
                    .expect_err("injected exception-register write should fail");
            assert_eq!(error.failed_register(), failed_register);
            assert_eq!(error.completed_writes(), failed_index);
            assert_eq!(
                error.source().map(ToString::to_string),
                Some("invalid backend state: fake exception-register restore failed".to_string())
            );
            assert_eq!(*writes.borrow(), expected[..=failed_index]);
            assert_eq!(
                error.to_string(),
                format!(
                    "failed to restore arm64 system register id {} after {failed_index} successful writes: invalid backend state: fake exception-register restore failed",
                    failed_register.raw()
                )
            );

            writes.borrow_mut().clear();
            restore_arm64_vcpu_exception_register_state_with(&state, &write_system_register)
                .expect("complete exception-register restore retry should succeed");
            assert_eq!(*writes.borrow(), expected);
        }
    }

    #[test]
    fn captures_arm64_execution_control_register_state_in_documented_order() {
        let mut reads = Vec::new();

        let state = capture_arm64_vcpu_execution_control_register_state_with(|register| {
            reads.push(register);
            Ok(0xa55a_0000_0000_0000 | u64::from(register.raw()))
        })
        .expect("execution-control capture should succeed");

        assert_eq!(
            reads,
            [HvfSystemRegister::ACTLR_EL1, HvfSystemRegister::CPACR_EL1,]
        );
        assert_eq!(
            state.actlr_el1(),
            0xa55a_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_ACTLR_EL1)
        );
        assert_eq!(
            state.cpacr_el1(),
            0xa55a_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_CPACR_EL1)
        );
        assert_eq!(HvfSystemRegister::ACTLR_EL1.raw(), 0xc081);
        assert_eq!(HvfSystemRegister::CPACR_EL1.raw(), 0xc082);
    }

    fn execution_control_restore_test_state() -> super::HvfArm64VcpuExecutionControlRegisterState {
        capture_arm64_vcpu_execution_control_register_state_with(|register| {
            Ok(0xd800_0000_0000_0000 | u64::from(register.raw()))
        })
        .expect("execution-control test state should be captured")
    }

    fn execution_control_restore_test_entries(
        state: super::HvfArm64VcpuExecutionControlRegisterState,
    ) -> [(HvfSystemRegister, u64); 2] {
        [
            (HvfSystemRegister::ACTLR_EL1, state.actlr_el1()),
            (HvfSystemRegister::CPACR_EL1, state.cpacr_el1()),
        ]
    }

    #[test]
    fn restores_arm64_execution_control_register_state_in_capture_order() {
        let state = execution_control_restore_test_state();
        let expected = execution_control_restore_test_entries(state);
        let mut writes = Vec::new();

        restore_arm64_vcpu_execution_control_register_state_with(&state, |register, value| {
            writes.push((register, value));
            Ok(())
        })
        .expect("execution-control restore should succeed");

        assert_eq!(writes, expected);
    }

    #[test]
    fn every_arm64_execution_control_restore_failure_stops_and_can_retry() {
        use std::error::Error as _;

        let state = execution_control_restore_test_state();
        let expected = execution_control_restore_test_entries(state);

        for (failed_index, (failed_register, _)) in expected.iter().copied().enumerate() {
            let fail_once = Cell::new(true);
            let writes = RefCell::new(Vec::new());
            let write_system_register = |register, value| {
                writes.borrow_mut().push((register, value));
                if register == failed_register && fail_once.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake execution-control restore failed",
                    ))
                } else {
                    Ok(())
                }
            };

            let error = restore_arm64_vcpu_execution_control_register_state_with(
                &state,
                &write_system_register,
            )
            .expect_err("injected execution-control write should fail");
            assert_eq!(error.failed_register(), failed_register);
            assert_eq!(error.completed_writes(), failed_index);
            assert_eq!(
                error.source().map(ToString::to_string),
                Some("invalid backend state: fake execution-control restore failed".to_string())
            );
            assert_eq!(*writes.borrow(), expected[..=failed_index]);
            assert_eq!(
                error.to_string(),
                format!(
                    "failed to restore arm64 system register id {} after {failed_index} successful writes: invalid backend state: fake execution-control restore failed",
                    failed_register.raw()
                )
            );

            writes.borrow_mut().clear();
            restore_arm64_vcpu_execution_control_register_state_with(
                &state,
                &write_system_register,
            )
            .expect("complete execution-control restore retry should succeed");
            assert_eq!(*writes.borrow(), expected);
        }
    }

    #[test]
    fn captures_arm64_cache_selection_register_state() {
        let mut reads = Vec::new();

        let state = capture_arm64_vcpu_cache_selection_register_state_with(|register| {
            reads.push(register);
            Ok(0xca5e_0000_0000_0000 | u64::from(register.raw()))
        })
        .expect("cache-selection capture should succeed");

        assert_eq!(reads, [HvfSystemRegister::CSSELR_EL1]);
        assert_eq!(
            state.csselr_el1(),
            0xca5e_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_CSSELR_EL1)
        );
        assert_eq!(HvfSystemRegister::CSSELR_EL1.raw(), 0xd000);
    }

    fn cache_selection_restore_test_state() -> super::HvfArm64VcpuCacheSelectionRegisterState {
        super::HvfArm64VcpuCacheSelectionRegisterState::new(0x0123_4567_89ab_cdef)
    }

    #[test]
    fn restores_arm64_cache_selection_register_state() {
        let state = cache_selection_restore_test_state();
        let mut writes = Vec::new();

        restore_arm64_vcpu_cache_selection_register_state_with(&state, |register, value| {
            writes.push((register, value));
            Ok(())
        })
        .expect("cache-selection restore should succeed");

        assert_eq!(
            writes,
            [(HvfSystemRegister::CSSELR_EL1, state.csselr_el1())]
        );
    }

    #[test]
    fn arm64_cache_selection_register_restore_failure_can_retry() {
        use std::error::Error as _;

        let state = cache_selection_restore_test_state();
        let fail_next = Cell::new(true);
        let writes = RefCell::new(Vec::new());
        let write_system_register = |register, value| {
            writes.borrow_mut().push((register, value));
            if fail_next.replace(false) {
                Err(BackendError::InvalidState(
                    "fake cache-selection register restore failed",
                ))
            } else {
                Ok(())
            }
        };

        let error =
            restore_arm64_vcpu_cache_selection_register_state_with(&state, &write_system_register)
                .expect_err("injected cache-selection write should fail");
        assert_eq!(error.failed_register(), HvfSystemRegister::CSSELR_EL1);
        assert_eq!(error.completed_writes(), 0);
        assert_eq!(
            error.source().map(ToString::to_string),
            Some("invalid backend state: fake cache-selection register restore failed".to_string())
        );
        assert_eq!(
            error.to_string(),
            "failed to restore arm64 system register id 53248 after 0 successful writes: invalid backend state: fake cache-selection register restore failed"
        );
        assert_eq!(
            *writes.borrow(),
            [(HvfSystemRegister::CSSELR_EL1, state.csselr_el1())]
        );

        writes.borrow_mut().clear();
        restore_arm64_vcpu_cache_selection_register_state_with(&state, &write_system_register)
            .expect("complete cache-selection restore retry should succeed");
        assert_eq!(
            *writes.borrow(),
            [(HvfSystemRegister::CSSELR_EL1, state.csselr_el1())]
        );
    }

    #[test]
    fn maps_all_arm64_debug_breakpoint_register_slots() {
        for index in 0_u8..16 {
            assert_eq!(
                HvfSystemRegister::debug_breakpoint_value(index)
                    .expect("breakpoint value slot should be mapped")
                    .raw(),
                0x8004 + u16::from(index) * 8
            );
            assert_eq!(
                HvfSystemRegister::debug_breakpoint_control(index)
                    .expect("breakpoint control slot should be mapped")
                    .raw(),
                0x8005 + u16::from(index) * 8
            );
        }

        assert_eq!(HvfSystemRegister::debug_breakpoint_value(16), None);
        assert_eq!(HvfSystemRegister::debug_breakpoint_control(16), None);
        assert_eq!(HvfSystemRegister::debug_breakpoint_value(u8::MAX), None);
        assert_eq!(HvfSystemRegister::debug_breakpoint_control(u8::MAX), None);
    }

    #[test]
    fn captures_implemented_arm64_breakpoint_register_pairs_in_order() {
        for implemented_count in [1_u8, 3, 16] {
            let mut reads = Vec::new();
            let dfr0 = u64::from(implemented_count - 1) << 12;
            let state = capture_arm64_vcpu_breakpoint_register_state_with(|register| {
                reads.push(register);
                if register == HvfSystemRegister::ID_AA64DFR0_EL1 {
                    Ok(dfr0)
                } else {
                    Ok(0xb00_0000_0000_0000 | u64::from(register.raw()))
                }
            })
            .expect("breakpoint-register capture should succeed");

            let mut expected_reads = vec![HvfSystemRegister::ID_AA64DFR0_EL1];
            for index in 0..implemented_count {
                expected_reads.push(
                    HvfSystemRegister::debug_breakpoint_value(index)
                        .expect("implemented value slot should be mapped"),
                );
                expected_reads.push(
                    HvfSystemRegister::debug_breakpoint_control(index)
                        .expect("implemented control slot should be mapped"),
                );
            }
            assert_eq!(reads, expected_reads);
            assert_eq!(state.implemented_breakpoint_count(), implemented_count);
            assert_eq!(
                state.breakpoint_value_registers().len(),
                usize::from(implemented_count)
            );
            assert_eq!(
                state.breakpoint_control_registers().len(),
                usize::from(implemented_count)
            );
            for index in 0..implemented_count {
                assert_eq!(
                    state.breakpoint_value_register(index),
                    Some(
                        0xb00_0000_0000_0000
                            | u64::from(
                                HvfSystemRegister::debug_breakpoint_value(index)
                                    .expect("implemented value slot should be mapped")
                                    .raw()
                            )
                    )
                );
                assert_eq!(
                    state.breakpoint_control_register(index),
                    Some(
                        0xb00_0000_0000_0000
                            | u64::from(
                                HvfSystemRegister::debug_breakpoint_control(index)
                                    .expect("implemented control slot should be mapped")
                                    .raw()
                            )
                    )
                );
            }
            assert_eq!(state.breakpoint_value_register(implemented_count), None);
            assert_eq!(state.breakpoint_control_register(implemented_count), None);
        }
    }

    #[test]
    fn maps_all_arm64_debug_watchpoint_register_slots() {
        for index in 0_u8..16 {
            assert_eq!(
                HvfSystemRegister::debug_watchpoint_value(index)
                    .expect("watchpoint value slot should be mapped")
                    .raw(),
                0x8006 + u16::from(index) * 8
            );
            assert_eq!(
                HvfSystemRegister::debug_watchpoint_control(index)
                    .expect("watchpoint control slot should be mapped")
                    .raw(),
                0x8007 + u16::from(index) * 8
            );
        }

        assert_eq!(HvfSystemRegister::debug_watchpoint_value(16), None);
        assert_eq!(HvfSystemRegister::debug_watchpoint_control(16), None);
        assert_eq!(HvfSystemRegister::debug_watchpoint_value(u8::MAX), None);
        assert_eq!(HvfSystemRegister::debug_watchpoint_control(u8::MAX), None);
    }

    #[test]
    fn captures_implemented_arm64_watchpoint_register_pairs_in_order() {
        for implemented_count in [1_u8, 3, 16] {
            let mut reads = Vec::new();
            let dfr0 = u64::from(implemented_count - 1) << 20;
            let state = capture_arm64_vcpu_watchpoint_register_state_with(|register| {
                reads.push(register);
                if register == HvfSystemRegister::ID_AA64DFR0_EL1 {
                    Ok(dfr0)
                } else {
                    Ok(0xa00_0000_0000_0000 | u64::from(register.raw()))
                }
            })
            .expect("watchpoint-register capture should succeed");

            let mut expected_reads = vec![HvfSystemRegister::ID_AA64DFR0_EL1];
            for index in 0..implemented_count {
                expected_reads.push(
                    HvfSystemRegister::debug_watchpoint_value(index)
                        .expect("implemented value slot should be mapped"),
                );
                expected_reads.push(
                    HvfSystemRegister::debug_watchpoint_control(index)
                        .expect("implemented control slot should be mapped"),
                );
            }
            assert_eq!(reads, expected_reads);
            assert_eq!(state.implemented_watchpoint_count(), implemented_count);
            assert_eq!(
                state.watchpoint_value_registers().len(),
                usize::from(implemented_count)
            );
            assert_eq!(
                state.watchpoint_control_registers().len(),
                usize::from(implemented_count)
            );
            for index in 0..implemented_count {
                assert_eq!(
                    state.watchpoint_value_register(index),
                    Some(
                        0xa00_0000_0000_0000
                            | u64::from(
                                HvfSystemRegister::debug_watchpoint_value(index)
                                    .expect("implemented value slot should be mapped")
                                    .raw()
                            )
                    )
                );
                assert_eq!(
                    state.watchpoint_control_register(index),
                    Some(
                        0xa00_0000_0000_0000
                            | u64::from(
                                HvfSystemRegister::debug_watchpoint_control(index)
                                    .expect("implemented control slot should be mapped")
                                    .raw()
                            )
                    )
                );
            }
            assert_eq!(state.watchpoint_value_register(implemented_count), None);
            assert_eq!(state.watchpoint_control_register(implemented_count), None);
        }
    }

    #[test]
    fn captures_arm64_debug_control_register_state_in_documented_order() {
        let mut reads = Vec::new();

        let state = capture_arm64_vcpu_debug_control_register_state_with(|register| {
            reads.push(register);
            Ok(0xd06_0000_0000_0000 | u64::from(register.raw()))
        })
        .expect("debug-control capture should succeed");

        assert_eq!(
            reads,
            [HvfSystemRegister::MDCCINT_EL1, HvfSystemRegister::MDSCR_EL1,]
        );
        assert_eq!(
            state.mdccint_el1(),
            0xd06_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_MDCCINT_EL1)
        );
        assert_eq!(
            state.mdscr_el1(),
            0xd06_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_MDSCR_EL1)
        );
        assert_eq!(HvfSystemRegister::MDCCINT_EL1.raw(), 0x8010);
        assert_eq!(HvfSystemRegister::MDSCR_EL1.raw(), 0x8012);
    }

    fn debug_control_restore_test_state() -> super::HvfArm64VcpuDebugControlRegisterState {
        super::HvfArm64VcpuDebugControlRegisterState::new(
            0xd06c_0000_0000_8010,
            0xd06c_0000_0000_8012,
        )
    }

    fn debug_control_restore_test_entries(
        state: super::HvfArm64VcpuDebugControlRegisterState,
    ) -> [(HvfSystemRegister, u64); 2] {
        [
            (HvfSystemRegister::MDCCINT_EL1, state.mdccint_el1()),
            (HvfSystemRegister::MDSCR_EL1, state.mdscr_el1()),
        ]
    }

    #[test]
    fn restores_arm64_debug_control_register_state_in_capture_order() {
        let state = debug_control_restore_test_state();
        let expected = debug_control_restore_test_entries(state);
        let mut writes = Vec::new();

        restore_arm64_vcpu_debug_control_register_state_with(&state, |register, value| {
            writes.push((register, value));
            Ok(())
        })
        .expect("debug-control restore should succeed");

        assert_eq!(writes, expected);
    }

    #[test]
    fn every_arm64_debug_control_restore_failure_stops_and_can_retry() {
        use std::error::Error as _;

        let state = debug_control_restore_test_state();
        let expected = debug_control_restore_test_entries(state);

        for (failed_index, (failed_register, _)) in expected.iter().copied().enumerate() {
            let fail_once = Cell::new(true);
            let writes = RefCell::new(Vec::new());
            let write_system_register = |register, value| {
                writes.borrow_mut().push((register, value));
                if register == failed_register && fail_once.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake debug-control restore failed",
                    ))
                } else {
                    Ok(())
                }
            };

            let error = restore_arm64_vcpu_debug_control_register_state_with(
                &state,
                &write_system_register,
            )
            .expect_err("injected debug-control write should fail");
            assert_eq!(error.failed_register(), failed_register);
            assert_eq!(error.completed_writes(), failed_index);
            assert_eq!(
                error.source().map(ToString::to_string),
                Some("invalid backend state: fake debug-control restore failed".to_string())
            );
            assert_eq!(*writes.borrow(), expected[..=failed_index]);
            assert_eq!(
                error.to_string(),
                format!(
                    "failed to restore arm64 system register id {} after {failed_index} successful writes: invalid backend state: fake debug-control restore failed",
                    failed_register.raw()
                )
            );

            writes.borrow_mut().clear();
            restore_arm64_vcpu_debug_control_register_state_with(&state, &write_system_register)
                .expect("complete debug-control restore retry should succeed");
            assert_eq!(*writes.borrow(), expected);
        }
    }

    #[test]
    fn captures_arm64_debug_trap_state_in_documented_order() {
        for (trap_debug_exceptions, trap_debug_reg_accesses) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            let mut reads = Vec::new();
            let state = capture_arm64_vcpu_debug_trap_state_with(
                &mut reads,
                |reads| {
                    reads.push(DebugTrapRead::DebugExceptions);
                    Ok(trap_debug_exceptions)
                },
                |reads| {
                    reads.push(DebugTrapRead::DebugRegisterAccesses);
                    Ok(trap_debug_reg_accesses)
                },
            )
            .expect("debug-trap state capture should succeed");

            assert_eq!(
                reads,
                [
                    DebugTrapRead::DebugExceptions,
                    DebugTrapRead::DebugRegisterAccesses,
                ]
            );
            assert_eq!(state.trap_debug_exceptions(), trap_debug_exceptions);
            assert_eq!(state.trap_debug_reg_accesses(), trap_debug_reg_accesses);
        }
    }

    #[test]
    fn restores_arm64_debug_trap_state_in_documented_order() {
        for (trap_debug_exceptions, trap_debug_reg_accesses) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            let state = super::HvfArm64VcpuDebugTrapState::new(
                trap_debug_exceptions,
                trap_debug_reg_accesses,
            );
            let mut writes = Vec::new();

            restore_arm64_vcpu_debug_trap_state_with(
                &state,
                &mut writes,
                |writes, value| {
                    writes.push(DebugTrapWrite::DebugExceptions(value));
                    Ok(())
                },
                |writes, value| {
                    writes.push(DebugTrapWrite::DebugRegisterAccesses(value));
                    Ok(())
                },
            )
            .expect("debug-trap state restore should succeed");

            assert_eq!(
                writes,
                [
                    DebugTrapWrite::DebugExceptions(trap_debug_exceptions),
                    DebugTrapWrite::DebugRegisterAccesses(trap_debug_reg_accesses),
                ]
            );
        }
    }

    #[test]
    fn captures_all_arm64_sme_pstate_combinations_with_one_read() {
        for (streaming_sve_mode_enabled, za_storage_enabled) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            let reads = Cell::new(0);
            let state = capture_arm64_vcpu_sme_pstate_with(|| {
                reads.set(reads.get() + 1);
                Ok((streaming_sve_mode_enabled, za_storage_enabled))
            })
            .expect("SME PSTATE capture should succeed");

            assert_eq!(reads.get(), 1);
            assert_eq!(
                state.streaming_sve_mode_enabled(),
                streaming_sve_mode_enabled
            );
            assert_eq!(state.za_storage_enabled(), za_storage_enabled);
        }
    }

    #[test]
    fn captures_all_arm64_sme_p_registers_in_order() {
        let maximum_svl_bytes = 24;
        let predicate_width_bytes = 3;
        let allocation_size = Cell::new(None);
        let mut reader = SmePTestReader::active(maximum_svl_bytes);

        let state = capture_sme_p_test_reader(&mut reader, |size| {
            allocation_size.set(Some(size));
            Ok(vec![0; size])
        })
        .expect("SME P-register capture should succeed");

        assert_eq!(
            allocation_size.get(),
            Some(HvfArm64VcpuSmePRegisterState::REGISTER_COUNT * predicate_width_bytes)
        );
        let mut expected_calls = vec![SmePCaptureCall::Pstate, SmePCaptureCall::MaximumSvl];
        expected_calls.extend((0..16).map(|register| SmePCaptureCall::P {
            register,
            length: predicate_width_bytes,
        }));
        assert_eq!(reader.calls, expected_calls);
        assert_eq!(state.maximum_svl_bytes(), maximum_svl_bytes);
        assert_eq!(state.predicate_width_bytes(), predicate_width_bytes);
        assert_eq!(HvfArm64VcpuSmePRegisterState::REGISTER_COUNT, 16);
        for register in 0..HvfArm64VcpuSmePRegisterState::REGISTER_COUNT {
            let register_id = u32::try_from(register).expect("P-register index should fit in u32");
            let expected = (0..predicate_width_bytes)
                .map(|offset| sme_p_test_byte(register_id, offset))
                .collect::<Vec<_>>();
            assert_eq!(state.p_register(register), Some(expected.as_slice()));
        }
        assert_eq!(state.p_register(16), None);
        assert_eq!(state.p_register(usize::MAX), None);
        assert_eq!(state.clone(), state);
        assert_eq!(
            format!("{state:?}"),
            "HvfArm64VcpuSmePRegisterState { registers: \"<redacted>\" }"
        );
    }

    #[test]
    fn inactive_sme_p_capture_stops_before_sizing_or_allocation() {
        let mut reader = SmePTestReader {
            pstate_result: Ok((false, true)),
            maximum_svl_result: Ok(24),
            fail_once_on_register: None,
            calls: Vec::new(),
        };

        assert_eq!(
            capture_sme_p_test_reader(&mut reader, |_| {
                panic!("inactive SME P capture must not allocate")
            }),
            Err(HvfArm64VcpuSmePRegisterCaptureError::StreamingSveModeDisabled)
        );
        assert_eq!(reader.calls, [SmePCaptureCall::Pstate]);
    }

    #[test]
    fn sme_p_capture_preserves_pstate_and_maximum_errors() {
        let pstate_error = BackendError::InvalidState("fake SME PSTATE read failed");
        let mut pstate_reader = SmePTestReader {
            pstate_result: Err(pstate_error.clone()),
            maximum_svl_result: Ok(24),
            fail_once_on_register: None,
            calls: Vec::new(),
        };
        assert_eq!(
            capture_sme_p_test_reader(&mut pstate_reader, |_| {
                panic!("failed SME PSTATE must not allocate")
            }),
            Err(HvfArm64VcpuSmePRegisterCaptureError::Backend(pstate_error))
        );
        assert_eq!(pstate_reader.calls, [SmePCaptureCall::Pstate]);

        let maximum_error = BackendError::InvalidState("fake maximum-SVL query failed");
        let mut maximum_reader = SmePTestReader {
            pstate_result: Ok((true, false)),
            maximum_svl_result: Err(maximum_error.clone()),
            fail_once_on_register: None,
            calls: Vec::new(),
        };
        assert_eq!(
            capture_sme_p_test_reader(&mut maximum_reader, |_| {
                panic!("failed maximum-SVL query must not allocate")
            }),
            Err(HvfArm64VcpuSmePRegisterCaptureError::Backend(maximum_error))
        );
        assert_eq!(
            maximum_reader.calls,
            [SmePCaptureCall::Pstate, SmePCaptureCall::MaximumSvl]
        );
    }

    #[test]
    fn sme_p_capture_rejects_invalid_sizes_and_allocation_failure() {
        let mut zero_reader = SmePTestReader::active(0);
        assert_eq!(
            capture_sme_p_test_reader(&mut zero_reader, |_| {
                panic!("zero maximum SVL must not allocate")
            }),
            Err(HvfArm64VcpuSmePRegisterCaptureError::ZeroMaximumSvl)
        );
        assert_eq!(
            zero_reader.calls,
            [SmePCaptureCall::Pstate, SmePCaptureCall::MaximumSvl]
        );

        let non_divisible_maximum = 23;
        let mut non_divisible_reader = SmePTestReader::active(non_divisible_maximum);
        assert_eq!(
            capture_sme_p_test_reader(&mut non_divisible_reader, |_| {
                panic!("non-divisible maximum SVL must not allocate")
            }),
            Err(
                HvfArm64VcpuSmePRegisterCaptureError::MaximumSvlNotDivisibleByEight {
                    maximum_svl_bytes: non_divisible_maximum
                }
            )
        );
        assert_eq!(
            non_divisible_reader.calls,
            [SmePCaptureCall::Pstate, SmePCaptureCall::MaximumSvl]
        );

        let overflowing_maximum = usize::MAX - 7;
        let mut overflow_reader = SmePTestReader::active(overflowing_maximum);
        assert_eq!(
            capture_sme_p_test_reader(&mut overflow_reader, |_| {
                panic!("overflowing SME P capture must not allocate")
            }),
            Err(HvfArm64VcpuSmePRegisterCaptureError::CaptureSizeOverflow {
                maximum_svl_bytes: overflowing_maximum
            })
        );
        assert_eq!(
            overflow_reader.calls,
            [SmePCaptureCall::Pstate, SmePCaptureCall::MaximumSvl]
        );

        let maximum_svl_bytes = 72;
        let allocation_size = 16 * (maximum_svl_bytes / 8);
        let mut allocation_reader = SmePTestReader::active(maximum_svl_bytes);
        assert_eq!(
            capture_sme_p_test_reader(&mut allocation_reader, |size| {
                assert_eq!(size, allocation_size);
                Err(HvfArm64VcpuSmePRegisterCaptureError::AllocationFailed { size })
            }),
            Err(HvfArm64VcpuSmePRegisterCaptureError::AllocationFailed {
                size: allocation_size
            })
        );
        assert_eq!(
            allocation_reader.calls,
            [SmePCaptureCall::Pstate, SmePCaptureCall::MaximumSvl]
        );
    }

    #[test]
    fn every_sme_p_register_failure_stops_without_partial_state_and_can_retry() {
        let maximum_svl_bytes = 24;
        let predicate_width_bytes = maximum_svl_bytes / 8;

        for failed_register in 0..16 {
            let mut reader = SmePTestReader::active(maximum_svl_bytes);
            reader.fail_once_on_register = Some(failed_register);

            assert_eq!(
                capture_sme_p_test_reader(&mut reader, |size| Ok(vec![0; size])),
                Err(HvfArm64VcpuSmePRegisterCaptureError::Backend(
                    BackendError::InvalidState("fake SME P-register read failed")
                ))
            );
            let expected_failed_calls = 2 + usize::try_from(failed_register).unwrap() + 1;
            assert_eq!(reader.calls.len(), expected_failed_calls);
            assert_eq!(
                reader.calls.last(),
                Some(&SmePCaptureCall::P {
                    register: failed_register,
                    length: predicate_width_bytes,
                })
            );

            reader.calls.clear();
            let state = capture_sme_p_test_reader(&mut reader, |size| Ok(vec![0; size]))
                .expect("SME P-register capture retry should succeed");
            assert_eq!(state.maximum_svl_bytes(), maximum_svl_bytes);
            assert_eq!(state.predicate_width_bytes(), predicate_width_bytes);
            assert_eq!(reader.calls.len(), 2 + 16);
        }
    }

    #[test]
    fn displays_sme_p_capture_errors_and_preserves_backend_source() {
        use std::error::Error as _;

        let backend = HvfArm64VcpuSmePRegisterCaptureError::Backend(BackendError::InvalidState(
            "fake SME P backend failure",
        ));
        assert_eq!(
            backend.to_string(),
            "invalid backend state: fake SME P backend failure"
        );
        assert_eq!(
            backend.source().map(ToString::to_string),
            Some("invalid backend state: fake SME P backend failure".to_string())
        );
        assert_eq!(
            HvfArm64VcpuSmePRegisterCaptureError::StreamingSveModeDisabled.to_string(),
            "cannot capture SME P registers while streaming SVE mode is disabled"
        );
        assert_eq!(
            HvfArm64VcpuSmePRegisterCaptureError::ZeroMaximumSvl.to_string(),
            "Hypervisor.framework reported a zero maximum streaming vector length"
        );
        assert_eq!(
            HvfArm64VcpuSmePRegisterCaptureError::MaximumSvlNotDivisibleByEight {
                maximum_svl_bytes: 23
            }
            .to_string(),
            "maximum SVL 23 bytes is not divisible by 8 for SME P-register capture"
        );
        assert_eq!(
            HvfArm64VcpuSmePRegisterCaptureError::CaptureSizeOverflow {
                maximum_svl_bytes: usize::MAX - 7
            }
            .to_string(),
            format!(
                "SME P-register capture size overflows for maximum SVL {} bytes",
                usize::MAX - 7
            )
        );
        assert_eq!(
            HvfArm64VcpuSmePRegisterCaptureError::AllocationFailed { size: 2048 }.to_string(),
            "failed to allocate 2048 bytes for SME P-register capture"
        );
    }

    #[test]
    fn captures_all_arm64_sme_z_registers_in_order() {
        let maximum_svl_bytes = 5;
        let allocation_size = Cell::new(None);
        let mut reader = SmeZTestReader::active(maximum_svl_bytes);

        let state = capture_sme_z_test_reader(&mut reader, |size| {
            allocation_size.set(Some(size));
            Ok(vec![0; size])
        })
        .expect("SME Z-register capture should succeed");

        assert_eq!(
            allocation_size.get(),
            Some(HvfArm64VcpuSmeZRegisterState::REGISTER_COUNT * maximum_svl_bytes)
        );
        let mut expected_calls = vec![SmeZCaptureCall::Pstate, SmeZCaptureCall::MaximumSvl];
        expected_calls.extend((0..32).map(|register| SmeZCaptureCall::Z {
            register,
            length: maximum_svl_bytes,
        }));
        assert_eq!(reader.calls, expected_calls);
        assert_eq!(state.maximum_svl_bytes(), maximum_svl_bytes);
        assert_eq!(HvfArm64VcpuSmeZRegisterState::REGISTER_COUNT, 32);
        for register in 0..HvfArm64VcpuSmeZRegisterState::REGISTER_COUNT {
            let register_id = u32::try_from(register).expect("Z-register index should fit in u32");
            let expected = (0..maximum_svl_bytes)
                .map(|offset| sme_z_test_byte(register_id, offset))
                .collect::<Vec<_>>();
            assert_eq!(state.z_register(register), Some(expected.as_slice()));
        }
        assert_eq!(state.z_register(32), None);
        assert_eq!(state.z_register(usize::MAX), None);
        assert!(format!("{state:?}").contains("<redacted>"));
    }

    #[test]
    fn inactive_sme_z_capture_stops_before_sizing_or_allocation() {
        let mut reader = SmeZTestReader {
            pstate_result: Ok((false, true)),
            maximum_svl_result: Ok(16),
            fail_once_on_register: None,
            calls: Vec::new(),
        };

        assert_eq!(
            capture_sme_z_test_reader(&mut reader, |_| {
                panic!("inactive SME Z capture must not allocate")
            }),
            Err(HvfArm64VcpuSmeZRegisterCaptureError::StreamingSveModeDisabled)
        );
        assert_eq!(reader.calls, [SmeZCaptureCall::Pstate]);
    }

    #[test]
    fn sme_z_capture_preserves_pstate_and_maximum_errors() {
        let pstate_error = BackendError::InvalidState("fake SME PSTATE read failed");
        let mut pstate_reader = SmeZTestReader {
            pstate_result: Err(pstate_error.clone()),
            maximum_svl_result: Ok(16),
            fail_once_on_register: None,
            calls: Vec::new(),
        };
        assert_eq!(
            capture_sme_z_test_reader(&mut pstate_reader, |_| {
                panic!("failed SME PSTATE must not allocate")
            }),
            Err(HvfArm64VcpuSmeZRegisterCaptureError::Backend(pstate_error))
        );
        assert_eq!(pstate_reader.calls, [SmeZCaptureCall::Pstate]);

        let maximum_error = BackendError::InvalidState("fake maximum-SVL query failed");
        let mut maximum_reader = SmeZTestReader {
            pstate_result: Ok((true, false)),
            maximum_svl_result: Err(maximum_error.clone()),
            fail_once_on_register: None,
            calls: Vec::new(),
        };
        assert_eq!(
            capture_sme_z_test_reader(&mut maximum_reader, |_| {
                panic!("failed maximum-SVL query must not allocate")
            }),
            Err(HvfArm64VcpuSmeZRegisterCaptureError::Backend(maximum_error))
        );
        assert_eq!(
            maximum_reader.calls,
            [SmeZCaptureCall::Pstate, SmeZCaptureCall::MaximumSvl]
        );
    }

    #[test]
    fn sme_z_capture_rejects_zero_overflow_and_allocation_failure() {
        let mut zero_reader = SmeZTestReader::active(0);
        assert_eq!(
            capture_sme_z_test_reader(&mut zero_reader, |_| {
                panic!("zero maximum SVL must not allocate")
            }),
            Err(HvfArm64VcpuSmeZRegisterCaptureError::ZeroMaximumSvl)
        );
        assert_eq!(
            zero_reader.calls,
            [SmeZCaptureCall::Pstate, SmeZCaptureCall::MaximumSvl]
        );

        let overflowing_maximum = usize::MAX / 32 + 1;
        let mut overflow_reader = SmeZTestReader::active(overflowing_maximum);
        assert_eq!(
            capture_sme_z_test_reader(&mut overflow_reader, |_| {
                panic!("overflowing SME Z capture must not allocate")
            }),
            Err(HvfArm64VcpuSmeZRegisterCaptureError::CaptureSizeOverflow {
                maximum_svl_bytes: overflowing_maximum
            })
        );
        assert_eq!(
            overflow_reader.calls,
            [SmeZCaptureCall::Pstate, SmeZCaptureCall::MaximumSvl]
        );

        let maximum_svl_bytes = 9;
        let allocation_size = 32 * maximum_svl_bytes;
        let mut allocation_reader = SmeZTestReader::active(maximum_svl_bytes);
        assert_eq!(
            capture_sme_z_test_reader(&mut allocation_reader, |size| {
                assert_eq!(size, allocation_size);
                Err(HvfArm64VcpuSmeZRegisterCaptureError::AllocationFailed { size })
            }),
            Err(HvfArm64VcpuSmeZRegisterCaptureError::AllocationFailed {
                size: allocation_size
            })
        );
        assert_eq!(
            allocation_reader.calls,
            [SmeZCaptureCall::Pstate, SmeZCaptureCall::MaximumSvl]
        );
    }

    #[test]
    fn every_sme_z_register_failure_stops_without_partial_state_and_can_retry() {
        let maximum_svl_bytes = 3;

        for failed_register in 0..32 {
            let mut reader = SmeZTestReader::active(maximum_svl_bytes);
            reader.fail_once_on_register = Some(failed_register);

            assert_eq!(
                capture_sme_z_test_reader(&mut reader, |size| Ok(vec![0; size])),
                Err(HvfArm64VcpuSmeZRegisterCaptureError::Backend(
                    BackendError::InvalidState("fake SME Z-register read failed")
                ))
            );
            let expected_failed_calls = 2 + usize::try_from(failed_register).unwrap() + 1;
            assert_eq!(reader.calls.len(), expected_failed_calls);
            assert_eq!(
                reader.calls.last(),
                Some(&SmeZCaptureCall::Z {
                    register: failed_register,
                    length: maximum_svl_bytes,
                })
            );

            reader.calls.clear();
            let state = capture_sme_z_test_reader(&mut reader, |size| Ok(vec![0; size]))
                .expect("SME Z-register capture retry should succeed");
            assert_eq!(state.maximum_svl_bytes(), maximum_svl_bytes);
            assert_eq!(reader.calls.len(), 2 + 32);
        }
    }

    #[test]
    fn displays_sme_z_capture_errors_and_preserves_backend_source() {
        use std::error::Error as _;

        let backend = HvfArm64VcpuSmeZRegisterCaptureError::Backend(BackendError::InvalidState(
            "fake SME Z backend failure",
        ));
        assert_eq!(
            backend.to_string(),
            "invalid backend state: fake SME Z backend failure"
        );
        assert_eq!(
            backend.source().map(ToString::to_string),
            Some("invalid backend state: fake SME Z backend failure".to_string())
        );
        assert_eq!(
            HvfArm64VcpuSmeZRegisterCaptureError::StreamingSveModeDisabled.to_string(),
            "cannot capture SME Z registers while streaming SVE mode is disabled"
        );
        assert_eq!(
            HvfArm64VcpuSmeZRegisterCaptureError::ZeroMaximumSvl.to_string(),
            "Hypervisor.framework reported a zero maximum streaming vector length"
        );
        assert_eq!(
            HvfArm64VcpuSmeZRegisterCaptureError::CaptureSizeOverflow {
                maximum_svl_bytes: usize::MAX
            }
            .to_string(),
            format!(
                "SME Z-register capture size overflows for maximum SVL {} bytes",
                usize::MAX
            )
        );
        assert_eq!(
            HvfArm64VcpuSmeZRegisterCaptureError::AllocationFailed { size: 4096 }.to_string(),
            "failed to allocate 4096 bytes for SME Z-register capture"
        );
    }

    #[test]
    fn captures_complete_arm64_sme_za_register_without_streaming_mode() {
        let maximum_svl_bytes = 3;
        let capture_size = maximum_svl_bytes * maximum_svl_bytes;
        let allocation_size = Cell::new(None);
        let mut reader = SmeZaTestReader::active(maximum_svl_bytes);

        let state = capture_sme_za_test_reader(&mut reader, |size| {
            allocation_size.set(Some(size));
            Ok(vec![0; size])
        })
        .expect("SME ZA-register capture should succeed");

        assert_eq!(allocation_size.get(), Some(capture_size));
        assert_eq!(
            reader.calls,
            [
                SmeZaCaptureCall::Pstate,
                SmeZaCaptureCall::MaximumSvl,
                SmeZaCaptureCall::Za {
                    length: capture_size
                }
            ]
        );
        let expected = (0..capture_size).map(sme_za_test_byte).collect::<Vec<_>>();
        assert_eq!(state.maximum_svl_bytes(), maximum_svl_bytes);
        assert_eq!(state.as_bytes(), expected);
        assert_eq!(state.len(), capture_size);
        assert!(!state.is_empty());
        assert_eq!(state.clone(), state);
        assert_eq!(
            format!("{state:?}"),
            "HvfArm64VcpuSmeZaRegisterState { register: \"<redacted>\" }"
        );
    }

    #[test]
    fn inactive_sme_za_capture_stops_before_sizing_or_allocation() {
        for streaming_sve_mode_enabled in [false, true] {
            let mut reader = SmeZaTestReader {
                pstate_result: Ok((streaming_sve_mode_enabled, false)),
                maximum_svl_result: Ok(3),
                fail_once: false,
                calls: Vec::new(),
            };

            assert_eq!(
                capture_sme_za_test_reader(&mut reader, |_| {
                    panic!("inactive SME ZA capture must not allocate")
                }),
                Err(HvfArm64VcpuSmeZaRegisterCaptureError::ZaStorageDisabled)
            );
            assert_eq!(reader.calls, [SmeZaCaptureCall::Pstate]);
        }
    }

    #[test]
    fn sme_za_capture_preserves_pstate_and_maximum_errors() {
        let pstate_error = BackendError::InvalidState("fake SME PSTATE read failed");
        let mut pstate_reader = SmeZaTestReader {
            pstate_result: Err(pstate_error.clone()),
            maximum_svl_result: Ok(3),
            fail_once: false,
            calls: Vec::new(),
        };
        assert_eq!(
            capture_sme_za_test_reader(&mut pstate_reader, |_| {
                panic!("failed SME PSTATE must not allocate")
            }),
            Err(HvfArm64VcpuSmeZaRegisterCaptureError::Backend(pstate_error))
        );
        assert_eq!(pstate_reader.calls, [SmeZaCaptureCall::Pstate]);

        let maximum_error = BackendError::InvalidState("fake maximum-SVL query failed");
        let mut maximum_reader = SmeZaTestReader {
            pstate_result: Ok((false, true)),
            maximum_svl_result: Err(maximum_error.clone()),
            fail_once: false,
            calls: Vec::new(),
        };
        assert_eq!(
            capture_sme_za_test_reader(&mut maximum_reader, |_| {
                panic!("failed maximum-SVL query must not allocate")
            }),
            Err(HvfArm64VcpuSmeZaRegisterCaptureError::Backend(
                maximum_error
            ))
        );
        assert_eq!(
            maximum_reader.calls,
            [SmeZaCaptureCall::Pstate, SmeZaCaptureCall::MaximumSvl]
        );
    }

    #[test]
    fn sme_za_capture_rejects_zero_overflow_and_allocation_failures() {
        let mut zero_reader = SmeZaTestReader::active(0);
        assert_eq!(
            capture_sme_za_test_reader(&mut zero_reader, |_| {
                panic!("zero maximum SVL must not allocate")
            }),
            Err(HvfArm64VcpuSmeZaRegisterCaptureError::ZeroMaximumSvl)
        );
        assert_eq!(
            zero_reader.calls,
            [SmeZaCaptureCall::Pstate, SmeZaCaptureCall::MaximumSvl]
        );

        let overflowing_maximum = usize::MAX;
        let mut overflow_reader = SmeZaTestReader::active(overflowing_maximum);
        assert_eq!(
            capture_sme_za_test_reader(&mut overflow_reader, |_| {
                panic!("overflowing SME ZA capture must not allocate")
            }),
            Err(HvfArm64VcpuSmeZaRegisterCaptureError::CaptureSizeOverflow {
                maximum_svl_bytes: overflowing_maximum
            })
        );
        assert_eq!(
            overflow_reader.calls,
            [SmeZaCaptureCall::Pstate, SmeZaCaptureCall::MaximumSvl]
        );

        let maximum_svl_bytes = 4;
        let capture_size = maximum_svl_bytes * maximum_svl_bytes;
        let mut allocation_reader = SmeZaTestReader::active(maximum_svl_bytes);
        assert_eq!(
            capture_sme_za_test_reader(&mut allocation_reader, |size| {
                assert_eq!(size, capture_size);
                Err(HvfArm64VcpuSmeZaRegisterCaptureError::AllocationFailed { size })
            }),
            Err(HvfArm64VcpuSmeZaRegisterCaptureError::AllocationFailed { size: capture_size })
        );
        assert_eq!(
            allocation_reader.calls,
            [SmeZaCaptureCall::Pstate, SmeZaCaptureCall::MaximumSvl]
        );

        let mut short_reader = SmeZaTestReader::active(maximum_svl_bytes);
        assert_eq!(
            capture_sme_za_test_reader(&mut short_reader, |size| Ok(vec![0; size - 1])),
            Err(HvfArm64VcpuSmeZaRegisterCaptureError::AllocationFailed { size: capture_size })
        );
        assert_eq!(
            short_reader.calls,
            [SmeZaCaptureCall::Pstate, SmeZaCaptureCall::MaximumSvl]
        );
    }

    #[test]
    fn failed_sme_za_register_capture_publishes_nothing_and_can_retry() {
        let maximum_svl_bytes = 3;
        let capture_size = maximum_svl_bytes * maximum_svl_bytes;
        let mut reader = SmeZaTestReader::active(maximum_svl_bytes);
        reader.fail_once = true;

        assert_eq!(
            capture_sme_za_test_reader(&mut reader, |size| Ok(vec![0; size])),
            Err(HvfArm64VcpuSmeZaRegisterCaptureError::Backend(
                BackendError::InvalidState("fake SME ZA-register read failed")
            ))
        );
        assert_eq!(
            reader.calls,
            [
                SmeZaCaptureCall::Pstate,
                SmeZaCaptureCall::MaximumSvl,
                SmeZaCaptureCall::Za {
                    length: capture_size
                }
            ]
        );

        reader.calls.clear();
        let state = capture_sme_za_test_reader(&mut reader, |size| Ok(vec![0; size]))
            .expect("SME ZA-register capture retry should succeed");
        assert_eq!(state.maximum_svl_bytes(), maximum_svl_bytes);
        assert_eq!(state.len(), capture_size);
        assert_eq!(
            reader.calls,
            [
                SmeZaCaptureCall::Pstate,
                SmeZaCaptureCall::MaximumSvl,
                SmeZaCaptureCall::Za {
                    length: capture_size
                }
            ]
        );
    }

    #[test]
    fn displays_sme_za_capture_errors_and_preserves_backend_source() {
        use std::error::Error as _;

        let backend = HvfArm64VcpuSmeZaRegisterCaptureError::Backend(BackendError::InvalidState(
            "fake SME ZA backend failure",
        ));
        assert_eq!(
            backend.to_string(),
            "invalid backend state: fake SME ZA backend failure"
        );
        assert_eq!(
            backend.source().map(ToString::to_string),
            Some("invalid backend state: fake SME ZA backend failure".to_string())
        );
        assert_eq!(
            HvfArm64VcpuSmeZaRegisterCaptureError::ZaStorageDisabled.to_string(),
            "cannot capture the SME ZA register while ZA storage is disabled"
        );
        assert_eq!(
            HvfArm64VcpuSmeZaRegisterCaptureError::ZeroMaximumSvl.to_string(),
            "Hypervisor.framework reported a zero maximum streaming vector length"
        );
        assert_eq!(
            HvfArm64VcpuSmeZaRegisterCaptureError::CaptureSizeOverflow {
                maximum_svl_bytes: usize::MAX
            }
            .to_string(),
            format!(
                "SME ZA-register capture size overflows for maximum SVL {} bytes",
                usize::MAX
            )
        );
        assert_eq!(
            HvfArm64VcpuSmeZaRegisterCaptureError::AllocationFailed { size: 4096 }.to_string(),
            "failed to allocate 4096 bytes for SME ZA-register capture"
        );
    }

    #[test]
    fn captures_complete_arm64_sme_zt0_register_for_both_streaming_modes() {
        for streaming_sve_mode_enabled in [false, true] {
            let mut reader = SmeZt0TestReader {
                pstate_result: Ok((streaming_sve_mode_enabled, true)),
                fail_once: false,
                calls: Vec::new(),
            };

            let state = capture_sme_zt0_test_reader(&mut reader)
                .expect("SME ZT0-register capture should succeed");

            assert_eq!(
                reader.calls,
                [SmeZt0CaptureCall::Pstate, SmeZt0CaptureCall::Zt0]
            );
            assert_eq!(state.as_bytes(), &std::array::from_fn(sme_zt0_test_byte));
            assert_eq!(
                state.as_bytes().len(),
                HvfArm64VcpuSmeZt0RegisterState::BYTE_COUNT
            );
            assert_eq!(state.clone(), state);
            assert_eq!(
                format!("{state:?}"),
                "HvfArm64VcpuSmeZt0RegisterState { register: \"<redacted>\" }"
            );
        }
    }

    #[test]
    fn inactive_sme_zt0_capture_stops_before_register_read() {
        for streaming_sve_mode_enabled in [false, true] {
            let mut reader = SmeZt0TestReader {
                pstate_result: Ok((streaming_sve_mode_enabled, false)),
                fail_once: false,
                calls: Vec::new(),
            };

            assert_eq!(
                capture_sme_zt0_test_reader(&mut reader),
                Err(HvfArm64VcpuSmeZt0RegisterCaptureError::ZaStorageDisabled)
            );
            assert_eq!(reader.calls, [SmeZt0CaptureCall::Pstate]);
        }
    }

    #[test]
    fn sme_zt0_capture_preserves_pstate_failure() {
        let pstate_error = BackendError::InvalidState("fake SME PSTATE read failed");
        let mut reader = SmeZt0TestReader {
            pstate_result: Err(pstate_error.clone()),
            fail_once: false,
            calls: Vec::new(),
        };

        assert_eq!(
            capture_sme_zt0_test_reader(&mut reader),
            Err(HvfArm64VcpuSmeZt0RegisterCaptureError::Backend(
                pstate_error
            ))
        );
        assert_eq!(reader.calls, [SmeZt0CaptureCall::Pstate]);
    }

    #[test]
    fn failed_sme_zt0_register_capture_publishes_nothing_and_can_retry() {
        let mut reader = SmeZt0TestReader {
            pstate_result: Ok((false, true)),
            fail_once: true,
            calls: Vec::new(),
        };

        assert_eq!(
            capture_sme_zt0_test_reader(&mut reader),
            Err(HvfArm64VcpuSmeZt0RegisterCaptureError::Backend(
                BackendError::InvalidState("fake SME ZT0-register read failed")
            ))
        );
        assert_eq!(
            reader.calls,
            [SmeZt0CaptureCall::Pstate, SmeZt0CaptureCall::Zt0]
        );

        reader.calls.clear();
        let state = capture_sme_zt0_test_reader(&mut reader)
            .expect("SME ZT0-register capture retry should succeed");
        assert_eq!(state.as_bytes(), &std::array::from_fn(sme_zt0_test_byte));
        assert_eq!(
            reader.calls,
            [SmeZt0CaptureCall::Pstate, SmeZt0CaptureCall::Zt0]
        );
    }

    #[test]
    fn displays_sme_zt0_capture_errors_and_preserves_backend_source() {
        use std::error::Error as _;

        let backend = HvfArm64VcpuSmeZt0RegisterCaptureError::Backend(BackendError::InvalidState(
            "fake SME ZT0 backend failure",
        ));
        assert_eq!(
            backend.to_string(),
            "invalid backend state: fake SME ZT0 backend failure"
        );
        assert_eq!(
            backend.source().map(ToString::to_string),
            Some("invalid backend state: fake SME ZT0 backend failure".to_string())
        );
        assert_eq!(
            HvfArm64VcpuSmeZt0RegisterCaptureError::ZaStorageDisabled.to_string(),
            "cannot capture the SME ZT0 register while ZA storage is disabled"
        );
    }

    #[test]
    fn captures_arm64_sme_system_register_state_in_documented_order() {
        let expected_registers = sme_system_registers();
        let expected_values = [0, u64::MAX, 0x0123_4567_89ab_cdef];
        let mut reads = Vec::new();

        let state = capture_arm64_vcpu_sme_system_register_state_with(|register| {
            reads.push(register);
            expected_registers
                .iter()
                .position(|expected| *expected == register)
                .map(|index| expected_values[index])
                .ok_or(BackendError::InvalidState(
                    "unexpected fake SME system register",
                ))
        })
        .expect("SME system-register capture should succeed");

        assert_eq!(reads, expected_registers);
        assert_eq!(state.smcr_el1(), expected_values[0]);
        assert_eq!(state.smpri_el1(), expected_values[1]);
        assert_eq!(state.tpidr2_el0(), expected_values[2]);
        assert_eq!(HvfSystemRegister::SMPRI_EL1.raw(), 0xc094);
        assert_eq!(HvfSystemRegister::SMCR_EL1.raw(), 0xc096);
        assert_eq!(HvfSystemRegister::TPIDR2_EL0.raw(), 0xde85);
    }

    #[test]
    fn arm64_sme_system_register_state_debug_redacts_values() {
        let state = super::HvfArm64VcpuSmeSystemRegisterState::new(
            0x0123_4567_89ab_cdef,
            0xfedc_ba98_7654_3210,
            0x8877_6655_4433_2211,
        );

        let debug = format!("{state:?}");
        assert_eq!(
            debug,
            "HvfArm64VcpuSmeSystemRegisterState { registers: \"<redacted>\" }"
        );
        assert!(!debug.contains("0123"));
        assert!(!debug.contains("fedc"));
        assert!(!debug.contains("8877"));
    }

    #[test]
    fn captures_arm64_system_context_register_state_in_documented_order() {
        let expected_registers = system_context_registers();
        let expected_values = [0, u64::MAX];
        let mut reads = Vec::new();

        let state = capture_arm64_vcpu_system_context_register_state_with(|register| {
            reads.push(register);
            expected_registers
                .iter()
                .position(|expected| *expected == register)
                .map(|index| expected_values[index])
                .ok_or(BackendError::InvalidState(
                    "unexpected fake system-context register",
                ))
        })
        .expect("system-context register capture should succeed");

        assert_eq!(reads, expected_registers);
        assert_eq!(state.scxtnum_el0(), expected_values[0]);
        assert_eq!(state.scxtnum_el1(), expected_values[1]);
        assert_eq!(HvfSystemRegister::SCXTNUM_EL0.raw(), 0xde87);
        assert_eq!(HvfSystemRegister::SCXTNUM_EL1.raw(), 0xc687);
    }

    #[test]
    fn arm64_system_context_register_state_debug_redacts_values() {
        let state = super::HvfArm64VcpuSystemContextRegisterState::new(
            0x0123_4567_89ab_cdef,
            0xfedc_ba98_7654_3210,
        );

        let debug = format!("{state:?}");
        assert_eq!(
            debug,
            "HvfArm64VcpuSystemContextRegisterState { registers: \"<redacted>\" }"
        );
        assert!(!debug.contains("0123"));
        assert!(!debug.contains("fedc"));
    }

    #[test]
    fn restores_arm64_system_context_register_state_in_capture_order() {
        let state = system_context_restore_test_state();
        let expected = system_context_restore_test_entries();
        let mut writes = Vec::new();

        restore_arm64_vcpu_system_context_register_state_with(&state, |register, value| {
            writes.push((register, value));
            Ok(())
        })
        .expect("system-context register restore should succeed");

        assert_eq!(writes, expected);
        assert_eq!(
            format!("{state:?}"),
            "HvfArm64VcpuSystemContextRegisterState { registers: \"<redacted>\" }"
        );
    }

    #[test]
    fn captures_arm64_translation_register_state_in_documented_order() {
        let mut reads = Vec::new();

        let state = capture_arm64_vcpu_translation_register_state_with(|register| {
            reads.push(register);
            Ok(0xa5a5_0000_0000_0000 | u64::from(register.raw()))
        })
        .expect("translation-register capture should succeed");

        assert_eq!(
            reads,
            [
                HvfSystemRegister::SCTLR_EL1,
                HvfSystemRegister::TTBR0_EL1,
                HvfSystemRegister::TTBR1_EL1,
                HvfSystemRegister::TCR_EL1,
                HvfSystemRegister::MAIR_EL1,
                HvfSystemRegister::AMAIR_EL1,
                HvfSystemRegister::CONTEXTIDR_EL1,
            ]
        );
        assert_eq!(
            state.sctlr_el1(),
            0xa5a5_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_SCTLR_EL1)
        );
        assert_eq!(
            state.ttbr0_el1(),
            0xa5a5_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_TTBR0_EL1)
        );
        assert_eq!(
            state.ttbr1_el1(),
            0xa5a5_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_TTBR1_EL1)
        );
        assert_eq!(
            state.tcr_el1(),
            0xa5a5_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_TCR_EL1)
        );
        assert_eq!(
            state.mair_el1(),
            0xa5a5_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_MAIR_EL1)
        );
        assert_eq!(
            state.amair_el1(),
            0xa5a5_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_AMAIR_EL1)
        );
        assert_eq!(
            state.contextidr_el1(),
            0xa5a5_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_CONTEXTIDR_EL1)
        );
        assert_eq!(HvfSystemRegister::SCTLR_EL1.raw(), 0xc080);
        assert_eq!(HvfSystemRegister::TTBR0_EL1.raw(), 0xc100);
        assert_eq!(HvfSystemRegister::TTBR1_EL1.raw(), 0xc101);
        assert_eq!(HvfSystemRegister::TCR_EL1.raw(), 0xc102);
        assert_eq!(HvfSystemRegister::MAIR_EL1.raw(), 0xc510);
        assert_eq!(HvfSystemRegister::AMAIR_EL1.raw(), 0xc518);
        assert_eq!(HvfSystemRegister::CONTEXTIDR_EL1.raw(), 0xc681);
    }

    fn translation_restore_test_state() -> super::HvfArm64VcpuTranslationRegisterState {
        capture_arm64_vcpu_translation_register_state_with(|register| {
            Ok(0xda00_0000_0000_0000 | u64::from(register.raw()))
        })
        .expect("translation test state should be captured")
    }

    fn translation_restore_test_entries(
        state: super::HvfArm64VcpuTranslationRegisterState,
    ) -> [(HvfSystemRegister, u64); 7] {
        [
            (HvfSystemRegister::SCTLR_EL1, state.sctlr_el1()),
            (HvfSystemRegister::TTBR0_EL1, state.ttbr0_el1()),
            (HvfSystemRegister::TTBR1_EL1, state.ttbr1_el1()),
            (HvfSystemRegister::TCR_EL1, state.tcr_el1()),
            (HvfSystemRegister::MAIR_EL1, state.mair_el1()),
            (HvfSystemRegister::AMAIR_EL1, state.amair_el1()),
            (HvfSystemRegister::CONTEXTIDR_EL1, state.contextidr_el1()),
        ]
    }

    #[test]
    fn restores_arm64_translation_register_state_in_capture_order() {
        let state = translation_restore_test_state();
        let expected = translation_restore_test_entries(state);
        let mut writes = Vec::new();

        restore_arm64_vcpu_translation_register_state_with(&state, |register, value| {
            writes.push((register, value));
            Ok(())
        })
        .expect("translation restore should succeed");

        assert_eq!(writes, expected);
    }

    #[test]
    fn every_arm64_translation_restore_failure_stops_and_can_retry() {
        use std::error::Error as _;

        let state = translation_restore_test_state();
        let expected = translation_restore_test_entries(state);

        for (failed_index, (failed_register, _)) in expected.iter().copied().enumerate() {
            let fail_once = Cell::new(true);
            let writes = RefCell::new(Vec::new());
            let write_system_register = |register, value| {
                writes.borrow_mut().push((register, value));
                if register == failed_register && fail_once.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake translation restore failed",
                    ))
                } else {
                    Ok(())
                }
            };

            let error =
                restore_arm64_vcpu_translation_register_state_with(&state, &write_system_register)
                    .expect_err("injected translation write should fail");
            assert_eq!(error.failed_register(), failed_register);
            assert_eq!(error.completed_writes(), failed_index);
            assert_eq!(
                error.source().map(ToString::to_string),
                Some("invalid backend state: fake translation restore failed".to_string())
            );
            assert_eq!(*writes.borrow(), expected[..=failed_index]);
            assert_eq!(
                error.to_string(),
                format!(
                    "failed to restore arm64 system register id {} after {failed_index} successful writes: invalid backend state: fake translation restore failed",
                    failed_register.raw()
                )
            );

            writes.borrow_mut().clear();
            restore_arm64_vcpu_translation_register_state_with(&state, &write_system_register)
                .expect("complete translation restore retry should succeed");
            assert_eq!(*writes.borrow(), expected);
        }
    }

    #[test]
    fn captures_arm64_identification_register_state_in_documented_order() {
        let mut reads = Vec::new();

        let state = capture_arm64_vcpu_identification_register_state_with(|register| {
            reads.push(register);
            Ok(identification_test_value(register))
        })
        .expect("identification-register capture should succeed");

        let registers = identification_registers();
        assert_eq!(reads, registers);
        assert_eq!(state.midr_el1(), identification_test_value(registers[0]));
        assert_eq!(state.mpidr_el1(), identification_test_value(registers[1]));
        assert_eq!(
            state.id_aa64pfr0_el1(),
            identification_test_value(registers[2])
        );
        assert_eq!(
            state.id_aa64pfr1_el1(),
            identification_test_value(registers[3])
        );
        assert_eq!(
            state.id_aa64dfr0_el1(),
            identification_test_value(registers[4])
        );
        assert_eq!(
            state.id_aa64dfr1_el1(),
            identification_test_value(registers[5])
        );
        assert_eq!(
            state.id_aa64isar0_el1(),
            identification_test_value(registers[6])
        );
        assert_eq!(
            state.id_aa64isar1_el1(),
            identification_test_value(registers[7])
        );
        assert_eq!(
            state.id_aa64mmfr0_el1(),
            identification_test_value(registers[8])
        );
        assert_eq!(
            state.id_aa64mmfr1_el1(),
            identification_test_value(registers[9])
        );
        assert_eq!(
            state.id_aa64mmfr2_el1(),
            identification_test_value(registers[10])
        );
        assert_eq!(
            registers.map(HvfSystemRegister::raw),
            [
                0xc000, 0xc005, 0xc020, 0xc021, 0xc028, 0xc029, 0xc030, 0xc031, 0xc038, 0xc039,
                0xc03a,
            ]
        );
    }

    #[test]
    fn captures_arm64_sve_sme_identification_register_state_in_documented_order() {
        let mut reads = Vec::new();

        let state = capture_arm64_vcpu_sve_sme_identification_register_state_with(|register| {
            reads.push(register);
            Ok(identification_test_value(register))
        })
        .expect("SVE/SME identification-register capture should succeed");

        let registers = sve_sme_identification_registers();
        assert_eq!(reads, registers);
        assert_eq!(
            state.id_aa64zfr0_el1(),
            identification_test_value(registers[0])
        );
        assert_eq!(
            state.id_aa64smfr0_el1(),
            identification_test_value(registers[1])
        );
        assert_eq!(registers.map(HvfSystemRegister::raw), [0xc024, 0xc025]);
    }

    #[test]
    fn captures_arm64_pointer_authentication_keys_in_documented_order() {
        let mut reads = Vec::new();

        let state = capture_arm64_vcpu_pointer_authentication_key_state_with(|register| {
            let value = POINTER_AUTHENTICATION_TEST_HALVES[reads.len()];
            reads.push(register);
            Ok(value)
        })
        .expect("pointer-authentication key capture should succeed");

        let registers = pointer_authentication_key_registers();
        assert_eq!(reads, registers);
        assert_eq!(state.apia_key(), pointer_authentication_test_key(0));
        assert_eq!(state.apib_key(), pointer_authentication_test_key(1));
        assert_eq!(state.apda_key(), pointer_authentication_test_key(2));
        assert_eq!(state.apdb_key(), pointer_authentication_test_key(3));
        assert_eq!(state.apga_key(), pointer_authentication_test_key(4));
        assert_eq!(
            registers.map(HvfSystemRegister::raw),
            [
                0xc108, 0xc109, 0xc10a, 0xc10b, 0xc110, 0xc111, 0xc112, 0xc113, 0xc118, 0xc119,
            ]
        );
        assert_eq!(
            format!("{state:?}"),
            "HvfArm64VcpuPointerAuthenticationKeyState { keys: \"<redacted>\" }"
        );
    }

    #[test]
    fn restores_arm64_pointer_authentication_keys_in_capture_order() {
        let state = pointer_authentication_restore_test_state();
        let expected = pointer_authentication_restore_test_entries();
        let mut writes = Vec::new();

        restore_arm64_vcpu_pointer_authentication_key_state_with(&state, |register, value| {
            writes.push((register, value));
            Ok(())
        })
        .expect("pointer-authentication key restore should succeed");

        assert_eq!(writes, expected);
        assert_eq!(
            format!("{state:?}"),
            "HvfArm64VcpuPointerAuthenticationKeyState { keys: \"<redacted>\" }"
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

    fn pending_interrupt_restore_test_state() -> super::HvfArm64VcpuPendingInterruptState {
        super::HvfArm64VcpuPendingInterruptState::new(true, false)
    }

    #[test]
    fn restores_arm64_pending_interrupt_state_in_irq_then_fiq_order() {
        let state = pending_interrupt_restore_test_state();
        let mut writes = Vec::new();

        restore_arm64_vcpu_pending_interrupt_state_with(&state, |interrupt_type, pending| {
            writes.push((interrupt_type, pending));
            Ok(())
        })
        .expect("pending-interrupt restore should succeed");

        assert_eq!(
            writes,
            [
                (HvfInterruptType::Irq, true),
                (HvfInterruptType::Fiq, false),
            ]
        );
    }

    #[test]
    fn arm64_pending_interrupt_restore_stops_after_each_error_and_can_retry() {
        use std::error::Error as _;

        let state = pending_interrupt_restore_test_state();
        let expected_writes = [
            (HvfInterruptType::Irq, state.irq_pending()),
            (HvfInterruptType::Fiq, state.fiq_pending()),
        ];

        for (failed_index, failed_type) in [HvfInterruptType::Irq, HvfInterruptType::Fiq]
            .into_iter()
            .enumerate()
        {
            let fail_next = Cell::new(true);
            let writes = RefCell::new(Vec::new());
            let set_pending_interrupt = |interrupt_type, pending| {
                writes.borrow_mut().push((interrupt_type, pending));
                if interrupt_type == failed_type && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake pending-interrupt restore failed",
                    ))
                } else {
                    Ok(())
                }
            };

            let error =
                restore_arm64_vcpu_pending_interrupt_state_with(&state, &set_pending_interrupt)
                    .expect_err("injected pending-interrupt write should fail");
            assert_eq!(error.failed_interrupt_type(), failed_type);
            assert_eq!(error.completed_writes(), failed_index);
            assert_eq!(
                error.source().map(ToString::to_string),
                Some("invalid backend state: fake pending-interrupt restore failed".to_string())
            );
            let interrupt_name = match failed_type {
                HvfInterruptType::Irq => "IRQ",
                HvfInterruptType::Fiq => "FIQ",
            };
            assert_eq!(
                error.to_string(),
                format!(
                    "failed to restore arm64 {interrupt_name} pending interrupt after {failed_index} successful writes: invalid backend state: fake pending-interrupt restore failed"
                )
            );
            assert_eq!(*writes.borrow(), expected_writes[..=failed_index]);

            writes.borrow_mut().clear();
            restore_arm64_vcpu_pending_interrupt_state_with(&state, &set_pending_interrupt)
                .expect("complete pending-interrupt restore retry should succeed");
            assert_eq!(*writes.borrow(), expected_writes);
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
    fn arm64_exception_register_capture_stops_after_each_error_and_can_retry() {
        let registers = [
            HvfSystemRegister::AFSR0_EL1,
            HvfSystemRegister::AFSR1_EL1,
            HvfSystemRegister::ESR_EL1,
            HvfSystemRegister::FAR_EL1,
            HvfSystemRegister::PAR_EL1,
            HvfSystemRegister::VBAR_EL1,
        ];

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let fail_next = Cell::new(true);
            let reads = RefCell::new(Vec::new());
            let read_system_register = |register: HvfSystemRegister| {
                reads.borrow_mut().push(register);
                if register == failed_register && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake exception register read failed",
                    ))
                } else {
                    Ok(u64::from(register.raw()))
                }
            };

            assert_eq!(
                capture_arm64_vcpu_exception_register_state_with(&read_system_register),
                Err(BackendError::InvalidState(
                    "fake exception register read failed"
                ))
            );
            assert_eq!(*reads.borrow(), registers[..=failed_index]);

            reads.borrow_mut().clear();
            let state = capture_arm64_vcpu_exception_register_state_with(&read_system_register)
                .expect("exception-register capture retry should succeed");
            assert_eq!(
                state.afsr0_el1(),
                u64::from(HvfSystemRegister::AFSR0_EL1.raw())
            );
            assert_eq!(
                state.vbar_el1(),
                u64::from(HvfSystemRegister::VBAR_EL1.raw())
            );
            assert_eq!(*reads.borrow(), registers);
        }
    }

    #[test]
    fn arm64_execution_control_register_capture_stops_after_each_error_and_can_retry() {
        let registers = [HvfSystemRegister::ACTLR_EL1, HvfSystemRegister::CPACR_EL1];

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let fail_next = Cell::new(true);
            let reads = RefCell::new(Vec::new());
            let read_system_register = |register: HvfSystemRegister| {
                reads.borrow_mut().push(register);
                if register == failed_register && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake execution-control register read failed",
                    ))
                } else {
                    Ok(u64::from(register.raw()))
                }
            };

            assert_eq!(
                capture_arm64_vcpu_execution_control_register_state_with(&read_system_register),
                Err(BackendError::InvalidState(
                    "fake execution-control register read failed"
                ))
            );
            assert_eq!(*reads.borrow(), registers[..=failed_index]);

            reads.borrow_mut().clear();
            let state =
                capture_arm64_vcpu_execution_control_register_state_with(&read_system_register)
                    .expect("execution-control capture retry should succeed");
            assert_eq!(
                state.actlr_el1(),
                u64::from(HvfSystemRegister::ACTLR_EL1.raw())
            );
            assert_eq!(
                state.cpacr_el1(),
                u64::from(HvfSystemRegister::CPACR_EL1.raw())
            );
            assert_eq!(*reads.borrow(), registers);
        }
    }

    #[test]
    fn arm64_cache_selection_register_capture_failure_can_retry() {
        let fail_next = Cell::new(true);
        let reads = RefCell::new(Vec::new());
        let read_system_register = |register: HvfSystemRegister| {
            reads.borrow_mut().push(register);
            if fail_next.replace(false) {
                Err(BackendError::InvalidState(
                    "fake cache-selection register read failed",
                ))
            } else {
                Ok(u64::from(register.raw()))
            }
        };

        assert_eq!(
            capture_arm64_vcpu_cache_selection_register_state_with(&read_system_register),
            Err(BackendError::InvalidState(
                "fake cache-selection register read failed"
            ))
        );
        assert_eq!(*reads.borrow(), [HvfSystemRegister::CSSELR_EL1]);

        reads.borrow_mut().clear();
        let state = capture_arm64_vcpu_cache_selection_register_state_with(&read_system_register)
            .expect("cache-selection capture retry should succeed");
        assert_eq!(
            state.csselr_el1(),
            u64::from(HvfSystemRegister::CSSELR_EL1.raw())
        );
        assert_eq!(*reads.borrow(), [HvfSystemRegister::CSSELR_EL1]);
    }

    #[test]
    fn arm64_breakpoint_register_capture_stops_after_each_error_and_can_retry() {
        let implemented_count = 16_u8;
        let dfr0 = u64::from(implemented_count - 1) << 12;
        let mut registers = vec![HvfSystemRegister::ID_AA64DFR0_EL1];
        for index in 0..implemented_count {
            registers.push(
                HvfSystemRegister::debug_breakpoint_value(index)
                    .expect("implemented value slot should be mapped"),
            );
            registers.push(
                HvfSystemRegister::debug_breakpoint_control(index)
                    .expect("implemented control slot should be mapped"),
            );
        }

        for (failed_index, failed_register) in registers.iter().copied().enumerate() {
            let fail_next = Cell::new(true);
            let reads = RefCell::new(Vec::new());
            let read_system_register = |register: HvfSystemRegister| {
                reads.borrow_mut().push(register);
                if register == failed_register && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake breakpoint register read failed",
                    ))
                } else if register == HvfSystemRegister::ID_AA64DFR0_EL1 {
                    Ok(dfr0)
                } else {
                    Ok(u64::from(register.raw()))
                }
            };

            assert_eq!(
                capture_arm64_vcpu_breakpoint_register_state_with(&read_system_register),
                Err(BackendError::InvalidState(
                    "fake breakpoint register read failed"
                ))
            );
            assert_eq!(*reads.borrow(), registers[..=failed_index]);

            reads.borrow_mut().clear();
            let state = capture_arm64_vcpu_breakpoint_register_state_with(&read_system_register)
                .expect("breakpoint-register capture retry should succeed");
            assert_eq!(state.implemented_breakpoint_count(), implemented_count);
            assert_eq!(*reads.borrow(), registers);
        }
    }

    #[test]
    fn arm64_watchpoint_register_capture_stops_after_each_error_and_can_retry() {
        let implemented_count = 16_u8;
        let dfr0 = u64::from(implemented_count - 1) << 20;
        let mut registers = vec![HvfSystemRegister::ID_AA64DFR0_EL1];
        for index in 0..implemented_count {
            registers.push(
                HvfSystemRegister::debug_watchpoint_value(index)
                    .expect("implemented value slot should be mapped"),
            );
            registers.push(
                HvfSystemRegister::debug_watchpoint_control(index)
                    .expect("implemented control slot should be mapped"),
            );
        }

        for (failed_index, failed_register) in registers.iter().copied().enumerate() {
            let fail_next = Cell::new(true);
            let reads = RefCell::new(Vec::new());
            let read_system_register = |register: HvfSystemRegister| {
                reads.borrow_mut().push(register);
                if register == failed_register && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake watchpoint register read failed",
                    ))
                } else if register == HvfSystemRegister::ID_AA64DFR0_EL1 {
                    Ok(dfr0)
                } else {
                    Ok(u64::from(register.raw()))
                }
            };

            assert_eq!(
                capture_arm64_vcpu_watchpoint_register_state_with(&read_system_register),
                Err(BackendError::InvalidState(
                    "fake watchpoint register read failed"
                ))
            );
            assert_eq!(*reads.borrow(), registers[..=failed_index]);

            reads.borrow_mut().clear();
            let state = capture_arm64_vcpu_watchpoint_register_state_with(&read_system_register)
                .expect("watchpoint-register capture retry should succeed");
            assert_eq!(state.implemented_watchpoint_count(), implemented_count);
            assert_eq!(*reads.borrow(), registers);
        }
    }

    #[test]
    fn arm64_debug_control_register_capture_stops_after_each_error_and_can_retry() {
        let registers = [HvfSystemRegister::MDCCINT_EL1, HvfSystemRegister::MDSCR_EL1];

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let fail_next = Cell::new(true);
            let reads = RefCell::new(Vec::new());
            let read_system_register = |register: HvfSystemRegister| {
                reads.borrow_mut().push(register);
                if register == failed_register && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake debug-control register read failed",
                    ))
                } else {
                    Ok(u64::from(register.raw()))
                }
            };

            assert_eq!(
                capture_arm64_vcpu_debug_control_register_state_with(&read_system_register),
                Err(BackendError::InvalidState(
                    "fake debug-control register read failed"
                ))
            );
            assert_eq!(*reads.borrow(), registers[..=failed_index]);

            reads.borrow_mut().clear();
            let state = capture_arm64_vcpu_debug_control_register_state_with(&read_system_register)
                .expect("debug-control capture retry should succeed");
            assert_eq!(
                state.mdccint_el1(),
                u64::from(HvfSystemRegister::MDCCINT_EL1.raw())
            );
            assert_eq!(
                state.mdscr_el1(),
                u64::from(HvfSystemRegister::MDSCR_EL1.raw())
            );
            assert_eq!(*reads.borrow(), registers);
        }
    }

    #[test]
    fn arm64_debug_trap_state_capture_stops_after_each_error_and_can_retry() {
        let expected_reads = [
            DebugTrapRead::DebugExceptions,
            DebugTrapRead::DebugRegisterAccesses,
        ];

        for (failed_index, failed_read) in expected_reads.into_iter().enumerate() {
            let fail_next = Cell::new(true);
            let mut reads = Vec::new();

            let result = capture_arm64_vcpu_debug_trap_state_with(
                &mut reads,
                |reads| {
                    reads.push(DebugTrapRead::DebugExceptions);
                    if failed_read == DebugTrapRead::DebugExceptions && fail_next.replace(false) {
                        Err(BackendError::InvalidState(
                            "fake debug-trap state read failed",
                        ))
                    } else {
                        Ok(true)
                    }
                },
                |reads| {
                    reads.push(DebugTrapRead::DebugRegisterAccesses);
                    if failed_read == DebugTrapRead::DebugRegisterAccesses
                        && fail_next.replace(false)
                    {
                        Err(BackendError::InvalidState(
                            "fake debug-trap state read failed",
                        ))
                    } else {
                        Ok(false)
                    }
                },
            );

            assert_eq!(
                result,
                Err(BackendError::InvalidState(
                    "fake debug-trap state read failed"
                ))
            );
            assert_eq!(reads, expected_reads[..=failed_index]);

            reads.clear();
            let state = capture_arm64_vcpu_debug_trap_state_with(
                &mut reads,
                |reads| {
                    reads.push(DebugTrapRead::DebugExceptions);
                    Ok(true)
                },
                |reads| {
                    reads.push(DebugTrapRead::DebugRegisterAccesses);
                    Ok(false)
                },
            )
            .expect("debug-trap state capture retry should succeed");
            assert!(state.trap_debug_exceptions());
            assert!(!state.trap_debug_reg_accesses());
            assert_eq!(reads, expected_reads);
        }
    }

    #[test]
    fn arm64_debug_trap_state_restore_stops_after_each_error_and_can_retry() {
        use std::error::Error as _;

        let state = super::HvfArm64VcpuDebugTrapState::new(true, false);
        let expected_writes = [
            DebugTrapWrite::DebugExceptions(true),
            DebugTrapWrite::DebugRegisterAccesses(false),
        ];

        for (failed_index, failed_write) in expected_writes.into_iter().enumerate() {
            let fail_next = Cell::new(true);
            let mut writes = Vec::new();
            let error = restore_arm64_vcpu_debug_trap_state_with(
                &state,
                &mut writes,
                |writes, value| {
                    let write = DebugTrapWrite::DebugExceptions(value);
                    writes.push(write);
                    if write == failed_write && fail_next.replace(false) {
                        Err(BackendError::InvalidState(
                            "fake debug-trap state restore failed",
                        ))
                    } else {
                        Ok(())
                    }
                },
                |writes, value| {
                    let write = DebugTrapWrite::DebugRegisterAccesses(value);
                    writes.push(write);
                    if write == failed_write && fail_next.replace(false) {
                        Err(BackendError::InvalidState(
                            "fake debug-trap state restore failed",
                        ))
                    } else {
                        Ok(())
                    }
                },
            )
            .expect_err("injected debug-trap state write should fail");

            assert_eq!(error.failed_operation(), failed_write.operation());
            assert_eq!(error.completed_writes(), failed_index);
            assert_eq!(
                error.source().map(ToString::to_string),
                Some("invalid backend state: fake debug-trap state restore failed".to_string())
            );
            let operation_name = match failed_write.operation() {
                HvfArm64VcpuDebugTrapRestoreOperation::DebugExceptions => {
                    "debug-exception trap policy"
                }
                HvfArm64VcpuDebugTrapRestoreOperation::DebugRegisterAccesses => {
                    "debug-register-access trap policy"
                }
            };
            assert_eq!(
                error.to_string(),
                format!(
                    "failed to restore arm64 {operation_name} after {failed_index} successful writes: invalid backend state: fake debug-trap state restore failed"
                )
            );
            assert_eq!(writes, expected_writes[..=failed_index]);

            writes.clear();
            restore_arm64_vcpu_debug_trap_state_with(
                &state,
                &mut writes,
                |writes, value| {
                    writes.push(DebugTrapWrite::DebugExceptions(value));
                    Ok(())
                },
                |writes, value| {
                    writes.push(DebugTrapWrite::DebugRegisterAccesses(value));
                    Ok(())
                },
            )
            .expect("complete debug-trap state restore retry should succeed");
            assert_eq!(writes, expected_writes);
        }
    }

    #[test]
    fn failed_arm64_sme_pstate_capture_can_retry_without_partial_state() {
        let fail_next = Cell::new(true);
        let reads = Cell::new(0);
        let read_sme_pstate = || {
            reads.set(reads.get() + 1);
            if fail_next.replace(false) {
                Err(BackendError::InvalidState("fake SME PSTATE capture failed"))
            } else {
                Ok((true, false))
            }
        };

        assert_eq!(
            capture_arm64_vcpu_sme_pstate_with(read_sme_pstate),
            Err(BackendError::InvalidState("fake SME PSTATE capture failed"))
        );
        assert_eq!(reads.get(), 1);

        let state = capture_arm64_vcpu_sme_pstate_with(read_sme_pstate)
            .expect("SME PSTATE capture retry should succeed");
        assert!(state.streaming_sve_mode_enabled());
        assert!(!state.za_storage_enabled());
        assert_eq!(reads.get(), 2);
    }

    #[test]
    fn arm64_sme_system_register_capture_stops_after_each_error_and_can_retry() {
        let registers = sme_system_registers();

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let fail_next = Cell::new(true);
            let reads = RefCell::new(Vec::new());
            let read_system_register = |register: HvfSystemRegister| {
                reads.borrow_mut().push(register);
                if register == failed_register && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake SME system register read failed",
                    ))
                } else {
                    Ok(0x5e00_0000_0000_0000 | u64::from(register.raw()))
                }
            };

            assert_eq!(
                capture_arm64_vcpu_sme_system_register_state_with(&read_system_register),
                Err(BackendError::InvalidState(
                    "fake SME system register read failed"
                ))
            );
            assert_eq!(*reads.borrow(), registers[..=failed_index]);

            reads.borrow_mut().clear();
            let state = capture_arm64_vcpu_sme_system_register_state_with(&read_system_register)
                .expect("SME system-register capture retry should succeed");
            assert_eq!(
                state.smcr_el1(),
                0x5e00_0000_0000_0000 | u64::from(registers[0].raw())
            );
            assert_eq!(
                state.smpri_el1(),
                0x5e00_0000_0000_0000 | u64::from(registers[1].raw())
            );
            assert_eq!(
                state.tpidr2_el0(),
                0x5e00_0000_0000_0000 | u64::from(registers[2].raw())
            );
            assert_eq!(*reads.borrow(), registers);
        }
    }

    #[test]
    fn arm64_system_context_register_capture_stops_after_each_error_and_can_retry() {
        let registers = system_context_registers();

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let fail_next = Cell::new(true);
            let reads = RefCell::new(Vec::new());
            let read_system_register = |register: HvfSystemRegister| {
                reads.borrow_mut().push(register);
                if register == failed_register && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake system-context register read failed",
                    ))
                } else {
                    Ok(0xc700_0000_0000_0000 | u64::from(register.raw()))
                }
            };

            assert_eq!(
                capture_arm64_vcpu_system_context_register_state_with(&read_system_register),
                Err(BackendError::InvalidState(
                    "fake system-context register read failed"
                ))
            );
            assert_eq!(*reads.borrow(), registers[..=failed_index]);

            reads.borrow_mut().clear();
            let state =
                capture_arm64_vcpu_system_context_register_state_with(&read_system_register)
                    .expect("system-context register capture retry should succeed");
            assert_eq!(
                state.scxtnum_el0(),
                0xc700_0000_0000_0000 | u64::from(registers[0].raw())
            );
            assert_eq!(
                state.scxtnum_el1(),
                0xc700_0000_0000_0000 | u64::from(registers[1].raw())
            );
            assert_eq!(*reads.borrow(), registers);
        }
    }

    #[test]
    fn every_arm64_system_context_register_restore_failure_stops_and_can_retry() {
        use std::error::Error as _;

        let state = system_context_restore_test_state();
        let expected = system_context_restore_test_entries();

        for (failed_index, (failed_register, _)) in expected.iter().copied().enumerate() {
            let fail_once = Cell::new(true);
            let writes = RefCell::new(Vec::new());
            let write_system_register = |register, value| {
                writes.borrow_mut().push((register, value));
                if register == failed_register && fail_once.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake system-context register restore failed",
                    ))
                } else {
                    Ok(())
                }
            };

            let error = restore_arm64_vcpu_system_context_register_state_with(
                &state,
                &write_system_register,
            )
            .expect_err("injected system-context register write should fail");
            assert_eq!(error.failed_register(), failed_register);
            assert_eq!(error.completed_writes(), failed_index);
            assert_eq!(
                error.source().map(ToString::to_string),
                Some(
                    "invalid backend state: fake system-context register restore failed"
                        .to_string()
                )
            );
            assert_eq!(*writes.borrow(), expected[..=failed_index]);
            assert_eq!(
                error.to_string(),
                format!(
                    "failed to restore arm64 system register id {} after {failed_index} successful writes: invalid backend state: fake system-context register restore failed",
                    failed_register.raw()
                )
            );

            writes.borrow_mut().clear();
            restore_arm64_vcpu_system_context_register_state_with(&state, &write_system_register)
                .expect("complete system-context register restore retry should succeed");
            assert_eq!(*writes.borrow(), expected);
        }
    }

    #[test]
    fn arm64_translation_register_capture_stops_after_each_error_and_can_retry() {
        let registers = [
            HvfSystemRegister::SCTLR_EL1,
            HvfSystemRegister::TTBR0_EL1,
            HvfSystemRegister::TTBR1_EL1,
            HvfSystemRegister::TCR_EL1,
            HvfSystemRegister::MAIR_EL1,
            HvfSystemRegister::AMAIR_EL1,
            HvfSystemRegister::CONTEXTIDR_EL1,
        ];

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let fail_next = Cell::new(true);
            let reads = RefCell::new(Vec::new());
            let read_system_register = |register: HvfSystemRegister| {
                reads.borrow_mut().push(register);
                if register == failed_register && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake translation register read failed",
                    ))
                } else {
                    Ok(u64::from(register.raw()))
                }
            };

            assert_eq!(
                capture_arm64_vcpu_translation_register_state_with(&read_system_register),
                Err(BackendError::InvalidState(
                    "fake translation register read failed"
                ))
            );
            assert_eq!(*reads.borrow(), registers[..=failed_index]);

            reads.borrow_mut().clear();
            let state = capture_arm64_vcpu_translation_register_state_with(&read_system_register)
                .expect("translation-register capture retry should succeed");
            assert_eq!(
                state.sctlr_el1(),
                u64::from(HvfSystemRegister::SCTLR_EL1.raw())
            );
            assert_eq!(
                state.contextidr_el1(),
                u64::from(HvfSystemRegister::CONTEXTIDR_EL1.raw())
            );
            assert_eq!(*reads.borrow(), registers);
        }
    }

    #[test]
    fn arm64_pointer_authentication_key_capture_stops_after_each_error_and_can_retry() {
        let registers = pointer_authentication_key_registers();

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let fail_next = Cell::new(true);
            let reads = RefCell::new(Vec::new());
            let read_system_register = |register: HvfSystemRegister| {
                reads.borrow_mut().push(register);
                if register == failed_register && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake pointer-authentication key read failed",
                    ))
                } else {
                    Ok(POINTER_AUTHENTICATION_TEST_HALVES[reads.borrow().len() - 1])
                }
            };

            assert_eq!(
                capture_arm64_vcpu_pointer_authentication_key_state_with(&read_system_register),
                Err(BackendError::InvalidState(
                    "fake pointer-authentication key read failed"
                ))
            );
            assert_eq!(*reads.borrow(), registers[..=failed_index]);

            reads.borrow_mut().clear();
            let state =
                capture_arm64_vcpu_pointer_authentication_key_state_with(&read_system_register)
                    .expect("pointer-authentication key capture retry should succeed");
            assert_eq!(state.apia_key(), pointer_authentication_test_key(0));
            assert_eq!(state.apga_key(), pointer_authentication_test_key(4));
            assert_eq!(*reads.borrow(), registers);
        }
    }

    #[test]
    fn every_arm64_pointer_authentication_key_restore_failure_stops_and_can_retry() {
        use std::error::Error as _;

        let state = pointer_authentication_restore_test_state();
        let expected = pointer_authentication_restore_test_entries();

        for (failed_index, (failed_register, _)) in expected.iter().copied().enumerate() {
            let fail_once = Cell::new(true);
            let writes = RefCell::new(Vec::new());
            let write_system_register = |register, value| {
                writes.borrow_mut().push((register, value));
                if register == failed_register && fail_once.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake pointer-authentication key restore failed",
                    ))
                } else {
                    Ok(())
                }
            };

            let error = restore_arm64_vcpu_pointer_authentication_key_state_with(
                &state,
                &write_system_register,
            )
            .expect_err("injected pointer-authentication key write should fail");
            assert_eq!(error.failed_register(), failed_register);
            assert_eq!(error.completed_writes(), failed_index);
            assert_eq!(
                error.source().map(ToString::to_string),
                Some(
                    "invalid backend state: fake pointer-authentication key restore failed"
                        .to_string()
                )
            );
            assert_eq!(*writes.borrow(), expected[..=failed_index]);
            assert_eq!(
                error.to_string(),
                format!(
                    "failed to restore arm64 system register id {} after {failed_index} successful writes: invalid backend state: fake pointer-authentication key restore failed",
                    failed_register.raw()
                )
            );

            writes.borrow_mut().clear();
            restore_arm64_vcpu_pointer_authentication_key_state_with(
                &state,
                &write_system_register,
            )
            .expect("complete pointer-authentication key restore retry should succeed");
            assert_eq!(*writes.borrow(), expected);
        }
    }

    #[test]
    fn arm64_identification_register_capture_stops_after_each_error_and_can_retry() {
        let registers = identification_registers();

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let fail_next = Cell::new(true);
            let reads = RefCell::new(Vec::new());
            let read_system_register = |register: HvfSystemRegister| {
                reads.borrow_mut().push(register);
                if register == failed_register && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake identification register read failed",
                    ))
                } else {
                    Ok(identification_test_value(register))
                }
            };

            assert_eq!(
                capture_arm64_vcpu_identification_register_state_with(&read_system_register),
                Err(BackendError::InvalidState(
                    "fake identification register read failed"
                ))
            );
            assert_eq!(*reads.borrow(), registers[..=failed_index]);

            reads.borrow_mut().clear();
            let state =
                capture_arm64_vcpu_identification_register_state_with(&read_system_register)
                    .expect("identification-register capture retry should succeed");
            assert_eq!(state.midr_el1(), identification_test_value(registers[0]));
            assert_eq!(
                state.id_aa64mmfr2_el1(),
                identification_test_value(registers[10])
            );
            assert_eq!(*reads.borrow(), registers);
        }
    }

    #[test]
    fn arm64_sve_sme_identification_capture_stops_after_each_error_and_can_retry() {
        let registers = sve_sme_identification_registers();

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let fail_next = Cell::new(true);
            let reads = RefCell::new(Vec::new());
            let read_system_register = |register: HvfSystemRegister| {
                reads.borrow_mut().push(register);
                if register == failed_register && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake SVE/SME identification register read failed",
                    ))
                } else {
                    Ok(identification_test_value(register))
                }
            };

            assert_eq!(
                capture_arm64_vcpu_sve_sme_identification_register_state_with(
                    &read_system_register
                ),
                Err(BackendError::InvalidState(
                    "fake SVE/SME identification register read failed"
                ))
            );
            assert_eq!(*reads.borrow(), registers[..=failed_index]);

            reads.borrow_mut().clear();
            let state = capture_arm64_vcpu_sve_sme_identification_register_state_with(
                &read_system_register,
            )
            .expect("SVE/SME identification-register capture retry should succeed");
            assert_eq!(
                state.id_aa64zfr0_el1(),
                identification_test_value(registers[0])
            );
            assert_eq!(
                state.id_aa64smfr0_el1(),
                identification_test_value(registers[1])
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

    fn thread_context_restore_test_state() -> super::HvfArm64VcpuThreadContextRegisterState {
        capture_arm64_vcpu_thread_context_register_state_with(|register| {
            Ok(0xd900_0000_0000_0000 | u64::from(register.raw()))
        })
        .expect("thread-context test state should be captured")
    }

    fn thread_context_restore_test_entries(
        state: super::HvfArm64VcpuThreadContextRegisterState,
    ) -> [(HvfSystemRegister, u64); 3] {
        [
            (HvfSystemRegister::TPIDR_EL0, state.tpidr_el0()),
            (HvfSystemRegister::TPIDRRO_EL0, state.tpidrro_el0()),
            (HvfSystemRegister::TPIDR_EL1, state.tpidr_el1()),
        ]
    }

    #[test]
    fn restores_arm64_thread_context_register_state_in_capture_order() {
        let state = thread_context_restore_test_state();
        let expected = thread_context_restore_test_entries(state);
        let mut writes = Vec::new();

        restore_arm64_vcpu_thread_context_register_state_with(&state, |register, value| {
            writes.push((register, value));
            Ok(())
        })
        .expect("thread-context restore should succeed");

        assert_eq!(writes, expected);
    }

    #[test]
    fn every_arm64_thread_context_restore_failure_stops_and_can_retry() {
        use std::error::Error as _;

        let state = thread_context_restore_test_state();
        let expected = thread_context_restore_test_entries(state);

        for (failed_index, (failed_register, _)) in expected.iter().copied().enumerate() {
            let fail_once = Cell::new(true);
            let writes = RefCell::new(Vec::new());
            let write_system_register = |register, value| {
                writes.borrow_mut().push((register, value));
                if register == failed_register && fail_once.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake thread-context restore failed",
                    ))
                } else {
                    Ok(())
                }
            };

            let error = restore_arm64_vcpu_thread_context_register_state_with(
                &state,
                &write_system_register,
            )
            .expect_err("injected thread-context write should fail");
            assert_eq!(error.failed_register(), failed_register);
            assert_eq!(error.completed_writes(), failed_index);
            assert_eq!(
                error.source().map(ToString::to_string),
                Some("invalid backend state: fake thread-context restore failed".to_string())
            );
            assert_eq!(*writes.borrow(), expected[..=failed_index]);
            assert_eq!(
                error.to_string(),
                format!(
                    "failed to restore arm64 system register id {} after {failed_index} successful writes: invalid backend state: fake thread-context restore failed",
                    failed_register.raw()
                )
            );

            writes.borrow_mut().clear();
            restore_arm64_vcpu_thread_context_register_state_with(&state, &write_system_register)
                .expect("complete thread-context restore retry should succeed");
            assert_eq!(*writes.borrow(), expected);
        }
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
    fn restores_arm64_simd_fp_state_in_capture_order() {
        let state = simd_fp_restore_test_state();
        let expected = expected_simd_fp_writes(&state);
        let writes = RefCell::new(Vec::new());
        let mut writer = ();

        restore_arm64_vcpu_simd_fp_state_with(
            &state,
            &mut writer,
            |_, register, value| {
                writes.borrow_mut().push(SimdFpWrite::Q(register, value));
                Ok(())
            },
            |_, register, value| {
                writes
                    .borrow_mut()
                    .push(SimdFpWrite::Scalar(register, value));
                Ok(())
            },
        )
        .expect("SIMD/FP restore should succeed");

        assert_eq!(*writes.borrow(), expected);
    }

    #[test]
    fn every_arm64_simd_fp_restore_failure_stops_and_can_retry() {
        use std::error::Error as _;

        let state = simd_fp_restore_test_state();
        let expected = expected_simd_fp_writes(&state);

        for (failed_index, failed_write) in expected.iter().copied().enumerate() {
            let failed_register = failed_write.register();
            let fail_once = Cell::new(true);
            let writes = RefCell::new(Vec::new());
            let record_write = |write: SimdFpWrite| {
                writes.borrow_mut().push(write);
                if write.register() == failed_register && fail_once.replace(false) {
                    Err(BackendError::InvalidState("fake SIMD/FP restore failed"))
                } else {
                    Ok(())
                }
            };
            let mut writer = ();

            let error = restore_arm64_vcpu_simd_fp_state_with(
                &state,
                &mut writer,
                |_, register, value| record_write(SimdFpWrite::Q(register, value)),
                |_, register, value| record_write(SimdFpWrite::Scalar(register, value)),
            )
            .expect_err("injected SIMD/FP write should fail");
            assert_eq!(error.failed_register(), failed_register);
            assert_eq!(error.completed_writes(), failed_index);
            assert_eq!(
                error.source().map(ToString::to_string),
                Some("invalid backend state: fake SIMD/FP restore failed".to_string())
            );
            assert_eq!(*writes.borrow(), expected[..=failed_index]);
            let (register_space, register_id) = match failed_register {
                HvfArm64VcpuSimdFpRestoreRegister::SimdFp(register) => ("SIMD/FP", register.raw()),
                HvfArm64VcpuSimdFpRestoreRegister::Scalar(register) => ("scalar", register.raw()),
            };
            assert_eq!(
                error.to_string(),
                format!(
                    "failed to restore arm64 {register_space} register id {register_id} after {failed_index} successful writes: invalid backend state: fake SIMD/FP restore failed"
                )
            );

            writes.borrow_mut().clear();
            restore_arm64_vcpu_simd_fp_state_with(
                &state,
                &mut writer,
                |_, register, value| record_write(SimdFpWrite::Q(register, value)),
                |_, register, value| record_write(SimdFpWrite::Scalar(register, value)),
            )
            .expect("complete SIMD/FP restore retry should succeed");
            assert_eq!(*writes.borrow(), expected);
        }
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
    fn captures_arm64_physical_timer_state_in_documented_order() {
        let mut reads = Vec::new();

        let state = capture_arm64_vcpu_physical_timer_state_with(|register| {
            reads.push(register);
            Ok(0xcafe_0000_0000_0000 | u64::from(register.raw()))
        })
        .expect("physical-timer capture should succeed");

        assert_eq!(
            reads,
            [
                HvfSystemRegister::CNTKCTL_EL1,
                HvfSystemRegister::CNTP_CTL_EL0,
                HvfSystemRegister::CNTP_CVAL_EL0,
                HvfSystemRegister::CNTP_TVAL_EL0,
            ]
        );
        assert_eq!(
            state.cntkctl_el1(),
            0xcafe_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_CNTKCTL_EL1)
        );
        assert_eq!(
            state.cntp_ctl_el0(),
            0xcafe_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_CNTP_CTL_EL0)
        );
        assert_eq!(
            state.cntp_cval_el0(),
            0xcafe_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_CNTP_CVAL_EL0)
        );
        assert_eq!(
            state.cntp_tval_el0(),
            0xcafe_0000_0000_0000 | u64::from(crate::ffi::HV_SYS_REG_CNTP_TVAL_EL0)
        );
        assert_eq!(HvfSystemRegister::CNTKCTL_EL1.raw(), 0xc708);
        assert_eq!(HvfSystemRegister::CNTP_CTL_EL0.raw(), 0xdf11);
        assert_eq!(HvfSystemRegister::CNTP_CVAL_EL0.raw(), 0xdf12);
        assert_eq!(HvfSystemRegister::CNTP_TVAL_EL0.raw(), 0xdf10);
    }

    #[test]
    fn physical_timer_capture_stops_after_each_error_and_can_retry() {
        let registers = [
            HvfSystemRegister::CNTKCTL_EL1,
            HvfSystemRegister::CNTP_CTL_EL0,
            HvfSystemRegister::CNTP_CVAL_EL0,
            HvfSystemRegister::CNTP_TVAL_EL0,
        ];

        for (failed_index, failed_register) in registers.into_iter().enumerate() {
            let fail_next = Cell::new(true);
            let reads = RefCell::new(Vec::new());
            let read_system_register = |register: HvfSystemRegister| {
                reads.borrow_mut().push(register);
                if register == failed_register && fail_next.replace(false) {
                    Err(BackendError::InvalidState(
                        "fake physical-timer register read failed",
                    ))
                } else {
                    Ok(u64::from(register.raw()))
                }
            };

            assert_eq!(
                capture_arm64_vcpu_physical_timer_state_with(&read_system_register),
                Err(BackendError::InvalidState(
                    "fake physical-timer register read failed"
                ))
            );
            assert_eq!(*reads.borrow(), registers[..=failed_index]);

            reads.borrow_mut().clear();
            let state = capture_arm64_vcpu_physical_timer_state_with(&read_system_register)
                .expect("physical-timer capture retry should succeed");
            assert_eq!(
                state.cntkctl_el1(),
                u64::from(HvfSystemRegister::CNTKCTL_EL1.raw())
            );
            assert_eq!(
                state.cntp_cval_el0(),
                u64::from(HvfSystemRegister::CNTP_CVAL_EL0.raw())
            );
            assert_eq!(
                state.cntp_tval_el0(),
                u64::from(HvfSystemRegister::CNTP_TVAL_EL0.raw())
            );
            assert_eq!(*reads.borrow(), registers);
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
