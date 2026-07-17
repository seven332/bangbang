//! HVF GIC v3 creation and metadata for later boot/FDT setup.

use std::fmt;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use bangbang_runtime::BackendError;
use bangbang_runtime::fdt::{
    ARM64_GICV2M_MSI_IIDR_OFFSET, ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET,
    ARM64_GICV2M_SPI_END_EXCLUSIVE, Arm64FdtError, Arm64FdtGic, Arm64FdtInterruptRange,
    Arm64FdtMsi, Arm64FdtRegion, Arm64FdtTimerInterrupts,
};
use bangbang_runtime::interrupt::{
    GuestInterruptLine, GuestInterruptLineError, InterruptSignalError, InterruptSink,
};

const GIC_REQUIRES_MACOS_15_MESSAGE: &str =
    "Hypervisor.framework GIC APIs require macOS 15.0 or newer";
const MMIO32_MEM_START: u64 = 1 << 30;
const DRAM_MEM_START: u64 = bangbang_runtime::memory::aarch64::DRAM_MEM_START;
const DYNAMIC_SYMBOL_SIZE_MISMATCH_MESSAGE: &str =
    "function pointer size does not match a dynamic symbol pointer";
const GIC_SPI_SIGNALER_LOCK_POISONED_MESSAGE: &str = "HVF GIC SPI signaler lock is poisoned";
const GIC_MSI_SIGNALER_LOCK_POISONED_MESSAGE: &str = "HVF GIC MSI signaler lock is poisoned";
const GIC_MSI_SIGNALER_INACTIVE_MESSAGE: &str = "HVF GIC MSI signaler is inactive";
const GIC_MSI_REGISTER_SIZE: u64 = 4;
const GIC_ICC_RPR_MISMATCH_MESSAGE: &str =
    "restored arm64 GIC ICC_RPR_EL1 does not match captured derived state";

const HV_GIC_INT_EL1_VIRTUAL_TIMER: u16 = 27;
const HV_GIC_INT_EL1_PHYSICAL_TIMER: u16 = 30;
const FIRST_PPI_INTID: u32 = 16;
const FIRST_SPI_INTID: u32 = 32;
const HV_GIC_REDISTRIBUTOR_REG_GICR_ISPENDR0: u32 = 0x10200;
const HV_GIC_REDISTRIBUTOR_REG_GICR_ICPENDR0: u32 = 0x10280;
const HV_GIC_ICC_REG_PMR_EL1: u16 = 0xc230;
const HV_GIC_ICC_REG_BPR0_EL1: u16 = 0xc643;
const HV_GIC_ICC_REG_AP0R0_EL1: u16 = 0xc644;
const HV_GIC_ICC_REG_AP1R0_EL1: u16 = 0xc648;
const HV_GIC_ICC_REG_RPR_EL1: u16 = 0xc65b;
const HV_GIC_ICC_REG_BPR1_EL1: u16 = 0xc663;
const HV_GIC_ICC_REG_CTLR_EL1: u16 = 0xc664;
const HV_GIC_ICC_REG_SRE_EL1: u16 = 0xc665;
const HV_GIC_ICC_REG_IGRPEN0_EL1: u16 = 0xc666;
const HV_GIC_ICC_REG_IGRPEN1_EL1: u16 = 0xc667;

/// One EL1 GIC ICC CPU-interface register in the captured arm64 state.
///
/// Raw Hypervisor.framework enum identifiers remain private. This enum names
/// the fixed complete inventory used for ordered capture, restore, and
/// value-free restore failure context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfArm64GicIccRegister {
    /// `ICC_PMR_EL1` priority mask.
    PmrEl1,
    /// `ICC_BPR0_EL1` Group 0 binary point.
    Bpr0El1,
    /// `ICC_AP0R0_EL1` Group 0 active priorities.
    Ap0r0El1,
    /// `ICC_AP1R0_EL1` Group 1 active priorities.
    Ap1r0El1,
    /// `ICC_RPR_EL1` running priority.
    RprEl1,
    /// `ICC_BPR1_EL1` Group 1 binary point.
    Bpr1El1,
    /// `ICC_CTLR_EL1` CPU-interface control.
    CtlrEl1,
    /// `ICC_SRE_EL1` system-register enablement.
    SreEl1,
    /// `ICC_IGRPEN0_EL1` Group 0 enablement.
    Igrpen0El1,
    /// `ICC_IGRPEN1_EL1` Group 1 enablement.
    Igrpen1El1,
}

impl HvfArm64GicIccRegister {
    const fn raw(self) -> u16 {
        match self {
            Self::PmrEl1 => HV_GIC_ICC_REG_PMR_EL1,
            Self::Bpr0El1 => HV_GIC_ICC_REG_BPR0_EL1,
            Self::Ap0r0El1 => HV_GIC_ICC_REG_AP0R0_EL1,
            Self::Ap1r0El1 => HV_GIC_ICC_REG_AP1R0_EL1,
            Self::RprEl1 => HV_GIC_ICC_REG_RPR_EL1,
            Self::Bpr1El1 => HV_GIC_ICC_REG_BPR1_EL1,
            Self::CtlrEl1 => HV_GIC_ICC_REG_CTLR_EL1,
            Self::SreEl1 => HV_GIC_ICC_REG_SRE_EL1,
            Self::Igrpen0El1 => HV_GIC_ICC_REG_IGRPEN0_EL1,
            Self::Igrpen1El1 => HV_GIC_ICC_REG_IGRPEN1_EL1,
        }
    }

    const fn architectural_name(self) -> &'static str {
        match self {
            Self::PmrEl1 => "ICC_PMR_EL1",
            Self::Bpr0El1 => "ICC_BPR0_EL1",
            Self::Ap0r0El1 => "ICC_AP0R0_EL1",
            Self::Ap1r0El1 => "ICC_AP1R0_EL1",
            Self::RprEl1 => "ICC_RPR_EL1",
            Self::Bpr1El1 => "ICC_BPR1_EL1",
            Self::CtlrEl1 => "ICC_CTLR_EL1",
            Self::SreEl1 => "ICC_SRE_EL1",
            Self::Igrpen0El1 => "ICC_IGRPEN0_EL1",
            Self::Igrpen1El1 => "ICC_IGRPEN1_EL1",
        }
    }
}

const ARM64_GIC_EL1_ICC_REGISTERS: [HvfArm64GicIccRegister; 10] = [
    HvfArm64GicIccRegister::PmrEl1,
    HvfArm64GicIccRegister::Bpr0El1,
    HvfArm64GicIccRegister::Ap0r0El1,
    HvfArm64GicIccRegister::Ap1r0El1,
    HvfArm64GicIccRegister::RprEl1,
    HvfArm64GicIccRegister::Bpr1El1,
    HvfArm64GicIccRegister::CtlrEl1,
    HvfArm64GicIccRegister::SreEl1,
    HvfArm64GicIccRegister::Igrpen0El1,
    HvfArm64GicIccRegister::Igrpen1El1,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfGicRegion {
    pub base: u64,
    pub size: u64,
}

impl HvfGicRegion {
    pub const fn end_exclusive(self) -> u64 {
        self.base.saturating_add(self.size)
    }

    const fn overlaps(self, other: Self) -> bool {
        self.base < other.end_exclusive() && other.base < self.end_exclusive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfGicInterruptRange {
    pub base: u32,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfGicTimerInterrupts {
    pub el1_virtual_timer_intid: u32,
    pub el1_physical_timer_intid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfGicRedistributor {
    pub region: HvfGicRegion,
    pub single_redistributor_size: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfGicMsiMetadata {
    pub region: HvfGicRegion,
    pub interrupt_range: HvfGicInterruptRange,
}

impl fmt::Debug for HvfGicMsiMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfGicMsiMetadata")
            .field("region", &"<redacted>")
            .field("interrupt_range", &"<redacted>")
            .finish()
    }
}

/// Demand for a contiguous Hypervisor.framework GIC MSI interrupt range.
///
/// The exact count is available to trusted platform composition through its
/// accessor, while automatic diagnostics redact it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfGicMsiConfiguration {
    interrupt_count: NonZeroU32,
}

impl HvfGicMsiConfiguration {
    pub const fn new(interrupt_count: NonZeroU32) -> Self {
        Self { interrupt_count }
    }

    pub const fn interrupt_count(self) -> NonZeroU32 {
        self.interrupt_count
    }
}

impl fmt::Debug for HvfGicMsiConfiguration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfGicMsiConfiguration")
            .field("interrupt_count", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfGicMetadata {
    pub distributor: HvfGicRegion,
    pub redistributor: HvfGicRedistributor,
    pub spi_interrupt_range: HvfGicInterruptRange,
    pub timer_interrupts: HvfGicTimerInterrupts,
    pub msi: Option<HvfGicMsiMetadata>,
}

/// Detached opaque Hypervisor.framework GIC device state.
///
/// Apple defines these bytes as stable, versioned serialized GIC device state,
/// excluding the vCPU-affine GIC CPU system registers. The contents are
/// sensitive guest/VMM execution state and are not a bangbang or Firecracker
/// snapshot schema. Hypervisor.framework can reapply the complete value only
/// after the destination GIC and vCPUs exist and before any vCPU runs;
/// compatible GIC CPU-interface state, restore orchestration, and host
/// compatibility policy remain outside this value.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfGicDeviceState {
    bytes: Vec<u8>,
}

impl HvfGicDeviceState {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Return the opaque serialized GIC device-state bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Return the opaque state size in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Return whether the opaque state contains no bytes.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl fmt::Debug for HvfGicDeviceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfGicDeviceState")
            .field("len", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// Detached raw EL1 GIC ICC register state captured from one arm64 vCPU.
///
/// This value contains every EL1 ICC CPU-interface register exposed by
/// Hypervisor.framework on macOS 15. The values are sensitive, unvalidated
/// execution state for later owner-thread orchestration, not a complete or
/// serialized snapshot schema. `ICC_SRE_EL2`, ICH/ICV virtualization state,
/// restore validation, and multi-vCPU association remain outside this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64GicIccRegisterState {
    values: [u64; ARM64_GIC_EL1_ICC_REGISTERS.len()],
}

impl HvfArm64GicIccRegisterState {
    pub(crate) const fn new(values: [u64; ARM64_GIC_EL1_ICC_REGISTERS.len()]) -> Self {
        Self { values }
    }

    /// Return the raw `ICC_PMR_EL1` value.
    pub const fn pmr_el1(self) -> u64 {
        self.values[0]
    }

    /// Return the raw `ICC_BPR0_EL1` value.
    pub const fn bpr0_el1(self) -> u64 {
        self.values[1]
    }

    /// Return the raw `ICC_AP0R0_EL1` value.
    pub const fn ap0r0_el1(self) -> u64 {
        self.values[2]
    }

    /// Return the raw `ICC_AP1R0_EL1` value.
    pub const fn ap1r0_el1(self) -> u64 {
        self.values[3]
    }

    /// Return the raw `ICC_RPR_EL1` value.
    pub const fn rpr_el1(self) -> u64 {
        self.values[4]
    }

    /// Return the raw `ICC_BPR1_EL1` value.
    pub const fn bpr1_el1(self) -> u64 {
        self.values[5]
    }

    /// Return the raw `ICC_CTLR_EL1` value.
    pub const fn ctlr_el1(self) -> u64 {
        self.values[6]
    }

    /// Return the raw `ICC_SRE_EL1` value.
    pub const fn sre_el1(self) -> u64 {
        self.values[7]
    }

    /// Return the raw `ICC_IGRPEN0_EL1` value.
    pub const fn igrpen0_el1(self) -> u64 {
        self.values[8]
    }

    /// Return the raw `ICC_IGRPEN1_EL1` value.
    pub const fn igrpen1_el1(self) -> u64 {
        self.values[9]
    }
}

/// Failure while restoring one arm64 GIC ICC CPU-interface register.
///
/// Hypervisor.framework writes one mutable ICC register at a time and provides
/// no batch transaction. [`Self::completed_writes`] reports the completed
/// prefix when either a write or the derived `ICC_RPR_EL1` validation fails.
/// Callers must retry the complete retained state or discard the vCPU before
/// execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfArm64GicIccRegisterRestoreError {
    failed_register: HvfArm64GicIccRegister,
    operation: HvfArm64GicIccRegisterRestoreOperation,
    completed_writes: usize,
    source: BackendError,
}

/// Operation that failed while restoring complete arm64 GIC ICC state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfArm64GicIccRegisterRestoreOperation {
    /// Write one architecturally mutable ICC register.
    Write,
    /// Read and compare an architecturally derived ICC register.
    ValidateDerived,
}

impl HvfArm64GicIccRegisterRestoreError {
    const fn new(
        failed_register: HvfArm64GicIccRegister,
        operation: HvfArm64GicIccRegisterRestoreOperation,
        completed_writes: usize,
        source: BackendError,
    ) -> Self {
        Self {
            failed_register,
            operation,
            completed_writes,
            source,
        }
    }

    /// Return the architectural ICC register whose restore operation failed.
    pub const fn failed_register(&self) -> HvfArm64GicIccRegister {
        self.failed_register
    }

    /// Return the write or derived-value validation that failed.
    pub const fn operation(&self) -> HvfArm64GicIccRegisterRestoreOperation {
        self.operation
    }

    /// Return the number of ICC registers written before the failure.
    pub const fn completed_writes(&self) -> usize {
        self.completed_writes
    }
}

impl fmt::Display for HvfArm64GicIccRegisterRestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let operation = match self.operation {
            HvfArm64GicIccRegisterRestoreOperation::Write => "write",
            HvfArm64GicIccRegisterRestoreOperation::ValidateDerived => "derived-value validation",
        };
        write!(
            f,
            "failed arm64 GIC {} {operation} after {} successful writes: {}",
            self.failed_register.architectural_name(),
            self.completed_writes,
            self.source
        )
    }
}

impl std::error::Error for HvfArm64GicIccRegisterRestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl HvfGicMetadata {
    pub const FDT_COMPATIBILITY: &'static str = "arm,gic-v3";
    pub const FDT_INTERRUPT_CELLS: u32 = 3;
    pub const FDT_MAINTENANCE_IRQ: u32 = 9;

    pub fn arm64_fdt_gic(&self) -> Arm64FdtGic {
        Arm64FdtGic {
            distributor: self.distributor.into(),
            redistributor: self.redistributor.region.into(),
            compatibility: Self::FDT_COMPATIBILITY,
            interrupt_cells: Self::FDT_INTERRUPT_CELLS,
            maintenance_irq: Self::FDT_MAINTENANCE_IRQ,
            msi: self.msi.map(Into::into),
        }
    }

    pub fn arm64_fdt_timer_interrupts(&self) -> Result<Arm64FdtTimerInterrupts, Arm64FdtError> {
        Arm64FdtTimerInterrupts::from_el1_timer_intids(
            self.timer_interrupts.el1_virtual_timer_intid,
            self.timer_interrupts.el1_physical_timer_intid,
        )
    }
}

impl From<HvfGicRegion> for Arm64FdtRegion {
    fn from(region: HvfGicRegion) -> Self {
        Self {
            base: region.base,
            size: region.size,
        }
    }
}

impl From<HvfGicInterruptRange> for Arm64FdtInterruptRange {
    fn from(range: HvfGicInterruptRange) -> Self {
        Self {
            base: range.base,
            count: range.count,
        }
    }
}

impl From<HvfGicMsiMetadata> for Arm64FdtMsi {
    fn from(metadata: HvfGicMsiMetadata) -> Self {
        Self {
            region: metadata.region.into(),
            interrupt_range: metadata.interrupt_range.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfInterruptLineAllocationError {
    InvalidRange(HvfGicError),
    InvalidLine(GuestInterruptLineError),
    Exhausted { range: HvfGicInterruptRange },
}

impl fmt::Display for HvfInterruptLineAllocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRange(source) => {
                write!(
                    f,
                    "invalid HVF GIC interrupt line allocation range: {source}"
                )
            }
            Self::InvalidLine(source) => {
                write!(f, "invalid HVF GIC interrupt line: {source}")
            }
            Self::Exhausted { range } => {
                write!(
                    f,
                    "HVF GIC SPI interrupt range base={} count={} is exhausted",
                    range.base, range.count
                )
            }
        }
    }
}

impl std::error::Error for HvfInterruptLineAllocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRange(source) => Some(source),
            Self::InvalidLine(source) => Some(source),
            Self::Exhausted { .. } => None,
        }
    }
}

#[derive(Debug)]
pub struct HvfGicInterruptLineAllocator {
    range: HvfGicInterruptRange,
    next: u32,
    remaining: u32,
}

impl HvfGicInterruptLineAllocator {
    pub fn new(range: HvfGicInterruptRange) -> Result<Self, HvfInterruptLineAllocationError> {
        validate_spi_interrupt_range(range)
            .map_err(HvfInterruptLineAllocationError::InvalidRange)?;

        Ok(Self {
            range,
            next: range.base,
            remaining: range.count,
        })
    }

    pub fn from_metadata(
        metadata: &HvfGicMetadata,
    ) -> Result<Self, HvfInterruptLineAllocationError> {
        Self::new(metadata.spi_interrupt_range)
    }

    pub const fn range(&self) -> HvfGicInterruptRange {
        self.range
    }

    pub const fn remaining(&self) -> u32 {
        self.remaining
    }

    pub const fn is_exhausted(&self) -> bool {
        self.remaining == 0
    }

    pub fn allocate(&mut self) -> Result<GuestInterruptLine, HvfInterruptLineAllocationError> {
        if self.remaining == 0 {
            return Err(HvfInterruptLineAllocationError::Exhausted { range: self.range });
        }

        let raw_line = self.next;
        let next = raw_line.checked_add(1).ok_or_else(|| {
            HvfInterruptLineAllocationError::InvalidRange(HvfGicError::InvalidParameter {
                name: "spi_interrupt_range.end_exclusive",
                value: u64::from(raw_line) + 1,
            })
        })?;
        let line = GuestInterruptLine::new(raw_line)
            .map_err(HvfInterruptLineAllocationError::InvalidLine)?;

        self.next = next;
        self.remaining -= 1;

        Ok(line)
    }
}

/// One interrupt identifier allocated from a GIC MSI-only range.
///
/// Values can only be created by [`HvfGicMsiInterruptAllocator`]. They remain
/// distinct from legacy [`GuestInterruptLine`] values accepted by
/// `hv_gic_set_spi` paths.
#[derive(Debug)]
struct HvfGicMsiProvenance;

#[derive(Clone)]
pub struct HvfGicMsiInterrupt {
    intid: u32,
    provenance: Arc<HvfGicMsiProvenance>,
}

impl HvfGicMsiInterrupt {
    pub const fn raw_value(&self) -> u32 {
        self.intid
    }
}

impl PartialEq for HvfGicMsiInterrupt {
    fn eq(&self, other: &Self) -> bool {
        self.intid == other.intid && Arc::ptr_eq(&self.provenance, &other.provenance)
    }
}

impl Eq for HvfGicMsiInterrupt {}

impl fmt::Debug for HvfGicMsiInterrupt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfGicMsiInterrupt")
            .field("intid", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfGicMsiInterruptAllocationError {
    InvalidMetadata(&'static str),
    Exhausted,
}

impl fmt::Display for HvfGicMsiInterruptAllocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMetadata(message) => {
                write!(f, "invalid HVF GIC MSI metadata: {message}")
            }
            Self::Exhausted => f.write_str("HVF GIC MSI interrupt range is exhausted"),
        }
    }
}

impl std::error::Error for HvfGicMsiInterruptAllocationError {}

#[derive(Clone)]
pub struct HvfGicMsiInterruptAllocator {
    range: HvfGicInterruptRange,
    next: Arc<AtomicU32>,
    provenance: Arc<HvfGicMsiProvenance>,
}

impl HvfGicMsiInterruptAllocator {
    fn from_metadata(
        metadata: HvfGicMsiMetadata,
    ) -> Result<Self, HvfGicMsiInterruptAllocationError> {
        validate_msi_metadata(metadata)
            .map_err(HvfGicMsiInterruptAllocationError::InvalidMetadata)?;

        Ok(Self {
            range: metadata.interrupt_range,
            next: Arc::new(AtomicU32::new(metadata.interrupt_range.base)),
            provenance: Arc::new(HvfGicMsiProvenance),
        })
    }

    pub fn remaining(&self) -> u32 {
        let end = self.range.base + self.range.count;
        end.saturating_sub(self.next.load(Ordering::Relaxed))
    }

    pub const fn range(&self) -> HvfGicInterruptRange {
        self.range
    }

    pub fn is_exhausted(&self) -> bool {
        self.remaining() == 0
    }

    pub fn allocate(&self) -> Result<HvfGicMsiInterrupt, HvfGicMsiInterruptAllocationError> {
        let end = self.range.base + self.range.count;
        let intid = self
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                (next < end).then_some(next + 1)
            })
            .map_err(|_| HvfGicMsiInterruptAllocationError::Exhausted)?;
        Ok(HvfGicMsiInterrupt {
            intid,
            provenance: Arc::clone(&self.provenance),
        })
    }
}

impl fmt::Debug for HvfGicMsiInterruptAllocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfGicMsiInterruptAllocator")
            .field("range", &"<redacted>")
            .field("remaining", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfGicMsiSignalError {
    InvalidMetadata(&'static str),
    InterruptOutOfRange,
    Signal { source: HvfGicError },
    InvalidState(&'static str),
}

impl fmt::Display for HvfGicMsiSignalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMetadata(message) => {
                write!(f, "invalid HVF GIC MSI signal metadata: {message}")
            }
            Self::InterruptOutOfRange => {
                f.write_str("HVF GIC MSI interrupt is outside the configured range")
            }
            Self::Signal { source } => write!(f, "failed to send HVF GIC MSI: {source}"),
            Self::InvalidState(message) => {
                write!(f, "invalid HVF GIC MSI signaler state: {message}")
            }
        }
    }
}

impl std::error::Error for HvfGicMsiSignalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Signal { source } => Some(source),
            Self::InvalidMetadata(_) | Self::InterruptOutOfRange | Self::InvalidState(_) => None,
        }
    }
}

#[derive(Clone)]
pub struct HvfGicMsiSignaler {
    address: u64,
    range: HvfGicInterruptRange,
    allocator: HvfGicMsiInterruptAllocator,
    state: Arc<Mutex<HvfGicMsiSignalerState>>,
}

struct HvfGicMsiSignalerState {
    active: bool,
    api: Box<dyn HvfGicMsiSignalApi + Send>,
}

impl HvfGicMsiSignaler {
    /// Return a handle to the one allocation sequence shared by this signaler.
    pub fn allocator(&self) -> HvfGicMsiInterruptAllocator {
        self.allocator.clone()
    }

    pub fn send(&self, interrupt: &HvfGicMsiInterrupt) -> Result<(), HvfGicMsiSignalError> {
        if !Arc::ptr_eq(&self.allocator.provenance, &interrupt.provenance)
            || !interrupt_range_contains(self.range, interrupt.intid)
        {
            return Err(HvfGicMsiSignalError::InterruptOutOfRange);
        }

        let state = self.state.lock().map_err(|_| {
            HvfGicMsiSignalError::InvalidState(GIC_MSI_SIGNALER_LOCK_POISONED_MESSAGE)
        })?;
        if !state.active {
            return Err(HvfGicMsiSignalError::InvalidState(
                GIC_MSI_SIGNALER_INACTIVE_MESSAGE,
            ));
        }
        state
            .api
            .send_msi(self.address, interrupt.intid)
            .map_err(|source| HvfGicMsiSignalError::Signal { source })
    }

    /// Revoke every clone after waiting for any in-flight send to finish.
    pub(crate) fn deactivate(&self) {
        match self.state.lock() {
            Ok(mut state) => state.active = false,
            Err(poisoned) => poisoned.into_inner().active = false,
        }
    }

