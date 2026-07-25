//! Unpublished native-v2 multi-vCPU HVF platform reconstruction.

use std::fmt;
use std::io::{Seek, Write};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

use bangbang_runtime::memory::{
    GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryRange, aarch64,
};
use bangbang_runtime::mmio::MmioDispatcher;
use bangbang_runtime::pvtime::{
    ARM64_PVTIME_STOLEN_TIME_OFFSET, ARM64_PVTIME_STRUCTURE_SIZE, Arm64PvTimeLayout,
    Arm64PvTimeStAbi,
};
use bangbang_runtime::rtc::RtcMmioLayout;
use bangbang_runtime::snapshot_memory_v2::{
    SnapshotV2MemoryBinding, write_snapshot_v2_memory_image,
};
use bangbang_runtime::startup::{
    Arm64BootResourceError, Arm64BootRtcDevice, Arm64BootVmClockDevice, Arm64BootVmGenIdDevice,
    PrepareArm64SnapshotTimeIdentityError, prepare_arm64_snapshot_time_identity,
    register_arm64_boot_rtc_mmio, replace_arm64_boot_vmgenid,
};
use bangbang_runtime::{BackendError, VmBackend};
use crc64::crc64;

use crate::backend::HvfBackend;
use crate::coordinator::{HvfVcpuRunControl, HvfVcpuRunCoordinatorError};
use crate::cpu_template::HvfArm64CpuTemplateError;
use crate::dirty::HvfDirtyWriteTrackerStartError;
use crate::gic::{
    HvfGicError, HvfGicMetadata, HvfGicMsiConfiguration, HvfGicSpiSignalError, HvfGicSpiSignaler,
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

/// Ordered reconstruction stage for the unpublished native-v2 platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfSnapshotV2PlatformRestoreStage {
    /// Validate supplied memory, FDT identity, cache identity, and cleanup storage.
    Preflight,
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
        f.debug_struct("HvfSnapshotV2PlatformRestoreFailure")
            .field("category", &self.category())
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
    if let Err(failure) = verify_fdt_identity(&memory, state.machine()) {
        return Err(HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::Preflight,
            failure,
            cleanup,
        ));
    }
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

    let mut dispatcher = MmioDispatcher::new();
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
        HvfSnapshotV2PlatformRestoreStage::Preflight => RestoreOwnership::Empty,
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

fn verify_fdt_identity(
    memory: &GuestMemory,
    machine: &HvfSnapshotV2MachineState,
) -> Result<(), HvfSnapshotV2PlatformRestoreFailure> {
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
    Ok(())
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
