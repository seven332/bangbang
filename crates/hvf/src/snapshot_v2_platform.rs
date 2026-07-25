//! Unpublished native-v2 multi-vCPU HVF platform reconstruction.

use std::fmt;
use std::io::{Seek, Write};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

use device_tree::{DeviceTree, Node};

use bangbang_runtime::memory::{
    GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryRange, aarch64,
};
use bangbang_runtime::mmio::{MmioDispatcher, MmioRegionId};
use bangbang_runtime::pvtime::{
    ARM64_PVTIME_STOLEN_TIME_OFFSET, ARM64_PVTIME_STRUCTURE_SIZE, Arm64PvTimeLayout,
    Arm64PvTimeStAbi,
};
use bangbang_runtime::rtc::{RTC_MMIO_DEVICE_WINDOW_SIZE, RtcMmioLayout};
use bangbang_runtime::serial::{SERIAL_MMIO_DEVICE_WINDOW_SIZE, SharedSerialOutput};
use bangbang_runtime::snapshot_memory_v2::{
    SnapshotV2MemoryBinding, write_snapshot_v2_memory_image,
};
use bangbang_runtime::startup::{
    Arm64BootResourceError, Arm64BootRtcDevice, Arm64BootSerialDevice, Arm64BootSerialDeviceConfig,
    Arm64BootVmClockDevice, Arm64BootVmGenIdDevice, PrepareArm64SnapshotTimeIdentityError,
    prepare_arm64_snapshot_time_identity, register_arm64_boot_rtc_mmio,
    register_arm64_boot_serial_mmio, replace_arm64_boot_vmgenid,
};
use bangbang_runtime::{BackendError, VmBackend};
use crc64::crc64;

use crate::backend::HvfBackend;
use crate::coordinator::{HvfVcpuRunControl, HvfVcpuRunCoordinatorError};
use crate::cpu_template::HvfArm64CpuTemplateError;
use crate::dirty::HvfDirtyWriteTrackerStartError;
use crate::gic::{
    HvfGicError, HvfGicInterruptLineAllocator, HvfGicMetadata, HvfGicMsiConfiguration,
    HvfGicSpiSignalError, HvfGicSpiSignaler, HvfInterruptLineAllocationError,
};
use crate::memory::{HvfGuestMemoryMappingError, HvfMemoryPermissions};
use crate::pvtime::HvfArm64PvTimeAccountingConfig;
use crate::runner::{HvfArm64SnapshotV2VcpuRestore, HvfVcpuRunStepOutcome, HvfVcpuRunnerError};
use crate::session_vcpu::{
    HvfArm64BootVcpuError, HvfArm64BootVcpuSession, HvfArm64StablePausedTopologyCaptureError,
    HvfArm64StablePausedTopologyImportError,
};
use crate::snapshot_bundle::HvfSnapshotV1CompatibilityState;
use crate::snapshot_v2::{
    HvfSnapshotV2GlobalState, HvfSnapshotV2MachineState, HvfSnapshotV2PlatformState,
    HvfSnapshotV2TimeState, HvfSnapshotV2VcpuState,
};
use crate::startup::{
    HvfArm64BootSnapshotV2CaptureError, HvfArm64BootSnapshotV2CaptureStage,
    HvfArm64BootVmClockRestoreError, HvfArm64BootVmGenIdRestoreError,
    capture_hvf_snapshot_v2_time_state, replace_vmgenid_and_signal_with,
    update_vmclock_and_signal_with,
};
use crate::topology::{HvfVcpuTopology, HvfVcpuTopologyError};
use crate::vcpu::HvfArm64VcpuIdentificationRegisterState;

const REDACTED: &str = "<redacted>";
const PROCESS_SERIAL_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_2000);
const PROCESS_SERIAL_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(20);
const PROCESS_RTC_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_1000);
const PROCESS_RTC_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(10);

/// Closed fresh-output shell accepted by native-v2 process reconstruction.
pub struct HvfSnapshotV2DefaultProcessShell {
    serial_output: SharedSerialOutput,
}

impl HvfSnapshotV2DefaultProcessShell {
    /// Bind one fresh destination output to the canonical process UART.
    pub const fn new(serial_output: SharedSerialOutput) -> Self {
        Self { serial_output }
    }
}

impl fmt::Debug for HvfSnapshotV2DefaultProcessShell {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2DefaultProcessShell")
            .field("profile", &"default-uart")
            .field("output", &REDACTED)
            .finish()
    }
}

/// Ordered reconstruction stage for the unpublished native-v2 platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfSnapshotV2PlatformRestoreStage {
    /// Validate supplied memory, FDT identity, cache identity, and cleanup storage.
    Preflight,
    /// Validate and install the exact fresh destination process shell.
    ProcessShell,
    /// Create the empty Hypervisor.framework VM.
    Vm,
    /// Register guest memory and optional dirty tracking.
    Memory,
    /// Create and exactly validate the destination GIC.
    Gic,
    /// Create and validate the complete never-run vCPU topology.
    Topology,
    /// Verify that the complete created topology has the prepared identities.
    TopologyIdentity,
    /// Replay retained effective CPU-template targets.
    CpuTemplate,
    /// Validate one destination member's common processor identity.
    Compatibility { index: usize },
    /// Restore the singular VM-global GIC state.
    GlobalGic,
    /// Restore one complete per-vCPU component.
    Vcpu { index: usize },
    /// Install the fresh destination-time PL031 MMIO handler.
    Rtc,
    /// Configure one never-run vCPU's restored PVTime accumulator.
    PvTime { index: usize },
    /// Validate all runners and identity notification lines before mutation.
    TimeIdentityPreflight,
    /// Replace and notify the destination VMGenID.
    VmGenId,
    /// Atomically update and notify the destination VMClock.
    VmClock,
    /// Import offline/runnable/suspended state into a coordinator born paused.
    Lifecycle,
    /// Publish the completed paused owner without exposing a raw topology.
    Publication,
}

impl fmt::Display for HvfSnapshotV2PlatformRestoreStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preflight => f.write_str("preflight"),
            Self::ProcessShell => f.write_str("default process shell"),
            Self::Vm => f.write_str("VM creation"),
            Self::Memory => f.write_str("guest memory"),
            Self::Gic => f.write_str("GIC creation"),
            Self::Topology => f.write_str("vCPU topology"),
            Self::TopologyIdentity => f.write_str("vCPU topology identity"),
            Self::CpuTemplate => f.write_str("CPU-template replay"),
            Self::Compatibility { index } => {
                write!(f, "vCPU {index} compatibility")
            }
            Self::GlobalGic => f.write_str("global GIC restore"),
            Self::Vcpu { index } => write!(f, "vCPU {index} restore"),
            Self::Rtc => f.write_str("destination PL031 installation"),
            Self::PvTime { index } => write!(f, "vCPU {index} PVTime restore"),
            Self::TimeIdentityPreflight => f.write_str("time/identity preflight"),
            Self::VmGenId => f.write_str("VMGenID replacement and notification"),
            Self::VmClock => f.write_str("VMClock update and notification"),
            Self::Lifecycle => f.write_str("paused lifecycle import"),
            Self::Publication => f.write_str("paused platform publication"),
        }
    }
}

/// Value-free reason that a retained FDT is not the canonical focused process shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfSnapshotV2ProcessFdtMismatch {
    /// The retained FDT does not describe the focused process profile.
    Profile,
    /// The retained FDT cannot be parsed.
    Parse,
    /// The retained FDT has an unexpected root-node inventory.
    RootInventory,
    /// The retained FDT has an unexpected CPU inventory or topology.
    CpuInventory,
    /// The retained FDT memory range does not match the admitted binding.
    Memory,
    /// The retained FDT boot metadata does not match the admitted state.
    Boot,
    /// The retained FDT GIC description does not match the admitted state.
    Gic,
    /// The retained FDT timer description does not match the admitted state.
    Timer,
    /// The retained FDT RTC description does not match the focused shell.
    Rtc,
    /// The retained FDT UART description does not match the focused shell.
    Serial,
    /// The retained FDT VM generation ID description does not match the admitted state.
    VmGenId,
    /// The retained FDT VM clock description does not match the admitted state.
    VmClock,
}

impl fmt::Display for HvfSnapshotV2ProcessFdtMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Profile => "profile",
            Self::Parse => "parse",
            Self::RootInventory => "root inventory",
            Self::CpuInventory => "CPU inventory",
            Self::Memory => "memory",
            Self::Boot => "boot",
            Self::Gic => "GIC",
            Self::Timer => "timer",
            Self::Rtc => "RTC",
            Self::Serial => "serial",
            Self::VmGenId => "VMGenID",
            Self::VmClock => "VMClock",
        })
    }
}

/// Typed primary failure retained by a native-v2 platform restore error.
pub enum HvfSnapshotV2PlatformRestoreFailure {
    /// Cleanup evidence could not be reserved before construction.
    Allocation,
    /// Supplied guest-memory ranges differ from the prepared binding.
    MemoryTopology,
    /// FDT verification storage could not be reserved.
    FdtAllocation,
    /// The prepared FDT range could not be read from supplied memory.
    FdtRead(GuestMemoryAccessError),
    /// Supplied memory does not contain the prepared FDT identity.
    FdtIdentity,
    /// The retained FDT is not the exact minimal destination process shell.
    ProcessShellFdt {
        mismatch: HvfSnapshotV2ProcessFdtMismatch,
    },
    /// Deterministic minimal-profile interrupt allocation failed or disagreed.
    ProcessShellInterrupt(HvfInterruptLineAllocationError),
    /// Retained identity lines differ from the minimal serial-first allocation.
    ProcessShellInterruptIdentity,
    /// Fresh destination UART installation failed.
    ProcessShellSerial(Arm64BootResourceError),
    /// Querying the destination cache profile failed.
    CacheQuery(BackendError),
    /// Destination cache facts differ from the prepared compatibility state.
    CacheMismatch,
    /// Portable identity devices disagree with supplied guest memory.
    TimePreparation(PrepareArm64SnapshotTimeIdentityError),
    /// One supplied PVTime record could not be read.
    PvTimeRead(GuestMemoryAccessError),
    /// One supplied PVTime record is invalid or differs from portable state.
    PvTimeMismatch,
    /// Empty VM construction failed.
    CreateVm(BackendError),
    /// Guest-memory registration failed.
    MapMemory(HvfGuestMemoryMappingError),
    /// Dirty-tracking setup requested by the machine state failed.
    DirtyTracking(HvfDirtyWriteTrackerStartError),
    /// GIC construction failed.
    CreateGic(HvfGicError),
    /// The created GIC metadata differs from the prepared graph.
    GicMetadata,
    /// Ordered never-run topology construction failed.
    Topology(HvfVcpuTopologyError),
    /// Destination topology MPIDRs differ from the prepared graph.
    TopologyIdentity,
    /// Retained effective CPU-template replay failed.
    CpuTemplate(HvfArm64CpuTemplateError),
    /// Reading one destination member's compatibility facts failed.
    CompatibilityRead(HvfVcpuRunnerError),
    /// One destination member's compatibility facts differ.
    CompatibilityMismatch,
    /// Singular VM-global GIC restoration failed.
    GlobalGic(HvfVcpuRunnerError),
    /// One complete per-vCPU restoration failed.
    Vcpu(HvfVcpuRunnerError),
    /// Fresh destination PL031 installation failed.
    Rtc(Arm64BootResourceError),
    /// A restored PVTime publisher address overflowed.
    PvTimeAddress,
    /// A restored PVTime atomic publisher could not be created.
    PvTimePublisher(GuestMemoryAccessError),
    /// Mapped guest memory became unavailable while preparing PVTime.
    PvTimeMemory(HvfGuestMemoryMappingError),
    /// One never-run vCPU rejected PVTime configuration.
    PvTime(HvfVcpuRunnerError),
    /// One destination runner was no longer available for identity restore.
    TimeIdentityRunner(HvfVcpuRunnerError),
    /// Identity interrupt signaling could not be prepared.
    TimeIdentitySignaler(HvfGicSpiSignalError),
    /// Mapped guest memory became unavailable before identity mutation.
    TimeIdentityMemory(HvfGuestMemoryMappingError),
    /// VMGenID replacement or notification failed.
    VmGenId(HvfArm64BootVmGenIdRestoreError),
    /// VMClock update or notification failed.
    VmClock(HvfArm64BootVmClockRestoreError),
    /// Stable paused lifecycle import failed and consumed the raw topology.
    Lifecycle(HvfArm64StablePausedTopologyImportError),
}