    fn with_api(
        metadata: HvfGicMsiMetadata,
        api: impl HvfGicMsiSignalApi + Send + 'static,
    ) -> Result<Self, HvfGicMsiSignalError> {
        validate_msi_metadata(metadata).map_err(HvfGicMsiSignalError::InvalidMetadata)?;
        let allocator =
            HvfGicMsiInterruptAllocator::from_metadata(metadata).map_err(|error| match error {
                HvfGicMsiInterruptAllocationError::InvalidMetadata(message) => {
                    HvfGicMsiSignalError::InvalidMetadata(message)
                }
                HvfGicMsiInterruptAllocationError::Exhausted => {
                    HvfGicMsiSignalError::InvalidState("validated MSI metadata has no allocator")
                }
            })?;
        let address = metadata
            .region
            .base
            .checked_add(ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET)
            .ok_or(HvfGicMsiSignalError::InvalidMetadata(
                "message address overflows",
            ))?;

        Ok(Self {
            address,
            range: metadata.interrupt_range,
            allocator,
            state: Arc::new(Mutex::new(HvfGicMsiSignalerState {
                active: true,
                api: Box::new(api),
            })),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_backend_test(metadata: HvfGicMsiMetadata) -> Self {
        Self::with_api(metadata, TestHvfGicMsiSignalApi)
            .expect("backend test MSI metadata should be valid")
    }
}

#[cfg(test)]
#[derive(Debug)]
struct TestHvfGicMsiSignalApi;

#[cfg(test)]
impl HvfGicMsiSignalApi for TestHvfGicMsiSignalApi {
    fn send_msi(&self, _: u64, _: u32) -> Result<(), HvfGicError> {
        Ok(())
    }
}

impl fmt::Debug for HvfGicMsiSignaler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfGicMsiSignaler")
            .field("message", &"<redacted>")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfGicSpiSignalError {
    Backend(HvfGicError),
    InvalidRange(HvfGicError),
    LineOutOfRange {
        line: GuestInterruptLine,
        range: HvfGicInterruptRange,
    },
    Signal {
        line: GuestInterruptLine,
        level: bool,
        source: HvfGicError,
    },
    InvalidState(&'static str),
}

impl fmt::Display for HvfGicSpiSignalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(source) => {
                write!(f, "failed to initialize HVF GIC SPI signaler: {source}")
            }
            Self::InvalidRange(source) => {
                write!(f, "invalid HVF GIC SPI signal range: {source}")
            }
            Self::LineOutOfRange { line, range } => {
                write!(
                    f,
                    "guest interrupt line {line} is outside HVF GIC SPI range base={} count={}",
                    range.base, range.count
                )
            }
            Self::Signal {
                line,
                level,
                source,
            } => {
                write!(
                    f,
                    "failed to set HVF GIC SPI interrupt line {line} to level {level}: {source}"
                )
            }
            Self::InvalidState(message) => {
                write!(f, "invalid HVF GIC SPI signaler state: {message}")
            }
        }
    }
}

impl std::error::Error for HvfGicSpiSignalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(source) | Self::InvalidRange(source) | Self::Signal { source, .. } => {
                Some(source)
            }
            Self::LineOutOfRange { .. } | Self::InvalidState(_) => None,
        }
    }
}

#[derive(Debug)]
pub struct HvfGicSpiSignaler {
    range: HvfGicInterruptRange,
    api: Mutex<Box<dyn HvfGicSpiSignalApi + Send>>,
}

impl HvfGicSpiSignaler {
    pub fn from_metadata(metadata: &HvfGicMetadata) -> Result<Self, HvfGicSpiSignalError> {
        validate_spi_interrupt_range(metadata.spi_interrupt_range)
            .map_err(HvfGicSpiSignalError::InvalidRange)?;
        let api = real_gic_spi_signal_api().map_err(HvfGicSpiSignalError::Backend)?;

        Self::with_api(metadata.spi_interrupt_range, api)
    }

    pub const fn range(&self) -> HvfGicInterruptRange {
        self.range
    }

    /// Validate one SPI line without changing its interrupt level.
    pub fn validate_line(&self, line: GuestInterruptLine) -> Result<(), HvfGicSpiSignalError> {
        validate_spi_signal_line(self.range, line)
    }

    pub fn set_level(
        &self,
        line: GuestInterruptLine,
        level: bool,
    ) -> Result<(), HvfGicSpiSignalError> {
        self.validate_line(line)?;

        let api = self.api.lock().map_err(|_| {
            HvfGicSpiSignalError::InvalidState(GIC_SPI_SIGNALER_LOCK_POISONED_MESSAGE)
        })?;
        api.set_spi(line.raw_value(), level)
            .map_err(|source| HvfGicSpiSignalError::Signal {
                line,
                level,
                source,
            })
    }

    fn with_api(
        range: HvfGicInterruptRange,
        api: impl HvfGicSpiSignalApi + Send + 'static,
    ) -> Result<Self, HvfGicSpiSignalError> {
        validate_spi_interrupt_range(range).map_err(HvfGicSpiSignalError::InvalidRange)?;

        Ok(Self {
            range,
            api: Mutex::new(Box::new(api)),
        })
    }
}

impl InterruptSink for HvfGicSpiSignaler {
    fn signal(&self, line: GuestInterruptLine) -> Result<(), InterruptSignalError> {
        self.set_level(line, true)
            .map_err(|source| InterruptSignalError::new(source.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfGicError {
    Backend(BackendError),
    Unsupported(&'static str),
    InvalidState(&'static str),
    MissingSymbol(&'static str),
    ConfigCreateFailed,
    StateCreateFailed,
    StateAllocationFailed {
        size: u64,
    },
    StateTooLarge {
        size: u64,
        maximum: usize,
    },
    InvalidParameter {
        name: &'static str,
        value: u64,
    },
    InvalidMsiParameter {
        name: &'static str,
    },
    InsufficientMsiInterruptCapacity,
    AddressUnderflow {
        region: &'static str,
        limit: u64,
        size: u64,
    },
    UnalignedAddress {
        region: &'static str,
        address: u64,
        alignment: u64,
    },
    RegionOverlap {
        first: &'static str,
        second: &'static str,
    },
    RegionOverlapsDram {
        region: &'static str,
        end_exclusive: u64,
        dram_start: u64,
    },
}

impl fmt::Display for HvfGicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(source) => write!(f, "{source}"),
            Self::Unsupported(message) => write!(f, "unsupported GIC operation: {message}"),
            Self::InvalidState(message) => write!(f, "invalid GIC state: {message}"),
            Self::MissingSymbol(symbol) => write!(
                f,
                "Hypervisor.framework GIC symbol {symbol} is unavailable; macOS 15.0 or newer is required"
            ),
            Self::ConfigCreateFailed => {
                f.write_str("failed to create Hypervisor.framework GIC configuration")
            }
            Self::StateCreateFailed => {
                f.write_str("failed to create Hypervisor.framework GIC state object")
            }
            Self::StateAllocationFailed { size } => {
                write!(
                    f,
                    "failed to allocate {size} bytes for Hypervisor.framework GIC state"
                )
            }
            Self::StateTooLarge { size, maximum } => write!(
                f,
                "Hypervisor.framework GIC state size {size} exceeds snapshot limit {maximum}"
            ),
            Self::InvalidParameter { name, value } => {
                write!(
                    f,
                    "invalid Hypervisor.framework GIC parameter {name}={value}"
                )
            }
            Self::InvalidMsiParameter { name } => {
                write!(f, "invalid Hypervisor.framework GIC MSI parameter {name}")
            }
            Self::InsufficientMsiInterruptCapacity => {
                f.write_str("insufficient Hypervisor.framework GIC MSI interrupt capacity")
            }
            Self::AddressUnderflow {
                region,
                limit,
                size,
            } => write!(
                f,
                "GIC {region} region of {size} bytes cannot fit below 0x{limit:x}"
            ),
            Self::UnalignedAddress {
                region,
                address,
                alignment,
            } => write!(
                f,
                "GIC {region} base 0x{address:x} is not aligned to {alignment} bytes"
            ),
            Self::RegionOverlap { first, second } => {
                write!(f, "GIC {first} region overlaps {second} region")
            }
            Self::RegionOverlapsDram {
                region,
                end_exclusive,
                dram_start,
            } => write!(
                f,
                "GIC {region} region ending at 0x{end_exclusive:x} overlaps DRAM starting at 0x{dram_start:x}"
            ),
        }
    }
}

impl std::error::Error for HvfGicError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(source) => Some(source),
            Self::Unsupported(_)
            | Self::InvalidState(_)
            | Self::MissingSymbol(_)
            | Self::ConfigCreateFailed
            | Self::StateCreateFailed
            | Self::StateAllocationFailed { .. }
            | Self::StateTooLarge { .. }
            | Self::InvalidParameter { .. }
            | Self::InvalidMsiParameter { .. }
            | Self::InsufficientMsiInterruptCapacity
            | Self::AddressUnderflow { .. }
            | Self::UnalignedAddress { .. }
            | Self::RegionOverlap { .. }
            | Self::RegionOverlapsDram { .. } => None,
        }
    }
}

impl From<BackendError> for HvfGicError {
    fn from(source: BackendError) -> Self {
        Self::Backend(source)
    }
}

pub(crate) struct CreatedHvfGic {
    pub(crate) metadata: HvfGicMetadata,
    pub(crate) msi_signaler: Option<HvfGicMsiSignaler>,
}

impl fmt::Debug for CreatedHvfGic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreatedHvfGic")
            .field("metadata", &self.metadata)
            .field("msi_signaler", &self.msi_signaler)
            .finish()
    }
}

pub(crate) trait HvfGicCreator: fmt::Debug + Send + Sync {
    fn create_gic(&self, msi: Option<HvfGicMsiConfiguration>)
    -> Result<CreatedHvfGic, HvfGicError>;
}

pub(crate) struct HvfGicStateSnapshotter {
    capture: Box<dyn HvfGicStateCapture>,
}

pub(crate) struct HvfGicStateRestorer {
    api: Box<dyn HvfGicStateRestoreApi>,
}

pub(crate) struct HvfGicIccRegisterReader {
    api: Box<dyn HvfGicIccRegisterApi>,
}

pub(crate) struct HvfGicIccRegisterRestorer {
    api: Box<dyn HvfGicIccRegisterWriteApi>,
}

trait HvfGicStateCapture: fmt::Debug {
    fn capture(&self, maximum_size: Option<usize>) -> Result<HvfGicDeviceState, HvfGicError>;
}

trait HvfGicStateApi: fmt::Debug {
    type State;

    fn create_state(&self) -> Result<Self::State, HvfGicError>;
    fn state_size(&self, state: &Self::State) -> Result<usize, HvfGicError>;
    fn copy_state(&self, state: &Self::State, data: &mut [u8]) -> Result<(), HvfGicError>;
    fn release_state(&self, state: Self::State);
}

trait HvfGicStateRestoreApi: fmt::Debug {
    fn restore(&self, data: &[u8]) -> Result<(), HvfGicError>;
}

trait HvfGicIccRegisterApi: fmt::Debug {
    fn get_icc_reg(
        &self,
        vcpu: crate::ffi::HvVcpu,
        register: HvfArm64GicIccRegister,
    ) -> Result<u64, BackendError>;
}

trait HvfGicIccRegisterWriteApi: fmt::Debug {
    fn set_icc_reg(
        &self,
        vcpu: crate::ffi::HvVcpu,
        register: HvfArm64GicIccRegister,
        value: u64,
    ) -> Result<(), BackendError>;
}

impl HvfGicStateSnapshotter {
    pub(crate) fn new() -> Result<Self, HvfGicError> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            Ok(Self {
                capture: Box::new(LoadedHvfGicStateCaptureApi::load()?),
            })
        }

        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        Err(HvfGicError::Unsupported(
            crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
        ))
    }

    pub(crate) fn capture(&self) -> Result<HvfGicDeviceState, HvfGicError> {
        self.capture.capture(None)
    }

    pub(crate) fn capture_bounded(
        &self,
        maximum_size: usize,
    ) -> Result<HvfGicDeviceState, HvfGicError> {
        self.capture.capture(Some(maximum_size))
    }
}

impl fmt::Debug for HvfGicStateSnapshotter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfGicStateSnapshotter")
            .finish_non_exhaustive()
    }
}

impl HvfGicStateRestorer {
    pub(crate) fn new() -> Result<Self, HvfGicError> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            Ok(Self {
                api: Box::new(LoadedHvfGicStateRestoreApi::load()?),
            })
        }

        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        Err(HvfGicError::Unsupported(
            crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
        ))
    }

    pub(crate) fn restore(&self, state: &HvfGicDeviceState) -> Result<(), HvfGicError> {
        restore_gic_device_state_with_api(self.api.as_ref(), state)
    }

    #[cfg(test)]
    fn with_api(api: impl HvfGicStateRestoreApi + 'static) -> Self {
        Self { api: Box::new(api) }
    }
}

impl fmt::Debug for HvfGicStateRestorer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfGicStateRestorer")
            .finish_non_exhaustive()
    }
}

impl HvfGicIccRegisterReader {
    pub(crate) fn new() -> Result<Self, HvfGicError> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            Ok(Self {
                api: Box::new(LoadedHvfGicIccRegisterApi::load()?),
            })
        }

        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        Err(HvfGicError::Unsupported(
            crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
        ))
    }

    pub(crate) fn capture(
        &self,
        vcpu: crate::ffi::HvVcpu,
    ) -> Result<HvfArm64GicIccRegisterState, HvfGicError> {
        capture_arm64_gic_icc_register_state_with_api(self.api.as_ref(), vcpu)
    }

    fn read(
        &self,
        vcpu: crate::ffi::HvVcpu,
        register: HvfArm64GicIccRegister,
    ) -> Result<u64, BackendError> {
        self.api.get_icc_reg(vcpu, register)
    }

    #[cfg(test)]
    fn with_api(api: impl HvfGicIccRegisterApi + 'static) -> Self {
        Self { api: Box::new(api) }
    }
}

impl fmt::Debug for HvfGicIccRegisterReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfGicIccRegisterReader")
            .finish_non_exhaustive()
    }
}

impl HvfGicIccRegisterRestorer {
    pub(crate) fn new() -> Result<Self, HvfGicError> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            Ok(Self {
                api: Box::new(LoadedHvfGicIccRegisterWriteApi::load()?),
            })
        }

        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        Err(HvfGicError::Unsupported(
            crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
        ))
    }

    pub(crate) fn restore(
        &self,
        reader: &HvfGicIccRegisterReader,
        vcpu: crate::ffi::HvVcpu,
        state: &HvfArm64GicIccRegisterState,
    ) -> Result<(), HvfArm64GicIccRegisterRestoreError> {
        restore_arm64_gic_icc_register_state_with(
            state,
            |register, value| self.api.set_icc_reg(vcpu, register, value),
            |register| reader.read(vcpu, register),
        )
    }

    #[cfg(test)]
    fn with_api(api: impl HvfGicIccRegisterWriteApi + 'static) -> Self {
        Self { api: Box::new(api) }
    }
}

impl fmt::Debug for HvfGicIccRegisterRestorer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfGicIccRegisterRestorer")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
pub(crate) struct RealHvfGicCreator;

#[derive(Debug, Clone, Copy)]
struct HvfGicParameters {
    distributor_size: u64,
    distributor_alignment: u64,
    redistributor_region_size: u64,
    redistributor_size: u64,
    redistributor_alignment: u64,
    spi_interrupt_range: HvfGicInterruptRange,
    timer_interrupts: HvfGicTimerInterrupts,
    msi: Option<HvfGicMsiParameters>,
}

#[derive(Debug, Clone, Copy)]
struct HvfGicMsiParameters {
    region_size: u64,
    region_alignment: u64,
    interrupt_count: NonZeroU32,
}

trait HvfGicSpiSignalApi: fmt::Debug {
    fn set_spi(&self, intid: u32, level: bool) -> Result<(), HvfGicError>;
}

trait HvfGicMsiSignalApi: fmt::Debug {
    fn send_msi(&self, address: u64, intid: u32) -> Result<(), HvfGicError>;
}

pub(crate) struct HvfGicPpiPendingWriter {
    api: Box<dyn HvfGicPpiPendingApi>,
}

trait HvfGicPpiPendingApi: fmt::Debug {
    fn set_redistributor_reg(
        &self,
        vcpu: crate::ffi::HvVcpu,
        reg: u32,
        value: u64,
    ) -> Result<(), HvfGicError>;
}

trait HvfGicApi {
    type Config;

    fn distributor_size(&self) -> Result<u64, HvfGicError>;
    fn distributor_alignment(&self) -> Result<u64, HvfGicError>;
    fn redistributor_region_size(&self) -> Result<u64, HvfGicError>;
    fn redistributor_size(&self) -> Result<u64, HvfGicError>;
    fn redistributor_alignment(&self) -> Result<u64, HvfGicError>;
    fn msi_region_size(&self) -> Result<u64, HvfGicError>;
    fn msi_region_alignment(&self) -> Result<u64, HvfGicError>;
    fn spi_interrupt_range(&self) -> Result<HvfGicInterruptRange, HvfGicError>;
    fn intid(&self, interrupt: u16) -> Result<u32, HvfGicError>;
    fn create_config(&self) -> Result<Self::Config, HvfGicError>;
    fn set_distributor_base(&self, config: &mut Self::Config, base: u64)
    -> Result<(), HvfGicError>;
    fn set_redistributor_base(
        &self,
        config: &mut Self::Config,
        base: u64,
    ) -> Result<(), HvfGicError>;
    fn set_msi_region_base(&self, config: &mut Self::Config, base: u64) -> Result<(), HvfGicError>;
    fn set_msi_interrupt_range(
        &self,
        config: &mut Self::Config,
        range: HvfGicInterruptRange,
    ) -> Result<(), HvfGicError>;
    fn create_gic(&self, config: &Self::Config) -> Result<(), HvfGicError>;
    fn release_config(&self, config: Self::Config);
}

impl HvfGicCreator for RealHvfGicCreator {
    fn create_gic(
        &self,
        msi: Option<HvfGicMsiConfiguration>,
    ) -> Result<CreatedHvfGic, HvfGicError> {
        create_real_gic(msi)
    }
}

impl HvfGicPpiPendingWriter {
    pub(crate) fn new() -> Result<Self, HvfGicError> {
        let api = real_gic_ppi_pending_api()?;

        Ok(Self { api: Box::new(api) })
    }

    pub(crate) fn set_pending(
        &self,
        vcpu: crate::ffi::HvVcpu,
        intid: u32,
        pending: bool,
    ) -> Result<(), HvfGicError> {
        set_ppi_pending_with_api(self.api.as_ref(), vcpu, intid, pending)
    }

    #[cfg(test)]
    fn with_api(api: impl HvfGicPpiPendingApi + 'static) -> Self {
        Self { api: Box::new(api) }
    }
}

impl fmt::Debug for HvfGicPpiPendingWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfGicPpiPendingWriter")
            .finish_non_exhaustive()
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn real_gic_spi_signal_api() -> Result<LoadedHvfGicSpiSignalApi, HvfGicError> {
    LoadedHvfGicSpiSignalApi::load()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn real_gic_ppi_pending_api() -> Result<LoadedHvfGicPpiPendingApi, HvfGicError> {
    LoadedHvfGicPpiPendingApi::load()
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn real_gic_spi_signal_api() -> Result<UnsupportedHvfGicSpiSignalApi, HvfGicError> {
    Err(HvfGicError::Unsupported(
        crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
    ))
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn real_gic_ppi_pending_api() -> Result<UnsupportedHvfGicPpiPendingApi, HvfGicError> {
    Err(HvfGicError::Unsupported(
        crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
    ))
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[derive(Debug)]
struct UnsupportedHvfGicSpiSignalApi;

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[derive(Debug)]
struct UnsupportedHvfGicPpiPendingApi;

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
impl HvfGicSpiSignalApi for UnsupportedHvfGicSpiSignalApi {
    fn set_spi(&self, _: u32, _: bool) -> Result<(), HvfGicError> {
        Err(HvfGicError::Unsupported(
            crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
        ))
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
impl HvfGicPpiPendingApi for UnsupportedHvfGicPpiPendingApi {
    fn set_redistributor_reg(
        &self,
        _: crate::ffi::HvVcpu,
        _: u32,
        _: u64,
    ) -> Result<(), HvfGicError> {
        Err(HvfGicError::Unsupported(
            crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
        ))
    }
}

#[cfg(test)]
fn create_gic_with_api(
    api: &impl HvfGicApi,
    msi: Option<HvfGicMsiConfiguration>,
) -> Result<HvfGicMetadata, HvfGicError> {
    let metadata = prepare_gic_metadata_with_api(api, msi)?;
    configure_gic_with_api(api, metadata)?;
    Ok(metadata)
}

fn prepare_gic_metadata_with_api(
    api: &impl HvfGicApi,
    msi: Option<HvfGicMsiConfiguration>,
) -> Result<HvfGicMetadata, HvfGicError> {
    let parameters = query_parameters(api, msi)?;
    metadata_from_parameters(parameters)
}

fn configure_gic_with_api(
    api: &impl HvfGicApi,
    metadata: HvfGicMetadata,
) -> Result<(), HvfGicError> {
    let mut config = GicConfigGuard::new(api)?;

    api.set_distributor_base(config.config_mut()?, metadata.distributor.base)?;
    api.set_redistributor_base(config.config_mut()?, metadata.redistributor.region.base)?;
    if let Some(msi) = metadata.msi {
        api.set_msi_region_base(config.config_mut()?, msi.region.base)?;
        api.set_msi_interrupt_range(config.config_mut()?, msi.interrupt_range)?;
    }
    api.create_gic(config.config()?)?;
    Ok(())
}

fn query_parameters(
    api: &impl HvfGicApi,
    msi: Option<HvfGicMsiConfiguration>,
) -> Result<HvfGicParameters, HvfGicError> {
    let msi = msi
        .map(|configuration| {
            Ok::<HvfGicMsiParameters, HvfGicError>(HvfGicMsiParameters {
                region_size: api.msi_region_size()?,
                region_alignment: api.msi_region_alignment()?,
                interrupt_count: configuration.interrupt_count(),
            })
        })
        .transpose()?;

    Ok(HvfGicParameters {
        distributor_size: api.distributor_size()?,
        distributor_alignment: api.distributor_alignment()?,
        redistributor_region_size: api.redistributor_region_size()?,
        redistributor_size: api.redistributor_size()?,
        redistributor_alignment: api.redistributor_alignment()?,
        spi_interrupt_range: api.spi_interrupt_range()?,
        timer_interrupts: HvfGicTimerInterrupts {
            el1_virtual_timer_intid: api.intid(HV_GIC_INT_EL1_VIRTUAL_TIMER)?,
            el1_physical_timer_intid: api.intid(HV_GIC_INT_EL1_PHYSICAL_TIMER)?,
        },
        msi,
    })
}

fn metadata_from_parameters(parameters: HvfGicParameters) -> Result<HvfGicMetadata, HvfGicError> {
    validate_parameter(
        "distributor_size",
        parameters.distributor_size,
        ParameterRule::NonZero,
    )?;
    validate_parameter(
        "distributor_base_alignment",
        parameters.distributor_alignment,
        ParameterRule::PowerOfTwo,
    )?;
    validate_parameter(
        "redistributor_region_size",
        parameters.redistributor_region_size,
        ParameterRule::NonZero,
    )?;
    validate_parameter(
        "redistributor_size",
        parameters.redistributor_size,
        ParameterRule::NonZero,
    )?;
    validate_parameter(
        "redistributor_base_alignment",
        parameters.redistributor_alignment,
        ParameterRule::PowerOfTwo,
    )?;
    if parameters.redistributor_size > parameters.redistributor_region_size {
        return Err(HvfGicError::InvalidParameter {
            name: "redistributor_size",
            value: parameters.redistributor_size,
        });
    }
    validate_spi_interrupt_range(parameters.spi_interrupt_range)?;
    validate_timer_interrupts(parameters.timer_interrupts)?;

    let (spi_interrupt_range, msi_interrupt_range) = parameters.msi.map_or_else(
        || Ok((parameters.spi_interrupt_range, None)),
        |msi| {
            validate_msi_parameter("region_size", msi.region_size, ParameterRule::NonZero)?;
            validate_msi_parameter(
                "region_base_alignment",
                msi.region_alignment,
                ParameterRule::PowerOfTwo,
            )?;
            let minimum_size = ARM64_GICV2M_MSI_IIDR_OFFSET
                .checked_add(GIC_MSI_REGISTER_SIZE)
                .ok_or(HvfGicError::InvalidMsiParameter {
                    name: "region_size",
                })?;
            if msi.region_size < minimum_size {
                return Err(HvfGicError::InvalidMsiParameter {
                    name: "region_size",
                });
            }
            let (legacy, reserved) =
                partition_spi_interrupt_range(parameters.spi_interrupt_range, msi.interrupt_count)?;
            Ok((legacy, Some(reserved)))
        },
    )?;

    let distributor = aligned_region_below(
        "distributor",
        MMIO32_MEM_START,
        parameters.distributor_size,
        parameters.distributor_alignment,
    )?;
    let redistributor = aligned_region_below(
        "redistributor",
        distributor.base,
        parameters.redistributor_region_size,
        parameters.redistributor_alignment,
    )?;

    validate_regions_do_not_overlap("distributor", distributor, "redistributor", redistributor)?;
    validate_region_below_dram("distributor", distributor)?;
    validate_region_below_dram("redistributor", redistributor)?;

    let msi = match (parameters.msi, msi_interrupt_range) {
        (Some(parameters), Some(interrupt_range)) => {
            let region = aligned_region_below(
                "msi",
                redistributor.base,
                parameters.region_size,
                parameters.region_alignment,
            )
            .map_err(|_| HvfGicError::InvalidMsiParameter {
                name: "region_placement",
            })?;
            validate_regions_do_not_overlap("distributor", distributor, "msi", region).map_err(
                |_| HvfGicError::InvalidMsiParameter {
                    name: "region_overlap",
                },
            )?;
            validate_regions_do_not_overlap("redistributor", redistributor, "msi", region)
                .map_err(|_| HvfGicError::InvalidMsiParameter {
                    name: "region_overlap",
                })?;
            validate_region_below_dram("msi", region).map_err(|_| {
                HvfGicError::InvalidMsiParameter {
                    name: "region_placement",
                }
            })?;
            Some(HvfGicMsiMetadata {
                region,
                interrupt_range,
            })
        }
        (None, None) => None,
        _ => {
            return Err(HvfGicError::InvalidState(
                "GIC MSI parameters and interrupt range disagree",
            ));
        }
    };

    Ok(HvfGicMetadata {
        distributor,
        redistributor: HvfGicRedistributor {
            region: redistributor,
            single_redistributor_size: parameters.redistributor_size,
        },
        spi_interrupt_range,
        timer_interrupts: parameters.timer_interrupts,
        msi,
    })
}

#[derive(Debug, Clone, Copy)]
enum ParameterRule {
    NonZero,
    PowerOfTwo,
}

fn validate_parameter(
    name: &'static str,
    value: u64,
    rule: ParameterRule,
) -> Result<(), HvfGicError> {
    let valid = match rule {
        ParameterRule::NonZero => value != 0,
        ParameterRule::PowerOfTwo => value != 0 && value.is_power_of_two(),
    };

    if valid {
        Ok(())
    } else {
        Err(HvfGicError::InvalidParameter { name, value })
    }
}

fn validate_msi_parameter(
    name: &'static str,
    value: u64,
    rule: ParameterRule,
) -> Result<(), HvfGicError> {
    let valid = match rule {
        ParameterRule::NonZero => value != 0,
        ParameterRule::PowerOfTwo => value != 0 && value.is_power_of_two(),
    };

    if valid {
        Ok(())
    } else {
        Err(HvfGicError::InvalidMsiParameter { name })
    }
}

fn validate_spi_interrupt_range(range: HvfGicInterruptRange) -> Result<(), HvfGicError> {
    if range.base < FIRST_SPI_INTID {
        return Err(HvfGicError::InvalidParameter {
            name: "spi_interrupt_range.base",
            value: u64::from(range.base),
        });
    }
    if range.count == 0 {
        return Err(HvfGicError::InvalidParameter {
            name: "spi_interrupt_range.count",
            value: 0,
        });
    }
    if range.base.checked_add(range.count).is_none() {
        return Err(HvfGicError::InvalidParameter {
            name: "spi_interrupt_range.end_exclusive",
            value: u64::from(range.base) + u64::from(range.count),
        });
    }

    Ok(())
}

fn partition_spi_interrupt_range(
    complete: HvfGicInterruptRange,
    msi_count: NonZeroU32,
) -> Result<(HvfGicInterruptRange, HvfGicInterruptRange), HvfGicError> {
    validate_spi_interrupt_range(complete)?;
    let complete_end =
        complete
            .base
            .checked_add(complete.count)
            .ok_or(HvfGicError::InvalidMsiParameter {
                name: "interrupt_range",
            })?;
    let usable_end = complete_end.min(ARM64_GICV2M_SPI_END_EXCLUSIVE);
    let Some(usable_count) = usable_end.checked_sub(complete.base) else {
        return Err(HvfGicError::InsufficientMsiInterruptCapacity);
    };
    let msi_count = msi_count.get();
    if msi_count >= usable_count {
        return Err(HvfGicError::InsufficientMsiInterruptCapacity);
    }

    let msi_base = usable_end - msi_count;
    let legacy_count = msi_base - complete.base;
    let legacy = HvfGicInterruptRange {
        base: complete.base,
        count: legacy_count,
    };
    let msi = HvfGicInterruptRange {
        base: msi_base,
        count: msi_count,
    };
    validate_spi_interrupt_range(legacy).map_err(|_| HvfGicError::InvalidMsiParameter {
        name: "legacy_interrupt_range",
    })?;
    validate_spi_interrupt_range(msi).map_err(|_| HvfGicError::InvalidMsiParameter {
        name: "interrupt_range",
    })?;

    Ok((legacy, msi))
}

fn validate_msi_metadata(metadata: HvfGicMsiMetadata) -> Result<(), &'static str> {
    if metadata.region.size == 0 {
        return Err("message frame is empty");
    }
    let Some(region_end) = metadata.region.base.checked_add(metadata.region.size) else {
        return Err("message frame overflows");
    };
    let Some(message_address) = metadata
        .region
        .base
        .checked_add(ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET)
    else {
        return Err("message address overflows");
    };
    let Some(message_end) = message_address.checked_add(GIC_MSI_REGISTER_SIZE) else {
        return Err("message register overflows");
    };
    if message_end > region_end {
        return Err("message frame does not contain SETSPI");
    }
    if metadata.interrupt_range.base < FIRST_SPI_INTID {
        return Err("interrupt range starts below the SPI domain");
    }
    if metadata.interrupt_range.count == 0 {
        return Err("interrupt range is empty");
    }
    let Some(interrupt_end) = metadata
        .interrupt_range
        .base
        .checked_add(metadata.interrupt_range.count)
    else {
        return Err("interrupt range overflows");
    };
    if interrupt_end > ARM64_GICV2M_SPI_END_EXCLUSIVE {
        return Err("interrupt range exceeds the GICv2m SPI domain");
    }

    Ok(())
}

fn interrupt_range_contains(range: HvfGicInterruptRange, intid: u32) -> bool {
    range
        .base
        .checked_add(range.count)
        .is_some_and(|end| (range.base..end).contains(&intid))
}

fn validate_spi_signal_line(
    range: HvfGicInterruptRange,
    line: GuestInterruptLine,
) -> Result<(), HvfGicSpiSignalError> {
    validate_spi_interrupt_range(range).map_err(HvfGicSpiSignalError::InvalidRange)?;

    let raw_line = line.raw_value();
    let end_exclusive = range.base + range.count;
    if (range.base..end_exclusive).contains(&raw_line) {
        Ok(())
    } else {
        Err(HvfGicSpiSignalError::LineOutOfRange { line, range })
    }
}

fn validate_timer_interrupts(timers: HvfGicTimerInterrupts) -> Result<(), HvfGicError> {
    validate_ppi_intid("el1_virtual_timer_intid", timers.el1_virtual_timer_intid)?;
    validate_ppi_intid("el1_physical_timer_intid", timers.el1_physical_timer_intid)?;
    if timers.el1_virtual_timer_intid == timers.el1_physical_timer_intid {
        return Err(HvfGicError::InvalidParameter {
            name: "timer_interrupts",
            value: u64::from(timers.el1_virtual_timer_intid),
        });
    }

    Ok(())
}

fn validate_ppi_intid(name: &'static str, intid: u32) -> Result<(), HvfGicError> {
    if (FIRST_PPI_INTID..FIRST_SPI_INTID).contains(&intid) {
        Ok(())
    } else {
        Err(HvfGicError::InvalidParameter {
            name,
            value: u64::from(intid),
        })
    }
}

pub(crate) fn validate_gic_ppi_pending_intid(intid: u32) -> Result<(), HvfGicError> {
    validate_ppi_intid("ppi_intid", intid)
}

fn set_ppi_pending_with_api(
    api: &dyn HvfGicPpiPendingApi,
    vcpu: crate::ffi::HvVcpu,
    intid: u32,
    pending: bool,
) -> Result<(), HvfGicError> {
    validate_gic_ppi_pending_intid(intid)?;

    let reg = if pending {
        HV_GIC_REDISTRIBUTOR_REG_GICR_ISPENDR0
    } else {
        HV_GIC_REDISTRIBUTOR_REG_GICR_ICPENDR0
    };
    let value = 1u64 << intid;

    api.set_redistributor_reg(vcpu, reg, value)
}

fn aligned_region_below(
    region: &'static str,
    limit: u64,
    size: u64,
    alignment: u64,
) -> Result<HvfGicRegion, HvfGicError> {
    let Some(unadjusted_base) = limit.checked_sub(size) else {
        return Err(HvfGicError::AddressUnderflow {
            region,
            limit,
            size,
        });
    };

    let base = unadjusted_base & !(alignment - 1);
    let Some(end_exclusive) = base.checked_add(size) else {
        return Err(HvfGicError::AddressUnderflow {
            region,
            limit,
            size,
        });
    };
    if end_exclusive > limit {
        return Err(HvfGicError::AddressUnderflow {
            region,
            limit,
            size,
        });
    }
    if !base.is_multiple_of(alignment) {
        return Err(HvfGicError::UnalignedAddress {
            region,
            address: base,
            alignment,
        });
    }

    Ok(HvfGicRegion { base, size })
}

fn validate_regions_do_not_overlap(
    first_name: &'static str,
    first: HvfGicRegion,
    second_name: &'static str,
    second: HvfGicRegion,
) -> Result<(), HvfGicError> {
    if first.overlaps(second) {
        Err(HvfGicError::RegionOverlap {
            first: first_name,
            second: second_name,
        })
    } else {
        Ok(())
    }
}

fn validate_region_below_dram(
    region_name: &'static str,
    region: HvfGicRegion,
) -> Result<(), HvfGicError> {
    if region.end_exclusive() > DRAM_MEM_START {
        Err(HvfGicError::RegionOverlapsDram {
            region: region_name,
            end_exclusive: region.end_exclusive(),
            dram_start: DRAM_MEM_START,
        })
    } else {
        Ok(())
    }
}

struct GicConfigGuard<'api, Api: HvfGicApi + ?Sized> {
    api: &'api Api,
    config: Option<Api::Config>,
}

impl<'api, Api: HvfGicApi + ?Sized> GicConfigGuard<'api, Api> {
    fn new(api: &'api Api) -> Result<Self, HvfGicError> {
        Ok(Self {
            config: Some(api.create_config()?),
            api,
        })
    }

    fn config(&self) -> Result<&Api::Config, HvfGicError> {
        self.config.as_ref().ok_or(HvfGicError::InvalidState(
            "GIC config has already been released",
        ))
    }

    fn config_mut(&mut self) -> Result<&mut Api::Config, HvfGicError> {
        self.config.as_mut().ok_or(HvfGicError::InvalidState(
            "GIC config has already been released",
        ))
    }
}

impl<Api: HvfGicApi + ?Sized> Drop for GicConfigGuard<'_, Api> {
    fn drop(&mut self) {
        if let Some(config) = self.config.take() {
            self.api.release_config(config);
        }
    }
}

struct GicStateGuard<'api, Api: HvfGicStateApi + ?Sized> {
    api: &'api Api,
    state: Option<Api::State>,
}

impl<'api, Api: HvfGicStateApi + ?Sized> GicStateGuard<'api, Api> {
    fn new(api: &'api Api) -> Result<Self, HvfGicError> {
        Ok(Self {
            api,
            state: Some(api.create_state()?),
        })
    }

    fn state(&self) -> Result<&Api::State, HvfGicError> {
        self.state.as_ref().ok_or(HvfGicError::InvalidState(
            "GIC state object has already been released",
        ))
    }
}

impl<Api: HvfGicStateApi + ?Sized> Drop for GicStateGuard<'_, Api> {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            self.api.release_state(state);
        }
    }
}

#[cfg(test)]
fn capture_gic_device_state_with_api<Api: HvfGicStateApi + ?Sized>(
    api: &Api,
) -> Result<HvfGicDeviceState, HvfGicError> {
    capture_gic_device_state_with_api_bounded(api, None)
}

fn capture_gic_device_state_with_api_bounded<Api: HvfGicStateApi + ?Sized>(
    api: &Api,
    maximum_size: Option<usize>,
) -> Result<HvfGicDeviceState, HvfGicError> {
    let state = GicStateGuard::new(api)?;
    let size = api.state_size(state.state()?)?;
    if size == 0 {
        return Err(HvfGicError::InvalidParameter {
            name: "gic_state_size",
            value: 0,
        });
    }

    let reported_size = u64::try_from(size).unwrap_or(u64::MAX);
    if let Some(maximum) = maximum_size
        && size > maximum
    {
        return Err(HvfGicError::StateTooLarge {
            size: reported_size,
            maximum,
        });
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| HvfGicError::StateAllocationFailed {
            size: reported_size,
        })?;
    bytes.resize(size, 0);
    api.copy_state(state.state()?, &mut bytes)?;

    Ok(HvfGicDeviceState::new(bytes))
}

fn restore_gic_device_state_with_api<Api: HvfGicStateRestoreApi + ?Sized>(
    api: &Api,
    state: &HvfGicDeviceState,
) -> Result<(), HvfGicError> {
    if state.is_empty() {
        return Err(HvfGicError::InvalidParameter {
            name: "gic_state_size",
            value: 0,
        });
    }

    api.restore(state.as_bytes())
}

fn capture_arm64_gic_icc_register_state_with_api<Api: HvfGicIccRegisterApi + ?Sized>(
    api: &Api,
    vcpu: crate::ffi::HvVcpu,
) -> Result<HvfArm64GicIccRegisterState, HvfGicError> {
    let mut values = [0; ARM64_GIC_EL1_ICC_REGISTERS.len()];
    for (value, register) in values.iter_mut().zip(ARM64_GIC_EL1_ICC_REGISTERS) {
        *value = api.get_icc_reg(vcpu, register)?;
    }

    Ok(HvfArm64GicIccRegisterState::new(values))
}