impl HvfSnapshotV2PlatformRestoreFailure {
    fn category(&self) -> &'static str {
        match self {
            Self::Allocation => "allocation",
            Self::MemoryTopology => "memory topology",
            Self::FdtAllocation => "FDT allocation",
            Self::FdtRead(_) => "FDT read",
            Self::FdtIdentity => "FDT identity",
            Self::ProcessShellFdt { .. } => "default process FDT shell",
            Self::ProcessShellInterrupt(_) => "default process interrupt shell",
            Self::ProcessShellInterruptIdentity => "default process interrupt identity",
            Self::ProcessShellSerial(_) => "default process serial shell",
            Self::CacheQuery(_) => "cache query",
            Self::CacheMismatch => "cache compatibility",
            Self::TimePreparation(_) => "time identity preparation",
            Self::PvTimeRead(_) => "PVTime preflight read",
            Self::PvTimeMismatch => "PVTime preflight agreement",
            Self::CreateVm(_) => "VM creation",
            Self::MapMemory(_) => "memory mapping",
            Self::DirtyTracking(_) => "dirty tracking",
            Self::CreateGic(_) => "GIC creation",
            Self::GicMetadata => "GIC metadata",
            Self::Topology(_) => "topology construction",
            Self::TopologyIdentity => "topology identity",
            Self::CpuTemplate(_) => "CPU-template replay",
            Self::CompatibilityRead(_) => "compatibility read",
            Self::CompatibilityMismatch => "compatibility mismatch",
            Self::GlobalGic(_) => "global GIC restore",
            Self::Vcpu(_) => "vCPU restore",
            Self::Rtc(_) => "PL031 installation",
            Self::PvTimeAddress => "PVTime publisher address",
            Self::PvTimePublisher(_) => "PVTime publisher",
            Self::PvTimeMemory(_) => "PVTime guest memory",
            Self::PvTime(_) => "PVTime restore",
            Self::TimeIdentityRunner(_) => "time identity runner preflight",
            Self::TimeIdentitySignaler(_) => "time identity signaler preflight",
            Self::TimeIdentityMemory(_) => "time identity guest memory",
            Self::VmGenId(_) => "VMGenID restore",
            Self::VmClock(_) => "VMClock restore",
            Self::Lifecycle(_) => "lifecycle import",
        }
    }
}

impl fmt::Debug for HvfSnapshotV2PlatformRestoreFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mismatch = match self {
            Self::ProcessShellFdt { mismatch } => Some(mismatch),
            _ => None,
        };
        f.debug_struct("HvfSnapshotV2PlatformRestoreFailure")
            .field("category", &self.category())
            .field("mismatch", &mismatch)
            .field("source", &REDACTED)
            .finish()
    }
}

impl fmt::Display for HvfSnapshotV2PlatformRestoreFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native-v2 platform {} failed", self.category())
    }
}

impl std::error::Error for HvfSnapshotV2PlatformRestoreFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::FdtRead(source) => Some(source),
            Self::ProcessShellInterrupt(source) => Some(source),
            Self::ProcessShellSerial(source) => Some(source),
            Self::CacheQuery(source) | Self::CreateVm(source) => Some(source),
            Self::TimePreparation(source) => Some(source),
            Self::PvTimeRead(source) => Some(source),
            Self::MapMemory(source) => Some(source),
            Self::DirtyTracking(source) => Some(source),
            Self::CreateGic(source) => Some(source),
            Self::Topology(source) => Some(source),
            Self::CpuTemplate(source) => Some(source),
            Self::CompatibilityRead(source) | Self::GlobalGic(source) | Self::Vcpu(source) => {
                Some(source)
            }
            Self::Rtc(source) => Some(source),
            Self::PvTimePublisher(source) => Some(source),
            Self::PvTimeMemory(source) => Some(source),
            Self::PvTime(source) | Self::TimeIdentityRunner(source) => Some(source),
            Self::TimeIdentitySignaler(source) => Some(source),
            Self::TimeIdentityMemory(source) => Some(source),
            Self::VmGenId(source) => Some(source),
            Self::VmClock(source) => Some(source),
            Self::Lifecycle(source) => Some(source),
            Self::Allocation
            | Self::MemoryTopology
            | Self::FdtAllocation
            | Self::FdtIdentity
            | Self::ProcessShellFdt { .. }
            | Self::ProcessShellInterruptIdentity
            | Self::CacheMismatch
            | Self::PvTimeMismatch
            | Self::GicMetadata
            | Self::TopologyIdentity
            | Self::CompatibilityMismatch
            | Self::PvTimeAddress => None,
        }
    }
}

/// Reverse cleanup operation attempted after reconstruction failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfSnapshotV2PlatformCleanupStage {
    /// Shut down a lifecycle session that was already born paused.
    Session,
    /// Shut down every raw vCPU owner in reverse topology order.
    Topology,
    /// Unmap guest memory and destroy the destination VM.
    Backend,
}

impl fmt::Display for HvfSnapshotV2PlatformCleanupStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session => f.write_str("paused session shutdown"),
            Self::Topology => f.write_str("topology shutdown"),
            Self::Backend => f.write_str("backend destruction"),
        }
    }
}

/// One redacted cleanup failure retained in reverse attempt order.
pub struct HvfSnapshotV2PlatformCleanupFailure {
    stage: HvfSnapshotV2PlatformCleanupStage,
    source: Box<dyn std::error::Error + Send + Sync + 'static>,
}

impl HvfSnapshotV2PlatformCleanupFailure {
    /// Return the failed cleanup operation.
    pub const fn stage(&self) -> HvfSnapshotV2PlatformCleanupStage {
        self.stage
    }

    /// Return the detailed cleanup source to trusted callers.
    pub fn source_error(&self) -> &(dyn std::error::Error + 'static) {
        self.source.as_ref()
    }
}

impl fmt::Debug for HvfSnapshotV2PlatformCleanupFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV2PlatformCleanupFailure")
            .field("stage", &self.stage)
            .field("source", &REDACTED)
            .finish()
    }
}

impl fmt::Display for HvfSnapshotV2PlatformCleanupFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native-v2 cleanup failed at {}", self.stage)
    }
}

impl std::error::Error for HvfSnapshotV2PlatformCleanupFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Primary restore failure plus every later reverse-cleanup failure.
pub struct HvfSnapshotV2PlatformRestoreError {
    stage: HvfSnapshotV2PlatformRestoreStage,
    committed: bool,
    failure: Box<HvfSnapshotV2PlatformRestoreFailure>,
    cleanup: Vec<HvfSnapshotV2PlatformCleanupFailure>,
}

impl HvfSnapshotV2PlatformRestoreError {
    fn new(
        stage: HvfSnapshotV2PlatformRestoreStage,
        failure: HvfSnapshotV2PlatformRestoreFailure,
        cleanup: Vec<HvfSnapshotV2PlatformCleanupFailure>,
    ) -> Self {
        let committed = restore_failure_is_committed(stage, &failure);
        Self {
            stage,
            committed,
            failure: Box::new(failure),
            cleanup,
        }
    }

    /// Return the value-free primary reconstruction stage.
    pub const fn stage(&self) -> HvfSnapshotV2PlatformRestoreStage {
        self.stage
    }

    /// Return whether guest-visible clone identity had already committed.
    pub const fn is_committed(&self) -> bool {
        self.committed
    }

    /// Return the typed primary failure.
    pub fn primary_failure(&self) -> &HvfSnapshotV2PlatformRestoreFailure {
        self.failure.as_ref()
    }

    /// Return reverse cleanup failures in attempt order.
    pub fn cleanup_failures(&self) -> &[HvfSnapshotV2PlatformCleanupFailure] {
        &self.cleanup
    }
}

impl fmt::Debug for HvfSnapshotV2PlatformRestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV2PlatformRestoreError")
            .field("stage", &self.stage)
            .field("committed", &self.committed)
            .field("failure", &self.failure)
            .field("cleanup", &self.cleanup)
            .finish()
    }
}

impl fmt::Display for HvfSnapshotV2PlatformRestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native-v2 platform restore failed at {}", self.stage)?;
        if self.committed {
            f.write_str(" after destination identity committed")?;
        }
        if !self.cleanup.is_empty() {
            write!(
                f,
                "; {} cleanup operation(s) also failed",
                self.cleanup.len()
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for HvfSnapshotV2PlatformRestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.failure.as_ref())
    }
}

fn restore_failure_is_committed(
    stage: HvfSnapshotV2PlatformRestoreStage,
    failure: &HvfSnapshotV2PlatformRestoreFailure,
) -> bool {
    match (stage, failure) {
        (
            HvfSnapshotV2PlatformRestoreStage::VmGenId,
            HvfSnapshotV2PlatformRestoreFailure::VmGenId(source),
        ) => source.is_committed(),
        (
            HvfSnapshotV2PlatformRestoreStage::VmClock,
            HvfSnapshotV2PlatformRestoreFailure::VmClock(_),
        )
        | (
            HvfSnapshotV2PlatformRestoreStage::Lifecycle,
            HvfSnapshotV2PlatformRestoreFailure::Lifecycle(_),
        )
        | (HvfSnapshotV2PlatformRestoreStage::Publication, _) => true,
        _ => false,
    }
}

/// Failure while explicitly shutting down a published native-v2 platform.
#[derive(Debug)]
pub enum HvfSnapshotV2PlatformShutdownError {
    /// One or more vCPU owners did not shut down.
    Vcpu(HvfVcpuRunCoordinatorError),
    /// Guest-memory teardown or VM destruction failed.
    Backend(BackendError),
}

impl fmt::Display for HvfSnapshotV2PlatformShutdownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vcpu(_) => f.write_str("native-v2 vCPU topology shutdown failed"),
            Self::Backend(_) => f.write_str("native-v2 backend shutdown failed"),
        }
    }
}

impl std::error::Error for HvfSnapshotV2PlatformShutdownError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Vcpu(source) => Some(source),
            Self::Backend(source) => Some(source),
        }
    }
}

/// Focused unpublished destination whose first observable lifecycle is paused.
///
/// This owner includes the destination-owned time/identity resources required
/// before execution, but intentionally excludes general devices, public
/// actions, and path-based loading. The vCPU session is declared before the
/// backend so owners are dropped before VM memory.
pub struct RestoredHvfSnapshotV2Platform {
    runner: HvfArm64BootVcpuSession<'static>,
    backend: HvfBackend,
    memory_binding: SnapshotV2MemoryBinding,
    machine: HvfSnapshotV2MachineState,
    compatibility: HvfSnapshotV1CompatibilityState,
    rtc_device: Arm64BootRtcDevice,
    serial_device: Option<Arm64BootSerialDevice>,
    vmgenid_device: Arm64BootVmGenIdDevice,
    vmclock_device: Arm64BootVmClockDevice,
    pvtime_layout: Arm64PvTimeLayout,
}

impl RestoredHvfSnapshotV2Platform {
    /// Return the complete destination vCPU count.
    pub fn vcpu_count(&self) -> usize {
        self.runner.member_count()
    }