pub(crate) fn restore_arm64_gic_icc_register_state_with(
    state: &HvfArm64GicIccRegisterState,
    mut set_register: impl FnMut(HvfArm64GicIccRegister, u64) -> Result<(), BackendError>,
    mut read_register: impl FnMut(HvfArm64GicIccRegister) -> Result<u64, BackendError>,
) -> Result<(), HvfArm64GicIccRegisterRestoreError> {
    let mut completed_writes = 0;
    for (register, value) in ARM64_GIC_EL1_ICC_REGISTERS.into_iter().zip(state.values) {
        if register == HvfArm64GicIccRegister::RprEl1 {
            let restored_value = read_register(register).map_err(|source| {
                HvfArm64GicIccRegisterRestoreError::new(
                    register,
                    HvfArm64GicIccRegisterRestoreOperation::ValidateDerived,
                    completed_writes,
                    source,
                )
            })?;
            if restored_value != value {
                return Err(HvfArm64GicIccRegisterRestoreError::new(
                    register,
                    HvfArm64GicIccRegisterRestoreOperation::ValidateDerived,
                    completed_writes,
                    BackendError::InvalidState(GIC_ICC_RPR_MISMATCH_MESSAGE),
                ));
            }
            continue;
        }

        set_register(register, value).map_err(|source| {
            HvfArm64GicIccRegisterRestoreError::new(
                register,
                HvfArm64GicIccRegisterRestoreOperation::Write,
                completed_writes,
                source,
            )
        })?;
        completed_writes += 1;
    }

    Ok(())
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn create_real_gic(msi: Option<HvfGicMsiConfiguration>) -> Result<CreatedHvfGic, HvfGicError> {
    let api = LoadedHvfGicApi::load(msi.is_some())?;
    let msi_api = msi.map(|_| LoadedHvfGicMsiSignalApi::load()).transpose()?;
    let metadata = prepare_gic_metadata_with_api(&api, msi)?;
    let msi_signaler = match (metadata.msi, msi_api) {
        (Some(metadata), Some(api)) => Some(
            HvfGicMsiSignaler::with_api(metadata, api)
                .map_err(|_| HvfGicError::InvalidMsiParameter { name: "metadata" })?,
        ),
        (None, None) => None,
        _ => {
            return Err(HvfGicError::InvalidState(
                "GIC MSI metadata and send capability disagree",
            ));
        }
    };
    configure_gic_with_api(&api, metadata)?;

    Ok(CreatedHvfGic {
        metadata,
        msi_signaler,
    })
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn create_real_gic(_: Option<HvfGicMsiConfiguration>) -> Result<CreatedHvfGic, HvfGicError> {
    Err(HvfGicError::Unsupported(
        crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
    ))
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod dynamic {
    use std::ffi::{CStr, c_void};
    use std::fmt;
    use std::mem;
    use std::ptr::NonNull;

    use bangbang_runtime::BackendError;

    use super::{
        DYNAMIC_SYMBOL_SIZE_MISMATCH_MESSAGE, HvfArm64GicIccRegister, HvfGicError,
        HvfGicInterruptRange,
    };

    type HvReturn = i32;
    type HvGicConfig = NonNull<c_void>;
    type HvGicConfigCreate = unsafe extern "C" fn() -> *mut c_void;
    type HvGicSetBase = unsafe extern "C" fn(*mut c_void, u64) -> HvReturn;
    type HvGicSetMsiRange = unsafe extern "C" fn(*mut c_void, u32, u32) -> HvReturn;
    type HvGicCreate = unsafe extern "C" fn(*mut c_void) -> HvReturn;
    type HvGicSetSpi = unsafe extern "C" fn(u32, bool) -> HvReturn;
    type HvGicSendMsi = unsafe extern "C" fn(u64, u32) -> HvReturn;
    type HvGicSetRedistributorReg = unsafe extern "C" fn(u64, u32, u64) -> HvReturn;
    type HvGicStateCreate = unsafe extern "C" fn() -> *mut c_void;
    type HvGicStateGetSize = unsafe extern "C" fn(*mut c_void, *mut usize) -> HvReturn;
    type HvGicStateGetData = unsafe extern "C" fn(*mut c_void, *mut c_void) -> HvReturn;
    type HvGicSetState = unsafe extern "C" fn(*const c_void, usize) -> HvReturn;
    type HvGicGetIccReg = unsafe extern "C" fn(u64, u16, *mut u64) -> HvReturn;
    type HvGicSetIccReg = unsafe extern "C" fn(u64, u16, u64) -> HvReturn;
    type HvGicGetSize = unsafe extern "C" fn(*mut usize) -> HvReturn;
    type HvGicGetSpiRange = unsafe extern "C" fn(*mut u32, *mut u32) -> HvReturn;
    type HvGicGetIntid = unsafe extern "C" fn(u16, *mut u32) -> HvReturn;
    type OsRelease = unsafe extern "C" fn(*mut c_void);

    const HYPERVISOR_FRAMEWORK_PATH: &CStr =
        c"/System/Library/Frameworks/Hypervisor.framework/Hypervisor";

    pub(super) struct LoadedHvfGicApi {
        _library: DynamicLibrary,
        symbols: HvfGicSymbols,
    }

    pub(super) struct LoadedHvfGicSpiSignalApi {
        _library: DynamicLibrary,
        symbols: HvfGicSpiSignalSymbols,
    }

    pub(super) struct LoadedHvfGicMsiSignalApi {
        _library: DynamicLibrary,
        symbols: HvfGicMsiSignalSymbols,
    }

    pub(super) struct LoadedHvfGicPpiPendingApi {
        _library: DynamicLibrary,
        symbols: HvfGicPpiPendingSymbols,
    }

    pub(super) struct LoadedHvfGicStateCaptureApi {
        _library: DynamicLibrary,
        symbols: HvfGicStateCaptureSymbols,
    }

    pub(super) struct LoadedHvfGicStateRestoreApi {
        _library: DynamicLibrary,
        symbols: HvfGicStateRestoreSymbols,
    }

    pub(super) struct LoadedHvfGicIccRegisterApi {
        _library: DynamicLibrary,
        symbols: HvfGicIccRegisterSymbols,
    }

    pub(super) struct LoadedHvfGicIccRegisterWriteApi {
        _library: DynamicLibrary,
        symbols: HvfGicIccRegisterWriteSymbols,
    }

    struct DynamicLibrary {
        handle: NonNull<c_void>,
    }

    // SAFETY: The handle is owned by `DynamicLibrary`, closed exactly once on
    // drop, and loaded function symbols cannot outlive the owner that keeps
    // the framework loaded.
    unsafe impl Send for DynamicLibrary {}

    #[derive(Clone, Copy)]
    struct HvfGicSymbols {
        config_create: HvGicConfigCreate,
        config_set_distributor_base: HvGicSetBase,
        config_set_redistributor_base: HvGicSetBase,
        create: HvGicCreate,
        get_distributor_size: HvGicGetSize,
        get_distributor_base_alignment: HvGicGetSize,
        get_redistributor_region_size: HvGicGetSize,
        get_redistributor_size: HvGicGetSize,
        get_redistributor_base_alignment: HvGicGetSize,
        get_spi_interrupt_range: HvGicGetSpiRange,
        get_intid: HvGicGetIntid,
        msi: Option<HvfGicMsiConfigSymbols>,
        os_release: OsRelease,
    }

    #[derive(Clone, Copy)]
    struct HvfGicMsiConfigSymbols {
        get_region_size: HvGicGetSize,
        get_region_base_alignment: HvGicGetSize,
        config_set_region_base: HvGicSetBase,
        config_set_interrupt_range: HvGicSetMsiRange,
    }

    #[derive(Clone, Copy)]
    struct HvfGicSpiSignalSymbols {
        set_spi: HvGicSetSpi,
    }

    #[derive(Clone, Copy)]
    struct HvfGicMsiSignalSymbols {
        send_msi: HvGicSendMsi,
    }

    #[derive(Clone, Copy)]
    struct HvfGicPpiPendingSymbols {
        set_redistributor_reg: HvGicSetRedistributorReg,
    }

    #[derive(Clone, Copy)]
    struct HvfGicStateCaptureSymbols {
        state_create: HvGicStateCreate,
        state_get_size: HvGicStateGetSize,
        state_get_data: HvGicStateGetData,
        os_release: OsRelease,
    }

    #[derive(Clone, Copy)]
    struct HvfGicStateRestoreSymbols {
        set_state: HvGicSetState,
    }

    #[derive(Clone, Copy)]
    struct HvfGicIccRegisterSymbols {
        get_icc_reg: HvGicGetIccReg,
    }

    #[derive(Clone, Copy)]
    struct HvfGicIccRegisterWriteSymbols {
        set_icc_reg: HvGicSetIccReg,
    }

    impl LoadedHvfGicApi {
        pub(super) fn load(include_msi: bool) -> Result<Self, HvfGicError> {
            let library = DynamicLibrary::open(HYPERVISOR_FRAMEWORK_PATH)?;
            let symbols = HvfGicSymbols::load(library.handle(), include_msi)?;

            Ok(Self {
                _library: library,
                symbols,
            })
        }

        fn get_size(
            &self,
            function: HvGicGetSize,
            operation: &'static str,
        ) -> Result<u64, HvfGicError> {
            let mut value = 0usize;
            // SAFETY: `value` is a valid out-pointer for the duration of the call.
            unsafe { crate::ffi::check(function(&mut value), operation)? };

            u64::try_from(value).map_err(|_| HvfGicError::InvalidParameter {
                name: operation,
                value: u64::MAX,
            })
        }
    }

    impl fmt::Debug for LoadedHvfGicApi {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("LoadedHvfGicApi").finish_non_exhaustive()
        }
    }

    impl LoadedHvfGicMsiSignalApi {
        pub(super) fn load() -> Result<Self, HvfGicError> {
            let library = DynamicLibrary::open(HYPERVISOR_FRAMEWORK_PATH)?;
            let symbols = HvfGicMsiSignalSymbols::load(library.handle())?;

            Ok(Self {
                _library: library,
                symbols,
            })
        }
    }

    impl fmt::Debug for LoadedHvfGicMsiSignalApi {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("LoadedHvfGicMsiSignalApi")
                .finish_non_exhaustive()
        }
    }

    impl LoadedHvfGicSpiSignalApi {
        pub(super) fn load() -> Result<Self, HvfGicError> {
            let library = DynamicLibrary::open(HYPERVISOR_FRAMEWORK_PATH)?;
            let symbols = HvfGicSpiSignalSymbols::load(library.handle())?;

            Ok(Self {
                _library: library,
                symbols,
            })
        }
    }

    impl fmt::Debug for LoadedHvfGicSpiSignalApi {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("LoadedHvfGicSpiSignalApi")
                .finish_non_exhaustive()
        }
    }

    impl LoadedHvfGicPpiPendingApi {
        pub(super) fn load() -> Result<Self, HvfGicError> {
            let library = DynamicLibrary::open(HYPERVISOR_FRAMEWORK_PATH)?;
            let symbols = HvfGicPpiPendingSymbols::load(library.handle())?;

            Ok(Self {
                _library: library,
                symbols,
            })
        }
    }

    impl LoadedHvfGicStateCaptureApi {
        pub(super) fn load() -> Result<Self, HvfGicError> {
            let library = DynamicLibrary::open(HYPERVISOR_FRAMEWORK_PATH)?;
            let symbols = HvfGicStateCaptureSymbols::load(library.handle())?;

            Ok(Self {
                _library: library,
                symbols,
            })
        }
    }

    impl LoadedHvfGicStateRestoreApi {
        pub(super) fn load() -> Result<Self, HvfGicError> {
            let library = DynamicLibrary::open(HYPERVISOR_FRAMEWORK_PATH)?;
            let symbols = HvfGicStateRestoreSymbols::load(library.handle())?;

            Ok(Self {
                _library: library,
                symbols,
            })
        }
    }

    impl LoadedHvfGicIccRegisterApi {
        pub(super) fn load() -> Result<Self, HvfGicError> {
            let library = DynamicLibrary::open(HYPERVISOR_FRAMEWORK_PATH)?;
            let symbols = HvfGicIccRegisterSymbols::load(library.handle())?;

            Ok(Self {
                _library: library,
                symbols,
            })
        }
    }

    impl LoadedHvfGicIccRegisterWriteApi {
        pub(super) fn load() -> Result<Self, HvfGicError> {
            let library = DynamicLibrary::open(HYPERVISOR_FRAMEWORK_PATH)?;
            let symbols = HvfGicIccRegisterWriteSymbols::load(library.handle())?;

            Ok(Self {
                _library: library,
                symbols,
            })
        }
    }

    impl fmt::Debug for LoadedHvfGicPpiPendingApi {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("LoadedHvfGicPpiPendingApi")
                .finish_non_exhaustive()
        }
    }

    impl fmt::Debug for LoadedHvfGicStateCaptureApi {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("LoadedHvfGicStateCaptureApi")
                .finish_non_exhaustive()
        }
    }

    impl fmt::Debug for LoadedHvfGicStateRestoreApi {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("LoadedHvfGicStateRestoreApi")
                .finish_non_exhaustive()
        }
    }

    impl fmt::Debug for LoadedHvfGicIccRegisterApi {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("LoadedHvfGicIccRegisterApi")
                .finish_non_exhaustive()
        }
    }

    impl fmt::Debug for LoadedHvfGicIccRegisterWriteApi {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("LoadedHvfGicIccRegisterWriteApi")
                .finish_non_exhaustive()
        }
    }

    impl DynamicLibrary {
        fn open(path: &CStr) -> Result<Self, HvfGicError> {
            // SAFETY: `path` is a NUL-terminated static framework path.
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
            let handle = NonNull::new(handle).ok_or(HvfGicError::Unsupported(
                super::GIC_REQUIRES_MACOS_15_MESSAGE,
            ))?;

            Ok(Self { handle })
        }

        fn handle(&self) -> NonNull<c_void> {
            self.handle
        }
    }

    impl Drop for DynamicLibrary {
        fn drop(&mut self) {
            // SAFETY: `handle` was returned by `dlopen` and is closed exactly once here.
            unsafe {
                let _ = libc::dlclose(self.handle.as_ptr());
            }
        }
    }

    impl fmt::Debug for DynamicLibrary {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("DynamicLibrary")
                .field("handle", &self.handle)
                .finish()
        }
    }

    impl HvfGicSymbols {
        fn load(library: NonNull<c_void>, include_msi: bool) -> Result<Self, HvfGicError> {
            let msi = include_msi
                .then(|| HvfGicMsiConfigSymbols::load(library))
                .transpose()?;
            Ok(Self {
                config_create: load_symbol(
                    library,
                    c"hv_gic_config_create",
                    "hv_gic_config_create",
                )?,
                config_set_distributor_base: load_symbol(
                    library,
                    c"hv_gic_config_set_distributor_base",
                    "hv_gic_config_set_distributor_base",
                )?,
                config_set_redistributor_base: load_symbol(
                    library,
                    c"hv_gic_config_set_redistributor_base",
                    "hv_gic_config_set_redistributor_base",
                )?,
                create: load_symbol(library, c"hv_gic_create", "hv_gic_create")?,
                get_distributor_size: load_symbol(
                    library,
                    c"hv_gic_get_distributor_size",
                    "hv_gic_get_distributor_size",
                )?,
                get_distributor_base_alignment: load_symbol(
                    library,
                    c"hv_gic_get_distributor_base_alignment",
                    "hv_gic_get_distributor_base_alignment",
                )?,
                get_redistributor_region_size: load_symbol(
                    library,
                    c"hv_gic_get_redistributor_region_size",
                    "hv_gic_get_redistributor_region_size",
                )?,
                get_redistributor_size: load_symbol(
                    library,
                    c"hv_gic_get_redistributor_size",
                    "hv_gic_get_redistributor_size",
                )?,
                get_redistributor_base_alignment: load_symbol(
                    library,
                    c"hv_gic_get_redistributor_base_alignment",
                    "hv_gic_get_redistributor_base_alignment",
                )?,
                get_spi_interrupt_range: load_symbol(
                    library,
                    c"hv_gic_get_spi_interrupt_range",
                    "hv_gic_get_spi_interrupt_range",
                )?,
                get_intid: load_symbol(library, c"hv_gic_get_intid", "hv_gic_get_intid")?,
                msi,
                os_release: load_symbol(
                    NonNull::new(libc::RTLD_DEFAULT)
                        .ok_or(HvfGicError::MissingSymbol("os_release"))?,
                    c"os_release",
                    "os_release",
                )?,
            })
        }
    }

    impl HvfGicMsiConfigSymbols {
        fn load(library: NonNull<c_void>) -> Result<Self, HvfGicError> {
            Ok(Self {
                get_region_size: load_symbol(
                    library,
                    c"hv_gic_get_msi_region_size",
                    "hv_gic_get_msi_region_size",
                )?,
                get_region_base_alignment: load_symbol(
                    library,
                    c"hv_gic_get_msi_region_base_alignment",
                    "hv_gic_get_msi_region_base_alignment",
                )?,
                config_set_region_base: load_symbol(
                    library,
                    c"hv_gic_config_set_msi_region_base",
                    "hv_gic_config_set_msi_region_base",
                )?,
                config_set_interrupt_range: load_symbol(
                    library,
                    c"hv_gic_config_set_msi_interrupt_range",
                    "hv_gic_config_set_msi_interrupt_range",
                )?,
            })
        }
    }

    impl HvfGicSpiSignalSymbols {
        fn load(library: NonNull<c_void>) -> Result<Self, HvfGicError> {
            Ok(Self {
                set_spi: load_symbol(library, c"hv_gic_set_spi", "hv_gic_set_spi")?,
            })
        }
    }

    impl HvfGicMsiSignalSymbols {
        fn load(library: NonNull<c_void>) -> Result<Self, HvfGicError> {
            Ok(Self {
                send_msi: load_symbol(library, c"hv_gic_send_msi", "hv_gic_send_msi")?,
            })
        }
    }

    impl HvfGicPpiPendingSymbols {
        fn load(library: NonNull<c_void>) -> Result<Self, HvfGicError> {
            Ok(Self {
                set_redistributor_reg: load_symbol(
                    library,
                    c"hv_gic_set_redistributor_reg",
                    "hv_gic_set_redistributor_reg",
                )?,
            })
        }
    }

    impl HvfGicStateCaptureSymbols {
        fn load(library: NonNull<c_void>) -> Result<Self, HvfGicError> {
            Ok(Self {
                state_create: load_symbol(library, c"hv_gic_state_create", "hv_gic_state_create")?,
                state_get_size: load_symbol(
                    library,
                    c"hv_gic_state_get_size",
                    "hv_gic_state_get_size",
                )?,
                state_get_data: load_symbol(
                    library,
                    c"hv_gic_state_get_data",
                    "hv_gic_state_get_data",
                )?,
                os_release: load_symbol(
                    NonNull::new(libc::RTLD_DEFAULT)
                        .ok_or(HvfGicError::MissingSymbol("os_release"))?,
                    c"os_release",
                    "os_release",
                )?,
            })
        }
    }

    impl HvfGicStateRestoreSymbols {
        fn load(library: NonNull<c_void>) -> Result<Self, HvfGicError> {
            Ok(Self {
                set_state: load_symbol(library, c"hv_gic_set_state", "hv_gic_set_state")?,
            })
        }
    }

    impl HvfGicIccRegisterSymbols {
        fn load(library: NonNull<c_void>) -> Result<Self, HvfGicError> {
            Ok(Self {
                get_icc_reg: load_symbol(library, c"hv_gic_get_icc_reg", "hv_gic_get_icc_reg")?,
            })
        }
    }

    impl HvfGicIccRegisterWriteSymbols {
        fn load(library: NonNull<c_void>) -> Result<Self, HvfGicError> {
            Ok(Self {
                set_icc_reg: load_symbol(library, c"hv_gic_set_icc_reg", "hv_gic_set_icc_reg")?,
            })
        }
    }

    fn load_symbol<T: Copy>(
        handle: NonNull<c_void>,
        name: &CStr,
        symbol_name: &'static str,
    ) -> Result<T, HvfGicError> {
        if mem::size_of::<T>() != mem::size_of::<*mut c_void>() {
            return Err(HvfGicError::InvalidState(
                DYNAMIC_SYMBOL_SIZE_MISMATCH_MESSAGE,
            ));
        }

        // SAFETY: `handle` comes from `dlopen` or `RTLD_DEFAULT`, and `name`
        // is a NUL-terminated static symbol name.
        let symbol = unsafe { libc::dlsym(handle.as_ptr(), name.as_ptr()) };
        if symbol.is_null() {
            return Err(HvfGicError::MissingSymbol(symbol_name));
        }

        // SAFETY: The caller picks `T` to match the requested symbol's C
        // function type. Function pointers and dynamic symbol pointers have
        // the same representation on this target, checked above.
        Ok(unsafe { mem::transmute_copy::<*mut c_void, T>(&symbol) })
    }

    impl super::HvfGicApi for LoadedHvfGicApi {
        type Config = HvGicConfig;

        fn distributor_size(&self) -> Result<u64, HvfGicError> {
            self.get_size(
                self.symbols.get_distributor_size,
                "hv_gic_get_distributor_size",
            )
        }

        fn distributor_alignment(&self) -> Result<u64, HvfGicError> {
            self.get_size(
                self.symbols.get_distributor_base_alignment,
                "hv_gic_get_distributor_base_alignment",
            )
        }

        fn redistributor_region_size(&self) -> Result<u64, HvfGicError> {
            self.get_size(
                self.symbols.get_redistributor_region_size,
                "hv_gic_get_redistributor_region_size",
            )
        }

        fn redistributor_size(&self) -> Result<u64, HvfGicError> {
            self.get_size(
                self.symbols.get_redistributor_size,
                "hv_gic_get_redistributor_size",
            )
        }

        fn redistributor_alignment(&self) -> Result<u64, HvfGicError> {
            self.get_size(
                self.symbols.get_redistributor_base_alignment,
                "hv_gic_get_redistributor_base_alignment",
            )
        }

        fn msi_region_size(&self) -> Result<u64, HvfGicError> {
            let symbols = self.symbols.msi.as_ref().ok_or(HvfGicError::InvalidState(
                "GIC MSI configuration symbols were not loaded",
            ))?;
            self.get_size(symbols.get_region_size, "hv_gic_get_msi_region_size")
        }

        fn msi_region_alignment(&self) -> Result<u64, HvfGicError> {
            let symbols = self.symbols.msi.as_ref().ok_or(HvfGicError::InvalidState(
                "GIC MSI configuration symbols were not loaded",
            ))?;
            self.get_size(
                symbols.get_region_base_alignment,
                "hv_gic_get_msi_region_base_alignment",
            )
        }

        fn spi_interrupt_range(&self) -> Result<HvfGicInterruptRange, HvfGicError> {
            let mut base = 0;
            let mut count = 0;
            // SAFETY: `base` and `count` are valid out-pointers for the duration of the call.
            unsafe {
                crate::ffi::check(
                    (self.symbols.get_spi_interrupt_range)(&mut base, &mut count),
                    "hv_gic_get_spi_interrupt_range",
                )?
            };

            Ok(HvfGicInterruptRange { base, count })
        }

        fn intid(&self, interrupt: u16) -> Result<u32, HvfGicError> {
            let mut intid = 0;
            // SAFETY: `intid` is a valid out-pointer for the duration of the call.
            unsafe {
                crate::ffi::check(
                    (self.symbols.get_intid)(interrupt, &mut intid),
                    "hv_gic_get_intid",
                )?
            };

            Ok(intid)
        }

        fn create_config(&self) -> Result<Self::Config, HvfGicError> {
            // SAFETY: Creates a new retained GIC config object per Hypervisor.framework.
            let config = unsafe { (self.symbols.config_create)() };
            NonNull::new(config).ok_or(HvfGicError::ConfigCreateFailed)
        }

        fn set_distributor_base(
            &self,
            config: &mut Self::Config,
            base: u64,
        ) -> Result<(), HvfGicError> {
            // SAFETY: `config` is a live GIC config object owned by the guard.
            unsafe {
                crate::ffi::check(
                    (self.symbols.config_set_distributor_base)(config.as_ptr(), base),
                    "hv_gic_config_set_distributor_base",
                )?
            };
            Ok(())
        }

        fn set_redistributor_base(
            &self,
            config: &mut Self::Config,
            base: u64,
        ) -> Result<(), HvfGicError> {
            // SAFETY: `config` is a live GIC config object owned by the guard.
            unsafe {
                crate::ffi::check(
                    (self.symbols.config_set_redistributor_base)(config.as_ptr(), base),
                    "hv_gic_config_set_redistributor_base",
                )?
            };
            Ok(())
        }

        fn set_msi_region_base(
            &self,
            config: &mut Self::Config,
            base: u64,
        ) -> Result<(), HvfGicError> {
            let symbols = self.symbols.msi.as_ref().ok_or(HvfGicError::InvalidState(
                "GIC MSI configuration symbols were not loaded",
            ))?;
            // SAFETY: `config` is live and `base` was validated against the
            // host-reported MSI frame geometry before this call.
            unsafe {
                crate::ffi::check(
                    (symbols.config_set_region_base)(config.as_ptr(), base),
                    "hv_gic_config_set_msi_region_base",
                )?
            };
            Ok(())
        }

        fn set_msi_interrupt_range(
            &self,
            config: &mut Self::Config,
            range: HvfGicInterruptRange,
        ) -> Result<(), HvfGicError> {
            let symbols = self.symbols.msi.as_ref().ok_or(HvfGicError::InvalidState(
                "GIC MSI configuration symbols were not loaded",
            ))?;
            // SAFETY: `config` is live and the range is a validated suffix of
            // the host-reported SPI domain.
            unsafe {
                crate::ffi::check(
                    (symbols.config_set_interrupt_range)(config.as_ptr(), range.base, range.count),
                    "hv_gic_config_set_msi_interrupt_range",
                )?
            };
            Ok(())
        }

        fn create_gic(&self, config: &Self::Config) -> Result<(), HvfGicError> {
            // SAFETY: The VM is live, and `config` has valid distributor and
            // redistributor bases configured before this call.
            unsafe { crate::ffi::check((self.symbols.create)(config.as_ptr()), "hv_gic_create")? };
            Ok(())
        }

        fn release_config(&self, config: Self::Config) {
            // SAFETY: `config` is a retained OS object created by
            // `hv_gic_config_create` and is released exactly once by the guard.
            unsafe {
                (self.symbols.os_release)(config.as_ptr());
            }
        }
    }

    impl super::HvfGicSpiSignalApi for LoadedHvfGicSpiSignalApi {
        fn set_spi(&self, intid: u32, level: bool) -> Result<(), HvfGicError> {
            // SAFETY: `intid` and `level` are plain values, and range validation
            // is performed before public callers reach this wrapper.
            unsafe { crate::ffi::check((self.symbols.set_spi)(intid, level), "hv_gic_set_spi")? };
            Ok(())
        }
    }

    impl super::HvfGicMsiSignalApi for LoadedHvfGicMsiSignalApi {
        fn send_msi(&self, address: u64, intid: u32) -> Result<(), HvfGicError> {
            // SAFETY: the signaler derives `address` from validated metadata
            // and accepts only an allocated INTID inside the configured range.
            unsafe {
                crate::ffi::check((self.symbols.send_msi)(address, intid), "hv_gic_send_msi")?
            };
            Ok(())
        }
    }

    impl super::HvfGicPpiPendingApi for LoadedHvfGicPpiPendingApi {
        fn set_redistributor_reg(
            &self,
            vcpu: crate::ffi::HvVcpu,
            reg: u32,
            value: u64,
        ) -> Result<(), HvfGicError> {
            // SAFETY: `vcpu` is a live current-thread vCPU id, and callers
            // validate that `reg` and `value` target only PPI pending writes.
            unsafe {
                crate::ffi::check(
                    (self.symbols.set_redistributor_reg)(vcpu, reg, value),
                    "hv_gic_set_redistributor_reg",
                )?
            };
            Ok(())
        }
    }

    impl super::HvfGicStateApi for LoadedHvfGicStateCaptureApi {
        type State = NonNull<c_void>;

        fn create_state(&self) -> Result<Self::State, HvfGicError> {
            // SAFETY: Creates a new retained GIC state object for the current
            // stopped VM according to Hypervisor.framework's ownership rules.
            let state = unsafe { (self.symbols.state_create)() };
            NonNull::new(state).ok_or(HvfGicError::StateCreateFailed)
        }

        fn state_size(&self, state: &Self::State) -> Result<usize, HvfGicError> {
            let mut size = 0;
            // SAFETY: `state` is a live retained object owned by the guard and
            // `size` is a valid out-pointer for the duration of the call.
            unsafe {
                crate::ffi::check(
                    (self.symbols.state_get_size)(state.as_ptr(), &mut size),
                    "hv_gic_state_get_size",
                )?
            };
            Ok(size)
        }

        fn copy_state(&self, state: &Self::State, data: &mut [u8]) -> Result<(), HvfGicError> {
            // SAFETY: `state` is live and `data` is the initialized mutable
            // buffer sized from this same state object's successful size query.
            unsafe {
                crate::ffi::check(
                    (self.symbols.state_get_data)(state.as_ptr(), data.as_mut_ptr().cast()),
                    "hv_gic_state_get_data",
                )?
            };
            Ok(())
        }

        fn release_state(&self, state: Self::State) {
            // SAFETY: `state` is a retained OS object returned by
            // `hv_gic_state_create` and is released exactly once by the guard.
            unsafe {
                (self.symbols.os_release)(state.as_ptr());
            }
        }
    }

    impl super::HvfGicStateCapture for LoadedHvfGicStateCaptureApi {
        fn capture(
            &self,
            maximum_size: Option<usize>,
        ) -> Result<super::HvfGicDeviceState, HvfGicError> {
            super::capture_gic_device_state_with_api_bounded(self, maximum_size)
        }
    }

    impl super::HvfGicStateRestoreApi for LoadedHvfGicStateRestoreApi {
        fn restore(&self, data: &[u8]) -> Result<(), HvfGicError> {
            // SAFETY: `data` is a non-empty immutable buffer that remains live
            // for this synchronous call, its Rust `usize` length is the SDK's
            // `size_t`, and the loaded symbol has the declared SDK signature.
            unsafe {
                crate::ffi::check(
                    (self.symbols.set_state)(data.as_ptr().cast(), data.len()),
                    "hv_gic_set_state",
                )?
            };
            Ok(())
        }
    }

    impl super::HvfGicIccRegisterApi for LoadedHvfGicIccRegisterApi {
        fn get_icc_reg(
            &self,
            vcpu: crate::ffi::HvVcpu,
            register: HvfArm64GicIccRegister,
        ) -> Result<u64, BackendError> {
            let mut value = 0;
            // SAFETY: `vcpu` is a live current-thread vCPU id,
            // `register.raw()` is one of the fixed SDK `hv_gic_icc_reg_t`
            // values, and `value` is a valid out-pointer for this loaded call.
            unsafe {
                crate::ffi::check(
                    (self.symbols.get_icc_reg)(vcpu, register.raw(), &mut value),
                    "hv_gic_get_icc_reg",
                )?
            };
            Ok(value)
        }
    }

    impl super::HvfGicIccRegisterWriteApi for LoadedHvfGicIccRegisterWriteApi {
        fn set_icc_reg(
            &self,
            vcpu: crate::ffi::HvVcpu,
            register: HvfArm64GicIccRegister,
            value: u64,
        ) -> Result<(), BackendError> {
            // SAFETY: `vcpu` is a live current-thread vCPU id, `register.raw()`
            // is one of the fixed SDK `hv_gic_icc_reg_t` values, and `value`
            // is passed by value through the loaded SDK signature.
            unsafe {
                crate::ffi::check(
                    (self.symbols.set_icc_reg)(vcpu, register.raw(), value),
                    "hv_gic_set_icc_reg",
                )
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::ffi::c_void;
        use std::ptr::NonNull;

        use super::load_symbol;
        use crate::gic::{DYNAMIC_SYMBOL_SIZE_MISMATCH_MESSAGE, HvfGicError};

        fn default_symbols() -> NonNull<c_void> {
            NonNull::new(libc::RTLD_DEFAULT).expect("RTLD_DEFAULT should be non-null")
        }

        #[test]
        fn load_symbol_reports_missing_symbol() {
            type MissingSymbol = unsafe extern "C" fn();

            let err = load_symbol::<MissingSymbol>(
                default_symbols(),
                c"bangbang_hvf_missing_test_symbol",
                "bangbang_hvf_missing_test_symbol",
            )
            .expect_err("missing dynamic symbol should fail");

            assert_eq!(
                err,
                HvfGicError::MissingSymbol("bangbang_hvf_missing_test_symbol")
            );
        }

        #[test]
        fn load_symbol_rejects_size_mismatch() {
            let err = load_symbol::<u8>(
                default_symbols(),
                c"bangbang_hvf_missing_test_symbol",
                "bangbang_hvf_missing_test_symbol",
            )
            .expect_err("non-pointer-sized symbol type should fail before dlsym");

            assert_eq!(
                err,
                HvfGicError::InvalidState(DYNAMIC_SYMBOL_SIZE_MISMATCH_MESSAGE)
            );
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use dynamic::{
    LoadedHvfGicApi, LoadedHvfGicIccRegisterApi, LoadedHvfGicIccRegisterWriteApi,
    LoadedHvfGicMsiSignalApi, LoadedHvfGicPpiPendingApi, LoadedHvfGicSpiSignalApi,
    LoadedHvfGicStateCaptureApi, LoadedHvfGicStateRestoreApi,
};

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::error::Error as _;
    use std::fmt;
    use std::num::NonZeroU32;
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::{Arc, Barrier, Mutex, mpsc};
    use std::time::Duration;

    use bangbang_runtime::BackendError;
    use bangbang_runtime::fdt::{
        ARM64_FDT_HYPERVISOR_TIMER_PPI, ARM64_FDT_NON_SECURE_PHYSICAL_TIMER_PPI,
        ARM64_FDT_SECURE_PHYSICAL_TIMER_PPI, ARM64_FDT_VIRTUAL_TIMER_PPI,
        ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET, ARM64_GICV2M_SPI_END_EXCLUSIVE, Arm64FdtError,
        Arm64FdtInterruptRange, Arm64FdtMsi, Arm64FdtRegion, Arm64FdtTimerInterrupts,
    };
    use bangbang_runtime::interrupt::{GuestInterruptLine, GuestInterruptLineError, InterruptSink};

    use super::{
        ARM64_GIC_EL1_ICC_REGISTERS, GIC_ICC_RPR_MISMATCH_MESSAGE,
        GIC_SPI_SIGNALER_LOCK_POISONED_MESSAGE, GicConfigGuard, HV_GIC_ICC_REG_AP0R0_EL1,
        HV_GIC_ICC_REG_AP1R0_EL1, HV_GIC_ICC_REG_BPR0_EL1, HV_GIC_ICC_REG_BPR1_EL1,
        HV_GIC_ICC_REG_CTLR_EL1, HV_GIC_ICC_REG_IGRPEN0_EL1, HV_GIC_ICC_REG_IGRPEN1_EL1,
        HV_GIC_ICC_REG_PMR_EL1, HV_GIC_ICC_REG_RPR_EL1, HV_GIC_ICC_REG_SRE_EL1,
        HV_GIC_INT_EL1_PHYSICAL_TIMER, HV_GIC_INT_EL1_VIRTUAL_TIMER,
        HV_GIC_REDISTRIBUTOR_REG_GICR_ICPENDR0, HV_GIC_REDISTRIBUTOR_REG_GICR_ISPENDR0,
        HvfArm64GicIccRegister, HvfArm64GicIccRegisterRestoreOperation,
        HvfArm64GicIccRegisterState, HvfGicApi, HvfGicDeviceState, HvfGicError,
        HvfGicIccRegisterApi, HvfGicIccRegisterReader, HvfGicIccRegisterRestorer,
        HvfGicIccRegisterWriteApi, HvfGicInterruptLineAllocator, HvfGicInterruptRange,
        HvfGicMetadata, HvfGicMsiConfiguration, HvfGicMsiInterruptAllocationError,
        HvfGicMsiInterruptAllocator, HvfGicMsiMetadata, HvfGicMsiParameters, HvfGicMsiSignalError,
        HvfGicMsiSignaler, HvfGicParameters, HvfGicPpiPendingWriter, HvfGicRegion,
        HvfGicSpiSignalError, HvfGicSpiSignaler, HvfGicStateApi, HvfGicStateRestoreApi,
        HvfGicStateRestorer, HvfGicTimerInterrupts, HvfInterruptLineAllocationError,
        capture_gic_device_state_with_api, capture_gic_device_state_with_api_bounded,
        create_gic_with_api, metadata_from_parameters, restore_arm64_gic_icc_register_state_with,
    };

    const DIST_SIZE: u64 = 0x1_0000;
    const REDIST_REGION_SIZE: u64 = 0x2_0000;
    const REDIST_SIZE: u64 = 0x2_0000;
    const ALIGNMENT: u64 = 0x1_0000;

    #[test]
    fn captures_all_arm64_gic_icc_registers_in_sdk_order() {
        let api = FakeGicIccRegisterApi::default();
        let reader = HvfGicIccRegisterReader::with_api(api.clone());

        let state = reader
            .capture(7)
            .expect("arm64 GIC ICC register state should be captured");

        assert_eq!(state.pmr_el1(), fake_icc_value(HV_GIC_ICC_REG_PMR_EL1));
        assert_eq!(state.bpr0_el1(), fake_icc_value(HV_GIC_ICC_REG_BPR0_EL1));
        assert_eq!(state.ap0r0_el1(), fake_icc_value(HV_GIC_ICC_REG_AP0R0_EL1));
        assert_eq!(state.ap1r0_el1(), fake_icc_value(HV_GIC_ICC_REG_AP1R0_EL1));
        assert_eq!(state.rpr_el1(), fake_icc_value(HV_GIC_ICC_REG_RPR_EL1));
        assert_eq!(state.bpr1_el1(), fake_icc_value(HV_GIC_ICC_REG_BPR1_EL1));
        assert_eq!(state.ctlr_el1(), fake_icc_value(HV_GIC_ICC_REG_CTLR_EL1));
        assert_eq!(state.sre_el1(), fake_icc_value(HV_GIC_ICC_REG_SRE_EL1));
        assert_eq!(
            state.igrpen0_el1(),
            fake_icc_value(HV_GIC_ICC_REG_IGRPEN0_EL1)
        );
        assert_eq!(
            state.igrpen1_el1(),
            fake_icc_value(HV_GIC_ICC_REG_IGRPEN1_EL1)
        );
        assert_eq!(
            api.calls(),
            ARM64_GIC_EL1_ICC_REGISTERS
                .map(|register| (7, register))
                .to_vec()
        );
    }

    #[test]
    fn every_arm64_gic_icc_read_failure_is_atomic_and_retryable() {
        for (failed_index, failed_register) in ARM64_GIC_EL1_ICC_REGISTERS.into_iter().enumerate() {
            let failed_register_id = failed_register.raw();
            let api = FakeGicIccRegisterApi::default().with_failure(failed_register);
            let reader = HvfGicIccRegisterReader::with_api(api.clone());

            assert_eq!(
                reader.capture(11),
                Err(HvfGicError::Backend(BackendError::Hypervisor(format!(
                    "injected ICC register 0x{failed_register_id:x} failure"
                ))))
            );
            assert_eq!(api.calls().len(), failed_index + 1);
            assert_eq!(api.calls().last(), Some(&(11, failed_register)));

            let state = reader
                .capture(11)
                .expect("ICC capture should restart cleanly after a failed read");
            assert_eq!(state.pmr_el1(), fake_icc_value(HV_GIC_ICC_REG_PMR_EL1));
            assert_eq!(
                &api.calls()[failed_index + 1..],
                &ARM64_GIC_EL1_ICC_REGISTERS.map(|register| (11, register))
            );
        }
    }

    #[test]
    fn arm64_gic_icc_capture_preserves_backend_error_source() {
        let failed_register = ARM64_GIC_EL1_ICC_REGISTERS[0];
        let reader = HvfGicIccRegisterReader::with_api(
            FakeGicIccRegisterApi::default().with_failure(failed_register),
        );

        let err = reader
            .capture(7)
            .expect_err("injected ICC read failure should propagate");

        assert_eq!(
            err.source().map(ToString::to_string),
            Some(format!(
                "hypervisor error: injected ICC register 0x{:x} failure",
                failed_register.raw()
            ))
        );
    }

    #[test]
    fn arm64_gic_icc_register_ids_match_sdk_inventory() {
        assert_eq!(
            ARM64_GIC_EL1_ICC_REGISTERS.map(HvfArm64GicIccRegister::raw),
            [
                HV_GIC_ICC_REG_PMR_EL1,
                HV_GIC_ICC_REG_BPR0_EL1,
                HV_GIC_ICC_REG_AP0R0_EL1,
                HV_GIC_ICC_REG_AP1R0_EL1,
                HV_GIC_ICC_REG_RPR_EL1,
                HV_GIC_ICC_REG_BPR1_EL1,
                HV_GIC_ICC_REG_CTLR_EL1,
                HV_GIC_ICC_REG_SRE_EL1,
                HV_GIC_ICC_REG_IGRPEN0_EL1,
                HV_GIC_ICC_REG_IGRPEN1_EL1,
            ]
        );
    }

    #[test]
    fn restores_mutable_arm64_gic_icc_registers_and_validates_rpr_in_capture_order() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum RestoreAccess {
            Write(HvfArm64GicIccRegister, u64),
            Read(HvfArm64GicIccRegister),
        }

        let state = fake_icc_restore_state();
        let accesses = RefCell::new(Vec::new());

        restore_arm64_gic_icc_register_state_with(
            &state,
            |register, value| {
                accesses
                    .borrow_mut()
                    .push(RestoreAccess::Write(register, value));
                Ok(())
            },
            |register| {
                accesses.borrow_mut().push(RestoreAccess::Read(register));
                Ok(state.rpr_el1())
            },
        )
        .expect("arm64 GIC ICC register state should be restored");

        assert_eq!(
            accesses.into_inner(),
            vec![
                RestoreAccess::Write(HvfArm64GicIccRegister::PmrEl1, state.pmr_el1()),
                RestoreAccess::Write(HvfArm64GicIccRegister::Bpr0El1, state.bpr0_el1()),
                RestoreAccess::Write(HvfArm64GicIccRegister::Ap0r0El1, state.ap0r0_el1()),
                RestoreAccess::Write(HvfArm64GicIccRegister::Ap1r0El1, state.ap1r0_el1()),
                RestoreAccess::Read(HvfArm64GicIccRegister::RprEl1),
                RestoreAccess::Write(HvfArm64GicIccRegister::Bpr1El1, state.bpr1_el1()),
                RestoreAccess::Write(HvfArm64GicIccRegister::CtlrEl1, state.ctlr_el1()),
                RestoreAccess::Write(HvfArm64GicIccRegister::SreEl1, state.sre_el1()),
                RestoreAccess::Write(HvfArm64GicIccRegister::Igrpen0El1, state.igrpen0_el1(),),
                RestoreAccess::Write(HvfArm64GicIccRegister::Igrpen1El1, state.igrpen1_el1(),),
            ]
        );
    }

    #[test]
    fn every_mutable_arm64_gic_icc_restore_failure_is_typed_and_retryable() {
        let state = fake_icc_restore_state();
        let mutable_registers: Vec<_> = ARM64_GIC_EL1_ICC_REGISTERS
            .into_iter()
            .filter(|register| *register != HvfArm64GicIccRegister::RprEl1)
            .collect();

        for (failed_index, failed_register) in mutable_registers.into_iter().enumerate() {
            let api = FakeGicIccRegisterWriteApi::default().with_failure(failed_register);
            let reader_api = FakeGicIccRegisterApi::default();
            let reader = HvfGicIccRegisterReader::with_api(reader_api.clone());
            let restorer = HvfGicIccRegisterRestorer::with_api(api.clone());

            let err = restorer
                .restore(&reader, 11, &state)
                .expect_err("injected ICC write failure should propagate");
            assert_eq!(err.failed_register(), failed_register);
            assert_eq!(
                err.operation(),
                HvfArm64GicIccRegisterRestoreOperation::Write
            );
            assert_eq!(err.completed_writes(), failed_index);
            assert_eq!(
                err.source().map(ToString::to_string),
                Some("invalid backend state: injected ICC setter failure".to_string())
            );
            assert_eq!(api.calls().len(), failed_index + 1);
            assert_eq!(
                api.calls().last(),
                expected_icc_restore_writes(11).get(failed_index)
            );

            restorer
                .restore(&reader, 11, &state)
                .expect("ICC restore should restart from PMR after a failed write");
            assert_eq!(
                &api.calls()[failed_index + 1..],
                expected_icc_restore_writes(11).as_slice()
            );
        }
    }

    #[test]
    fn arm64_gic_icc_rpr_read_failure_is_typed_and_retryable() {
        let state = fake_icc_restore_state();
        let api = FakeGicIccRegisterWriteApi::default();
        let reader_api =
            FakeGicIccRegisterApi::default().with_failure(HvfArm64GicIccRegister::RprEl1);
        let reader = HvfGicIccRegisterReader::with_api(reader_api.clone());
        let restorer = HvfGicIccRegisterRestorer::with_api(api.clone());

        let err = restorer
            .restore(&reader, 11, &state)
            .expect_err("injected ICC_RPR_EL1 read failure should propagate");

        assert_eq!(err.failed_register(), HvfArm64GicIccRegister::RprEl1);
        assert_eq!(
            err.operation(),
            HvfArm64GicIccRegisterRestoreOperation::ValidateDerived
        );
        assert_eq!(err.completed_writes(), 4);
        assert_eq!(
            err.source().map(ToString::to_string),
            Some(format!(
                "hypervisor error: injected ICC register 0x{:x} failure",
                HV_GIC_ICC_REG_RPR_EL1
            ))
        );
        assert_eq!(api.calls(), expected_icc_restore_writes(11)[..4]);
        assert_eq!(reader_api.calls(), [(11, HvfArm64GicIccRegister::RprEl1)]);

        restorer
            .restore(&reader, 11, &state)
            .expect("ICC restore should restart from PMR after a failed RPR read");
        assert_eq!(&api.calls()[4..], expected_icc_restore_writes(11));
        assert_eq!(
            reader_api.calls(),
            [
                (11, HvfArm64GicIccRegister::RprEl1),
                (11, HvfArm64GicIccRegister::RprEl1),
            ]
        );
    }

    #[test]
    fn arm64_gic_icc_rpr_mismatch_stops_before_later_writes() {
        let state = fake_icc_restore_state();
        let api = FakeGicIccRegisterWriteApi::default();
        let reader_api = FakeGicIccRegisterApi::default().with_rpr_value(state.rpr_el1() ^ 1);
        let reader = HvfGicIccRegisterReader::with_api(reader_api.clone());
        let restorer = HvfGicIccRegisterRestorer::with_api(api.clone());

        let err = restorer
            .restore(&reader, 7, &state)
            .expect_err("mismatched derived ICC_RPR_EL1 should fail restore");

        assert_eq!(err.failed_register(), HvfArm64GicIccRegister::RprEl1);
        assert_eq!(
            err.operation(),
            HvfArm64GicIccRegisterRestoreOperation::ValidateDerived
        );
        assert_eq!(err.completed_writes(), 4);
        assert_eq!(
            err.source().map(ToString::to_string),
            Some(format!(
                "invalid backend state: {GIC_ICC_RPR_MISMATCH_MESSAGE}"
            ))
        );
        assert_eq!(api.calls(), expected_icc_restore_writes(7)[..4]);
        assert_eq!(reader_api.calls(), [(7, HvfArm64GicIccRegister::RprEl1)]);
    }

    #[test]
    fn arm64_gic_icc_restore_error_does_not_expose_attempted_values() {
        let state = HvfArm64GicIccRegisterState::new([
            0xdead_beef_1000_0001,
            0xdead_beef_1000_0002,
            0xdead_beef_1000_0003,
            0xdead_beef_1000_0004,
            0xdead_beef_1000_0005,
            0xdead_beef_1000_0006,
            0xdead_beef_1000_0007,
            0xdead_beef_1000_0008,
            0xdead_beef_1000_0009,
            0xdead_beef_1000_000a,
        ]);
        let restorer = HvfGicIccRegisterRestorer::with_api(
            FakeGicIccRegisterWriteApi::default().with_failure(HvfArm64GicIccRegister::PmrEl1),
        );
        let reader = HvfGicIccRegisterReader::with_api(FakeGicIccRegisterApi::default());

        let err = restorer
            .restore(&reader, 7, &state)
            .expect_err("injected ICC write failure should propagate");
        let formatted = format!("{err}\n{err:?}");

        assert!(formatted.contains("ICC_PMR_EL1"));
        assert!(formatted.contains("0 successful writes"));
        for value in state.values {
            assert!(!formatted.contains(&value.to_string()));
            assert!(!formatted.contains(&format!("{value:x}")));
        }
    }

    #[test]
    fn captures_opaque_gic_device_state_and_releases_object() {
        let api = FakeGicStateApi::new(vec![0x12, 0x34, 0x56, 0x78]);

        let state = capture_gic_device_state_with_api(&api)
            .expect("opaque GIC device state should be captured");

        assert_eq!(state.as_bytes(), [0x12, 0x34, 0x56, 0x78]);
        assert_eq!(state.len(), 4);
        assert!(!state.is_empty());
        assert_eq!(
            api.calls(),
            ["state_create", "state_size", "state_data", "os_release"]
        );
        assert_eq!(api.released_states(), [7]);
    }

    #[test]
    fn bounded_gic_capture_rejects_reported_size_before_allocation_or_copy() {
        let api = FakeGicStateApi::new(vec![1, 2, 3, 4]);

        assert_eq!(
            capture_gic_device_state_with_api_bounded(&api, Some(3)),
            Err(HvfGicError::StateTooLarge {
                size: 4,
                maximum: 3,
            })
        );
        assert_eq!(api.calls(), ["state_create", "state_size", "os_release"]);
        assert_eq!(api.released_states(), [7]);

        let state = capture_gic_device_state_with_api_bounded(&api, Some(4))
            .expect("exact bounded GIC state should capture on retry");
        assert_eq!(state.as_bytes(), [1, 2, 3, 4]);
    }

    #[test]
    fn restores_opaque_gic_device_state_with_exact_pointer_and_size() {
        let api = FakeGicStateRestoreApi::default();
        let restorer = HvfGicStateRestorer::with_api(api.clone());
        let state = HvfGicDeviceState::new(vec![0x12, 0x34, 0x56, 0x78]);
        let expected_pointer = state.as_bytes().as_ptr() as usize;

        restorer
            .restore(&state)
            .expect("opaque GIC device state should be restored");

        assert_ne!(expected_pointer, 0);
        assert_eq!(api.calls(), [(expected_pointer, state.len())]);
        assert_eq!(state.len(), 4);
    }

    #[test]
    fn empty_gic_device_state_is_rejected_without_calling_setter() {
        let api = FakeGicStateRestoreApi::default();
        let restorer = HvfGicStateRestorer::with_api(api.clone());
        let state = HvfGicDeviceState::new(Vec::new());

        assert_eq!(
            restorer.restore(&state),
            Err(HvfGicError::InvalidParameter {
                name: "gic_state_size",
                value: 0,
            })
        );
        assert!(api.calls().is_empty());
    }

    #[test]
    fn gic_device_state_restore_preserves_backend_error_without_bytes() {
        let api = FakeGicStateRestoreApi::default().with_failure();
        let restorer = HvfGicStateRestorer::with_api(api.clone());
        let state = HvfGicDeviceState::new(b"sensitive-gic-state".to_vec());

        let err = restorer
            .restore(&state)
            .expect_err("injected GIC state restore failure should propagate");

        assert_eq!(
            err,
            HvfGicError::Backend(BackendError::Hypervisor(
                "injected GIC state restore failure".to_string()
            ))
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("hypervisor error: injected GIC state restore failure".to_string())
        );
        assert!(!err.to_string().contains("sensitive"));
        assert_eq!(api.calls().len(), 1);
    }

    #[test]
    fn gic_device_state_restore_panic_has_only_one_setter_call() {
        let api = FakeGicStateRestoreApi::default().with_panic();
        let restorer = HvfGicStateRestorer::with_api(api.clone());
        let state = HvfGicDeviceState::new(vec![1]);

        let unwind = panic::catch_unwind(AssertUnwindSafe(|| {
            let _ = restorer.restore(&state);
        }));

        assert!(unwind.is_err());
        assert_eq!(api.calls().len(), 1);
    }

    #[test]
    fn opaque_gic_device_state_debug_redacts_bytes() {
        let state = HvfGicDeviceState::new(b"sensitive-gic-state".to_vec());

        let debug = format!("{state:?}");

        assert!(debug.contains("HvfGicDeviceState"));
        assert!(debug.contains("len: 19"));
        assert!(!debug.contains("sensitive"));
        assert!(!debug.contains("115, 101, 110"));
    }

    #[test]
    fn displays_gic_state_capture_errors_without_buffer_contents() {
        let create = HvfGicError::StateCreateFailed;
        assert_eq!(
            create.to_string(),
            "failed to create Hypervisor.framework GIC state object"
        );
        assert!(create.source().is_none());

        let allocation = HvfGicError::StateAllocationFailed { size: 4096 };
        assert_eq!(
            allocation.to_string(),
            "failed to allocate 4096 bytes for Hypervisor.framework GIC state"
        );
        assert!(allocation.source().is_none());
    }

    #[test]
    fn gic_state_create_failure_has_no_object_to_release() {
        let api = FakeGicStateApi::new(vec![1]).with_failure("state_create");

        assert_eq!(
            capture_gic_device_state_with_api(&api),
            Err(HvfGicError::StateCreateFailed)
        );
        assert_eq!(api.calls(), ["state_create"]);
        assert!(api.released_states().is_empty());
    }

    #[test]
    fn gic_state_size_and_data_failures_release_object_without_partial_value() {
        for failure in ["state_size", "state_data"] {
            let api = FakeGicStateApi::new(vec![1, 2, 3]).with_failure(failure);

            assert_eq!(
                capture_gic_device_state_with_api(&api),
                Err(HvfGicError::Backend(BackendError::Hypervisor(format!(
                    "injected {failure} failure"
                ))))
            );
            let expected_calls = if failure == "state_size" {
                vec!["state_create", "state_size", "os_release"]
            } else {
                vec!["state_create", "state_size", "state_data", "os_release"]
            };
            assert_eq!(api.calls(), expected_calls);
            assert_eq!(api.released_states(), [7]);
        }
    }

    #[test]
    fn zero_gic_state_size_releases_object_without_copying_data() {
        let api = FakeGicStateApi::new(Vec::new());

        assert_eq!(
            capture_gic_device_state_with_api(&api),
            Err(HvfGicError::InvalidParameter {
                name: "gic_state_size",
                value: 0,
            })
        );
        assert_eq!(api.calls(), ["state_create", "state_size", "os_release"]);
        assert_eq!(api.released_states(), [7]);
    }

    #[test]
    fn gic_state_allocation_failure_releases_object_without_copying_data() {
        let api = FakeGicStateApi::new(vec![1]).with_reported_size(usize::MAX);

        assert_eq!(
            capture_gic_device_state_with_api(&api),
            Err(HvfGicError::StateAllocationFailed { size: u64::MAX })
        );
        assert_eq!(api.calls(), ["state_create", "state_size", "os_release"]);
        assert_eq!(api.released_states(), [7]);
    }

    #[test]
    fn gic_state_guard_releases_object_during_unwind() {
        let api = FakeGicStateApi::new(vec![1]).with_panic("state_data");

        let unwind = panic::catch_unwind(AssertUnwindSafe(|| {
            let _ = capture_gic_device_state_with_api(&api);
        }));

        assert!(unwind.is_err());
        assert_eq!(
            api.calls(),
            ["state_create", "state_size", "state_data", "os_release"]
        );
        assert_eq!(api.released_states(), [7]);
    }

    #[test]
    fn metadata_places_gic_regions_below_mmio32_start() {
        let metadata = metadata_from_parameters(default_parameters())
            .expect("default GIC parameters should produce metadata");

        assert_eq!(
            metadata.distributor,
            HvfGicRegion {
                base: 0x3fff_0000,
                size: DIST_SIZE
            }
        );
        assert_eq!(
            metadata.redistributor.region,
            HvfGicRegion {
                base: 0x3ffd_0000,
                size: REDIST_REGION_SIZE
            }
        );
        assert_eq!(
            metadata.redistributor.single_redistributor_size,
            REDIST_SIZE
        );
        assert_eq!(
            metadata.spi_interrupt_range,
            HvfGicInterruptRange {
                base: 32,
                count: 96
            }
        );
        assert_eq!(
            metadata.timer_interrupts,
            HvfGicTimerInterrupts {
                el1_virtual_timer_intid: 27,
                el1_physical_timer_intid: 30,
            }
        );
        assert_eq!(metadata.msi, None);
        assert_eq!(HvfGicMetadata::FDT_COMPATIBILITY, "arm,gic-v3");
        assert_eq!(HvfGicMetadata::FDT_INTERRUPT_CELLS, 3);
        assert_eq!(HvfGicMetadata::FDT_MAINTENANCE_IRQ, 9);
    }

    #[test]
    fn metadata_converts_to_arm64_fdt_gic_input() {
        let metadata = metadata_from_parameters(default_parameters())
            .expect("default GIC parameters should produce metadata");

        let fdt_gic = metadata.arm64_fdt_gic();

        assert_eq!(
            fdt_gic.distributor,
            Arm64FdtRegion {
                base: 0x3fff_0000,
                size: DIST_SIZE,
            }
        );
        assert_eq!(
            fdt_gic.redistributor,
            Arm64FdtRegion {
                base: 0x3ffd_0000,
                size: REDIST_REGION_SIZE,
            }
        );
        assert_eq!(fdt_gic.compatibility, "arm,gic-v3");
        assert_eq!(fdt_gic.interrupt_cells, 3);
        assert_eq!(fdt_gic.maintenance_irq, 9);
        assert_eq!(fdt_gic.msi, None);
    }

    #[test]
    fn metadata_converts_optional_msi_to_arm64_fdt_input() {
        let mut metadata = metadata_from_parameters(default_parameters())
            .expect("default GIC parameters should produce metadata");
        metadata.msi = Some(HvfGicMsiMetadata {
            region: HvfGicRegion {
                base: 0x3ffc_0000,
                size: 0x1_0000,
            },
            interrupt_range: HvfGicInterruptRange {
                base: 128,
                count: 32,
            },
        });

        assert_eq!(
            metadata.arm64_fdt_gic().msi,
            Some(Arm64FdtMsi {
                region: Arm64FdtRegion {
                    base: 0x3ffc_0000,
                    size: 0x1_0000,
                },
                interrupt_range: Arm64FdtInterruptRange {
                    base: 128,
                    count: 32,
                },
            })
        );
    }

    #[test]
    fn msi_metadata_reserves_the_highest_usable_host_spis() {
        let metadata = metadata_from_parameters(parameters_with_msi(8))
            .expect("valid MSI demand should produce GIC metadata");

        assert_eq!(
            metadata.spi_interrupt_range,
            HvfGicInterruptRange {
                base: 32,
                count: 88,
            }
        );
        assert_eq!(
            metadata.msi,
            Some(HvfGicMsiMetadata {
                region: HvfGicRegion {
                    base: 0x3ffc_0000,
                    size: 0x1_0000,
                },
                interrupt_range: HvfGicInterruptRange {
                    base: 120,
                    count: 8,
                },
            })
        );
    }

    #[test]
    fn msi_metadata_excludes_the_gicv2m_terminal_spi_from_both_ranges() {
        let mut parameters = parameters_with_msi(1);
        parameters.spi_interrupt_range = HvfGicInterruptRange {
            base: 32,
            count: 988,
        };

        let metadata = metadata_from_parameters(parameters)
            .expect("host range with a terminal guard should produce GIC metadata");

        assert_eq!(
            metadata.spi_interrupt_range,
            HvfGicInterruptRange {
                base: 32,
                count: 986,
            }
        );
        assert_eq!(
            metadata.msi,
            Some(HvfGicMsiMetadata {
                region: HvfGicRegion {
                    base: 0x3ffc_0000,
                    size: 0x1_0000,
                },
                interrupt_range: HvfGicInterruptRange {
                    base: ARM64_GICV2M_SPI_END_EXCLUSIVE - 1,
                    count: 1,
                },
            })
        );
        assert_eq!(
            metadata.spi_interrupt_range.base + metadata.spi_interrupt_range.count,
            ARM64_GICV2M_SPI_END_EXCLUSIVE - 1
        );
    }

    #[test]
    fn msi_metadata_rejects_demand_that_consumes_the_usable_gicv2m_domain() {
        let mut parameters = parameters_with_msi(987);
        parameters.spi_interrupt_range = HvfGicInterruptRange {
            base: 32,
            count: 988,
        };

        assert_eq!(
            metadata_from_parameters(parameters),
            Err(HvfGicError::InsufficientMsiInterruptCapacity)
        );
    }

    #[test]
    fn msi_metadata_rejects_demand_that_would_empty_the_legacy_range() {
        assert_eq!(
            metadata_from_parameters(parameters_with_msi(96)),
            Err(HvfGicError::InsufficientMsiInterruptCapacity)
        );
    }

    #[test]
    fn msi_metadata_rejects_invalid_frame_geometry_without_values_in_error() {
        for (region_size, region_alignment, expected_name) in [
            (0, 0x1_0000, "region_size"),
            (0x40, 0x1_0000, "region_size"),
            (0x1_0000, 3, "region_base_alignment"),
        ] {
            let mut parameters = parameters_with_msi(1);
            let msi = parameters
                .msi
                .as_mut()
                .expect("MSI parameters should exist");
            msi.region_size = region_size;
            msi.region_alignment = region_alignment;

            let error = metadata_from_parameters(parameters)
                .expect_err("invalid MSI frame geometry should fail");
            assert_eq!(
                error,
                HvfGicError::InvalidMsiParameter {
                    name: expected_name,
                }
            );
            assert!(!error.to_string().contains(&region_size.to_string()));
        }
    }

    #[test]
    fn msi_interrupt_allocator_allocates_only_the_reserved_range() {
        let metadata = metadata_from_parameters(parameters_with_msi(2))
            .expect("valid MSI demand should produce metadata");
        let signaler = HvfGicMsiSignaler::with_api(
            metadata.msi.expect("MSI metadata should exist"),
            FakeGicApi::default(),
        )
        .expect("MSI metadata should produce a signaler");
        let allocator = signaler.allocator();
        let peer_allocator = signaler.allocator();

        assert_eq!(
            allocator.range(),
            HvfGicInterruptRange {
                base: 126,
                count: 2,
            }
        );
        assert_eq!(
            allocator
                .allocate()
                .expect("first MSI should allocate")
                .raw_value(),
            126
        );
        assert_eq!(
            peer_allocator
                .allocate()
                .expect("last MSI should allocate")
                .raw_value(),
            127
        );
        assert!(allocator.is_exhausted());
        assert!(peer_allocator.is_exhausted());
        assert_eq!(
            allocator.allocate(),
            Err(HvfGicMsiInterruptAllocationError::Exhausted)
        );
    }

    #[test]
    fn msi_interrupt_allocator_requires_valid_metadata() {
        let malformed = HvfGicMsiMetadata {
            region: HvfGicRegion {
                base: 0x3ffc_0000,
                size: 0x40,
            },
            interrupt_range: HvfGicInterruptRange {
                base: 127,
                count: 1,
            },
        };
        assert_eq!(
            HvfGicMsiInterruptAllocator::from_metadata(malformed)
                .expect_err("short MSI frame should fail"),
            HvfGicMsiInterruptAllocationError::InvalidMetadata(
                "message frame does not contain SETSPI"
            )
        );
    }

    #[test]
    fn msi_allocator_and_signaler_reject_the_gicv2m_terminal_spi() {
        let terminal_metadata = HvfGicMsiMetadata {
            region: HvfGicRegion {
                base: 0x3ffc_0000,
                size: 0x1_0000,
            },
            interrupt_range: HvfGicInterruptRange {
                base: ARM64_GICV2M_SPI_END_EXCLUSIVE,
                count: 1,
            },
        };

        assert_eq!(
            HvfGicMsiInterruptAllocator::from_metadata(terminal_metadata)
                .expect_err("terminal SPI should not produce an MSI allocator"),
            HvfGicMsiInterruptAllocationError::InvalidMetadata(
                "interrupt range exceeds the GICv2m SPI domain"
            )
        );
        assert_eq!(
            HvfGicMsiSignaler::with_api(terminal_metadata, FakeGicApi::default())
                .expect_err("terminal SPI should not produce an MSI signaler"),
            HvfGicMsiSignalError::InvalidMetadata("interrupt range exceeds the GICv2m SPI domain")
        );
    }

    #[test]
    fn msi_signaler_uses_set_spi_nsr_address_and_allocated_intid() {
        let metadata = metadata_from_parameters(parameters_with_msi(2))
            .expect("valid MSI demand should produce metadata");
        let api = FakeGicApi::default();
        let signaler = HvfGicMsiSignaler::with_api(
            metadata.msi.expect("MSI metadata should exist"),
            api.clone(),
        )
        .expect("valid MSI metadata should produce a signaler");
        let allocator = signaler.allocator();
        let interrupt = allocator.allocate().expect("MSI should allocate");

        signaler
            .send(&interrupt)
            .expect("allocated MSI should signal");

        assert_eq!(
            api.msi_signals(),
            vec![(0x3ffc_0000 + ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET, 126,)]
        );
        assert_eq!(api.calls(), vec!["hv_gic_send_msi"]);
    }

    #[test]
    fn msi_signaler_rejects_interrupt_from_a_different_capability() {
        let metadata = metadata_from_parameters(parameters_with_msi(2))
            .expect("valid MSI demand should produce metadata");
        let signaler_api = FakeGicApi::default();
        let signaler = HvfGicMsiSignaler::with_api(
            metadata.msi.expect("MSI metadata should exist"),
            signaler_api.clone(),
        )
        .expect("valid MSI metadata should produce a signaler");
        let other_signaler = HvfGicMsiSignaler::with_api(
            metadata.msi.expect("alternate MSI metadata should exist"),
            FakeGicApi::default(),
        )
        .expect("alternate metadata should produce a signaler");
        let interrupt = other_signaler
            .allocator()
            .allocate()
            .expect("alternate MSI should allocate");

        assert_eq!(
            signaler
                .send(&interrupt)
                .expect_err("capability provenance mismatch should fail"),
            HvfGicMsiSignalError::InterruptOutOfRange
        );
        assert!(signaler_api.calls().is_empty());
        assert!(signaler_api.msi_signals().is_empty());
    }

    #[test]
    fn msi_signaler_deactivation_revokes_every_clone() {
        let metadata = metadata_from_parameters(parameters_with_msi(1))
            .expect("valid MSI demand should produce metadata");
        let api = FakeGicApi::default();
        let signaler = HvfGicMsiSignaler::with_api(
            metadata.msi.expect("MSI metadata should exist"),
            api.clone(),
        )
        .expect("valid MSI metadata should produce a signaler");
        let clone = signaler.clone();
        let interrupt = signaler
            .allocator()
            .allocate()
            .expect("MSI should allocate");

        signaler.deactivate();

        for revoked in [&signaler, &clone] {
            assert_eq!(
                revoked
                    .send(&interrupt)
                    .expect_err("revoked signaler should reject sends"),
                HvfGicMsiSignalError::InvalidState("HVF GIC MSI signaler is inactive")
            );
        }
        assert!(api.calls().is_empty());
        assert!(api.msi_signals().is_empty());
    }

    #[test]
    fn msi_signaler_serializes_sends_and_waits_before_deactivation() {
        struct BlockingMsiSignalApi {
            entered: mpsc::SyncSender<()>,
            releases: Mutex<mpsc::Receiver<()>>,
        }

        impl fmt::Debug for BlockingMsiSignalApi {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct("BlockingMsiSignalApi")
                    .finish_non_exhaustive()
            }
        }

        impl super::HvfGicMsiSignalApi for BlockingMsiSignalApi {
            fn send_msi(&self, _: u64, _: u32) -> Result<(), HvfGicError> {
                self.entered
                    .send(())
                    .expect("test should observe each entered send");
                self.releases
                    .lock()
                    .expect("test release receiver should be lockable")
                    .recv()
                    .expect("test should release each entered send");
                Ok(())
            }
        }

        let metadata = metadata_from_parameters(parameters_with_msi(2))
            .expect("valid MSI demand should produce metadata");
        let (entered_sender, entered_receiver) = mpsc::sync_channel(2);
        let (release_sender, release_receiver) = mpsc::sync_channel(2);
        let signaler = HvfGicMsiSignaler::with_api(
            metadata.msi.expect("MSI metadata should exist"),
            BlockingMsiSignalApi {
                entered: entered_sender,
                releases: Mutex::new(release_receiver),
            },
        )
        .expect("valid MSI metadata should produce a signaler");
        let allocator = signaler.allocator();
        let first_interrupt = allocator.allocate().expect("first MSI should allocate");
        let second_interrupt = allocator.allocate().expect("second MSI should allocate");

        std::thread::scope(|scope| {
            let first_signaler = signaler.clone();
            let first_interrupt_ref = &first_interrupt;
            let first_send = scope.spawn(move || first_signaler.send(first_interrupt_ref));
            entered_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("first send should enter the host API");

            let (second_started_sender, second_started_receiver) = mpsc::sync_channel(1);
            let second_signaler = signaler.clone();
            let second_send = scope.spawn(move || {
                second_started_sender
                    .send(())
                    .expect("second send start should be observable");
                second_signaler.send(&second_interrupt)
            });
            second_started_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("second send should start while the first is blocked");
            assert!(matches!(
                entered_receiver.recv_timeout(Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));

            release_sender
                .send(())
                .expect("first host send should be released");
            first_send
                .join()
                .expect("first send thread should not panic")
                .expect("first send should succeed");
            entered_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("second send should enter after the first releases the signaler lock");

            let (deactivate_started_sender, deactivate_started_receiver) = mpsc::sync_channel(1);
            let (deactivated_sender, deactivated_receiver) = mpsc::sync_channel(1);
            let retained_signaler = signaler.clone();
            let deactivate = scope.spawn(move || {
                deactivate_started_sender
                    .send(())
                    .expect("deactivation start should be observable");
                retained_signaler.deactivate();
                deactivated_sender
                    .send(())
                    .expect("deactivation completion should be observable");
            });
            deactivate_started_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("deactivation should start while the second send is blocked");
            assert!(matches!(
                deactivated_receiver.recv_timeout(Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));

            release_sender
                .send(())
                .expect("second host send should be released");
            second_send
                .join()
                .expect("second send thread should not panic")
                .expect("second send should succeed");
            deactivate
                .join()
                .expect("deactivation thread should not panic");
            deactivated_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("deactivation should complete after the in-flight send");
        });

        assert_eq!(
            signaler
                .send(&first_interrupt)
                .expect_err("deactivation should revoke later sends"),
            HvfGicMsiSignalError::InvalidState("HVF GIC MSI signaler is inactive")
        );
    }

    #[test]
    fn msi_signaler_preserves_hvf_failure_source() {
        let metadata = metadata_from_parameters(parameters_with_msi(1))
            .expect("valid MSI demand should produce metadata");
        let api = FakeGicApi::default().with_failure("hv_gic_send_msi");
        let signaler = HvfGicMsiSignaler::with_api(
            metadata.msi.expect("MSI metadata should exist"),
            api.clone(),
        )
        .expect("valid MSI metadata should produce a signaler");
        let interrupt = signaler
            .allocator()
            .allocate()
            .expect("MSI should allocate");

        let error = signaler
            .send(&interrupt)
            .expect_err("injected send failure should propagate");

        assert_eq!(
            error,
            HvfGicMsiSignalError::Signal {
                source: HvfGicError::Backend(BackendError::Hypervisor(
                    "injected hv_gic_send_msi failure".to_string(),
                )),
            }
        );
        assert!(error.source().is_some());
        assert!(api.msi_signals().is_empty());
    }

    #[test]
    fn msi_debug_output_redacts_configuration_and_capability_values() {
        let metadata = metadata_from_parameters(parameters_with_msi(7))
            .expect("valid MSI demand should produce metadata");
        let signaler = HvfGicMsiSignaler::with_api(
            metadata.msi.expect("MSI metadata should exist"),
            FakeGicApi::default(),
        )
        .expect("valid MSI metadata should produce a signaler");
        let allocator = signaler.allocator();
        let interrupt = allocator.allocate().expect("MSI should allocate");

        assert_eq!(
            format!("{:?}", msi_configuration(7)),
            "HvfGicMsiConfiguration { interrupt_count: \"<redacted>\" }"
        );
        assert!(!format!("{:?}", metadata.msi).contains("121"));
        assert!(!format!("{allocator:?}").contains("121"));
        assert!(!format!("{interrupt:?}").contains("121"));
        assert!(!format!("{signaler:?}").contains("3ffc0040"));
    }

    #[test]
    fn msi_allocator_and_signaler_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<HvfGicMsiInterruptAllocator>();
        assert_send_sync::<HvfGicMsiSignaler>();
    }

    #[test]
    fn metadata_converts_hvf_timer_intids_to_fdt_ppis() {
        let metadata = metadata_from_parameters(default_parameters())
            .expect("default GIC parameters should produce metadata");

        let timers = metadata
            .arm64_fdt_timer_interrupts()
            .expect("validated HVF timer INTIDs should map to FDT PPIs");

        assert_eq!(
            timers,
            Arm64FdtTimerInterrupts {
                secure_physical: ARM64_FDT_SECURE_PHYSICAL_TIMER_PPI,
                non_secure_physical: ARM64_FDT_NON_SECURE_PHYSICAL_TIMER_PPI,
                virtual_timer: ARM64_FDT_VIRTUAL_TIMER_PPI,
                hypervisor: ARM64_FDT_HYPERVISOR_TIMER_PPI,
            }
        );
    }

    #[test]
    fn interrupt_line_allocator_uses_metadata_spi_range() {
        let metadata = metadata_from_parameters(default_parameters())
            .expect("default GIC parameters should produce metadata");
        let mut allocator = HvfGicInterruptLineAllocator::from_metadata(&metadata)
            .expect("validated GIC metadata should produce an allocator");

        assert_eq!(
            allocator.range(),
            HvfGicInterruptRange {
                base: 32,
                count: 96,
            }
        );
        assert_eq!(allocator.remaining(), 96);
        assert!(!allocator.is_exhausted());

        let line = allocator
            .allocate()
            .expect("first SPI interrupt line should allocate");

        assert_eq!(line.raw_value(), 32);
        assert_eq!(allocator.remaining(), 95);
    }

    #[test]
    fn interrupt_line_allocator_allocates_first_last_and_exhausts() {
        let range = HvfGicInterruptRange { base: 40, count: 2 };
        let mut allocator = HvfGicInterruptLineAllocator::new(range)
            .expect("valid SPI range should produce an allocator");

        assert_eq!(
            allocator
                .allocate()
                .expect("first line should allocate")
                .raw_value(),
            40
        );
        assert_eq!(
            allocator
                .allocate()
                .expect("last line should allocate")
                .raw_value(),
            41
        );
        assert_eq!(allocator.remaining(), 0);
        assert!(allocator.is_exhausted());
        assert_eq!(
            allocator.allocate(),
            Err(HvfInterruptLineAllocationError::Exhausted { range })
        );
    }

    #[test]
    fn interrupt_line_allocator_does_not_repeat_lines() {
        let mut allocator =
            HvfGicInterruptLineAllocator::new(HvfGicInterruptRange { base: 32, count: 3 })
                .expect("valid SPI range should produce an allocator");
        let first = allocator
            .allocate()
            .expect("first line should allocate")
            .raw_value();
        let second = allocator
            .allocate()
            .expect("second line should allocate")
            .raw_value();
        let third = allocator
            .allocate()
            .expect("third line should allocate")
            .raw_value();

        assert_eq!([first, second, third], [32, 33, 34]);
    }

    #[test]
    fn interrupt_line_allocator_rejects_base_below_spi_range() {
        assert_eq!(
            HvfGicInterruptLineAllocator::new(HvfGicInterruptRange { base: 31, count: 1 })
                .expect_err("base below SPI range should fail"),
            HvfInterruptLineAllocationError::InvalidRange(HvfGicError::InvalidParameter {
                name: "spi_interrupt_range.base",
                value: 31,
            })
        );
    }

    #[test]
    fn interrupt_line_allocator_rejects_zero_count() {
        assert_eq!(
            HvfGicInterruptLineAllocator::new(HvfGicInterruptRange { base: 32, count: 0 })
                .expect_err("zero-count range should fail"),
            HvfInterruptLineAllocationError::InvalidRange(HvfGicError::InvalidParameter {
                name: "spi_interrupt_range.count",
                value: 0,
            })
        );
    }

    #[test]
    fn interrupt_line_allocator_accepts_last_non_overflowing_range() {
        let mut allocator = HvfGicInterruptLineAllocator::new(HvfGicInterruptRange {
            base: u32::MAX - 1,
            count: 1,
        })
        .expect("last non-overflowing SPI range should allocate");

        assert_eq!(
            allocator
                .allocate()
                .expect("single line should allocate")
                .raw_value(),
            u32::MAX - 1
        );
        assert!(allocator.is_exhausted());
    }

    #[test]
    fn interrupt_line_allocator_rejects_overflowing_range() {
        assert_eq!(
            HvfGicInterruptLineAllocator::new(HvfGicInterruptRange {
                base: u32::MAX - 1,
                count: 2,
            })
            .expect_err("overflowing range should fail"),
            HvfInterruptLineAllocationError::InvalidRange(HvfGicError::InvalidParameter {
                name: "spi_interrupt_range.end_exclusive",
                value: u64::from(u32::MAX) + 1,
            })
        );
    }

    #[test]
    fn displays_interrupt_line_allocation_errors() {
        let invalid_range =
            HvfInterruptLineAllocationError::InvalidRange(HvfGicError::InvalidParameter {
                name: "spi_interrupt_range.count",
                value: 0,
            });
        assert_eq!(
            invalid_range.to_string(),
            "invalid HVF GIC interrupt line allocation range: invalid Hypervisor.framework GIC parameter spi_interrupt_range.count=0"
        );
        assert_eq!(
            invalid_range.source().map(ToString::to_string),
            Some(
                "invalid Hypervisor.framework GIC parameter spi_interrupt_range.count=0"
                    .to_string()
            )
        );

        let invalid_line =
            HvfInterruptLineAllocationError::InvalidLine(GuestInterruptLineError::Zero);
        assert_eq!(
            invalid_line.to_string(),
            "invalid HVF GIC interrupt line: guest interrupt line 0 is invalid"
        );
        assert_eq!(
            invalid_line.source().map(ToString::to_string),
            Some("guest interrupt line 0 is invalid".to_string())
        );

        let exhausted = HvfInterruptLineAllocationError::Exhausted {
            range: HvfGicInterruptRange { base: 32, count: 1 },
        };
        assert_eq!(
            exhausted.to_string(),
            "HVF GIC SPI interrupt range base=32 count=1 is exhausted"
        );
        assert!(exhausted.source().is_none());
    }

    #[test]
    fn interrupt_line_allocator_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<HvfGicInterruptLineAllocator>();
    }

    #[test]
    fn spi_signaler_sets_high_level_at_range_base() {
        let api = FakeGicApi::default();
        let range = HvfGicInterruptRange { base: 32, count: 2 };
        let signaler = HvfGicSpiSignaler::with_api(range, api.clone())
            .expect("valid SPI range should create a signaler");

        signaler
            .set_level(line(32), true)
            .expect("base SPI line should signal");

        assert_eq!(signaler.range(), range);
        assert_eq!(api.spi_signals(), vec![(32, true)]);
        assert_eq!(api.calls(), vec!["hv_gic_set_spi"]);
    }

    #[test]
    fn spi_signaler_preflights_line_without_calling_hvf() {
        let api = FakeGicApi::default();
        let signaler =
            HvfGicSpiSignaler::with_api(HvfGicInterruptRange { base: 32, count: 2 }, api.clone())
                .expect("valid SPI range should create a signaler");

        signaler
            .validate_line(line(33))
            .expect("implemented SPI line should pass preflight");

        assert!(api.calls().is_empty());
        assert!(api.spi_signals().is_empty());
    }

    #[test]
    fn spi_signaler_sets_low_level_at_last_range_line() {
        let api = FakeGicApi::default();
        let signaler =
            HvfGicSpiSignaler::with_api(HvfGicInterruptRange { base: 32, count: 2 }, api.clone())
                .expect("valid SPI range should create a signaler");

        signaler
            .set_level(line(33), false)
            .expect("last SPI line should signal");

        assert_eq!(api.spi_signals(), vec![(33, false)]);
    }

    #[test]
    fn spi_signaler_rejects_line_before_range_without_calling_hvf() {
        let api = FakeGicApi::default();
        let signaler =
            HvfGicSpiSignaler::with_api(HvfGicInterruptRange { base: 32, count: 2 }, api.clone())
                .expect("valid SPI range should create a signaler");

        assert_eq!(
            signaler
                .set_level(line(31), true)
                .expect_err("line before range should fail"),
            HvfGicSpiSignalError::LineOutOfRange {
                line: line(31),
                range: HvfGicInterruptRange { base: 32, count: 2 },
            }
        );
        assert!(api.calls().is_empty());
        assert!(api.spi_signals().is_empty());
    }

    #[test]
    fn spi_signaler_rejects_end_exclusive_line_without_calling_hvf() {
        let api = FakeGicApi::default();
        let signaler =
            HvfGicSpiSignaler::with_api(HvfGicInterruptRange { base: 32, count: 2 }, api.clone())
                .expect("valid SPI range should create a signaler");

        assert_eq!(
            signaler
                .set_level(line(34), true)
                .expect_err("end-exclusive line should fail"),
            HvfGicSpiSignalError::LineOutOfRange {
                line: line(34),
                range: HvfGicInterruptRange { base: 32, count: 2 },
            }
        );
        assert!(api.calls().is_empty());
        assert!(api.spi_signals().is_empty());
    }

    #[test]
    fn spi_signaler_rejects_invalid_range_before_calling_hvf() {
        let api = FakeGicApi::default();

        assert_eq!(
            HvfGicSpiSignaler::with_api(HvfGicInterruptRange { base: 32, count: 0 }, api.clone())
                .expect_err("invalid range should fail before creating signaler"),
            HvfGicSpiSignalError::InvalidRange(HvfGicError::InvalidParameter {
                name: "spi_interrupt_range.count",
                value: 0,
            })
        );
        assert!(api.calls().is_empty());
        assert!(api.spi_signals().is_empty());
    }

    #[test]
    fn spi_signaler_from_metadata_rejects_invalid_range_before_backend_lookup() {
        let mut metadata =
            metadata_from_parameters(default_parameters()).expect("default metadata should build");
        metadata.spi_interrupt_range = HvfGicInterruptRange { base: 32, count: 0 };

        assert_eq!(
            HvfGicSpiSignaler::from_metadata(&metadata)
                .expect_err("invalid metadata range should fail before loading the backend"),
            HvfGicSpiSignalError::InvalidRange(HvfGicError::InvalidParameter {
                name: "spi_interrupt_range.count",
                value: 0,
            })
        );
    }

    #[test]
    fn spi_signaler_accepts_last_non_overflowing_range() {
        let api = FakeGicApi::default();
        let signaler = HvfGicSpiSignaler::with_api(
            HvfGicInterruptRange {
                base: u32::MAX - 1,
                count: 1,
            },
            api.clone(),
        )
        .expect("last non-overflowing SPI range should create a signaler");

        signaler
            .set_level(line(u32::MAX - 1), true)
            .expect("last non-overflowing SPI line should signal");

        assert_eq!(api.spi_signals(), vec![(u32::MAX - 1, true)]);
    }

    #[test]
    fn spi_signaler_rejects_overflowing_range_before_calling_hvf() {
        let api = FakeGicApi::default();

        assert_eq!(
            HvfGicSpiSignaler::with_api(
                HvfGicInterruptRange {
                    base: u32::MAX - 1,
                    count: 2,
                },
                api.clone(),
            )
            .expect_err("overflowing range should fail before creating signaler"),
            HvfGicSpiSignalError::InvalidRange(HvfGicError::InvalidParameter {
                name: "spi_interrupt_range.end_exclusive",
                value: u64::from(u32::MAX) + 1,
            })
        );
        assert!(api.calls().is_empty());
        assert!(api.spi_signals().is_empty());
    }

    #[test]
    fn spi_signaler_preserves_backend_signal_failure_source() {
        let api = FakeGicApi::default().with_failure("hv_gic_set_spi");
        let signaler =
            HvfGicSpiSignaler::with_api(HvfGicInterruptRange { base: 32, count: 2 }, api.clone())
                .expect("valid SPI range should create a signaler");

        let err = signaler
            .set_level(line(32), true)
            .expect_err("backend failure should propagate");

        assert_eq!(
            err,
            HvfGicSpiSignalError::Signal {
                line: line(32),
                level: true,
                source: HvfGicError::Backend(BackendError::Hypervisor(
                    "injected hv_gic_set_spi failure".to_string()
                )),
            }
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("hypervisor error: injected hv_gic_set_spi failure".to_string())
        );
        assert_eq!(api.calls(), vec!["hv_gic_set_spi"]);
        assert!(api.spi_signals().is_empty());
    }

    #[test]
    fn spi_signaler_interrupt_sink_asserts_high_level() {
        let api = FakeGicApi::default();
        let signaler =
            HvfGicSpiSignaler::with_api(HvfGicInterruptRange { base: 32, count: 2 }, api.clone())
                .expect("valid SPI range should create a signaler");

        InterruptSink::signal(&signaler, line(32)).expect("sink should assert SPI high");

        assert_eq!(api.spi_signals(), vec![(32, true)]);
    }

    #[test]
    fn spi_signaler_interrupt_sink_maps_typed_errors_to_runtime_signal_error() {
        let api = FakeGicApi::default();
        let signaler =
            HvfGicSpiSignaler::with_api(HvfGicInterruptRange { base: 32, count: 1 }, api.clone())
                .expect("valid SPI range should create a signaler");

        let err = InterruptSink::signal(&signaler, line(33))
            .expect_err("sink should convert typed signal failure");

        assert_eq!(
            err.message(),
            "guest interrupt line 33 is outside HVF GIC SPI range base=32 count=1"
        );
        assert!(api.calls().is_empty());
    }

    #[test]
    fn spi_signaler_supports_concurrent_sink_calls() {
        let api = FakeGicApi::default();
        let signaler = Arc::new(
            HvfGicSpiSignaler::with_api(HvfGicInterruptRange { base: 32, count: 4 }, api.clone())
                .expect("valid SPI range should create a signaler"),
        );
        let barrier = Arc::new(Barrier::new(4));
        let mut handles = Vec::new();

        for raw_line in 32..36 {
            let signaler = Arc::clone(&signaler);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                InterruptSink::signal(&*signaler, line(raw_line))
                    .expect("concurrent sink signal should succeed");
            }));
        }

        for handle in handles {
            handle
                .join()
                .expect("concurrent signal thread should finish");
        }

        let mut signals = api.spi_signals();
        signals.sort_unstable();
        assert_eq!(
            signals,
            vec![(32, true), (33, true), (34, true), (35, true)]
        );
    }

    #[test]
    fn spi_signaler_reports_poisoned_api_lock_without_deadlock() {
        let signaler =
            HvfGicSpiSignaler::with_api(HvfGicInterruptRange { base: 32, count: 1 }, PanicGicApi)
                .expect("valid SPI range should create a signaler");
        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = signaler.set_level(line(32), true);
        }));

        assert!(panic_result.is_err());
        assert_eq!(
            signaler
                .set_level(line(32), true)
                .expect_err("poisoned lock should become typed invalid state"),
            HvfGicSpiSignalError::InvalidState(GIC_SPI_SIGNALER_LOCK_POISONED_MESSAGE)
        );
    }

    #[test]
    fn displays_spi_signal_errors() {
        let invalid_range = HvfGicSpiSignalError::InvalidRange(HvfGicError::InvalidParameter {
            name: "spi_interrupt_range.count",
            value: 0,
        });
        assert_eq!(
            invalid_range.to_string(),
            "invalid HVF GIC SPI signal range: invalid Hypervisor.framework GIC parameter spi_interrupt_range.count=0"
        );
        assert_eq!(
            invalid_range.source().map(ToString::to_string),
            Some(
                "invalid Hypervisor.framework GIC parameter spi_interrupt_range.count=0"
                    .to_string()
            )
        );

        let line_out_of_range = HvfGicSpiSignalError::LineOutOfRange {
            line: line(40),
            range: HvfGicInterruptRange { base: 32, count: 4 },
        };
        assert_eq!(
            line_out_of_range.to_string(),
            "guest interrupt line 40 is outside HVF GIC SPI range base=32 count=4"
        );
        assert!(line_out_of_range.source().is_none());

        let signal = HvfGicSpiSignalError::Signal {
            line: line(32),
            level: true,
            source: HvfGicError::Backend(BackendError::Hypervisor("backend failed".to_string())),
        };
        assert_eq!(
            signal.to_string(),
            "failed to set HVF GIC SPI interrupt line 32 to level true: hypervisor error: backend failed"
        );
        assert_eq!(
            signal.source().map(ToString::to_string),
            Some("hypervisor error: backend failed".to_string())
        );

        let backend = HvfGicSpiSignalError::Backend(HvfGicError::Unsupported("not available"));
        assert_eq!(
            backend.to_string(),
            "failed to initialize HVF GIC SPI signaler: unsupported GIC operation: not available"
        );
        assert_eq!(
            backend.source().map(ToString::to_string),
            Some("unsupported GIC operation: not available".to_string())
        );

        let invalid_state =
            HvfGicSpiSignalError::InvalidState(GIC_SPI_SIGNALER_LOCK_POISONED_MESSAGE);
        assert_eq!(
            invalid_state.to_string(),
            "invalid HVF GIC SPI signaler state: HVF GIC SPI signaler lock is poisoned"
        );
        assert!(invalid_state.source().is_none());
    }

    #[test]
    fn spi_signaler_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<HvfGicSpiSignaler>();
    }

    #[test]
    fn ppi_pending_writer_sets_and_clears_pending_bit() {
        let api = FakeGicApi::default();
        let writer = HvfGicPpiPendingWriter::with_api(api.clone());

        writer
            .set_pending(7, 27, true)
            .expect("virtual timer PPI should be set pending");
        writer
            .set_pending(7, 30, false)
            .expect("physical timer PPI should be cleared pending");

        assert_eq!(
            api.ppi_pending_writes(),
            vec![
                (7, HV_GIC_REDISTRIBUTOR_REG_GICR_ISPENDR0, 1u64 << 27),
                (7, HV_GIC_REDISTRIBUTOR_REG_GICR_ICPENDR0, 1u64 << 30),
            ]
        );
        assert_eq!(
            api.calls(),
            vec![
                "hv_gic_set_redistributor_reg",
                "hv_gic_set_redistributor_reg",
            ]
        );
    }

    #[test]
    fn ppi_pending_writer_accepts_first_and_last_ppi_intids() {
        let api = FakeGicApi::default();
        let writer = HvfGicPpiPendingWriter::with_api(api.clone());

        writer
            .set_pending(7, 16, true)
            .expect("first PPI should be accepted");
        writer
            .set_pending(7, 31, true)
            .expect("last PPI should be accepted");

        assert_eq!(
            api.ppi_pending_writes(),
            vec![
                (7, HV_GIC_REDISTRIBUTOR_REG_GICR_ISPENDR0, 1u64 << 16),
                (7, HV_GIC_REDISTRIBUTOR_REG_GICR_ISPENDR0, 1u64 << 31),
            ]
        );
    }

    #[test]
    fn ppi_pending_writer_rejects_sgi_and_spi_intids_before_calling_hvf() {
        let api = FakeGicApi::default();
        let writer = HvfGicPpiPendingWriter::with_api(api.clone());

        assert_eq!(
            writer
                .set_pending(7, 15, true)
                .expect_err("SGI INTID should be rejected"),
            HvfGicError::InvalidParameter {
                name: "ppi_intid",
                value: 15,
            }
        );
        assert_eq!(
            writer
                .set_pending(7, 32, false)
                .expect_err("SPI INTID should be rejected"),
            HvfGicError::InvalidParameter {
                name: "ppi_intid",
                value: 32,
            }
        );
        assert!(api.calls().is_empty());
        assert!(api.ppi_pending_writes().is_empty());
    }

    #[test]
    fn ppi_pending_writer_preserves_backend_failure_source() {
        let api = FakeGicApi::default().with_failure("hv_gic_set_redistributor_reg");
        let writer = HvfGicPpiPendingWriter::with_api(api.clone());

        let err = writer
            .set_pending(7, 27, true)
            .expect_err("backend failure should propagate");

        assert_eq!(
            err,
            HvfGicError::Backend(BackendError::Hypervisor(
                "injected hv_gic_set_redistributor_reg failure".to_string()
            ))
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("hypervisor error: injected hv_gic_set_redistributor_reg failure".to_string())
        );
        assert_eq!(api.calls(), vec!["hv_gic_set_redistributor_reg"]);
        assert!(api.ppi_pending_writes().is_empty());
    }

    #[test]
    fn metadata_timer_conversion_rejects_publicly_constructed_non_ppi_intids() {
        let mut metadata = metadata_from_parameters(default_parameters())
            .expect("default GIC parameters should produce metadata");
        metadata.timer_interrupts = HvfGicTimerInterrupts {
            el1_virtual_timer_intid: 15,
            el1_physical_timer_intid: 30,
        };

        let err = metadata
            .arm64_fdt_timer_interrupts()
            .expect_err("non-PPI timer INTID should fail conversion");

        assert_eq!(
            err,
            Arm64FdtError::InvalidPpiIntid {
                name: "el1_virtual_timer_intid",
                intid: 15,
            }
        );
    }

    #[test]
    fn metadata_timer_conversion_rejects_publicly_constructed_duplicate_intids() {
        let mut metadata = metadata_from_parameters(default_parameters())
            .expect("default GIC parameters should produce metadata");
        metadata.timer_interrupts = HvfGicTimerInterrupts {
            el1_virtual_timer_intid: 27,
            el1_physical_timer_intid: 27,
        };

        let err = metadata
            .arm64_fdt_timer_interrupts()
            .expect_err("duplicate timer INTIDs should fail conversion");

        assert_eq!(
            err,
            Arm64FdtError::DuplicatePpi {
                first: "non_secure_physical_timer",
                second: "virtual_timer",
                value: 11,
            }
        );
    }

    #[test]
    fn metadata_aligns_regions_down_to_sdk_alignment() {
        let parameters = HvfGicParameters {
            distributor_size: 0x1_1000,
            redistributor_region_size: 0x2_1000,
            ..default_parameters()
        };

        let metadata =
            metadata_from_parameters(parameters).expect("unaligned sizes should align bases down");

        assert_eq!(metadata.distributor.base, 0x3ffe_0000);
        assert_eq!(metadata.distributor.end_exclusive(), 0x3fff_1000);
        assert_eq!(metadata.redistributor.region.base, 0x3ffb_0000);
        assert_eq!(metadata.redistributor.region.end_exclusive(), 0x3ffd_1000);
    }

    #[test]
    fn metadata_rejects_zero_sizes_before_config_creation() {
        let api = FakeGicApi::new(HvfGicParameters {
            distributor_size: 0,
            ..default_parameters()
        });

        assert_eq!(
            create_gic_with_api(&api, None),
            Err(HvfGicError::InvalidParameter {
                name: "distributor_size",
                value: 0,
            })
        );
        assert!(!api.created_config());
    }

    #[test]
    fn metadata_rejects_non_power_of_two_alignment() {
        let err = metadata_from_parameters(HvfGicParameters {
            redistributor_alignment: 3,
            ..default_parameters()
        })
        .expect_err("non-power-of-two alignment should fail");

        assert_eq!(
            err,
            HvfGicError::InvalidParameter {
                name: "redistributor_base_alignment",
                value: 3,
            }
        );
    }

    #[test]
    fn metadata_rejects_redistributor_size_larger_than_region() {
        let err = metadata_from_parameters(HvfGicParameters {
            redistributor_region_size: 0x1_0000,
            redistributor_size: 0x2_0000,
            ..default_parameters()
        })
        .expect_err("single redistributor larger than total region should fail");

        assert_eq!(
            err,
            HvfGicError::InvalidParameter {
                name: "redistributor_size",
                value: 0x2_0000,
            }
        );
    }

    #[test]
    fn metadata_rejects_non_spi_interrupt_range_base() {
        let err = metadata_from_parameters(HvfGicParameters {
            spi_interrupt_range: HvfGicInterruptRange { base: 31, count: 1 },
            ..default_parameters()
        })
        .expect_err("SPI range base below the first SPI INTID should fail");

        assert_eq!(
            err,
            HvfGicError::InvalidParameter {
                name: "spi_interrupt_range.base",
                value: 31,
            }
        );
    }

    #[test]
    fn metadata_rejects_zero_spi_interrupt_count_before_config_creation() {
        let api = FakeGicApi::new(HvfGicParameters {
            spi_interrupt_range: HvfGicInterruptRange { base: 32, count: 0 },
            ..default_parameters()
        });

        assert_eq!(
            create_gic_with_api(&api, None),
            Err(HvfGicError::InvalidParameter {
                name: "spi_interrupt_range.count",
                value: 0,
            })
        );
        assert!(!api.created_config());
    }

    #[test]
    fn metadata_rejects_spi_interrupt_range_overflow() {
        let err = metadata_from_parameters(HvfGicParameters {
            spi_interrupt_range: HvfGicInterruptRange {
                base: u32::MAX,
                count: 2,
            },
            ..default_parameters()
        })
        .expect_err("SPI range end should not overflow");

        assert_eq!(
            err,
            HvfGicError::InvalidParameter {
                name: "spi_interrupt_range.end_exclusive",
                value: u64::from(u32::MAX) + 2,
            }
        );
    }

    #[test]
    fn metadata_rejects_non_ppi_timer_interrupts_before_config_creation() {
        let api = FakeGicApi::new(HvfGicParameters {
            timer_interrupts: HvfGicTimerInterrupts {
                el1_virtual_timer_intid: 15,
                ..default_parameters().timer_interrupts
            },
            ..default_parameters()
        });

        assert_eq!(
            create_gic_with_api(&api, None),
            Err(HvfGicError::InvalidParameter {
                name: "el1_virtual_timer_intid",
                value: 15,
            })
        );
        assert!(!api.created_config());
    }

    #[test]
    fn metadata_rejects_timer_interrupts_in_spi_range() {
        let err = metadata_from_parameters(HvfGicParameters {
            timer_interrupts: HvfGicTimerInterrupts {
                el1_physical_timer_intid: 32,
                ..default_parameters().timer_interrupts
            },
            ..default_parameters()
        })
        .expect_err("timer interrupt INTIDs should be PPIs");

        assert_eq!(
            err,
            HvfGicError::InvalidParameter {
                name: "el1_physical_timer_intid",
                value: 32,
            }
        );
    }

    #[test]
    fn metadata_rejects_duplicate_timer_interrupts() {
        let err = metadata_from_parameters(HvfGicParameters {
            timer_interrupts: HvfGicTimerInterrupts {
                el1_virtual_timer_intid: 27,
                el1_physical_timer_intid: 27,
            },
            ..default_parameters()
        })
        .expect_err("timer interrupt INTIDs should be distinct");

        assert_eq!(
            err,
            HvfGicError::InvalidParameter {
                name: "timer_interrupts",
                value: 27,
            }
        );
    }

    #[test]
    fn metadata_rejects_region_that_cannot_fit_below_mmio32() {
        let err = metadata_from_parameters(HvfGicParameters {
            redistributor_region_size: 0x4000_0000,
            ..default_parameters()
        })
        .expect_err("redistributor region should not fit below distributor");

        assert_eq!(
            err,
            HvfGicError::AddressUnderflow {
                region: "redistributor",
                limit: 0x3fff_0000,
                size: 0x4000_0000,
            }
        );
    }

    #[test]
    fn create_gic_configures_hvf_before_returning_metadata() {
        let api = FakeGicApi::default();

        let metadata = create_gic_with_api(&api, None).expect("GIC should be created");

        assert_eq!(metadata.distributor.base, 0x3fff_0000);
        assert_eq!(
            api.calls(),
            vec![
                "hv_gic_get_distributor_size",
                "hv_gic_get_distributor_base_alignment",
                "hv_gic_get_redistributor_region_size",
                "hv_gic_get_redistributor_size",
                "hv_gic_get_redistributor_base_alignment",
                "hv_gic_get_spi_interrupt_range",
                "hv_gic_get_intid",
                "hv_gic_get_intid",
                "hv_gic_config_create",
                "hv_gic_config_set_distributor_base",
                "hv_gic_config_set_redistributor_base",
                "hv_gic_create",
                "os_release",
            ]
        );
        assert_eq!(api.released_configs(), vec![1]);
    }

    #[test]
    fn create_gic_with_msi_configures_exact_tail_range_and_frame() {
        let api = FakeGicApi::new(parameters_with_msi(1));

        let metadata = create_gic_with_api(&api, Some(msi_configuration(8)))
            .expect("valid MSI GIC should be created");

        assert_eq!(
            metadata.spi_interrupt_range,
            HvfGicInterruptRange {
                base: 32,
                count: 88,
            }
        );
        assert_eq!(
            metadata
                .msi
                .expect("MSI metadata should exist")
                .interrupt_range,
            HvfGicInterruptRange {
                base: 120,
                count: 8,
            }
        );
        assert_eq!(api.msi_region_bases(), vec![0x3ffc_0000]);
        assert_eq!(
            api.msi_ranges(),
            vec![HvfGicInterruptRange {
                base: 120,
                count: 8,
            }]
        );
        assert_eq!(
            api.calls(),
            vec![
                "hv_gic_get_msi_region_size",
                "hv_gic_get_msi_region_base_alignment",
                "hv_gic_get_distributor_size",
                "hv_gic_get_distributor_base_alignment",
                "hv_gic_get_redistributor_region_size",
                "hv_gic_get_redistributor_size",
                "hv_gic_get_redistributor_base_alignment",
                "hv_gic_get_spi_interrupt_range",
                "hv_gic_get_intid",
                "hv_gic_get_intid",
                "hv_gic_config_create",
                "hv_gic_config_set_distributor_base",
                "hv_gic_config_set_redistributor_base",
                "hv_gic_config_set_msi_region_base",
                "hv_gic_config_set_msi_interrupt_range",
                "hv_gic_create",
                "os_release",
            ]
        );
        assert_eq!(api.released_configs(), vec![1]);
    }

    #[test]
    fn invalid_msi_demand_fails_before_config_creation() {
        let api = FakeGicApi::new(parameters_with_msi(1));

        assert_eq!(
            create_gic_with_api(&api, Some(msi_configuration(96))),
            Err(HvfGicError::InsufficientMsiInterruptCapacity)
        );
        assert!(!api.created_config());
        assert!(api.msi_region_bases().is_empty());
        assert!(api.msi_ranges().is_empty());
    }

    #[test]
    fn msi_configuration_failure_releases_config_without_creating_gic() {
        let api = FakeGicApi::new(parameters_with_msi(1))
            .with_failure("hv_gic_config_set_msi_interrupt_range");

        assert_eq!(
            create_gic_with_api(&api, Some(msi_configuration(8))),
            Err(HvfGicError::Backend(BackendError::Hypervisor(
                "injected hv_gic_config_set_msi_interrupt_range failure".to_string()
            )))
        );
        assert_eq!(api.msi_region_bases(), vec![0x3ffc_0000]);
        assert!(api.msi_ranges().is_empty());
        assert!(!api.calls().contains(&"hv_gic_create"));
        assert_eq!(api.released_configs(), vec![1]);
    }

    #[test]
    fn create_gic_releases_config_after_set_failure() {
        let api = FakeGicApi::default().with_failure("hv_gic_config_set_redistributor_base");

        assert_eq!(
            create_gic_with_api(&api, None),
            Err(HvfGicError::Backend(BackendError::Hypervisor(
                "injected hv_gic_config_set_redistributor_base failure".to_string()
            )))
        );
        assert_eq!(
            api.calls(),
            vec![
                "hv_gic_get_distributor_size",
                "hv_gic_get_distributor_base_alignment",
                "hv_gic_get_redistributor_region_size",
                "hv_gic_get_redistributor_size",
                "hv_gic_get_redistributor_base_alignment",
                "hv_gic_get_spi_interrupt_range",
                "hv_gic_get_intid",
                "hv_gic_get_intid",
                "hv_gic_config_create",
                "hv_gic_config_set_distributor_base",
                "hv_gic_config_set_redistributor_base",
                "os_release",
            ]
        );
    }

    #[test]
    fn create_gic_releases_config_after_create_failure() {
        let api = FakeGicApi::default().with_failure("hv_gic_create");

        assert_eq!(
            create_gic_with_api(&api, None),
            Err(HvfGicError::Backend(BackendError::Hypervisor(
                "injected hv_gic_create failure".to_string()
            )))
        );
        assert_eq!(
            api.calls(),
            vec![
                "hv_gic_get_distributor_size",
                "hv_gic_get_distributor_base_alignment",
                "hv_gic_get_redistributor_region_size",
                "hv_gic_get_redistributor_size",
                "hv_gic_get_redistributor_base_alignment",
                "hv_gic_get_spi_interrupt_range",
                "hv_gic_get_intid",
                "hv_gic_get_intid",
                "hv_gic_config_create",
                "hv_gic_config_set_distributor_base",
                "hv_gic_config_set_redistributor_base",
                "hv_gic_create",
                "os_release",
            ]
        );
        assert_eq!(api.released_configs(), vec![1]);
    }

    #[test]
    fn config_guard_releases_on_drop() {
        let api = FakeGicApi::default();

        {
            let _guard = GicConfigGuard::new(&api).expect("config should be created");
        }

        assert_eq!(api.calls(), vec!["hv_gic_config_create", "os_release"]);
    }

    fn default_parameters() -> HvfGicParameters {
        HvfGicParameters {
            distributor_size: DIST_SIZE,
            distributor_alignment: ALIGNMENT,
            redistributor_region_size: REDIST_REGION_SIZE,
            redistributor_size: REDIST_SIZE,
            redistributor_alignment: ALIGNMENT,
            spi_interrupt_range: HvfGicInterruptRange {
                base: 32,
                count: 96,
            },
            timer_interrupts: HvfGicTimerInterrupts {
                el1_virtual_timer_intid: 27,
                el1_physical_timer_intid: 30,
            },
            msi: None,
        }
    }

    fn msi_configuration(interrupt_count: u32) -> HvfGicMsiConfiguration {
        HvfGicMsiConfiguration::new(
            NonZeroU32::new(interrupt_count).expect("test MSI count should be nonzero"),
        )
    }

    fn parameters_with_msi(interrupt_count: u32) -> HvfGicParameters {
        HvfGicParameters {
            msi: Some(HvfGicMsiParameters {
                region_size: 0x1_0000,
                region_alignment: 0x1_0000,
                interrupt_count: NonZeroU32::new(interrupt_count)
                    .expect("test MSI count should be nonzero"),
            }),
            ..default_parameters()
        }
    }

    fn line(value: u32) -> GuestInterruptLine {
        GuestInterruptLine::new(value).expect("test interrupt line should be valid")
    }

    fn fake_icc_value(register: u16) -> u64 {
        0xa5a5_0000_0000_0000 | u64::from(register)
    }

    fn fake_icc_restore_state() -> HvfArm64GicIccRegisterState {
        HvfArm64GicIccRegisterState::new(
            ARM64_GIC_EL1_ICC_REGISTERS.map(|register| fake_icc_value(register.raw())),
        )
    }

    fn expected_icc_restore_writes(
        vcpu: crate::ffi::HvVcpu,
    ) -> Vec<(crate::ffi::HvVcpu, HvfArm64GicIccRegister, u64)> {
        ARM64_GIC_EL1_ICC_REGISTERS
            .into_iter()
            .filter(|register| *register != HvfArm64GicIccRegister::RprEl1)
            .map(|register| (vcpu, register, fake_icc_value(register.raw())))
            .collect()
    }

    #[derive(Debug, Clone, Default)]
    struct FakeGicIccRegisterApi {
        state: Arc<Mutex<FakeGicIccRegisterApiState>>,
    }

    #[derive(Debug, Default)]
    struct FakeGicIccRegisterApiState {
        calls: Vec<(crate::ffi::HvVcpu, HvfArm64GicIccRegister)>,
        fail_next_register: Option<HvfArm64GicIccRegister>,
        override_rpr_value: Option<u64>,
    }

    impl FakeGicIccRegisterApi {
        fn with_failure(self, register: HvfArm64GicIccRegister) -> Self {
            self.state
                .lock()
                .expect("fake GIC ICC API should be lockable")
                .fail_next_register = Some(register);
            self
        }

        fn with_rpr_value(self, value: u64) -> Self {
            self.state
                .lock()
                .expect("fake GIC ICC API should be lockable")
                .override_rpr_value = Some(value);
            self
        }

        fn calls(&self) -> Vec<(crate::ffi::HvVcpu, HvfArm64GicIccRegister)> {
            self.state
                .lock()
                .expect("fake GIC ICC API should be lockable")
                .calls
                .clone()
        }
    }

    impl HvfGicIccRegisterApi for FakeGicIccRegisterApi {
        fn get_icc_reg(
            &self,
            vcpu: crate::ffi::HvVcpu,
            register: HvfArm64GicIccRegister,
        ) -> Result<u64, BackendError> {
            let (should_fail, override_rpr_value) = {
                let mut state = self
                    .state
                    .lock()
                    .expect("fake GIC ICC API should be lockable");
                state.calls.push((vcpu, register));
                if state.fail_next_register == Some(register) {
                    state.fail_next_register = None;
                    (true, state.override_rpr_value)
                } else {
                    (false, state.override_rpr_value)
                }
            };

            if should_fail {
                Err(BackendError::Hypervisor(format!(
                    "injected ICC register 0x{:x} failure",
                    register.raw()
                )))
            } else if register == HvfArm64GicIccRegister::RprEl1 {
                Ok(override_rpr_value.unwrap_or_else(|| fake_icc_value(register.raw())))
            } else {
                Ok(fake_icc_value(register.raw()))
            }
        }
    }

    #[derive(Debug, Clone, Default)]
    struct FakeGicIccRegisterWriteApi {
        state: Arc<Mutex<FakeGicIccRegisterWriteApiState>>,
    }

    #[derive(Debug, Default)]
    struct FakeGicIccRegisterWriteApiState {
        calls: Vec<(crate::ffi::HvVcpu, HvfArm64GicIccRegister, u64)>,
        fail_next_register: Option<HvfArm64GicIccRegister>,
    }

    impl FakeGicIccRegisterWriteApi {
        fn with_failure(self, register: HvfArm64GicIccRegister) -> Self {
            self.state
                .lock()
                .expect("fake GIC ICC write API should be lockable")
                .fail_next_register = Some(register);
            self
        }

        fn calls(&self) -> Vec<(crate::ffi::HvVcpu, HvfArm64GicIccRegister, u64)> {
            self.state
                .lock()
                .expect("fake GIC ICC write API should be lockable")
                .calls
                .clone()
        }
    }

    impl HvfGicIccRegisterWriteApi for FakeGicIccRegisterWriteApi {
        fn set_icc_reg(
            &self,
            vcpu: crate::ffi::HvVcpu,
            register: HvfArm64GicIccRegister,
            value: u64,
        ) -> Result<(), BackendError> {
            let should_fail = {
                let mut state = self
                    .state
                    .lock()
                    .expect("fake GIC ICC write API should be lockable");
                state.calls.push((vcpu, register, value));
                if state.fail_next_register == Some(register) {
                    state.fail_next_register = None;
                    true
                } else {
                    false
                }
            };

            if should_fail {
                Err(BackendError::InvalidState("injected ICC setter failure"))
            } else {
                Ok(())
            }
        }
    }

    #[derive(Debug, Clone)]
    struct FakeGicStateApi {
        state: Arc<Mutex<FakeGicStateApiState>>,
    }

    impl FakeGicStateApi {
        fn new(bytes: Vec<u8>) -> Self {
            let reported_size = bytes.len();
            Self {
                state: Arc::new(Mutex::new(FakeGicStateApiState {
                    calls: Vec::new(),
                    released_states: Vec::new(),
                    bytes,
                    reported_size,
                    failure: None,
                    panic: None,
                })),
            }
        }

        fn with_failure(self, failure: &'static str) -> Self {
            self.state
                .lock()
                .expect("fake GIC state API should be lockable")
                .failure = Some(failure);
            self
        }

        fn with_panic(self, panic: &'static str) -> Self {
            self.state
                .lock()
                .expect("fake GIC state API should be lockable")
                .panic = Some(panic);
            self
        }

        fn with_reported_size(self, reported_size: usize) -> Self {
            self.state
                .lock()
                .expect("fake GIC state API should be lockable")
                .reported_size = reported_size;
            self
        }

        fn calls(&self) -> Vec<&'static str> {
            self.state
                .lock()
                .expect("fake GIC state API should be lockable")
                .calls
                .clone()
        }

        fn released_states(&self) -> Vec<u64> {
            self.state
                .lock()
                .expect("fake GIC state API should be lockable")
                .released_states
                .clone()
        }

        fn record(&self, call: &'static str) -> Result<(), HvfGicError> {
            let (failure, should_panic) = {
                let mut state = self
                    .state
                    .lock()
                    .expect("fake GIC state API should be lockable");
                state.calls.push(call);
                (state.failure == Some(call), state.panic == Some(call))
            };

            assert!(!should_panic, "injected {call} panic");
            if failure {
                if call == "state_create" {
                    Err(HvfGicError::StateCreateFailed)
                } else {
                    Err(HvfGicError::Backend(BackendError::Hypervisor(format!(
                        "injected {call} failure"
                    ))))
                }
            } else {
                Ok(())
            }
        }
    }

    impl HvfGicStateApi for FakeGicStateApi {
        type State = u64;

        fn create_state(&self) -> Result<Self::State, HvfGicError> {
            self.record("state_create")?;
            Ok(7)
        }

        fn state_size(&self, _: &Self::State) -> Result<usize, HvfGicError> {
            self.record("state_size")?;
            Ok(self
                .state
                .lock()
                .expect("fake GIC state API should be lockable")
                .reported_size)
        }

        fn copy_state(&self, _: &Self::State, data: &mut [u8]) -> Result<(), HvfGicError> {
            self.record("state_data")?;
            let state = self
                .state
                .lock()
                .expect("fake GIC state API should be lockable");
            assert_eq!(data.len(), state.bytes.len());
            data.copy_from_slice(&state.bytes);
            Ok(())
        }

        fn release_state(&self, state_handle: Self::State) {
            let mut state = self
                .state
                .lock()
                .expect("fake GIC state API should be lockable");
            state.calls.push("os_release");
            state.released_states.push(state_handle);
        }
    }

    #[derive(Debug)]
    struct FakeGicStateApiState {
        calls: Vec<&'static str>,
        released_states: Vec<u64>,
        bytes: Vec<u8>,
        reported_size: usize,
        failure: Option<&'static str>,
        panic: Option<&'static str>,
    }

    #[derive(Debug, Clone, Default)]
    struct FakeGicStateRestoreApi {
        state: Arc<Mutex<FakeGicStateRestoreApiState>>,
    }

    impl FakeGicStateRestoreApi {
        fn with_failure(self) -> Self {
            self.state
                .lock()
                .expect("fake GIC state restore API should be lockable")
                .fail = true;
            self
        }

        fn with_panic(self) -> Self {
            self.state
                .lock()
                .expect("fake GIC state restore API should be lockable")
                .panic = true;
            self
        }

        fn calls(&self) -> Vec<(usize, usize)> {
            self.state
                .lock()
                .expect("fake GIC state restore API should be lockable")
                .calls
                .clone()
        }
    }

    impl HvfGicStateRestoreApi for FakeGicStateRestoreApi {
        fn restore(&self, data: &[u8]) -> Result<(), HvfGicError> {
            let (fail, should_panic) = {
                let mut state = self
                    .state
                    .lock()
                    .expect("fake GIC state restore API should be lockable");
                state.calls.push((data.as_ptr() as usize, data.len()));
                (state.fail, state.panic)
            };

            assert!(!should_panic, "injected GIC state restore panic");
            if fail {
                Err(HvfGicError::Backend(BackendError::Hypervisor(
                    "injected GIC state restore failure".to_string(),
                )))
            } else {
                Ok(())
            }
        }
    }

    #[derive(Debug, Default)]
    struct FakeGicStateRestoreApiState {
        calls: Vec<(usize, usize)>,
        fail: bool,
        panic: bool,
    }

    #[derive(Debug, Clone)]
    struct FakeGicApi {
        parameters: HvfGicParameters,
        state: Arc<Mutex<FakeGicApiState>>,
    }

    impl Default for FakeGicApi {
        fn default() -> Self {
            Self::new(default_parameters())
        }
    }

    impl FakeGicApi {
        fn new(parameters: HvfGicParameters) -> Self {
            Self {
                parameters,
                state: Arc::new(Mutex::new(FakeGicApiState {
                    calls: Vec::new(),
                    next_config: 1,
                    released_configs: Vec::new(),
                    spi_signals: Vec::new(),
                    msi_region_bases: Vec::new(),
                    msi_ranges: Vec::new(),
                    msi_signals: Vec::new(),
                    ppi_pending_writes: Vec::new(),
                    failure: None,
                    created_config: false,
                })),
            }
        }

        fn with_failure(self, failure: &'static str) -> Self {
            self.state
                .lock()
                .expect("fake GIC API state should be lockable")
                .failure = Some(failure);
            self
        }

        fn calls(&self) -> Vec<&'static str> {
            self.state
                .lock()
                .expect("fake GIC API state should be lockable")
                .calls
                .clone()
        }

        fn released_configs(&self) -> Vec<u64> {
            self.state
                .lock()
                .expect("fake GIC API state should be lockable")
                .released_configs
                .clone()
        }

        fn spi_signals(&self) -> Vec<(u32, bool)> {
            self.state
                .lock()
                .expect("fake GIC API state should be lockable")
                .spi_signals
                .clone()
        }

        fn msi_region_bases(&self) -> Vec<u64> {
            self.state
                .lock()
                .expect("fake GIC API state should be lockable")
                .msi_region_bases
                .clone()
        }

        fn msi_ranges(&self) -> Vec<HvfGicInterruptRange> {
            self.state
                .lock()
                .expect("fake GIC API state should be lockable")
                .msi_ranges
                .clone()
        }

        fn msi_signals(&self) -> Vec<(u64, u32)> {
            self.state
                .lock()
                .expect("fake GIC API state should be lockable")
                .msi_signals
                .clone()
        }

        fn ppi_pending_writes(&self) -> Vec<(crate::ffi::HvVcpu, u32, u64)> {
            self.state
                .lock()
                .expect("fake GIC API state should be lockable")
                .ppi_pending_writes
                .clone()
        }

        fn created_config(&self) -> bool {
            self.state
                .lock()
                .expect("fake GIC API state should be lockable")
                .created_config
        }

        fn record(&self, call: &'static str) -> Result<(), HvfGicError> {
            let mut state = self
                .state
                .lock()
                .expect("fake GIC API state should be lockable");
            state.calls.push(call);

            if state.failure == Some(call) {
                Err(HvfGicError::Backend(BackendError::Hypervisor(format!(
                    "injected {call} failure"
                ))))
            } else {
                Ok(())
            }
        }
    }

    impl super::HvfGicSpiSignalApi for FakeGicApi {
        fn set_spi(&self, intid: u32, level: bool) -> Result<(), HvfGicError> {
            let mut state = self
                .state
                .lock()
                .expect("fake GIC API state should be lockable");
            state.calls.push("hv_gic_set_spi");

            if state.failure == Some("hv_gic_set_spi") {
                Err(HvfGicError::Backend(BackendError::Hypervisor(
                    "injected hv_gic_set_spi failure".to_string(),
                )))
            } else {
                state.spi_signals.push((intid, level));
                Ok(())
            }
        }
    }

    impl super::HvfGicMsiSignalApi for FakeGicApi {
        fn send_msi(&self, address: u64, intid: u32) -> Result<(), HvfGicError> {
            let mut state = self
                .state
                .lock()
                .expect("fake GIC API state should be lockable");
            state.calls.push("hv_gic_send_msi");

            if state.failure == Some("hv_gic_send_msi") {
                Err(HvfGicError::Backend(BackendError::Hypervisor(
                    "injected hv_gic_send_msi failure".to_string(),
                )))
            } else {
                state.msi_signals.push((address, intid));
                Ok(())
            }
        }
    }

    impl super::HvfGicPpiPendingApi for FakeGicApi {
        fn set_redistributor_reg(
            &self,
            vcpu: crate::ffi::HvVcpu,
            reg: u32,
            value: u64,
        ) -> Result<(), HvfGicError> {
            let mut state = self
                .state
                .lock()
                .expect("fake GIC API state should be lockable");
            state.calls.push("hv_gic_set_redistributor_reg");

            if state.failure == Some("hv_gic_set_redistributor_reg") {
                Err(HvfGicError::Backend(BackendError::Hypervisor(
                    "injected hv_gic_set_redistributor_reg failure".to_string(),
                )))
            } else {
                state.ppi_pending_writes.push((vcpu, reg, value));
                Ok(())
            }
        }
    }

    #[derive(Debug)]
    struct PanicGicApi;

    impl super::HvfGicSpiSignalApi for PanicGicApi {
        fn set_spi(&self, _: u32, _: bool) -> Result<(), HvfGicError> {
            panic!("injected hv_gic_set_spi panic");
        }
    }

    impl HvfGicApi for FakeGicApi {
        type Config = u64;

        fn distributor_size(&self) -> Result<u64, HvfGicError> {
            self.record("hv_gic_get_distributor_size")?;
            Ok(self.parameters.distributor_size)
        }

        fn distributor_alignment(&self) -> Result<u64, HvfGicError> {
            self.record("hv_gic_get_distributor_base_alignment")?;
            Ok(self.parameters.distributor_alignment)
        }

        fn redistributor_region_size(&self) -> Result<u64, HvfGicError> {
            self.record("hv_gic_get_redistributor_region_size")?;
            Ok(self.parameters.redistributor_region_size)
        }

        fn redistributor_size(&self) -> Result<u64, HvfGicError> {
            self.record("hv_gic_get_redistributor_size")?;
            Ok(self.parameters.redistributor_size)
        }

        fn redistributor_alignment(&self) -> Result<u64, HvfGicError> {
            self.record("hv_gic_get_redistributor_base_alignment")?;
            Ok(self.parameters.redistributor_alignment)
        }

        fn msi_region_size(&self) -> Result<u64, HvfGicError> {
            self.record("hv_gic_get_msi_region_size")?;
            self.parameters
                .msi
                .map(|parameters| parameters.region_size)
                .ok_or(HvfGicError::InvalidMsiParameter {
                    name: "region_size",
                })
        }

        fn msi_region_alignment(&self) -> Result<u64, HvfGicError> {
            self.record("hv_gic_get_msi_region_base_alignment")?;
            self.parameters
                .msi
                .map(|parameters| parameters.region_alignment)
                .ok_or(HvfGicError::InvalidMsiParameter {
                    name: "region_base_alignment",
                })
        }

        fn spi_interrupt_range(&self) -> Result<HvfGicInterruptRange, HvfGicError> {
            self.record("hv_gic_get_spi_interrupt_range")?;
            Ok(self.parameters.spi_interrupt_range)
        }

        fn intid(&self, interrupt: u16) -> Result<u32, HvfGicError> {
            self.record("hv_gic_get_intid")?;
            match interrupt {
                HV_GIC_INT_EL1_VIRTUAL_TIMER => {
                    Ok(self.parameters.timer_interrupts.el1_virtual_timer_intid)
                }
                HV_GIC_INT_EL1_PHYSICAL_TIMER => {
                    Ok(self.parameters.timer_interrupts.el1_physical_timer_intid)
                }
                _ => Err(HvfGicError::InvalidParameter {
                    name: "interrupt",
                    value: u64::from(interrupt),
                }),
            }
        }

        fn create_config(&self) -> Result<Self::Config, HvfGicError> {
            self.record("hv_gic_config_create")?;
            let mut state = self
                .state
                .lock()
                .expect("fake GIC API state should be lockable");
            let config = state.next_config;
            state.next_config += 1;
            state.created_config = true;
            Ok(config)
        }

        fn set_distributor_base(&self, _: &mut Self::Config, _: u64) -> Result<(), HvfGicError> {
            self.record("hv_gic_config_set_distributor_base")
        }

        fn set_redistributor_base(&self, _: &mut Self::Config, _: u64) -> Result<(), HvfGicError> {
            self.record("hv_gic_config_set_redistributor_base")
        }

        fn set_msi_region_base(&self, _: &mut Self::Config, base: u64) -> Result<(), HvfGicError> {
            self.record("hv_gic_config_set_msi_region_base")?;
            self.state
                .lock()
                .expect("fake GIC API state should be lockable")
                .msi_region_bases
                .push(base);
            Ok(())
        }

        fn set_msi_interrupt_range(
            &self,
            _: &mut Self::Config,
            range: HvfGicInterruptRange,
        ) -> Result<(), HvfGicError> {
            self.record("hv_gic_config_set_msi_interrupt_range")?;
            self.state
                .lock()
                .expect("fake GIC API state should be lockable")
                .msi_ranges
                .push(range);
            Ok(())
        }

        fn create_gic(&self, _: &Self::Config) -> Result<(), HvfGicError> {
            self.record("hv_gic_create")
        }

        fn release_config(&self, config: Self::Config) {
            let mut state = self
                .state
                .lock()
                .expect("fake GIC API state should be lockable");
            state.calls.push("os_release");
            state.released_configs.push(config);
        }
    }

    #[derive(Debug)]
    struct FakeGicApiState {
        calls: Vec<&'static str>,
        next_config: u64,
        released_configs: Vec<u64>,
        spi_signals: Vec<(u32, bool)>,
        msi_region_bases: Vec<u64>,
        msi_ranges: Vec<HvfGicInterruptRange>,
        msi_signals: Vec<(u64, u32)>,
        ppi_pending_writes: Vec<(crate::ffi::HvVcpu, u32, u64)>,
        failure: Option<&'static str>,
        created_config: bool,
    }
}