    /// Return owner-thread-verified canonical MPIDRs.
    pub fn vcpu_mpidrs(&self) -> &[u64] {
        self.runner.mpidrs()
    }

    /// Return the retained exact memory-image binding.
    pub const fn memory_binding(&self) -> &SnapshotV2MemoryBinding {
        &self.memory_binding
    }

    /// Return retained logical machine, boot, FDT, and CPU-template facts.
    pub const fn machine(&self) -> &HvfSnapshotV2MachineState {
        &self.machine
    }

    /// Return the destination-validated common compatibility facts.
    pub const fn compatibility(&self) -> &HvfSnapshotV1CompatibilityState {
        &self.compatibility
    }

    /// Borrow the fresh process UART output when this owner was reconstructed
    /// through the closed process shell.
    #[doc(hidden)]
    pub fn serial_output(&self) -> Option<&SharedSerialOutput> {
        self.serial_device.as_ref().map(|device| &device.output)
    }

    /// Reobserve the complete paused lifecycle graph.
    pub fn capture_stable_paused_topology(
        &mut self,
    ) -> Result<
        crate::paused_topology::HvfArm64StablePausedTopologyState,
        HvfArm64StablePausedTopologyCaptureError,
    > {
        self.runner.capture_stable_paused_topology()
    }

    /// Recapture the complete reconstructed platform while it remains paused.
    ///
    /// This creates a fresh binding only after PVTime publication, reuses inert
    /// machine metadata, verifies the mapped FDT identity, and reobserves every
    /// owner-thread vCPU component without opening a path.
    pub fn capture_snapshot_v2_platform<W: Write + Seek>(
        &mut self,
        memory_writer: &mut W,
    ) -> Result<HvfSnapshotV2PlatformState, HvfArm64BootSnapshotV2CaptureError> {
        let (stable, captures, pvtime_capture) =
            self.runner
                .capture_arm64_snapshot_v2_topology()
                .map_err(|source| HvfArm64BootSnapshotV2CaptureError::Topology { source })?;
        let memory = self
            .backend
            .mapped_guest_memory()
            .map_err(|source| HvfArm64BootSnapshotV2CaptureError::GuestMemory { source })?;
        verify_capture_fdt_identity(memory, &self.machine)?;
        if captures.len() != stable.members().len() {
            return Err(HvfArm64BootSnapshotV2CaptureError::CompatibilityMismatch {
                index: captures.len(),
            });
        }

        let expected_identification = self.compatibility.identification();
        let expected_optional_identification = self.compatibility.optional_sve_sme_identification();
        let mut vcpus = Vec::new();
        vcpus
            .try_reserve_exact(captures.len())
            .map_err(|_| HvfArm64BootSnapshotV2CaptureError::Allocation)?;
        let mut global_gic = None;
        for (capture, member) in captures.into_iter().zip(stable.members()) {
            let (
                identification,
                optional_identification,
                mandatory,
                timer,
                pending_interrupts,
                captured_global_gic,
                gic_icc,
                reviewed_optional,
            ) = capture.into_parts();
            if identification != identification_with_mpidr(expected_identification, member.mpidr())
                || optional_identification != expected_optional_identification
            {
                return Err(HvfArm64BootSnapshotV2CaptureError::CompatibilityMismatch {
                    index: member.index(),
                });
            }
            if (member.index() == 0) != captured_global_gic.is_some() {
                return Err(HvfArm64BootSnapshotV2CaptureError::GlobalGicShape {
                    index: member.index(),
                });
            }
            if member.index() == 0 {
                global_gic = captured_global_gic;
            }
            let index = u32::try_from(member.index()).map_err(|_| {
                HvfArm64BootSnapshotV2CaptureError::CompatibilityMismatch {
                    index: member.index(),
                }
            })?;
            let vcpu = HvfSnapshotV2VcpuState::try_new(
                index,
                member.mpidr(),
                mandatory,
                timer,
                pending_interrupts,
                gic_icc,
                reviewed_optional,
            )
            .map_err(|source| HvfArm64BootSnapshotV2CaptureError::Build {
                stage: HvfArm64BootSnapshotV2CaptureStage::Vcpu {
                    index: member.index(),
                },
                source,
            })?;
            vcpus.push(vcpu);
        }
        let global_gic =
            global_gic.ok_or(HvfArm64BootSnapshotV2CaptureError::GlobalGicShape { index: 0 })?;
        let global = HvfSnapshotV2GlobalState::try_new(self.compatibility.clone(), global_gic)
            .map_err(|source| HvfArm64BootSnapshotV2CaptureError::Build {
                stage: HvfArm64BootSnapshotV2CaptureStage::GlobalGic,
                source,
            })?;
        let rtc_layout = RtcMmioLayout::new(
            self.rtc_device.region.range().start(),
            self.rtc_device.region.id(),
        );
        let time = capture_hvf_snapshot_v2_time_state(
            memory,
            rtc_layout,
            &self.vmgenid_device,
            &self.vmclock_device,
            Some(&self.pvtime_layout),
            &pvtime_capture,
        )
        .map_err(|source| HvfArm64BootSnapshotV2CaptureError::Time { source })?;
        let memory_binding = write_snapshot_v2_memory_image(memory, memory_writer)
            .map_err(|source| HvfArm64BootSnapshotV2CaptureError::MemoryImage { source })?;
        HvfSnapshotV2PlatformState::try_new(
            memory_binding,
            self.machine.clone(),
            global,
            stable,
            vcpus,
            time,
        )
        .map_err(|source| HvfArm64BootSnapshotV2CaptureError::Build {
            stage: HvfArm64BootSnapshotV2CaptureStage::Platform,
            source,
        })
    }

    /// Begin execution only after the fully restored platform was published.
    #[doc(hidden)]
    pub fn resume(&mut self) -> Result<(), HvfVcpuRunCoordinatorError> {
        self.runner.resume()
    }

    /// Return the post-publication topology control capability.
    #[doc(hidden)]
    pub fn control(&self) -> HvfVcpuRunControl {
        self.runner.control()
    }

    /// Run one post-publication boot-session step.
    #[doc(hidden)]
    pub fn run_step(
        &mut self,
        entry_is_valid: impl FnMut(u64) -> bool,
    ) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        self.runner.run_step(entry_is_valid)
    }

    /// Set the last stepped member's PPI pending bit.
    #[doc(hidden)]
    pub fn set_last_step_ppi_pending(&self, intid: u32) -> Result<(), HvfArm64BootVcpuError> {
        self.runner.set_last_step_ppi_pending(intid)
    }

    /// Borrow already-authorized destination guest memory.
    #[doc(hidden)]
    pub fn guest_memory(&self) -> Result<&GuestMemory, HvfGuestMemoryMappingError> {
        self.backend.mapped_guest_memory_for_public_access()
    }

    /// Shut down vCPU owners before unmapping memory and destroying the VM.
    pub fn shutdown(&mut self) -> Result<(), HvfSnapshotV2PlatformShutdownError> {
        debug_assert_eq!(
            cleanup_sequence(RestoreOwnership::Session),
            [
                HvfSnapshotV2PlatformCleanupStage::Session,
                HvfSnapshotV2PlatformCleanupStage::Backend,
            ]
        );
        self.runner
            .shutdown()
            .map_err(HvfSnapshotV2PlatformShutdownError::Vcpu)?;
        <HvfBackend as VmBackend>::destroy_vm(&mut self.backend)
            .map_err(HvfSnapshotV2PlatformShutdownError::Backend)
    }
}

impl fmt::Debug for RestoredHvfSnapshotV2Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RestoredHvfSnapshotV2Platform")
            .field("vcpu_count", &self.vcpu_count())
            .field("state", &REDACTED)
            .finish()
    }
}

impl Drop for RestoredHvfSnapshotV2Platform {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Reconstruct one unpublished, initially paused native-v2 HVF platform.
///
/// `state` must already have passed native-v2 decoding/cross-validation and
/// `memory` must come from an already-authorized loader. This function opens no
/// path and performs all supplied-memory checks before creating an HVF VM.
pub fn restore_hvf_snapshot_v2_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell(state, memory, None)
}

/// Reconstruct one native-v2 platform with the exact default process UART.
///
/// The retained FDT, minimal interrupt sequence, and fresh output owner are
/// validated and installed before Hypervisor.framework VM construction.
pub fn restore_hvf_snapshot_v2_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2DefaultProcessShell,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell(state, memory, Some(shell))
}

fn restore_hvf_snapshot_v2_platform_with_shell(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    process_shell: Option<HvfSnapshotV2DefaultProcessShell>,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    debug_assert!(cleanup_sequence(RestoreOwnership::Empty).is_empty());
    let mut cleanup = Vec::new();
    if cleanup.try_reserve_exact(2).is_err() {
        return Err(HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::Preflight,
            HvfSnapshotV2PlatformRestoreFailure::Allocation,
            cleanup,
        ));
    }
    if !memory_matches_binding(&memory, state.memory()) {
        return Err(HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::Preflight,
            HvfSnapshotV2PlatformRestoreFailure::MemoryTopology,
            cleanup,
        ));
    }
    let fdt_bytes = match verified_fdt_bytes(&memory, state.machine()) {
        Ok(bytes) => bytes,
        Err(failure) => {
            return Err(HvfSnapshotV2PlatformRestoreError::new(
                HvfSnapshotV2PlatformRestoreStage::Preflight,
                failure,
                cleanup,
            ));
        }
    };
    let (mut dispatcher, serial_device) =
        match prepare_process_shell(process_shell, &state, &fdt_bytes) {
            Ok(prepared) => prepared,
            Err(failure) => {
                return Err(HvfSnapshotV2PlatformRestoreError::new(
                    HvfSnapshotV2PlatformRestoreStage::ProcessShell,
                    failure,
                    cleanup,
                ));
            }
        };
    let prepared_time_identity = match prepare_arm64_snapshot_time_identity(
        &memory,
        state.time().vmgenid(),
        state.time().vmclock(),
        state.time().vmclock_abi(),
    ) {
        Ok(prepared) => prepared,
        Err(source) => {
            return Err(HvfSnapshotV2PlatformRestoreError::new(
                HvfSnapshotV2PlatformRestoreStage::Preflight,
                HvfSnapshotV2PlatformRestoreFailure::TimePreparation(source),
                cleanup,
            ));
        }
    };
    let pvtime_layout = match verify_snapshot_v2_pvtime_memory(&memory, state.time()) {
        Ok(layout) => layout,
        Err(failure) => {
            return Err(HvfSnapshotV2PlatformRestoreError::new(
                HvfSnapshotV2PlatformRestoreStage::Preflight,
                failure,
                cleanup,
            ));
        }
    };
    let destination_cache = match HvfBackend::arm64_vcpu_cache_manifest() {
        Ok(destination_cache) => destination_cache,
        Err(source) => {
            return Err(HvfSnapshotV2PlatformRestoreError::new(
                HvfSnapshotV2PlatformRestoreStage::Preflight,
                HvfSnapshotV2PlatformRestoreFailure::CacheQuery(source),
                cleanup,
            ));
        }
    };
    if destination_cache != state.global().compatibility().cache_manifest() {
        return Err(HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::Preflight,
            HvfSnapshotV2PlatformRestoreFailure::CacheMismatch,
            cleanup,
        ));
    }

    let (memory_binding, machine, global, stable, vcpus, time) = state.into_parts();
    let (compatibility, global_gic) = global.into_parts();
    let (mut vmgenid_device, mut vmclock_device) = prepared_time_identity.into_parts();
    let mut backend = HvfBackend::new();
    let mut topology: Option<HvfVcpuTopology<'static>> = None;

    if let Err(source) = <HvfBackend as VmBackend>::create_vm(&mut backend) {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Vm,
            HvfSnapshotV2PlatformRestoreFailure::CreateVm(source),
            &mut topology,
            &mut backend,
            cleanup,
        ));
    }
    if let Err(source) = backend.map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM) {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Memory,
            HvfSnapshotV2PlatformRestoreFailure::MapMemory(source),
            &mut topology,
            &mut backend,
            cleanup,
        ));
    }
    if machine.machine().track_dirty_pages()
        && let Err(source) = backend.start_dirty_write_tracking()
    {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Memory,
            HvfSnapshotV2PlatformRestoreFailure::DirtyTracking(source),
            &mut topology,
            &mut backend,
            cleanup,
        ));
    }

    let expected_gic = compatibility.gic_metadata();
    let created_gic = match expected_gic.msi {
        Some(msi) => {
            let Some(interrupt_count) = NonZeroU32::new(msi.interrupt_range.count) else {
                return Err(failed_restore(
                    HvfSnapshotV2PlatformRestoreStage::Gic,
                    HvfSnapshotV2PlatformRestoreFailure::GicMetadata,
                    &mut topology,
                    &mut backend,
                    cleanup,
                ));
            };
            backend.create_gic_with_msi(HvfGicMsiConfiguration::new(interrupt_count))
        }
        None => backend.create_gic(),
    };
    let created_gic = match created_gic {
        Ok(metadata) => *metadata,
        Err(source) => {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Gic,
                HvfSnapshotV2PlatformRestoreFailure::CreateGic(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    };
    if created_gic != expected_gic {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Gic,
            HvfSnapshotV2PlatformRestoreFailure::GicMetadata,
            &mut topology,
            &mut backend,
            cleanup,
        ));
    }

    topology = match backend.start_session_vcpu_topology(machine.machine().vcpu_count()) {
        Ok(created) => Some(created),
        Err(source) => {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Topology,
                HvfSnapshotV2PlatformRestoreFailure::Topology(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    };
    let Some(created_topology) = topology.as_ref() else {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Topology,
            HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
            &mut topology,
            &mut backend,
            cleanup,
        ));
    };
    if created_topology.mpidrs().len() != stable.members().len()
        || created_topology
            .mpidrs()
            .iter()
            .zip(stable.members())
            .any(|(actual, expected)| *actual != expected.mpidr())
    {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::TopologyIdentity,
            HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
            &mut topology,
            &mut backend,
            cleanup,
        ));
    }
    if let Some(cpu_template) = machine.cpu_template() {
        let Some(created_topology) = topology.as_ref() else {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Topology,
                HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
                &mut topology,
                &mut backend,
                cleanup,
            ));
        };
        if let Err(source) = created_topology.apply_retained_arm64_cpu_template_state(cpu_template)
        {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::CpuTemplate,
                HvfSnapshotV2PlatformRestoreFailure::CpuTemplate(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    }

    let source_identification = compatibility.identification();
    let source_optional_identification = compatibility.optional_sve_sme_identification();
    for member in stable.members() {
        let index = member.index();
        let Some(created_topology) = topology.as_ref() else {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Topology,
                HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
                &mut topology,
                &mut backend,
                cleanup,
            ));
        };
        let destination =
            created_topology.capture_arm64_snapshot_v2_destination_compatibility(index);
        let (destination_identification, destination_optional_identification) = match destination {
            Ok(destination) => destination,
            Err(source) => {
                return Err(failed_restore(
                    HvfSnapshotV2PlatformRestoreStage::Compatibility { index },
                    HvfSnapshotV2PlatformRestoreFailure::CompatibilityRead(source),
                    &mut topology,
                    &mut backend,
                    cleanup,
                ));
            }
        };
        if destination_identification
            != identification_with_mpidr(source_identification, member.mpidr())
            || destination_optional_identification != source_optional_identification
        {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Compatibility { index },
                HvfSnapshotV2PlatformRestoreFailure::CompatibilityMismatch,
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    }

    let Some(created_topology) = topology.as_ref() else {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Topology,
            HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
            &mut topology,
            &mut backend,
            cleanup,
        ));
    };
    if let Err(source) = created_topology.restore_arm64_snapshot_v2_global_gic(&global_gic) {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::GlobalGic,
            HvfSnapshotV2PlatformRestoreFailure::GlobalGic(source),
            &mut topology,
            &mut backend,
            cleanup,
        ));
    }
    for vcpu in vcpus {
        let (index, mpidr, mandatory, timer, pending_interrupts, gic_icc, reviewed_optional) =
            vcpu.into_parts();
        let Ok(index) = usize::try_from(index) else {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Topology,
                HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
                &mut topology,
                &mut backend,
                cleanup,
            ));
        };
        let restore = HvfArm64SnapshotV2VcpuRestore::new(
            identification_with_mpidr(source_identification, mpidr),
            source_optional_identification,
            mpidr,
            (
                mandatory,
                timer,
                pending_interrupts,
                gic_icc,
                reviewed_optional,
            ),
        );
        let Some(created_topology) = topology.as_ref() else {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Topology,
                HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
                &mut topology,
                &mut backend,
                cleanup,
            ));
        };
        if let Err(source) = created_topology.restore_arm64_snapshot_v2_vcpu(index, restore) {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Vcpu { index },
                HvfSnapshotV2PlatformRestoreFailure::Vcpu(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    }

    let rtc_device = match register_arm64_boot_rtc_mmio(&mut dispatcher, time.rtc_layout()) {
        Ok(device) => device,
        Err(source) => {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Rtc,
                HvfSnapshotV2PlatformRestoreFailure::Rtc(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    };

    let pvtime_configs = match prepare_snapshot_v2_pvtime_configs(&backend, &time) {
        Ok(configs) => configs,
        Err(failure) => {
            let (index, failure) = *failure;
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::PvTime { index },
                failure,
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    };
    let Some(created_topology) = topology.as_ref() else {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Topology,
            HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
            &mut topology,
            &mut backend,
            cleanup,
        ));
    };
    for index in 0..pvtime_configs.len() {
        if let Err(source) = created_topology.ensure_snapshot_restore_available(index) {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::PvTime { index },
                HvfSnapshotV2PlatformRestoreFailure::PvTime(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    }
    for (index, config) in pvtime_configs.into_iter().enumerate() {
        if let Err(source) = created_topology.configure_arm64_snapshot_v2_pvtime(index, config) {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::PvTime { index },
                HvfSnapshotV2PlatformRestoreFailure::PvTime(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    }

    if let Err(failure) = restore_snapshot_v2_time_identity(
        created_topology,
        &mut backend,
        expected_gic,
        &time,
        &mut vmgenid_device,
        &mut vmclock_device,
    ) {
        let (stage, failure) = *failure;
        return Err(failed_restore(
            stage,
            failure,
            &mut topology,
            &mut backend,
            cleanup,
        ));
    }

    let dispatcher = Arc::new(Mutex::new(dispatcher));
    let Some(raw_topology) = topology.take() else {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Topology,
            HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
            &mut topology,
            &mut backend,
            cleanup,
        ));
    };
    let runner = match HvfArm64BootVcpuSession::from_stable_paused_topology(
        raw_topology,
        &stable,
        dispatcher,
        stable.virtual_timer_intid(),
    ) {
        Ok(runner) => runner,
        Err(source) => {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Lifecycle,
                HvfSnapshotV2PlatformRestoreFailure::Lifecycle(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    };

    Ok(RestoredHvfSnapshotV2Platform {
        runner,
        backend,
        memory_binding,
        machine,
        compatibility,
        rtc_device,
        serial_device,
        vmgenid_device,
        vmclock_device,
        pvtime_layout,
    })
}

fn verify_snapshot_v2_pvtime_memory(
    memory: &GuestMemory,
    time: &HvfSnapshotV2TimeState,
) -> Result<Arm64PvTimeLayout, HvfSnapshotV2PlatformRestoreFailure> {
    let arena_size = time
        .vmgenid()
        .range()
        .start()
        .raw_value()
        .checked_sub(aarch64::SYSTEM_MEM_START)
        .ok_or(HvfSnapshotV2PlatformRestoreFailure::PvTimeMismatch)?;
    let arena = GuestMemoryRange::new(GuestAddress::new(aarch64::SYSTEM_MEM_START), arena_size)
        .map_err(|_| HvfSnapshotV2PlatformRestoreFailure::PvTimeMismatch)?;
    let count = u8::try_from(time.pvtime_vcpus().len())
        .map_err(|_| HvfSnapshotV2PlatformRestoreFailure::PvTimeMismatch)?;
    let layout = Arm64PvTimeLayout::plan(count, arena)
        .map_err(|_| HvfSnapshotV2PlatformRestoreFailure::PvTimeMismatch)?;
    for (expected, captured) in layout.records().iter().zip(time.pvtime_vcpus()) {
        if expected.start() != captured.record_ipa() {
            return Err(HvfSnapshotV2PlatformRestoreFailure::PvTimeMismatch);
        }
        let mut bytes = [0; ARM64_PVTIME_STRUCTURE_SIZE];
        memory
            .read_slice(&mut bytes, expected.start())
            .map_err(HvfSnapshotV2PlatformRestoreFailure::PvTimeRead)?;
        let observed = Arm64PvTimeStAbi::from_bytes(bytes)
            .map_err(|_| HvfSnapshotV2PlatformRestoreFailure::PvTimeMismatch)?;
        if observed.stolen_time_ns() != captured.stolen_time_ns() {
            return Err(HvfSnapshotV2PlatformRestoreFailure::PvTimeMismatch);
        }
    }
    Ok(layout)
}

fn prepare_snapshot_v2_pvtime_configs(
    backend: &HvfBackend,
    time: &HvfSnapshotV2TimeState,
) -> Result<Vec<HvfArm64PvTimeAccountingConfig>, Box<(usize, HvfSnapshotV2PlatformRestoreFailure)>>
{
    let memory = backend.mapped_guest_memory().map_err(|source| {
        Box::new((0, HvfSnapshotV2PlatformRestoreFailure::PvTimeMemory(source)))
    })?;
    let mut configs = Vec::new();
    configs
        .try_reserve_exact(time.pvtime_vcpus().len())
        .map_err(|_| Box::new((0, HvfSnapshotV2PlatformRestoreFailure::Allocation)))?;
    for captured in time.pvtime_vcpus() {
        let index = usize::try_from(captured.index())
            .map_err(|_| Box::new((0, HvfSnapshotV2PlatformRestoreFailure::PvTimeAddress)))?;
        let address = captured
            .record_ipa()
            .checked_add(ARM64_PVTIME_STOLEN_TIME_OFFSET as u64)
            .ok_or_else(|| Box::new((index, HvfSnapshotV2PlatformRestoreFailure::PvTimeAddress)))?;
        let publisher = memory.atomic_u64(address).map_err(|source| {
            Box::new((
                index,
                HvfSnapshotV2PlatformRestoreFailure::PvTimePublisher(source),
            ))
        })?;
        configs.push(HvfArm64PvTimeAccountingConfig::new(
            captured.record_ipa().raw_value(),
            publisher,
            captured.stolen_time_ns(),
            None,
        ));
    }
    Ok(configs)
}

fn restore_snapshot_v2_time_identity(
    topology: &HvfVcpuTopology<'_>,
    backend: &mut HvfBackend,
    gic: HvfGicMetadata,
    time: &HvfSnapshotV2TimeState,
    vmgenid: &mut Arm64BootVmGenIdDevice,
    vmclock: &mut Arm64BootVmClockDevice,
) -> Result<
    (),
    Box<(
        HvfSnapshotV2PlatformRestoreStage,
        HvfSnapshotV2PlatformRestoreFailure,
    )>,
> {
    for index in 0..time.pvtime_vcpus().len() {
        topology
            .ensure_snapshot_restore_available(index)
            .map_err(|source| {
                Box::new((
                    HvfSnapshotV2PlatformRestoreStage::TimeIdentityPreflight,
                    HvfSnapshotV2PlatformRestoreFailure::TimeIdentityRunner(source),
                ))
            })?;
    }
    let signaler = HvfGicSpiSignaler::from_metadata(&gic).map_err(|source| {
        Box::new((
            HvfSnapshotV2PlatformRestoreStage::TimeIdentityPreflight,
            HvfSnapshotV2PlatformRestoreFailure::TimeIdentitySignaler(source),
        ))
    })?;
    for line in [
        time.vmgenid().interrupt_line(),
        time.vmclock().interrupt_line(),
    ] {
        signaler.validate_line(line).map_err(|source| {
            Box::new((
                HvfSnapshotV2PlatformRestoreStage::TimeIdentityPreflight,
                HvfSnapshotV2PlatformRestoreFailure::TimeIdentitySignaler(source),
            ))
        })?;
    }
    let memory = backend.mapped_guest_memory_mut().map_err(|source| {
        Box::new((
            HvfSnapshotV2PlatformRestoreStage::TimeIdentityPreflight,
            HvfSnapshotV2PlatformRestoreFailure::TimeIdentityMemory(source),
        ))
    })?;
    replace_vmgenid_and_signal_with(memory, vmgenid, replace_arm64_boot_vmgenid, || {
        signaler.set_level(time.vmgenid().interrupt_line(), true)
    })
    .map_err(|source| {
        Box::new((
            HvfSnapshotV2PlatformRestoreStage::VmGenId,
            HvfSnapshotV2PlatformRestoreFailure::VmGenId(source),
        ))
    })?;
    update_vmclock_and_signal_with(
        memory,
        vmclock,
        |memory, device| device.abi.update_after_restore(memory, device.range),
        || signaler.set_level(time.vmclock().interrupt_line(), true),
    )
    .map_err(|source| {
        Box::new((
            HvfSnapshotV2PlatformRestoreStage::VmClock,
            HvfSnapshotV2PlatformRestoreFailure::VmClock(source),
        ))
    })
}

fn failed_restore(
    stage: HvfSnapshotV2PlatformRestoreStage,
    failure: HvfSnapshotV2PlatformRestoreFailure,
    topology: &mut Option<HvfVcpuTopology<'static>>,
    backend: &mut HvfBackend,
    mut cleanup: Vec<HvfSnapshotV2PlatformCleanupFailure>,
) -> HvfSnapshotV2PlatformRestoreError {
    let ownership = restore_ownership_before_failure(stage);
    debug_assert_eq!(
        topology.is_some(),
        matches!(ownership, RestoreOwnership::Topology)
    );
    for cleanup_stage in cleanup_sequence(ownership) {
        match cleanup_stage {
            HvfSnapshotV2PlatformCleanupStage::Topology => {
                if let Some(created) = topology.take()
                    && let Err(source) = created.shutdown()
                {
                    cleanup.push(HvfSnapshotV2PlatformCleanupFailure {
                        stage: *cleanup_stage,
                        source: Box::new(source),
                    });
                }
            }
            HvfSnapshotV2PlatformCleanupStage::Backend => {
                if let Err(source) = <HvfBackend as VmBackend>::destroy_vm(backend) {
                    cleanup.push(HvfSnapshotV2PlatformCleanupFailure {
                        stage: *cleanup_stage,
                        source: Box::new(source),
                    });
                }
            }
            HvfSnapshotV2PlatformCleanupStage::Session => {
                debug_assert!(false, "raw restore failure cannot own a published session");
            }
        }
    }
    HvfSnapshotV2PlatformRestoreError::new(stage, failure, cleanup)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreOwnership {
    Empty,
    Backend,
    Topology,
    Session,
}

fn cleanup_sequence(ownership: RestoreOwnership) -> &'static [HvfSnapshotV2PlatformCleanupStage] {
    match ownership {
        RestoreOwnership::Empty => &[],
        RestoreOwnership::Backend => &[HvfSnapshotV2PlatformCleanupStage::Backend],
        RestoreOwnership::Topology => &[
            HvfSnapshotV2PlatformCleanupStage::Topology,
            HvfSnapshotV2PlatformCleanupStage::Backend,
        ],
        RestoreOwnership::Session => &[
            HvfSnapshotV2PlatformCleanupStage::Session,
            HvfSnapshotV2PlatformCleanupStage::Backend,
        ],
    }
}

fn restore_ownership_before_failure(stage: HvfSnapshotV2PlatformRestoreStage) -> RestoreOwnership {
    match stage {
        HvfSnapshotV2PlatformRestoreStage::Preflight
        | HvfSnapshotV2PlatformRestoreStage::ProcessShell => RestoreOwnership::Empty,
        HvfSnapshotV2PlatformRestoreStage::Vm
        | HvfSnapshotV2PlatformRestoreStage::Memory
        | HvfSnapshotV2PlatformRestoreStage::Gic
        | HvfSnapshotV2PlatformRestoreStage::Topology
        | HvfSnapshotV2PlatformRestoreStage::Lifecycle => RestoreOwnership::Backend,
        HvfSnapshotV2PlatformRestoreStage::TopologyIdentity
        | HvfSnapshotV2PlatformRestoreStage::CpuTemplate
        | HvfSnapshotV2PlatformRestoreStage::Compatibility { .. }
        | HvfSnapshotV2PlatformRestoreStage::GlobalGic
        | HvfSnapshotV2PlatformRestoreStage::Vcpu { .. }
        | HvfSnapshotV2PlatformRestoreStage::Rtc
        | HvfSnapshotV2PlatformRestoreStage::PvTime { .. }
        | HvfSnapshotV2PlatformRestoreStage::TimeIdentityPreflight
        | HvfSnapshotV2PlatformRestoreStage::VmGenId
        | HvfSnapshotV2PlatformRestoreStage::VmClock => RestoreOwnership::Topology,
        HvfSnapshotV2PlatformRestoreStage::Publication => RestoreOwnership::Session,
    }
}

fn memory_matches_binding(memory: &GuestMemory, binding: &SnapshotV2MemoryBinding) -> bool {
    memory.regions().len() == binding.extents().len()
        && memory
            .regions()
            .iter()
            .zip(binding.extents())
            .all(|(region, extent)| region.range() == extent.range())
}

fn prepare_process_shell(
    shell: Option<HvfSnapshotV2DefaultProcessShell>,
    state: &HvfSnapshotV2PlatformState,
    fdt_bytes: &[u8],
) -> Result<(MmioDispatcher, Option<Arm64BootSerialDevice>), HvfSnapshotV2PlatformRestoreFailure> {
    let mut dispatcher = MmioDispatcher::new();
    let Some(shell) = shell else {
        return Ok((dispatcher, None));
    };

    let gic = state.global().compatibility().gic_metadata();
    if gic.msi.is_some()
        || state.time().rtc_layout()
            != RtcMmioLayout::new(PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID)
    {
        return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
            mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
        });
    }
    let mut allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?;
    let serial_interrupt = allocator
        .allocate()
        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?;
    let vmgenid_interrupt = allocator
        .allocate()
        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?;
    let vmclock_interrupt = allocator
        .allocate()
        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?;
    if state.time().vmgenid().interrupt_line() != vmgenid_interrupt
        || state.time().vmclock().interrupt_line() != vmclock_interrupt
    {
        return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity);
    }
    validate_default_process_fdt(fdt_bytes, state, serial_interrupt)
        .map_err(|mismatch| HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt { mismatch })?;

    let serial = register_arm64_boot_serial_mmio(
        &mut dispatcher,
        Arm64BootSerialDeviceConfig::new(
            PROCESS_SERIAL_MMIO_REGION_ID,
            PROCESS_SERIAL_MMIO_BASE,
            serial_interrupt,
            shell.serial_output,
        ),
    )
    .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellSerial)?;
    Ok((dispatcher, Some(serial)))
}

fn validate_default_process_fdt(
    bytes: &[u8],
    state: &HvfSnapshotV2PlatformState,
    serial_interrupt: bangbang_runtime::interrupt::GuestInterruptLine,
) -> Result<(), HvfSnapshotV2ProcessFdtMismatch> {
    let tree = DeviceTree::load(bytes).map_err(|_| HvfSnapshotV2ProcessFdtMismatch::Parse)?;
    let time = state.time();
    let gic = state.global().compatibility().gic_metadata();
    let serial_name =
        |name: &str| node_name_has_number(name, "uart@", 16, PROCESS_SERIAL_MMIO_BASE.raw_value());
    let rtc_name =
        |name: &str| node_name_has_number(name, "rtc@", 16, PROCESS_RTC_MMIO_BASE.raw_value());
    let vmclock_name =
        |name: &str| node_name_has_number(name, "ptp@", 10, time.vmclock().fdt_region().base);
    if tree.root.children.len() != 11
        || [
            "cpus",
            "memory@ram",
            "chosen",
            "intc",
            "timer",
            "apb-pclk",
            "psci",
            "vmgenid",
        ]
        .iter()
        .any(|name| child_named(&tree.root, name).is_none())
        || child_matching(&tree.root, |node| serial_name(&node.name)).is_none()
        || child_matching(&tree.root, |node| rtc_name(&node.name)).is_none()
        || child_matching(&tree.root, |node| vmclock_name(&node.name)).is_none()
        || !tree
            .root
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "linux,dummy-virt")
        || !tree
            .root
            .prop_u32("#address-cells")
            .is_ok_and(|cells| cells == 2)
        || !tree
            .root
            .prop_u32("#size-cells")
            .is_ok_and(|cells| cells == 2)
        || !tree
            .root
            .prop_u32("interrupt-parent")
            .is_ok_and(|phandle| phandle == 1)
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory);
    }

    let Some(clock) = child_named(&tree.root, "apb-pclk") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory);
    };
    let Some(psci) = child_named(&tree.root, "psci") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory);
    };
    if !clock.children.is_empty()
        || !clock
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "fixed-clock")
        || !clock.prop_u32("#clock-cells").is_ok_and(|cells| cells == 0)
        || !clock
            .prop_u32("clock-frequency")
            .is_ok_and(|frequency| frequency == 24_000_000)
        || !clock
            .prop_str("clock-output-names")
            .is_ok_and(|name| name == "clk24mhz")
        || !clock.prop_u32("phandle").is_ok_and(|phandle| phandle == 2)
        || !psci.children.is_empty()
        || !psci
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "arm,psci-0.2")
        || !psci.prop_str("method").is_ok_and(|method| method == "hvc")
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory);
    }

    let Some(cpus) = child_named(&tree.root, "cpus") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::CpuInventory);
    };
    if cpus.children.len() != state.topology().members().len()
        || cpus
            .children
            .iter()
            .zip(state.topology().members())
            .any(|(node, member)| {
                !node_name_has_number(&node.name, "cpu@", 16, member.mpidr())
                    || !property_u64_cells_equal(node, "reg", &[member.mpidr()])
                    || !node
                        .prop_str("enable-method")
                        .is_ok_and(|method| method == "psci")
            })
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::CpuInventory);
    }

    let Some(memory) = child_named(&tree.root, "memory@ram") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Memory);
    };
    if !memory_property_matches_binding(memory, state.memory()) {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Memory);
    }

    let Some(chosen) = child_named(&tree.root, "chosen") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Boot);
    };
    let Some(expected_boot_args) =
        expected_process_boot_arguments(state.machine().boot().boot_arguments())
    else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Boot);
    };
    if !chosen
        .prop_str("bootargs")
        .is_ok_and(|arguments| arguments == expected_boot_args.as_str())
        || chosen.has_prop("linux,initrd-start") != state.machine().boot().initrd_path().is_some()
        || chosen.has_prop("linux,initrd-end") != state.machine().boot().initrd_path().is_some()
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Boot);
    }

    let Some(intc) = child_named(&tree.root, "intc") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Gic);
    };
    if !intc.children.is_empty()
        || !intc
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "arm,gic-v3")
        || !property_u64_cells_equal(
            intc,
            "reg",
            &[
                gic.distributor.base,
                gic.distributor.size,
                gic.redistributor.region.base,
                gic.redistributor.region.size,
            ],
        )
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Gic);
    }
    let Ok(timer_metadata) = gic.arm64_fdt_timer_interrupts() else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Timer);
    };
    let Some(timer) = child_named(&tree.root, "timer") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Timer);
    };
    if !property_u32_cells_equal(
        timer,
        "interrupts",
        &[
            1,
            timer_metadata.secure_physical,
            4,
            1,
            timer_metadata.non_secure_physical,
            4,
            1,
            timer_metadata.virtual_timer,
            4,
            1,
            timer_metadata.hypervisor,
            4,
        ],
    ) {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Timer);
    }

    let Some(rtc) = child_matching(&tree.root, |node| rtc_name(&node.name)) else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Rtc);
    };
    if !rtc.children.is_empty()
        || rtc.prop_raw("compatible").map(Vec::as_slice) != Some(b"arm,pl031\0arm,primecell\0")
        || !property_u64_cells_equal(
            rtc,
            "reg",
            &[
                PROCESS_RTC_MMIO_BASE.raw_value(),
                RTC_MMIO_DEVICE_WINDOW_SIZE,
            ],
        )
        || !rtc.prop_u32("clocks").is_ok_and(|phandle| phandle == 2)
        || !rtc
            .prop_str("clock-names")
            .is_ok_and(|name| name == "apb_pclk")
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Rtc);
    }

    let Some(serial) = child_matching(&tree.root, |node| serial_name(&node.name)) else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Serial);
    };
    let Some(serial_interrupt_cell) = serial_interrupt.raw_value().checked_sub(32) else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Serial);
    };
    if !serial.children.is_empty()
        || !serial
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "ns16550a")
        || !property_u64_cells_equal(
            serial,
            "reg",
            &[
                PROCESS_SERIAL_MMIO_BASE.raw_value(),
                SERIAL_MMIO_DEVICE_WINDOW_SIZE,
            ],
        )
        || !property_u32_cells_equal(serial, "interrupts", &[0, serial_interrupt_cell, 1])
        || !serial.prop_u32("clocks").is_ok_and(|phandle| phandle == 2)
        || !serial
            .prop_str("clock-names")
            .is_ok_and(|name| name == "apb_pclk")
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Serial);
    }

    let Some(vmgenid) = child_named(&tree.root, "vmgenid") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::VmGenId);
    };
    let vmgenid_metadata = time.vmgenid();
    let Some(vmgenid_interrupt_cell) = vmgenid_metadata
        .interrupt_line()
        .raw_value()
        .checked_sub(32)
    else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::VmGenId);
    };
    if !vmgenid.children.is_empty()
        || !vmgenid
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "microsoft,vmgenid")
        || !property_u64_cells_equal(
            vmgenid,
            "reg",
            &[
                vmgenid_metadata.fdt_region().base,
                vmgenid_metadata.fdt_region().size,
            ],
        )
        || !property_u32_cells_equal(vmgenid, "interrupts", &[0, vmgenid_interrupt_cell, 1])
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::VmGenId);
    }

    let Some(vmclock) = child_matching(&tree.root, |node| vmclock_name(&node.name)) else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::VmClock);
    };
    let vmclock_metadata = time.vmclock();
    let Some(vmclock_interrupt_cell) = vmclock_metadata
        .interrupt_line()
        .raw_value()
        .checked_sub(32)
    else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::VmClock);
    };
    if vmclock.children.is_empty()
        && vmclock
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "amazon,vmclock")
        && property_u64_cells_equal(
            vmclock,
            "reg",
            &[
                vmclock_metadata.fdt_region().base,
                vmclock_metadata.fdt_region().size,
            ],
        )
        && property_u32_cells_equal(vmclock, "interrupts", &[0, vmclock_interrupt_cell, 1])
    {
        Ok(())
    } else {
        Err(HvfSnapshotV2ProcessFdtMismatch::VmClock)
    }
}

fn child_named<'a>(node: &'a Node, name: &str) -> Option<&'a Node> {
    let mut matches = node.children.iter().filter(|child| child.name == name);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn child_matching(node: &Node, mut predicate: impl FnMut(&Node) -> bool) -> Option<&Node> {
    let mut matches = node.children.iter().filter(|child| predicate(child));
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn node_name_has_number(name: &str, prefix: &str, radix: u32, expected: u64) -> bool {
    name.strip_prefix(prefix)
        .and_then(|value| u64::from_str_radix(value, radix).ok())
        == Some(expected)
}

fn property_u32_cells_equal(node: &Node, name: &str, expected: &[u32]) -> bool {
    let Some(raw) = node.prop_raw(name) else {
        return false;
    };
    raw.len() == expected.len().saturating_mul(4)
        && raw.chunks_exact(4).zip(expected).all(|(chunk, expected)| {
            <[u8; 4]>::try_from(chunk).map(u32::from_be_bytes).ok() == Some(*expected)
        })
}

fn property_u64_cells_equal(node: &Node, name: &str, expected: &[u64]) -> bool {
    let Some(raw) = node.prop_raw(name) else {
        return false;
    };
    raw.len() == expected.len().saturating_mul(8)
        && raw.chunks_exact(8).zip(expected).all(|(chunk, expected)| {
            <[u8; 8]>::try_from(chunk).map(u64::from_be_bytes).ok() == Some(*expected)
        })
}

fn memory_property_matches_binding(node: &Node, binding: &SnapshotV2MemoryBinding) -> bool {
    let Some(raw) = node.prop_raw("reg") else {
        return false;
    };
    if raw.len() != binding.extents().len().saturating_mul(16) {
        return false;
    }
    raw.chunks_exact(16)
        .zip(binding.extents())
        .enumerate()
        .all(|(index, (chunk, extent))| {
            let (start, size) = chunk.split_at(8);
            let range = extent.range();
            let (expected_start, expected_size) = if index == 0 {
                let Some(start) = range
                    .start()
                    .checked_add(bangbang_runtime::memory::aarch64::SYSTEM_MEM_SIZE)
                else {
                    return false;
                };
                let Some(size) = range
                    .size()
                    .checked_sub(bangbang_runtime::memory::aarch64::SYSTEM_MEM_SIZE)
                else {
                    return false;
                };
                (start.raw_value(), size)
            } else {
                (range.start().raw_value(), range.size())
            };
            <[u8; 8]>::try_from(start).map(u64::from_be_bytes).ok() == Some(expected_start)
                && <[u8; 8]>::try_from(size).map(u64::from_be_bytes).ok() == Some(expected_size)
        })
}

fn expected_process_boot_arguments(source: Option<&str>) -> Option<String> {
    let source = source.unwrap_or(bangbang_runtime::boot::DEFAULT_KERNEL_COMMAND_LINE);
    let separator = " -- ";
    let split = source.match_indices(separator).find(|(index, _)| {
        source
            .get(..*index)
            .is_some_and(|prefix| prefix.matches('"').count().is_multiple_of(2))
    });
    let (kernel, init) = match split {
        Some((index, _)) => (
            source.get(..index)?.trim(),
            source.get(index.checked_add(separator.len())?..)?.trim(),
        ),
        None => (source.trim(), ""),
    };
    if kernel.is_empty() && !init.is_empty() {
        return None;
    }
    let capacity = kernel
        .len()
        .checked_add(" pci=off".len())?
        .checked_add(if init.is_empty() {
            0
        } else {
            separator.len().checked_add(init.len())?
        })?;
    let mut expected = String::new();
    expected.try_reserve_exact(capacity).ok()?;
    expected.push_str(kernel);
    if !kernel.is_empty() {
        expected.push(' ');
    }
    expected.push_str("pci=off");
    if !init.is_empty() {
        expected.push_str(separator);
        expected.push_str(init);
    }
    Some(expected)
}

fn verified_fdt_bytes(
    memory: &GuestMemory,
    machine: &HvfSnapshotV2MachineState,
) -> Result<Vec<u8>, HvfSnapshotV2PlatformRestoreFailure> {
    let fdt = machine.fdt();
    let size = usize::try_from(fdt.size())
        .map_err(|_| HvfSnapshotV2PlatformRestoreFailure::FdtAllocation)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| HvfSnapshotV2PlatformRestoreFailure::FdtAllocation)?;
    bytes.resize(size, 0);
    memory
        .read_slice(&mut bytes, fdt.address())
        .map_err(HvfSnapshotV2PlatformRestoreFailure::FdtRead)?;
    if crc64(0, &bytes) != fdt.checksum() {
        return Err(HvfSnapshotV2PlatformRestoreFailure::FdtIdentity);
    }
    Ok(bytes)
}

fn verify_capture_fdt_identity(
    memory: &GuestMemory,
    machine: &HvfSnapshotV2MachineState,
) -> Result<(), HvfArm64BootSnapshotV2CaptureError> {
    let fdt = machine.fdt();
    let size = usize::try_from(fdt.size())
        .map_err(|_| HvfArm64BootSnapshotV2CaptureError::FdtAllocation)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| HvfArm64BootSnapshotV2CaptureError::FdtAllocation)?;
    bytes.resize(size, 0);
    memory
        .read_slice(&mut bytes, fdt.address())
        .map_err(|source| HvfArm64BootSnapshotV2CaptureError::FdtRead { source })?;
    if crc64(0, &bytes) != fdt.checksum() {
        return Err(HvfArm64BootSnapshotV2CaptureError::FdtIdentityMismatch);
    }
    Ok(())
}

fn identification_with_mpidr(
    common: HvfArm64VcpuIdentificationRegisterState,
    mpidr: u64,
) -> HvfArm64VcpuIdentificationRegisterState {
    HvfArm64VcpuIdentificationRegisterState::new([
        common.midr_el1(),
        mpidr,
        common.id_aa64pfr0_el1(),
        common.id_aa64pfr1_el1(),
        common.id_aa64dfr0_el1(),
        common.id_aa64dfr1_el1(),
        common.id_aa64isar0_el1(),
        common.id_aa64isar1_el1(),
        common.id_aa64mmfr0_el1(),
        common.id_aa64mmfr1_el1(),
        common.id_aa64mmfr2_el1(),
    ])
}

#[cfg(test)]
trait RestoreProtocol {
    type Error;

    fn execute(&mut self, stage: HvfSnapshotV2PlatformRestoreStage) -> Result<(), Self::Error>;

    fn cleanup(&mut self, stage: HvfSnapshotV2PlatformCleanupStage) -> Result<(), Self::Error>;
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
struct RestoreProtocolError<E> {
    stage: HvfSnapshotV2PlatformRestoreStage,
    primary: E,
    cleanup: Vec<(HvfSnapshotV2PlatformCleanupStage, E)>,
}

#[cfg(test)]
fn run_restore_protocol<P: RestoreProtocol>(
    protocol: &mut P,
    vcpu_count: usize,
) -> Result<(), RestoreProtocolError<P::Error>> {
    let stages = restore_protocol_stages(vcpu_count);
    let mut ownership = RestoreOwnership::Empty;
    for stage in stages {
        if stage == HvfSnapshotV2PlatformRestoreStage::Vm {
            // A failed create may still require the idempotent backend guard.
            ownership = RestoreOwnership::Backend;
        }
        if stage == HvfSnapshotV2PlatformRestoreStage::Lifecycle {
            // Lifecycle import consumes the raw topology and performs its own
            // owner rollback on failure.
            ownership = RestoreOwnership::Backend;
        }
        if let Err(primary) = protocol.execute(stage) {
            debug_assert_eq!(ownership, restore_ownership_before_failure(stage));
            let mut cleanup = Vec::new();
            for cleanup_stage in cleanup_sequence(ownership) {
                if let Err(source) = protocol.cleanup(*cleanup_stage) {
                    cleanup.push((*cleanup_stage, source));
                }
            }
            return Err(RestoreProtocolError {
                stage,
                primary,
                cleanup,
            });
        }
        match stage {
            HvfSnapshotV2PlatformRestoreStage::Topology => {
                ownership = RestoreOwnership::Topology;
            }
            HvfSnapshotV2PlatformRestoreStage::TopologyIdentity => {}
            HvfSnapshotV2PlatformRestoreStage::Lifecycle => {
                ownership = RestoreOwnership::Session;
            }
            HvfSnapshotV2PlatformRestoreStage::Publication => {
                ownership = RestoreOwnership::Empty;
            }
            HvfSnapshotV2PlatformRestoreStage::Preflight
            | HvfSnapshotV2PlatformRestoreStage::ProcessShell
            | HvfSnapshotV2PlatformRestoreStage::Vm
            | HvfSnapshotV2PlatformRestoreStage::Memory
            | HvfSnapshotV2PlatformRestoreStage::Gic
            | HvfSnapshotV2PlatformRestoreStage::CpuTemplate
            | HvfSnapshotV2PlatformRestoreStage::Compatibility { .. }
            | HvfSnapshotV2PlatformRestoreStage::GlobalGic
            | HvfSnapshotV2PlatformRestoreStage::Vcpu { .. }
            | HvfSnapshotV2PlatformRestoreStage::Rtc
            | HvfSnapshotV2PlatformRestoreStage::PvTime { .. }
            | HvfSnapshotV2PlatformRestoreStage::TimeIdentityPreflight
            | HvfSnapshotV2PlatformRestoreStage::VmGenId
            | HvfSnapshotV2PlatformRestoreStage::VmClock => {}
        }
    }
    Ok(())
}

#[cfg(test)]
fn restore_protocol_stages(vcpu_count: usize) -> Vec<HvfSnapshotV2PlatformRestoreStage> {
    let mut stages = vec![
        HvfSnapshotV2PlatformRestoreStage::Preflight,
        HvfSnapshotV2PlatformRestoreStage::ProcessShell,
        HvfSnapshotV2PlatformRestoreStage::Vm,
        HvfSnapshotV2PlatformRestoreStage::Memory,
        HvfSnapshotV2PlatformRestoreStage::Gic,
        HvfSnapshotV2PlatformRestoreStage::Topology,
        HvfSnapshotV2PlatformRestoreStage::TopologyIdentity,
        HvfSnapshotV2PlatformRestoreStage::CpuTemplate,
    ];
    stages.extend(
        (0..vcpu_count).map(|index| HvfSnapshotV2PlatformRestoreStage::Compatibility { index }),
    );
    stages.push(HvfSnapshotV2PlatformRestoreStage::GlobalGic);
    stages.extend((0..vcpu_count).map(|index| HvfSnapshotV2PlatformRestoreStage::Vcpu { index }));
    stages.push(HvfSnapshotV2PlatformRestoreStage::Rtc);
    stages.extend((0..vcpu_count).map(|index| HvfSnapshotV2PlatformRestoreStage::PvTime { index }));
    stages.extend([
        HvfSnapshotV2PlatformRestoreStage::TimeIdentityPreflight,
        HvfSnapshotV2PlatformRestoreStage::VmGenId,
        HvfSnapshotV2PlatformRestoreStage::VmClock,
        HvfSnapshotV2PlatformRestoreStage::Lifecycle,
        HvfSnapshotV2PlatformRestoreStage::Publication,
    ]);
    stages
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_platform_fixture() -> HvfSnapshotV2PlatformState {
        let state = crate::snapshot_v2::tests::platform_fixture(false);
        let gic = state.global().compatibility().gic_metadata();
        let mut allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
            .expect("fixture GIC should provide an interrupt allocator");
        let _serial_interrupt = allocator
            .allocate()
            .expect("fixture serial interrupt should allocate");
        let vmgenid_interrupt = allocator
            .allocate()
            .expect("fixture VMGenID interrupt should allocate");
        let vmclock_interrupt = allocator
            .allocate()
            .expect("fixture VMClock interrupt should allocate");
        let (memory, machine, global, topology, vcpus, time) = state.into_parts();
        let boot = crate::snapshot_v2::HvfSnapshotV2BootState::try_new(
            machine.boot().kernel_path().clone(),
            None,
            machine.boot().boot_arguments(),
        )
        .expect("process fixture boot metadata should validate");
        let machine = HvfSnapshotV2MachineState::try_new(
            machine.machine(),
            boot,
            machine.fdt(),
            machine.cpu_template().cloned(),
        )
        .expect("process fixture machine metadata should validate");
        let (rtc, vmgenid, vmclock, vmclock_abi, pvtime) = time.into_parts();
        let vmgenid = bangbang_runtime::snapshot_device::SnapshotV1PlatformDeviceMetadata::new(
            vmgenid.range(),
            vmgenid.fdt_region(),
            vmgenid_interrupt,
        );
        let vmclock = bangbang_runtime::snapshot_device::SnapshotV1PlatformDeviceMetadata::new(
            vmclock.range(),
            vmclock.fdt_region(),
            vmclock_interrupt,
        );
        let time = HvfSnapshotV2TimeState::try_new(rtc, vmgenid, vmclock, vmclock_abi, pvtime)
            .expect("process fixture time metadata should validate");
        HvfSnapshotV2PlatformState::try_new(memory, machine, global, topology, vcpus, time)
            .expect("process fixture platform should cross-validate")
    }

    fn fixture_shell_devices(
        state: &HvfSnapshotV2PlatformState,
    ) -> (
        bangbang_runtime::fdt::Arm64FdtSerialDevice,
        bangbang_runtime::fdt::Arm64FdtRtcDevice,
        bangbang_runtime::fdt::Arm64FdtVmGenIdDevice,
        bangbang_runtime::fdt::Arm64FdtVmClockDevice,
    ) {
        let gic = state.global().compatibility().gic_metadata();
        let mut allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
            .expect("fixture GIC should provide an interrupt allocator");
        let serial_interrupt = allocator
            .allocate()
            .expect("fixture serial interrupt should allocate");
        let expected_vmgenid = allocator
            .allocate()
            .expect("fixture VMGenID interrupt should allocate");
        let expected_vmclock = allocator
            .allocate()
            .expect("fixture VMClock interrupt should allocate");
        assert_eq!(state.time().vmgenid().interrupt_line(), expected_vmgenid);
        assert_eq!(state.time().vmclock().interrupt_line(), expected_vmclock);
        let rtc_layout = state.time().rtc_layout();
        (
            bangbang_runtime::fdt::Arm64FdtSerialDevice {
                region: bangbang_runtime::fdt::Arm64FdtRegion {
                    base: PROCESS_SERIAL_MMIO_BASE.raw_value(),
                    size: SERIAL_MMIO_DEVICE_WINDOW_SIZE,
                },
                interrupt_line: serial_interrupt,
            },
            bangbang_runtime::fdt::Arm64FdtRtcDevice {
                region: bangbang_runtime::fdt::Arm64FdtRegion {
                    base: rtc_layout.base().raw_value(),
                    size: RTC_MMIO_DEVICE_WINDOW_SIZE,
                },
            },
            bangbang_runtime::fdt::Arm64FdtVmGenIdDevice {
                region: state.time().vmgenid().fdt_region(),
                interrupt_line: state.time().vmgenid().interrupt_line(),
            },
            bangbang_runtime::fdt::Arm64FdtVmClockDevice {
                region: state.time().vmclock().fdt_region(),
                interrupt_line: state.time().vmclock().interrupt_line(),
            },
        )
    }

    fn build_process_fdt_fixture(
        state: &HvfSnapshotV2PlatformState,
        serial: bangbang_runtime::fdt::Arm64FdtSerialDevice,
        rtc: bangbang_runtime::fdt::Arm64FdtRtcDevice,
        vmgenid: bangbang_runtime::fdt::Arm64FdtVmGenIdDevice,
        vmclock: bangbang_runtime::fdt::Arm64FdtVmClockDevice,
        optional_devices: &[bangbang_runtime::fdt::Arm64FdtVirtioMmioDevice],
    ) -> Vec<u8> {
        let ranges = state
            .memory()
            .extents()
            .iter()
            .map(|extent| extent.range())
            .collect();
        let layout = bangbang_runtime::memory::GuestMemoryLayout::new(ranges)
            .expect("fixture memory layout should validate");
        let cache = bangbang_runtime::fdt::Arm64FdtCache::new(
            1,
            bangbang_runtime::fdt::Arm64FdtCacheType::Unified,
            32_768,
            64,
            64,
            8,
            1,
        )
        .expect("fixture cache geometry should validate");
        let cache_hierarchy = bangbang_runtime::fdt::Arm64FdtCacheHierarchy::new(vec![cache])
            .expect("fixture cache hierarchy should validate");
        let mpidrs = state
            .topology()
            .members()
            .iter()
            .map(|member| member.mpidr())
            .collect::<Vec<_>>();
        let command_line = expected_process_boot_arguments(state.machine().boot().boot_arguments())
            .expect("fixture command line should normalize");
        let initrd =
            state
                .machine()
                .boot()
                .initrd_path()
                .map(|_| bangbang_runtime::boot::LoadedInitrd {
                    address: GuestAddress::new(aarch64::DRAM_MEM_START + 0x1_0000),
                    size: 4096,
                });
        let gic = state.global().compatibility().gic_metadata();
        bangbang_runtime::fdt::build_arm64_fdt(&bangbang_runtime::fdt::Arm64FdtConfig {
            layout: &layout,
            boot: bangbang_runtime::fdt::Arm64FdtBootInfo {
                command_line: &command_line,
                initrd,
            },
            vcpu_mpidrs: &mpidrs,
            cache_hierarchy: &cache_hierarchy,
            gic: gic.arm64_fdt_gic(),
            timer: gic
                .arm64_fdt_timer_interrupts()
                .expect("fixture timer metadata should validate"),
            rtc_device: Some(rtc),
            serial_device: Some(serial),
            vmgenid_device: Some(vmgenid),
            vmclock_device: Some(vmclock),
            virtio_mmio_devices: optional_devices,
        })
        .expect("fixture FDT should build")
    }

    #[test]
    fn exact_default_process_fdt_is_accepted_and_hostile_profiles_are_rejected() {
        let state = process_platform_fixture();
        let (serial, rtc, vmgenid, vmclock) = fixture_shell_devices(&state);
        let valid = build_process_fdt_fixture(&state, serial, rtc, vmgenid, vmclock, &[]);
        assert_eq!(
            validate_default_process_fdt(&valid, &state, serial.interrupt_line),
            Ok(())
        );
        assert_eq!(
            validate_default_process_fdt(b"not an FDT", &state, serial.interrupt_line),
            Err(HvfSnapshotV2ProcessFdtMismatch::Parse)
        );

        let optional = bangbang_runtime::fdt::Arm64FdtVirtioMmioDevice {
            region: bangbang_runtime::fdt::Arm64FdtRegion {
                base: 0x5000_0000,
                size: bangbang_runtime::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            },
            interrupt_line: bangbang_runtime::interrupt::GuestInterruptLine::new(
                vmclock.interrupt_line.raw_value() + 1,
            )
            .expect("fixture optional-device interrupt should validate"),
        };
        let with_optional =
            build_process_fdt_fixture(&state, serial, rtc, vmgenid, vmclock, &[optional]);
        assert_eq!(
            validate_default_process_fdt(&with_optional, &state, serial.interrupt_line),
            Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory)
        );
    }

    #[test]
    fn default_process_fdt_rejects_serial_rtc_and_identity_drift() {
        let state = process_platform_fixture();
        let (serial, rtc, vmgenid, vmclock) = fixture_shell_devices(&state);

        let wrong_serial_size = bangbang_runtime::fdt::Arm64FdtSerialDevice {
            region: bangbang_runtime::fdt::Arm64FdtRegion {
                size: SERIAL_MMIO_DEVICE_WINDOW_SIZE * 2,
                ..serial.region
            },
            ..serial
        };
        let bytes =
            build_process_fdt_fixture(&state, wrong_serial_size, rtc, vmgenid, vmclock, &[]);
        assert_eq!(
            validate_default_process_fdt(&bytes, &state, serial.interrupt_line),
            Err(HvfSnapshotV2ProcessFdtMismatch::Serial)
        );

        let wrong_serial_interrupt = bangbang_runtime::fdt::Arm64FdtSerialDevice {
            interrupt_line: bangbang_runtime::interrupt::GuestInterruptLine::new(
                vmclock.interrupt_line.raw_value() + 1,
            )
            .expect("fixture serial interrupt drift should validate"),
            ..serial
        };
        let bytes =
            build_process_fdt_fixture(&state, wrong_serial_interrupt, rtc, vmgenid, vmclock, &[]);
        assert_eq!(
            validate_default_process_fdt(&bytes, &state, serial.interrupt_line),
            Err(HvfSnapshotV2ProcessFdtMismatch::Serial)
        );

        let wrong_rtc_size = bangbang_runtime::fdt::Arm64FdtRtcDevice {
            region: bangbang_runtime::fdt::Arm64FdtRegion {
                size: RTC_MMIO_DEVICE_WINDOW_SIZE / 2,
                ..rtc.region
            },
        };
        let bytes =
            build_process_fdt_fixture(&state, serial, wrong_rtc_size, vmgenid, vmclock, &[]);
        assert_eq!(
            validate_default_process_fdt(&bytes, &state, serial.interrupt_line),
            Err(HvfSnapshotV2ProcessFdtMismatch::Rtc)
        );

        let wrong_vmgenid = bangbang_runtime::fdt::Arm64FdtVmGenIdDevice {
            interrupt_line: bangbang_runtime::interrupt::GuestInterruptLine::new(
                vmclock.interrupt_line.raw_value() + 1,
            )
            .expect("fixture VMGenID interrupt drift should validate"),
            ..vmgenid
        };
        let bytes = build_process_fdt_fixture(&state, serial, rtc, wrong_vmgenid, vmclock, &[]);
        assert_eq!(
            validate_default_process_fdt(&bytes, &state, serial.interrupt_line),
            Err(HvfSnapshotV2ProcessFdtMismatch::VmGenId)
        );

        let wrong_vmclock = bangbang_runtime::fdt::Arm64FdtVmClockDevice {
            interrupt_line: bangbang_runtime::interrupt::GuestInterruptLine::new(
                vmclock.interrupt_line.raw_value() + 2,
            )
            .expect("fixture VMClock interrupt drift should validate"),
            ..vmclock
        };
        let bytes = build_process_fdt_fixture(&state, serial, rtc, vmgenid, wrong_vmclock, &[]);
        assert_eq!(
            validate_default_process_fdt(&bytes, &state, serial.interrupt_line),
            Err(HvfSnapshotV2ProcessFdtMismatch::VmClock)
        );
    }

    #[test]
    fn process_fdt_memory_omits_the_reserved_arm64_system_area() {
        let range = GuestMemoryRange::new(
            GuestAddress::new(aarch64::DRAM_MEM_START),
            aarch64::SYSTEM_MEM_SIZE * 2,
        )
        .expect("test memory range should validate");
        let layout = bangbang_runtime::memory::GuestMemoryLayout::new(vec![range])
            .expect("test memory layout should validate");
        let memory = GuestMemory::allocate(&layout).expect("test guest memory should allocate");
        let mut image = std::io::Cursor::new(Vec::new());
        let binding = write_snapshot_v2_memory_image(&memory, &mut image)
            .expect("test native-v2 binding should encode");
        let advertised_start = aarch64::DRAM_MEM_START + aarch64::SYSTEM_MEM_SIZE;
        let advertised_size = range.size() - aarch64::SYSTEM_MEM_SIZE;
        let mut advertised = Vec::with_capacity(16);
        advertised.extend_from_slice(&advertised_start.to_be_bytes());
        advertised.extend_from_slice(&advertised_size.to_be_bytes());
        let node = Node {
            name: "memory@ram".to_owned(),
            props: vec![("reg".to_owned(), advertised)],
            children: Vec::new(),
        };

        assert!(memory_property_matches_binding(&node, &binding));

        let mut unadjusted = Vec::with_capacity(16);
        unadjusted.extend_from_slice(&range.start().raw_value().to_be_bytes());
        unadjusted.extend_from_slice(&range.size().to_be_bytes());
        let unadjusted_node = Node {
            name: "memory@ram".to_owned(),
            props: vec![("reg".to_owned(), unadjusted)],
            children: Vec::new(),
        };
        assert!(!memory_property_matches_binding(&unadjusted_node, &binding));
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ProtocolEvent {
        Execute(HvfSnapshotV2PlatformRestoreStage),
        Cleanup(HvfSnapshotV2PlatformCleanupStage),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Injected;

    impl fmt::Display for Injected {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("injected")
        }
    }

    impl std::error::Error for Injected {}

    struct FakeProtocol {
        fail_stage: Option<HvfSnapshotV2PlatformRestoreStage>,
        fail_cleanup: Vec<HvfSnapshotV2PlatformCleanupStage>,
        events: Vec<ProtocolEvent>,
    }

    impl RestoreProtocol for FakeProtocol {
        type Error = Injected;

        fn execute(&mut self, stage: HvfSnapshotV2PlatformRestoreStage) -> Result<(), Self::Error> {
            self.events.push(ProtocolEvent::Execute(stage));
            if self.fail_stage == Some(stage) {
                Err(Injected)
            } else {
                Ok(())
            }
        }

        fn cleanup(&mut self, stage: HvfSnapshotV2PlatformCleanupStage) -> Result<(), Self::Error> {
            self.events.push(ProtocolEvent::Cleanup(stage));
            if self.fail_cleanup.contains(&stage) {
                Err(Injected)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn every_restore_stage_cleans_the_exact_owned_prefix_once() {
        let stages = restore_protocol_stages(3);
        for (failed_position, failed_stage) in stages.iter().copied().enumerate() {
            let mut protocol = FakeProtocol {
                fail_stage: Some(failed_stage),
                fail_cleanup: Vec::new(),
                events: Vec::new(),
            };
            let error = run_restore_protocol(&mut protocol, 3)
                .expect_err("injected stage should fail the protocol");
            assert_eq!(error.stage, failed_stage);
            assert!(error.cleanup.is_empty());

            let mut expected = stages
                .iter()
                .take(failed_position + 1)
                .copied()
                .map(ProtocolEvent::Execute)
                .collect::<Vec<_>>();
            expected.extend(
                cleanup_sequence(restore_ownership_before_failure(failed_stage))
                    .iter()
                    .copied()
                    .map(ProtocolEvent::Cleanup),
            );
            assert_eq!(protocol.events, expected);
        }
    }

    #[test]
    fn cleanup_failures_are_retained_without_skipping_later_cleanup() {
        let failed_stage = HvfSnapshotV2PlatformRestoreStage::Vcpu { index: 1 };
        let mut protocol = FakeProtocol {
            fail_stage: Some(failed_stage),
            fail_cleanup: vec![
                HvfSnapshotV2PlatformCleanupStage::Topology,
                HvfSnapshotV2PlatformCleanupStage::Backend,
            ],
            events: Vec::new(),
        };
        let error = run_restore_protocol(&mut protocol, 3)
            .expect_err("injected vCPU stage should fail the protocol");
        assert_eq!(
            error.cleanup,
            vec![
                (HvfSnapshotV2PlatformCleanupStage::Topology, Injected),
                (HvfSnapshotV2PlatformCleanupStage::Backend, Injected),
            ]
        );
        assert_eq!(
            protocol
                .events
                .iter()
                .rev()
                .take(2)
                .copied()
                .collect::<Vec<_>>(),
            vec![
                ProtocolEvent::Cleanup(HvfSnapshotV2PlatformCleanupStage::Backend),
                ProtocolEvent::Cleanup(HvfSnapshotV2PlatformCleanupStage::Topology),
            ]
        );
    }

    #[test]
    fn publication_failure_shuts_session_before_backend() {
        let mut protocol = FakeProtocol {
            fail_stage: Some(HvfSnapshotV2PlatformRestoreStage::Publication),
            fail_cleanup: Vec::new(),
            events: Vec::new(),
        };
        run_restore_protocol(&mut protocol, 2).expect_err("injected publication stage should fail");
        assert_eq!(
            protocol
                .events
                .iter()
                .rev()
                .take(2)
                .copied()
                .collect::<Vec<_>>(),
            vec![
                ProtocolEvent::Cleanup(HvfSnapshotV2PlatformCleanupStage::Backend),
                ProtocolEvent::Cleanup(HvfSnapshotV2PlatformCleanupStage::Session),
            ]
        );
    }

    #[test]
    fn successful_protocol_publishes_without_cleanup() {
        let mut protocol = FakeProtocol {
            fail_stage: None,
            fail_cleanup: Vec::new(),
            events: Vec::new(),
        };
        run_restore_protocol(&mut protocol, 3).expect("complete protocol should publish");
        assert_eq!(
            protocol.events,
            restore_protocol_stages(3)
                .into_iter()
                .map(ProtocolEvent::Execute)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn restore_error_marks_the_first_guest_visible_identity_commit_boundary() {
        let before_identity = HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::Preflight,
            HvfSnapshotV2PlatformRestoreFailure::MemoryTopology,
            Vec::new(),
        );
        assert!(!before_identity.is_committed());

        let replacement_failure = HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::VmGenId,
            HvfSnapshotV2PlatformRestoreFailure::VmGenId(
                HvfArm64BootVmGenIdRestoreError::Replacement {
                    source: bangbang_runtime::startup::Arm64BootVmGenIdReplacementError::Random,
                },
            ),
            Vec::new(),
        );
        assert!(!replacement_failure.is_committed());

        let vmgenid_signal_failure = HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::VmGenId,
            HvfSnapshotV2PlatformRestoreFailure::VmGenId(HvfArm64BootVmGenIdRestoreError::Signal {
                source: HvfGicSpiSignalError::InvalidState("injected"),
            }),
            Vec::new(),
        );
        assert!(vmgenid_signal_failure.is_committed());

        let vmclock_failure = HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::VmClock,
            HvfSnapshotV2PlatformRestoreFailure::VmClock(HvfArm64BootVmClockRestoreError::Update {
                source: bangbang_runtime::vmclock::VmClockRestoreUpdateError::InvalidRange,
            }),
            Vec::new(),
        );
        assert!(vmclock_failure.is_committed());
        assert!(
            vmclock_failure
                .to_string()
                .contains("after destination identity committed")
        );
    }

    #[test]
    fn restore_diagnostics_redact_sensitive_values() {
        let failure = HvfSnapshotV2PlatformRestoreFailure::CompatibilityMismatch;
        let error = HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::Compatibility { index: 2 },
            failure,
            Vec::new(),
        );
        let debug = format!("{error:?}");
        let display = error.to_string();
        assert!(debug.contains(REDACTED));
        for secret in ["deadbeef", "12345678", "guest/path", "gic-bytes"] {
            assert!(!debug.contains(secret));
            assert!(!display.contains(secret));
        }
    }
}
