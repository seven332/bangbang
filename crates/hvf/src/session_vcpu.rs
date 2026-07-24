use std::collections::VecDeque;
use std::fmt;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use bangbang_runtime::memory::GuestAddress;
use bangbang_runtime::mmio::MmioDispatcher;

use crate::HvfVcpuRunStepOutcome;
use crate::coordinator::{
    HvfVcpuCoordinatorWork, HvfVcpuRunAdmission, HvfVcpuRunControl, HvfVcpuRunControlReason,
    HvfVcpuRunCoordinator, HvfVcpuRunCoordinatorError, HvfVcpuRunEvent, HvfVcpuRunMemberOutcome,
    HvfVcpuRunMemberResult, HvfVcpuRunTerminalReport, HvfVcpuStableMemberDispatch,
    HvfVcpuStablePausedMemberObservation,
};
use crate::memory::HvfGuestMemoryMappingError;
use crate::paused_topology::{
    HvfArm64StablePausedTopologyMember, HvfArm64StablePausedTopologyState,
    HvfArm64StableVcpuDisposition,
};
use crate::psci::{
    PsciCoordinatorRequest, PsciCoordinatorResponse, PsciCpuOffBegin, PsciCpuOnBegin,
    PsciCpuOnToken, PsciCpuOnWork, PsciCpuPowerCoordinator, PsciCpuPowerState,
    PsciCpuStableMemberObservation, PsciCpuSuspendResponse, PsciCpuSuspendToken,
};
use crate::pvtime::HvfArm64PvTimeCaptureState;
use crate::runner::{
    HvfArm64SnapshotV2VcpuCapture, HvfVcpuRetainedVtimerWaitOutcome, HvfVcpuRunner,
    HvfVcpuRunnerError,
};
use crate::topology::HvfVcpuTopology;
use crate::vcpu::HvfArm64SecondaryBootRegisters;

/// Failure while translating aggregate vCPU events into boot-session steps.
#[derive(Debug)]
pub enum HvfArm64BootVcpuError {
    /// The mapped guest memory needed for PSCI entry validation is unavailable.
    GuestMemory {
        source: Box<HvfGuestMemoryMappingError>,
    },
    /// One identified owner-thread run failed.
    Member {
        index: usize,
        mpidr: u64,
        generation: u64,
        source: Box<HvfVcpuRunnerError>,
    },
    /// The aggregate coordinator rejected an indexed lifecycle operation.
    Coordinator {
        stage: &'static str,
        index: usize,
        mpidr: u64,
        cleanup_failed: bool,
        source: Box<HvfVcpuRunCoordinatorError>,
    },
    /// The PSCI power transaction rejected an internal transition.
    Power {
        stage: &'static str,
        index: usize,
        mpidr: u64,
    },
}

impl fmt::Display for HvfArm64BootVcpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GuestMemory { source } => {
                write!(f, "boot-session vCPU guest-memory access failed: {source}")
            }
            Self::Member {
                index,
                mpidr,
                generation,
                source,
            } => write!(
                f,
                "boot-session vCPU {index} (MPIDR 0x{mpidr:x}) generation {generation} failed: {source}"
            ),
            Self::Coordinator {
                stage,
                index,
                mpidr,
                cleanup_failed,
                source,
            } => write!(
                f,
                "boot-session vCPU {index} (MPIDR 0x{mpidr:x}) {stage} failed (cleanup_failed={cleanup_failed}): {source}"
            ),
            Self::Power {
                stage,
                index,
                mpidr,
            } => write!(
                f,
                "boot-session vCPU {index} (MPIDR 0x{mpidr:x}) PSCI transaction failed during {stage}"
            ),
        }
    }
}

impl std::error::Error for HvfArm64BootVcpuError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::GuestMemory { source } => Some(source.as_ref()),
            Self::Member { source, .. } => Some(source.as_ref()),
            Self::Coordinator { source, .. } => Some(source.as_ref()),
            Self::Power { .. } => None,
        }
    }
}

impl From<HvfVcpuRunnerError> for HvfArm64BootVcpuError {
    fn from(source: HvfVcpuRunnerError) -> Self {
        Self::Member {
            index: 0,
            mpidr: 0,
            generation: 0,
            source: Box::new(source),
        }
    }
}

/// Failure while exporting one completed paused vCPU lifecycle graph.
pub struct HvfArm64StablePausedTopologyCaptureError {
    stage: &'static str,
    index: Option<usize>,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl HvfArm64StablePausedTopologyCaptureError {
    fn new(stage: &'static str, index: Option<usize>) -> Self {
        Self {
            stage,
            index,
            source: None,
        }
    }

    fn with_source(
        stage: &'static str,
        index: Option<usize>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            stage,
            index,
            source: Some(Box::new(source)),
        }
    }

    /// Return the value-free capture stage.
    pub const fn stage(&self) -> &'static str {
        self.stage
    }

    /// Return the affected topology member, when the stage is member-scoped.
    pub const fn index(&self) -> Option<usize> {
        self.index
    }
}

impl fmt::Debug for HvfArm64StablePausedTopologyCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64StablePausedTopologyCaptureError")
            .field("stage", &self.stage)
            .field("index", &self.index)
            .field("source", &self.source.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl fmt::Display for HvfArm64StablePausedTopologyCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stable paused topology capture failed at {}", self.stage)?;
        if let Some(index) = self.index {
            write!(f, " for member {index}")?;
        }
        Ok(())
    }
}

impl std::error::Error for HvfArm64StablePausedTopologyCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn std::error::Error + 'static))
    }
}

/// Cleanup operation attempted after a stable paused topology import failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64StablePausedTopologyCleanupStage {
    RunnerAbort,
    CoordinatorDispatch,
    TopologyShutdown,
}

impl fmt::Display for HvfArm64StablePausedTopologyCleanupStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RunnerAbort => f.write_str("runner abort"),
            Self::CoordinatorDispatch => f.write_str("coordinator dispatch rollback"),
            Self::TopologyShutdown => f.write_str("topology shutdown"),
        }
    }
}

/// One value-free cleanup failure retained by an import error.
pub struct HvfArm64StablePausedTopologyCleanupFailure {
    stage: HvfArm64StablePausedTopologyCleanupStage,
    index: Option<usize>,
    source: Box<dyn std::error::Error + Send + Sync + 'static>,
}

impl HvfArm64StablePausedTopologyCleanupFailure {
    /// Return the failed cleanup class.
    pub const fn stage(&self) -> HvfArm64StablePausedTopologyCleanupStage {
        self.stage
    }

    /// Return the affected member for member-scoped cleanup.
    pub const fn index(&self) -> Option<usize> {
        self.index
    }

    /// Return the detailed internal cleanup source.
    pub fn source_error(&self) -> &(dyn std::error::Error + 'static) {
        self.source.as_ref()
    }
}

impl fmt::Debug for HvfArm64StablePausedTopologyCleanupFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64StablePausedTopologyCleanupFailure")
            .field("stage", &self.stage)
            .field("index", &self.index)
            .field("source", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for HvfArm64StablePausedTopologyCleanupFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stable paused topology cleanup failed at {}", self.stage)?;
        if let Some(index) = self.index {
            write!(f, " for member {index}")?;
        }
        Ok(())
    }
}

impl std::error::Error for HvfArm64StablePausedTopologyCleanupFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Failure while consuming a destination topology into paused lifecycle state.
pub struct HvfArm64StablePausedTopologyImportError {
    stage: &'static str,
    index: Option<usize>,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
    cleanup: Vec<HvfArm64StablePausedTopologyCleanupFailure>,
}

/// Failure while capturing every native-v2 vCPU around one stable pause.
#[derive(Debug)]
pub enum HvfArm64SnapshotV2TopologyCaptureError {
    Lifecycle {
        stage: &'static str,
        source: Box<HvfArm64StablePausedTopologyCaptureError>,
    },
    Member {
        index: usize,
        source: Box<HvfVcpuRunCoordinatorError>,
    },
    Allocation,
    LifecycleChanged,
}

impl fmt::Display for HvfArm64SnapshotV2TopologyCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lifecycle { stage, source } => {
                write!(f, "native-v2 topology {stage} failed: {source}")
            }
            Self::Member { index, source } => {
                write!(f, "native-v2 vCPU {index} capture failed: {source}")
            }
            Self::Allocation => f.write_str("native-v2 topology capture allocation failed"),
            Self::LifecycleChanged => {
                f.write_str("native-v2 lifecycle changed during topology capture")
            }
        }
    }
}

impl std::error::Error for HvfArm64SnapshotV2TopologyCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Lifecycle { source, .. } => Some(source.as_ref()),
            Self::Member { source, .. } => Some(source.as_ref()),
            Self::Allocation | Self::LifecycleChanged => None,
        }
    }
}

impl HvfArm64StablePausedTopologyImportError {
    fn new(stage: &'static str, index: Option<usize>) -> Self {
        Self {
            stage,
            index,
            source: None,
            cleanup: Vec::new(),
        }
    }

    fn with_source(
        stage: &'static str,
        index: Option<usize>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            stage,
            index,
            source: Some(Box::new(source)),
            cleanup: Vec::new(),
        }
    }

    fn with_cleanup_storage(
        mut self,
        cleanup: Vec<HvfArm64StablePausedTopologyCleanupFailure>,
    ) -> Self {
        self.cleanup = cleanup;
        self
    }

    /// Return the value-free primary import stage.
    pub const fn stage(&self) -> &'static str {
        self.stage
    }

    /// Return the affected topology member, when applicable.
    pub const fn index(&self) -> Option<usize> {
        self.index
    }

    /// Return every reverse cleanup failure in attempt order.
    pub fn cleanup_failures(&self) -> &[HvfArm64StablePausedTopologyCleanupFailure] {
        &self.cleanup
    }
}

impl fmt::Debug for HvfArm64StablePausedTopologyImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64StablePausedTopologyImportError")
            .field("stage", &self.stage)
            .field("index", &self.index)
            .field("source", &self.source.as_ref().map(|_| "<redacted>"))
            .field("cleanup", &self.cleanup)
            .finish()
    }
}

impl fmt::Display for HvfArm64StablePausedTopologyImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stable paused topology import failed at {}", self.stage)?;
        if let Some(index) = self.index {
            write!(f, " for member {index}")?;
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

impl std::error::Error for HvfArm64StablePausedTopologyImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn std::error::Error + 'static))
    }
}

#[derive(Debug)]
struct IndexedBootStep {
    index: usize,
    outcome: HvfVcpuRunStepOutcome,
}

#[derive(Debug, Clone, Copy)]
struct PendingCpuSuspend {
    power_token: PsciCpuSuspendToken,
    coordinator_work: HvfVcpuCoordinatorWork,
}

#[derive(Debug, Clone, Copy)]
struct InstalledStableCpuSuspend {
    index: usize,
    runner_token: crate::runner::HvfVcpuPsciCallToken,
    dispatch_installed: bool,
}

/// Boot-session vCPU aggregate that owns every runner and its PSCI power model.
///
/// The stable paused topology methods expose the in-memory lifecycle
/// foundation used by snapshot reconstruction. They do not encode snapshot
/// bytes or restore memory, devices, registers, or time state.
pub struct HvfArm64BootVcpuSession<'vm> {
    coordinator: HvfVcpuRunCoordinator<'vm>,
    power: PsciCpuPowerCoordinator,
    virtual_timer_intid: u32,
    pending_cpu_suspends: Vec<Option<PendingCpuSuspend>>,
    pending_steps: VecDeque<Result<IndexedBootStep, HvfArm64BootVcpuError>>,
    last_step_index: usize,
    last_terminal_report: Option<HvfVcpuRunTerminalReport>,
}

impl fmt::Debug for HvfArm64BootVcpuSession<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64BootVcpuSession")
            .field("member_count", &self.coordinator.member_count())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl<'vm> HvfArm64BootVcpuSession<'vm> {
    pub(crate) fn new(
        coordinator: HvfVcpuRunCoordinator<'vm>,
        power: PsciCpuPowerCoordinator,
        virtual_timer_intid: u32,
    ) -> Self {
        let pending_cpu_suspends = vec![None; coordinator.member_count()];
        Self {
            coordinator,
            power,
            virtual_timer_intid,
            pending_cpu_suspends,
            pending_steps: VecDeque::new(),
            last_step_index: 0,
            last_terminal_report: None,
        }
    }

    pub(crate) fn from_restored_runner(
        runner: HvfVcpuRunner<'vm>,
        mpidr: u64,
        dispatcher: Arc<Mutex<MmioDispatcher>>,
        virtual_timer_intid: u32,
    ) -> Result<Self, HvfVcpuRunCoordinatorError> {
        let coordinator = HvfVcpuRunCoordinator::from_runner(runner, mpidr, dispatcher, true)?;
        let power = PsciCpuPowerCoordinator::new(&[mpidr]).map_err(|_| {
            HvfVcpuRunCoordinatorError::InvalidState(
                "restored vCPU power topology is incompatible with its MPIDR",
            )
        })?;
        Ok(Self::new(coordinator, power, virtual_timer_intid))
    }

    pub(crate) fn member_count(&self) -> usize {
        self.coordinator.member_count()
    }

    pub(crate) fn mpidrs(&self) -> &[u64] {
        self.coordinator.mpidrs()
    }

    pub(crate) fn primary_mpidr(&self) -> u64 {
        self.coordinator.primary_mpidr()
    }

    pub(crate) fn shutdown(&mut self) -> Result<(), HvfVcpuRunCoordinatorError> {
        self.coordinator.shutdown()
    }

    pub(crate) fn control(&self) -> HvfVcpuRunControl {
        self.coordinator.control()
    }

    pub(crate) fn capture_arm64_pvtime(
        &self,
    ) -> Result<HvfArm64PvTimeCaptureState, HvfVcpuRunCoordinatorError> {
        self.coordinator.capture_arm64_pvtime()
    }

    pub(crate) fn pause_idle_for_arm64_pvtime_capture(
        &self,
    ) -> Result<(), HvfVcpuRunCoordinatorError> {
        self.coordinator.pause_idle_for_arm64_pvtime_capture()
    }

    pub(crate) fn pause_for_arm64_snapshot_v2_capture(
        &mut self,
    ) -> Result<(), HvfArm64BootVcpuError> {
        if !self.pending_steps.is_empty() {
            return Err(self.power_error("native-v2 pause pending-step validation", 0));
        }
        let waiter = self
            .control()
            .request_pause()
            .map_err(|source| self.coordinator_error("native-v2 pause request", 0, source))?;
        let mut invalid_index = None;
        loop {
            let event = self.coordinator.receive_event().map_err(|source| {
                self.coordinator_error("native-v2 pause collection", 0, source)
            })?;
            let completed = matches!(event, HvfVcpuRunEvent::Barrier(_));
            self.enqueue_event(event, &mut |_| false)?;
            while let Some(step) = self.pending_steps.pop_front() {
                match step? {
                    IndexedBootStep {
                        outcome: HvfVcpuRunStepOutcome::Canceled,
                        ..
                    } => {}
                    IndexedBootStep { index, .. } => {
                        invalid_index.get_or_insert(index);
                    }
                }
            }
            if completed {
                break;
            }
        }
        waiter
            .wait()
            .map_err(|source| self.coordinator_error("native-v2 pause completion", 0, source))?;
        if let Some(index) = invalid_index {
            return Err(self.power_error("native-v2 pause observed guest progress", index));
        }
        Ok(())
    }

    /// Export one fully quiesced paused lifecycle graph.
    pub fn capture_stable_paused_topology(
        &mut self,
    ) -> Result<HvfArm64StablePausedTopologyState, HvfArm64StablePausedTopologyCaptureError> {
        if !self.pending_steps.is_empty() {
            return Err(HvfArm64StablePausedTopologyCaptureError::new(
                "pending boot-session step",
                None,
            ));
        }
        if self.last_terminal_report.is_some() {
            return Err(HvfArm64StablePausedTopologyCaptureError::new(
                "terminal boot-session state",
                None,
            ));
        }

        let before = self
            .coordinator
            .capture_stable_paused_members()
            .map_err(|source| {
                HvfArm64StablePausedTopologyCaptureError::with_source(
                    "coordinator pause validation",
                    None,
                    source,
                )
            })?;
        if before.len() != self.pending_cpu_suspends.len()
            || before.len() != self.coordinator.member_count()
        {
            return Err(HvfArm64StablePausedTopologyCaptureError::new(
                "cross-owner member count",
                None,
            ));
        }

        let mut members = Vec::new();
        members.try_reserve_exact(before.len()).map_err(|source| {
            HvfArm64StablePausedTopologyCaptureError::with_source(
                "stable member allocation",
                None,
                source,
            )
        })?;
        for coordinator in before.iter().copied() {
            let index = coordinator.index();
            let power = self
                .power
                .stable_member_observation(index)
                .map_err(|source| {
                    HvfArm64StablePausedTopologyCaptureError::with_source(
                        "PSCI power observation",
                        Some(index),
                        source,
                    )
                })?;
            let disposition = self.capture_stable_member_disposition(coordinator, power)?;
            members.push(HvfArm64StablePausedTopologyMember::new(
                index,
                coordinator.mpidr(),
                disposition,
            ));
        }

        let after = self
            .coordinator
            .capture_stable_paused_members()
            .map_err(|source| {
                HvfArm64StablePausedTopologyCaptureError::with_source(
                    "coordinator revalidation",
                    None,
                    source,
                )
            })?;
        if before != after || !self.pending_steps.is_empty() || self.last_terminal_report.is_some()
        {
            return Err(HvfArm64StablePausedTopologyCaptureError::new(
                "cross-owner revalidation",
                None,
            ));
        }

        HvfArm64StablePausedTopologyState::new(self.virtual_timer_intid, members).map_err(
            |source| {
                HvfArm64StablePausedTopologyCaptureError::with_source(
                    "stable value construction",
                    None,
                    source,
                )
            },
        )
    }

    pub(crate) fn capture_arm64_snapshot_v2_topology(
        &mut self,
    ) -> Result<
        (
            HvfArm64StablePausedTopologyState,
            Vec<HvfArm64SnapshotV2VcpuCapture>,
        ),
        HvfArm64SnapshotV2TopologyCaptureError,
    > {
        let stable = self.capture_stable_paused_topology().map_err(|source| {
            HvfArm64SnapshotV2TopologyCaptureError::Lifecycle {
                stage: "initial lifecycle capture",
                source: Box::new(source),
            }
        })?;
        let mut captures = Vec::new();
        captures
            .try_reserve_exact(stable.members().len())
            .map_err(|_| HvfArm64SnapshotV2TopologyCaptureError::Allocation)?;
        for member in stable.members() {
            let index = member.index();
            let capture = self
                .coordinator
                .capture_arm64_snapshot_v2_vcpu(index, member.disposition())
                .map_err(|source| HvfArm64SnapshotV2TopologyCaptureError::Member {
                    index,
                    source: Box::new(source),
                })?;
            captures.push(capture);
        }
        let after = self.capture_stable_paused_topology().map_err(|source| {
            HvfArm64SnapshotV2TopologyCaptureError::Lifecycle {
                stage: "final lifecycle capture",
                source: Box::new(source),
            }
        })?;
        if after != stable {
            return Err(HvfArm64SnapshotV2TopologyCaptureError::LifecycleChanged);
        }
        Ok((stable, captures))
    }

    /// Consume never-run destination owners into one coordinator born paused.
    ///
    /// Register restore must already have completed on every owner. Any
    /// failure consumes and shuts down the topology; callers must create fresh
    /// destination owners before retrying.
    pub fn from_stable_paused_topology(
        topology: HvfVcpuTopology<'vm>,
        stable: &HvfArm64StablePausedTopologyState,
        dispatcher: Arc<Mutex<MmioDispatcher>>,
        virtual_timer_intid: u32,
    ) -> Result<Self, HvfArm64StablePausedTopologyImportError> {
        if stable.virtual_timer_intid() != virtual_timer_intid {
            let mut error =
                HvfArm64StablePausedTopologyImportError::new("virtual timer PPI validation", None);
            shutdown_unmodified_stable_import(&topology, &mut error);
            return Err(error);
        }
        if topology.len() != stable.members().len() {
            let mut error = HvfArm64StablePausedTopologyImportError::new(
                "destination member count validation",
                None,
            );
            shutdown_unmodified_stable_import(&topology, &mut error);
            return Err(error);
        }
        for (index, (mpidr, member)) in topology.mpidrs().iter().zip(stable.members()).enumerate() {
            if member.index() != index || member.mpidr() != *mpidr {
                let mut error = HvfArm64StablePausedTopologyImportError::new(
                    "destination MPIDR validation",
                    Some(index),
                );
                shutdown_unmodified_stable_import(&topology, &mut error);
                return Err(error);
            }
        }
        for index in 0..stable.members().len() {
            if let Err(source) = topology.ensure_stable_import_ready(index) {
                let mut error = HvfArm64StablePausedTopologyImportError::with_source(
                    "destination owner readiness",
                    Some(index),
                    source,
                );
                shutdown_unmodified_stable_import(&topology, &mut error);
                return Err(error);
            }
        }

        let mut pending_cpu_suspends = Vec::new();
        if let Err(source) = pending_cpu_suspends.try_reserve_exact(stable.members().len()) {
            let mut error = HvfArm64StablePausedTopologyImportError::with_source(
                "pending CPU_SUSPEND allocation",
                None,
                source,
            );
            shutdown_unmodified_stable_import(&topology, &mut error);
            return Err(error);
        }
        pending_cpu_suspends.resize(stable.members().len(), None);
        let mut installed = Vec::new();
        if let Err(source) = installed.try_reserve_exact(stable.members().len()) {
            let mut error = HvfArm64StablePausedTopologyImportError::with_source(
                "rollback allocation",
                None,
                source,
            );
            shutdown_unmodified_stable_import(&topology, &mut error);
            return Err(error);
        }
        let cleanup_capacity = stable
            .members()
            .len()
            .checked_mul(2)
            .and_then(|capacity| capacity.checked_add(1))
            .ok_or_else(|| {
                HvfArm64StablePausedTopologyImportError::new("cleanup allocation size", None)
            });
        let cleanup_capacity = match cleanup_capacity {
            Ok(capacity) => capacity,
            Err(mut error) => {
                shutdown_unmodified_stable_import(&topology, &mut error);
                return Err(error);
            }
        };
        let mut rollback_cleanup = Vec::new();
        if let Err(source) = rollback_cleanup.try_reserve_exact(cleanup_capacity) {
            let mut error = HvfArm64StablePausedTopologyImportError::with_source(
                "cleanup allocation",
                None,
                source,
            );
            shutdown_unmodified_stable_import(&topology, &mut error);
            return Err(error);
        }
        let (power, power_suspends) =
            match PsciCpuPowerCoordinator::from_stable_paused_topology(stable) {
                Ok(prepared) => prepared,
                Err(source) => {
                    let mut error = HvfArm64StablePausedTopologyImportError::with_source(
                        "PSCI power preparation",
                        None,
                        source,
                    );
                    shutdown_unmodified_stable_import(&topology, &mut error);
                    return Err(error);
                }
            };
        let mut coordinator =
            HvfVcpuRunCoordinator::from_stable_paused_topology(topology, dispatcher, stable)
                .map_err(|source| {
                    HvfArm64StablePausedTopologyImportError::with_source(
                        "paused coordinator construction",
                        None,
                        source,
                    )
                })?;

        for member in stable.members() {
            let index = member.index();
            let HvfArm64StableVcpuDisposition::Suspended(stable_suspend) = member.disposition()
            else {
                if power_suspends.get(index).copied().flatten().is_some() {
                    let mut error = HvfArm64StablePausedTopologyImportError::new(
                        "PSCI power preparation identity",
                        Some(index),
                    )
                    .with_cleanup_storage(rollback_cleanup);
                    rollback_stable_import(&mut coordinator, &installed, &mut error);
                    return Err(error);
                }
                continue;
            };
            let Some(power_suspend) = power_suspends.get(index).copied().flatten() else {
                let mut error = HvfArm64StablePausedTopologyImportError::new(
                    "PSCI CPU_SUSPEND preparation",
                    Some(index),
                )
                .with_cleanup_storage(rollback_cleanup);
                rollback_stable_import(&mut coordinator, &installed, &mut error);
                return Err(error);
            };
            let runner_token = match coordinator.restore_stable_cpu_suspend(index, stable_suspend) {
                Ok(token) => token,
                Err(source) => {
                    let mut error = HvfArm64StablePausedTopologyImportError::with_source(
                        "runner CPU_SUSPEND install",
                        Some(index),
                        source,
                    )
                    .with_cleanup_storage(rollback_cleanup);
                    rollback_stable_import(&mut coordinator, &installed, &mut error);
                    return Err(error);
                }
            };
            installed.push(InstalledStableCpuSuspend {
                index,
                runner_token,
                dispatch_installed: false,
            });
            if let Err(source) = coordinator.install_imported_cpu_suspend_dispatch(
                index,
                runner_token,
                virtual_timer_intid,
            ) {
                let mut error = HvfArm64StablePausedTopologyImportError::with_source(
                    "coordinator CPU_SUSPEND install",
                    Some(index),
                    source,
                )
                .with_cleanup_storage(rollback_cleanup);
                rollback_stable_import(&mut coordinator, &installed, &mut error);
                return Err(error);
            }
            if let Some(record) = installed.last_mut() {
                record.dispatch_installed = true;
            }
            let work = HvfVcpuCoordinatorWork::restored_cpu_suspend(
                stable_suspend.convention().function_id(),
                runner_token,
            );
            let Some(slot) = pending_cpu_suspends.get_mut(index) else {
                let mut error = HvfArm64StablePausedTopologyImportError::new(
                    "pending CPU_SUSPEND identity",
                    Some(index),
                )
                .with_cleanup_storage(rollback_cleanup);
                rollback_stable_import(&mut coordinator, &installed, &mut error);
                return Err(error);
            };
            *slot = Some(PendingCpuSuspend {
                power_token: power_suspend.token(),
                coordinator_work: work,
            });
        }

        Ok(Self {
            coordinator,
            power,
            virtual_timer_intid,
            pending_cpu_suspends,
            pending_steps: VecDeque::new(),
            last_step_index: 0,
            last_terminal_report: None,
        })
    }

    /// Allow the imported paused topology to begin fresh run generation 1.
    pub fn resume(&mut self) -> Result<(), HvfVcpuRunCoordinatorError> {
        self.coordinator.resume()
    }

    pub(crate) fn singular_runner(&self) -> Result<&HvfVcpuRunner<'vm>, HvfVcpuRunnerError> {
        if self.member_count() != 1 {
            return Err(HvfVcpuRunnerError::InvalidState(
                "direct boot-session vCPU steps require exactly one topology member",
            ));
        }
        Ok(self.coordinator.primary_runner())
    }

    pub(crate) const fn last_terminal_report(&self) -> Option<&HvfVcpuRunTerminalReport> {
        self.last_terminal_report.as_ref()
    }

    fn capture_stable_member_disposition(
        &self,
        coordinator: HvfVcpuStablePausedMemberObservation,
        power: PsciCpuStableMemberObservation,
    ) -> Result<HvfArm64StableVcpuDisposition, HvfArm64StablePausedTopologyCaptureError> {
        let index = coordinator.index();
        if power.index() != index
            || power.mpidr() != coordinator.mpidr()
            || self.coordinator.mpidrs().get(index).copied() != Some(coordinator.mpidr())
        {
            return Err(HvfArm64StablePausedTopologyCaptureError::new(
                "member identity agreement",
                Some(index),
            ));
        }
        if power.cpu_on_token().is_some() || power.cpu_off_work().is_some() {
            return Err(HvfArm64StablePausedTopologyCaptureError::new(
                "transient PSCI power work",
                Some(index),
            ));
        }
        let pending = self.pending_cpu_suspends.get(index).copied().flatten();

        match (
            coordinator.online(),
            coordinator.dispatch(),
            power.power(),
            power.cpu_suspend_work(),
            pending,
        ) {
            (false, HvfVcpuStableMemberDispatch::Runnable, PsciCpuPowerState::Off, None, None) => {
                self.coordinator
                    .ensure_no_stable_deferred_psci_call(index)
                    .map_err(|source| {
                        HvfArm64StablePausedTopologyCaptureError::with_source(
                            "offline runner validation",
                            Some(index),
                            source,
                        )
                    })?;
                Ok(HvfArm64StableVcpuDisposition::Offline)
            }
            (true, HvfVcpuStableMemberDispatch::Runnable, PsciCpuPowerState::On, None, None) => {
                self.coordinator
                    .ensure_no_stable_deferred_psci_call(index)
                    .map_err(|source| {
                        HvfArm64StablePausedTopologyCaptureError::with_source(
                            "runnable runner validation",
                            Some(index),
                            source,
                        )
                    })?;
                Ok(HvfArm64StableVcpuDisposition::Runnable)
            }
            (
                true,
                HvfVcpuStableMemberDispatch::Suspended {
                    psci_token,
                    timer_intid,
                },
                PsciCpuPowerState::On,
                Some(power_suspend),
                Some(pending),
            ) => {
                if timer_intid != self.virtual_timer_intid
                    || power_suspend.caller_index() != index
                    || power_suspend.token() != pending.power_token
                {
                    return Err(HvfArm64StablePausedTopologyCaptureError::new(
                        "CPU_SUSPEND lifecycle identity",
                        Some(index),
                    ));
                }
                let (exit, function_id, work_runner_token, request) =
                    pending.coordinator_work.into_parts();
                if exit.immediate() != 0
                    || request != PsciCoordinatorRequest::CpuSuspend
                    || work_runner_token != psci_token
                {
                    return Err(HvfArm64StablePausedTopologyCaptureError::new(
                        "CPU_SUSPEND coordinator work",
                        Some(index),
                    ));
                }
                let runner =
                    self.coordinator
                        .capture_stable_cpu_suspend(index)
                        .map_err(|source| {
                            HvfArm64StablePausedTopologyCaptureError::with_source(
                                "CPU_SUSPEND runner capture",
                                Some(index),
                                source,
                            )
                        })?;
                if runner.token() != psci_token
                    || runner.state().convention().function_id() != function_id
                {
                    return Err(HvfArm64StablePausedTopologyCaptureError::new(
                        "CPU_SUSPEND runner agreement",
                        Some(index),
                    ));
                }
                Ok(HvfArm64StableVcpuDisposition::Suspended(
                    runner.state().clone(),
                ))
            }
            _ => Err(HvfArm64StablePausedTopologyCaptureError::new(
                "member lifecycle agreement",
                Some(index),
            )),
        }
    }

    pub(crate) fn run_step(
        &mut self,
        mut entry_is_valid: impl FnMut(u64) -> bool,
    ) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        if self.pending_steps.is_empty() {
            self.coordinator
                .dispatch_online()
                .map_err(|source| self.coordinator_error("run dispatch", 0, source))?;
            let event = self
                .coordinator
                .receive_event()
                .map_err(|source| self.coordinator_error("event collection", 0, source))?;
            self.enqueue_event(event, &mut entry_is_valid)?;
        }

        let step = self
            .pending_steps
            .pop_front()
            .ok_or_else(|| self.power_error("event translation", 0))??;
        self.last_step_index = step.index;
        Ok(step.outcome)
    }

    pub(crate) fn set_last_step_ppi_pending(
        &self,
        intid: u32,
    ) -> Result<(), HvfArm64BootVcpuError> {
        self.coordinator
            .set_gic_ppi_pending(self.last_step_index, intid)
            .map_err(|source| {
                self.coordinator_error("virtual-timer PPI delivery", self.last_step_index, source)
            })
    }

    fn enqueue_event(
        &mut self,
        event: HvfVcpuRunEvent,
        entry_is_valid: &mut impl FnMut(u64) -> bool,
    ) -> Result<(), HvfArm64BootVcpuError> {
        match event {
            HvfVcpuRunEvent::Member(member) => {
                let step = self.process_member(member, entry_is_valid, true);
                self.pending_steps.push_back(step);
            }
            HvfVcpuRunEvent::Barrier(report) => {
                let mut sentinel_index = 0;
                let cpu_on_admission = barrier_cpu_on_admission(report.reason());
                for member in report.acknowledgements().iter().cloned() {
                    sentinel_index = member.index();
                    if member_is_canceled(&member)
                        || (cpu_on_admission.is_none() && member_has_retained_vtimer(&member))
                        || (cpu_on_admission.is_none() && member_has_coordinator_work(&member))
                    {
                        continue;
                    }
                    let step = self.process_member(
                        member,
                        entry_is_valid,
                        cpu_on_admission.unwrap_or(false),
                    );
                    self.pending_steps.push_back(step);
                }
                self.pending_steps.push_back(Ok(IndexedBootStep {
                    index: sentinel_index,
                    outcome: HvfVcpuRunStepOutcome::Canceled,
                }));
            }
            HvfVcpuRunEvent::Terminal(report) => {
                self.last_terminal_report = Some(report.clone());
                let primary = (report.primary().index(), report.primary().generation());
                for member in report.members().iter().cloned() {
                    if (member.index(), member.generation()) == primary
                        || member_is_canceled(&member)
                        || member_is_terminal(&member)
                        || member_has_retained_vtimer(&member)
                        || member_has_coordinator_work(&member)
                    {
                        continue;
                    }
                    let step = self.process_member(member, entry_is_valid, false);
                    self.pending_steps.push_back(step);
                }
                let primary = self.process_member(report.primary().clone(), entry_is_valid, false);
                self.pending_steps.push_back(primary);
            }
        }
        Ok(())
    }

    fn process_member(
        &mut self,
        member: HvfVcpuRunMemberResult,
        entry_is_valid: &mut impl FnMut(u64) -> bool,
        cpu_on_admission: bool,
    ) -> Result<IndexedBootStep, HvfArm64BootVcpuError> {
        let index = member.index();
        let mpidr = member.mpidr();
        let generation = member.generation();
        let outcome = match member.result() {
            Ok(HvfVcpuRunMemberOutcome::Handled(outcome)) => *outcome,
            Ok(HvfVcpuRunMemberOutcome::Coordinator(work)) => {
                self.process_coordinator_work(index, *work, entry_is_valid, cpu_on_admission)?
            }
            Ok(HvfVcpuRunMemberOutcome::RetainedVtimer(outcome)) => {
                self.process_cpu_suspend_wakeup(index, *outcome)?
            }
            Err(source) => {
                return Err(HvfArm64BootVcpuError::Member {
                    index,
                    mpidr,
                    generation,
                    source: Box::new(source.clone()),
                });
            }
        };
        Ok(IndexedBootStep { index, outcome })
    }

    fn process_coordinator_work(
        &mut self,
        caller_index: usize,
        work: HvfVcpuCoordinatorWork,
        entry_is_valid: &mut impl FnMut(u64) -> bool,
        cpu_on_admission: bool,
    ) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        let (exit, function_id, _, request) = work.into_parts();
        let response = match request {
            PsciCoordinatorRequest::CpuSuspend => {
                return self.process_cpu_suspend(caller_index, work);
            }
            PsciCoordinatorRequest::CpuOff => {
                return self.process_cpu_off(caller_index, work);
            }
            PsciCoordinatorRequest::AffinityInfo(request) => {
                PsciCoordinatorResponse::AffinityInfo(self.power.affinity_info(request))
            }
            PsciCoordinatorRequest::CpuOn(request) => {
                return self.process_cpu_on(
                    caller_index,
                    work,
                    request,
                    entry_is_valid,
                    cpu_on_admission,
                );
            }
        };
        self.complete_caller(
            caller_index,
            work,
            response,
            "AFFINITY_INFO completion",
            false,
        )?;
        Ok(HvfVcpuRunStepOutcome::Hvc {
            exit,
            function_id,
            return_value: response.return_value(),
        })
    }

    fn process_cpu_suspend(
        &mut self,
        caller_index: usize,
        work: HvfVcpuCoordinatorWork,
    ) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        let (exit, function_id, _, _) = work.into_parts();
        if !matches!(self.pending_cpu_suspends.get(caller_index), Some(None)) {
            return Err(self.power_error("CPU_SUSPEND activation", caller_index));
        }
        let suspend = self
            .power
            .begin_cpu_suspend(caller_index)
            .map_err(|_| self.power_error("CPU_SUSPEND validation", caller_index))?;
        let suspend_index = suspend.caller_index();
        if suspend_index != caller_index {
            let _ = self.power.abort_cpu_suspend(suspend.token());
            return Err(self.power_error("CPU_SUSPEND identity", caller_index));
        }
        if let Err(source) =
            self.coordinator
                .suspend_for_cpu_suspend(suspend_index, work, self.virtual_timer_intid)
        {
            let cleanup_failed = self.power.abort_cpu_suspend(suspend.token()).is_err();
            return Err(HvfArm64BootVcpuError::Coordinator {
                stage: "CPU_SUSPEND scheduler activation",
                index: suspend_index,
                mpidr: self.mpidr(suspend_index),
                cleanup_failed,
                source: Box::new(source),
            });
        }
        let Some(slot) = self.pending_cpu_suspends.get_mut(suspend_index) else {
            return Err(self.power_error("CPU_SUSPEND activation", caller_index));
        };
        *slot = Some(PendingCpuSuspend {
            power_token: suspend.token(),
            coordinator_work: work,
        });

        Ok(HvfVcpuRunStepOutcome::CpuSuspend {
            index: suspend_index,
            mpidr: self.mpidr(suspend_index),
            exit,
            function_id,
        })
    }

    fn process_cpu_suspend_wakeup(
        &mut self,
        caller_index: usize,
        outcome: HvfVcpuRetainedVtimerWaitOutcome,
    ) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        if outcome == HvfVcpuRetainedVtimerWaitOutcome::Canceled {
            return Ok(HvfVcpuRunStepOutcome::Canceled);
        }
        let pending = self
            .pending_cpu_suspends
            .get(caller_index)
            .copied()
            .flatten()
            .ok_or_else(|| self.power_error("CPU_SUSPEND wakeup", caller_index))?;
        self.power
            .validate_cpu_suspend(pending.power_token, caller_index)
            .map_err(|_| self.power_error("CPU_SUSPEND wakeup validation", caller_index))?;
        self.coordinator
            .resume_from_cpu_suspend(caller_index, pending.coordinator_work)
            .map_err(|source| {
                self.coordinator_error("CPU_SUSPEND scheduler wakeup", caller_index, source)
            })?;

        let (exit, function_id, _, _) = pending.coordinator_work.into_parts();
        let response = PsciCoordinatorResponse::CpuSuspend(PsciCpuSuspendResponse::Success);
        if let Err(error) = self.complete_caller(
            caller_index,
            pending.coordinator_work,
            response,
            "CPU_SUSPEND completion",
            false,
        ) {
            let cleanup_failed = self
                .coordinator
                .suspend_for_cpu_suspend(
                    caller_index,
                    pending.coordinator_work,
                    self.virtual_timer_intid,
                )
                .is_err();
            return Err(with_cleanup_evidence(error, cleanup_failed));
        }
        self.power
            .commit_cpu_suspend(pending.power_token)
            .map_err(|_| self.power_error("CPU_SUSPEND power commit", caller_index))?;
        let Some(slot) = self.pending_cpu_suspends.get_mut(caller_index) else {
            return Err(self.power_error("CPU_SUSPEND completion", caller_index));
        };
        *slot = None;

        Ok(HvfVcpuRunStepOutcome::Hvc {
            exit,
            function_id,
            return_value: response.return_value(),
        })
    }

    fn process_cpu_off(
        &mut self,
        caller_index: usize,
        work: HvfVcpuCoordinatorWork,
    ) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        let (exit, function_id, _, _) = work.into_parts();
        let begin = self
            .power
            .begin_cpu_off(caller_index)
            .map_err(|_| self.power_error("CPU_OFF validation", caller_index))?;
        let PsciCpuOffBegin::Pending(cpu_off) = begin else {
            let PsciCpuOffBegin::Complete(response) = begin else {
                return Err(self.power_error("CPU_OFF validation", caller_index));
            };
            let response = PsciCoordinatorResponse::CpuOff(response);
            self.complete_caller(
                caller_index,
                work,
                response,
                "CPU_OFF failure completion",
                false,
            )?;
            return Ok(HvfVcpuRunStepOutcome::Hvc {
                exit,
                function_id,
                return_value: response.return_value(),
            });
        };
        let off_index = cpu_off.caller_index();

        if let Err(source) = self.coordinator.commit_cpu_off(off_index, work) {
            let cleanup_failed = self.power.abort_cpu_off(cpu_off.token()).is_err();
            return Err(HvfArm64BootVcpuError::Coordinator {
                stage: "CPU_OFF caller commit",
                index: off_index,
                mpidr: self.mpidr(off_index),
                cleanup_failed,
                source: Box::new(source),
            });
        }
        if let Err(source) = self.coordinator.set_online(off_index, false) {
            let cleanup_failed = self.power.abort_cpu_off(cpu_off.token()).is_err();
            return Err(HvfArm64BootVcpuError::Coordinator {
                stage: "CPU_OFF scheduler removal",
                index: off_index,
                mpidr: self.mpidr(off_index),
                cleanup_failed,
                source: Box::new(source),
            });
        }
        self.power
            .commit_cpu_off(cpu_off.token())
            .map_err(|_| self.power_error("CPU_OFF power commit", off_index))?;

        Ok(HvfVcpuRunStepOutcome::CpuOff {
            index: off_index,
            mpidr: self.mpidr(off_index),
            exit,
            function_id,
        })
    }

    fn process_cpu_on(
        &mut self,
        caller_index: usize,
        work: HvfVcpuCoordinatorWork,
        request: crate::psci::PsciCpuOnRequest,
        entry_is_valid: &mut impl FnMut(u64) -> bool,
        cpu_on_admission: bool,
    ) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        let (exit, function_id, _, _) = work.into_parts();
        let begin = self
            .power
            .begin_cpu_on(request, entry_is_valid)
            .map_err(|_| self.power_error("CPU_ON validation", caller_index))?;
        let PsciCpuOnBegin::Pending(cpu_on) = begin else {
            let PsciCpuOnBegin::Complete(response) = begin else {
                return Err(self.power_error("CPU_ON validation", caller_index));
            };
            let response = PsciCoordinatorResponse::CpuOn(response);
            self.complete_caller(caller_index, work, response, "CPU_ON completion", false)?;
            return Ok(HvfVcpuRunStepOutcome::Hvc {
                exit,
                function_id,
                return_value: response.return_value(),
            });
        };

        if !cpu_on_admission {
            return self.complete_failed_cpu_on(caller_index, work, cpu_on);
        }

        let target_index = cpu_on.target_index();
        let registers = HvfArm64SecondaryBootRegisters::new(
            GuestAddress::new(cpu_on.request().entry_point()),
            cpu_on.request().context_id(),
        );
        if self
            .coordinator
            .configure_arm64_secondary_boot_registers(target_index, registers)
            .is_err()
        {
            return self.complete_failed_cpu_on(caller_index, work, cpu_on);
        }

        let admission = match self
            .coordinator
            .activate_and_dispatch_member(target_index)
            .map_err(|source| {
                self.coordinator_error("target-only run admission", target_index, source)
            })? {
            Some(admission) => admission,
            None => return self.complete_failed_cpu_on(caller_index, work, cpu_on),
        };
        self.validate_admission(target_index, admission)?;
        let response = self
            .power
            .finish_target_setup(cpu_on.token(), true)
            .map_err(|_| self.power_error("target setup commit", target_index))?;

        let response = PsciCoordinatorResponse::CpuOn(response);
        if let Err(error) = self.complete_caller(
            caller_index,
            work,
            response,
            "CPU_ON success completion",
            false,
        ) {
            let cleanup_failed = self.cleanup_admitted_cpu_on(cpu_on.token());
            return Err(with_cleanup_evidence(error, cleanup_failed));
        }
        if self.power.commit_caller_completion(cpu_on.token()).is_err() {
            let cleanup_failed = self.cleanup_admitted_cpu_on(cpu_on.token());
            return Err(HvfArm64BootVcpuError::Power {
                stage: if cleanup_failed {
                    "caller commit and admitted-target cleanup"
                } else {
                    "caller commit"
                },
                index: target_index,
                mpidr: self.mpidr(target_index),
            });
        }
        self.power
            .mark_target_entered(cpu_on.token())
            .map_err(|_| self.power_error("target entered transition", target_index))?;

        Ok(HvfVcpuRunStepOutcome::Hvc {
            exit,
            function_id,
            return_value: response.return_value(),
        })
    }

    fn complete_failed_cpu_on(
        &mut self,
        caller_index: usize,
        work: HvfVcpuCoordinatorWork,
        cpu_on: PsciCpuOnWork,
    ) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        let (exit, function_id, _, _) = work.into_parts();
        let target_index = cpu_on.target_index();
        let response = self
            .power
            .finish_target_setup(cpu_on.token(), false)
            .map_err(|_| self.power_error("target setup failure", target_index))?;
        let response = PsciCoordinatorResponse::CpuOn(response);
        if let Err(error) = self.complete_caller(
            caller_index,
            work,
            response,
            "CPU_ON failure completion",
            false,
        ) {
            let cleanup_failed = self
                .power
                .abandon_caller_completion(cpu_on.token())
                .is_err();
            return Err(with_cleanup_evidence(error, cleanup_failed));
        }
        self.power
            .commit_caller_completion(cpu_on.token())
            .map_err(|_| self.power_error("CPU_ON failure commit", target_index))?;
        Ok(HvfVcpuRunStepOutcome::Hvc {
            exit,
            function_id,
            return_value: response.return_value(),
        })
    }

    fn complete_caller(
        &self,
        caller_index: usize,
        work: HvfVcpuCoordinatorWork,
        response: PsciCoordinatorResponse,
        stage: &'static str,
        cleanup_failed: bool,
    ) -> Result<(), HvfArm64BootVcpuError> {
        self.coordinator
            .complete_coordinator_work(caller_index, work, response)
            .map_err(|source| HvfArm64BootVcpuError::Coordinator {
                stage,
                index: caller_index,
                mpidr: self.mpidr(caller_index),
                cleanup_failed,
                source: Box::new(source),
            })
    }

    fn validate_admission(
        &self,
        target_index: usize,
        admission: HvfVcpuRunAdmission,
    ) -> Result<(), HvfArm64BootVcpuError> {
        if admission.index() == target_index && admission.mpidr() == self.mpidr(target_index) {
            let _generation = admission.generation();
            Ok(())
        } else {
            Err(self.power_error("target admission identity", target_index))
        }
    }

    fn cleanup_admitted_cpu_on(&mut self, token: PsciCpuOnToken) -> bool {
        if self.power.abandon_caller_completion(token).is_err() {
            return true;
        }
        self.power.mark_target_entered(token).is_err()
    }

    fn coordinator_error(
        &self,
        stage: &'static str,
        index: usize,
        source: HvfVcpuRunCoordinatorError,
    ) -> HvfArm64BootVcpuError {
        HvfArm64BootVcpuError::Coordinator {
            stage,
            index,
            mpidr: self.mpidr(index),
            cleanup_failed: false,
            source: Box::new(source),
        }
    }

    fn power_error(&self, stage: &'static str, index: usize) -> HvfArm64BootVcpuError {
        HvfArm64BootVcpuError::Power {
            stage,
            index,
            mpidr: self.mpidr(index),
        }
    }

    fn mpidr(&self, index: usize) -> u64 {
        self.coordinator.mpidrs().get(index).copied().unwrap_or(0)
    }
}

fn member_is_canceled(member: &HvfVcpuRunMemberResult) -> bool {
    matches!(
        member.result(),
        Ok(HvfVcpuRunMemberOutcome::Handled(
            HvfVcpuRunStepOutcome::Canceled
        )) | Ok(HvfVcpuRunMemberOutcome::RetainedVtimer(
            HvfVcpuRetainedVtimerWaitOutcome::Canceled
        ))
    )
}

const fn barrier_cpu_on_admission(reason: HvfVcpuRunControlReason) -> Option<bool> {
    match reason {
        HvfVcpuRunControlReason::Wakeup => Some(true),
        HvfVcpuRunControlReason::Pause => Some(false),
        HvfVcpuRunControlReason::Stop | HvfVcpuRunControlReason::Shutdown => None,
    }
}

fn member_has_coordinator_work(member: &HvfVcpuRunMemberResult) -> bool {
    matches!(member.result(), Ok(HvfVcpuRunMemberOutcome::Coordinator(_)))
}

fn member_has_retained_vtimer(member: &HvfVcpuRunMemberResult) -> bool {
    matches!(
        member.result(),
        Ok(HvfVcpuRunMemberOutcome::RetainedVtimer(_))
    )
}

fn member_is_terminal(member: &HvfVcpuRunMemberResult) -> bool {
    matches!(
        member.result(),
        Err(_)
            | Ok(HvfVcpuRunMemberOutcome::Handled(
                HvfVcpuRunStepOutcome::Unknown { .. }
                    | HvfVcpuRunStepOutcome::GuestReset { .. }
                    | HvfVcpuRunStepOutcome::GuestShutdown { .. }
            ))
    )
}

fn with_cleanup_evidence(
    error: HvfArm64BootVcpuError,
    cleanup_failed: bool,
) -> HvfArm64BootVcpuError {
    match error {
        HvfArm64BootVcpuError::Coordinator {
            stage,
            index,
            mpidr,
            source,
            ..
        } => HvfArm64BootVcpuError::Coordinator {
            stage,
            index,
            mpidr,
            cleanup_failed,
            source,
        },
        error => error,
    }
}

fn rollback_stable_import(
    coordinator: &mut HvfVcpuRunCoordinator<'_>,
    installed: &[InstalledStableCpuSuspend],
    error: &mut HvfArm64StablePausedTopologyImportError,
) {
    for installed in installed.iter().rev().copied() {
        if let Err(source) =
            coordinator.abort_stable_cpu_suspend(installed.index, installed.runner_token)
        {
            error
                .cleanup
                .push(HvfArm64StablePausedTopologyCleanupFailure {
                    stage: HvfArm64StablePausedTopologyCleanupStage::RunnerAbort,
                    index: Some(installed.index),
                    source: Box::new(source),
                });
        }
        if installed.dispatch_installed
            && let Err(source) = coordinator
                .clear_imported_cpu_suspend_dispatch(installed.index, installed.runner_token)
        {
            error
                .cleanup
                .push(HvfArm64StablePausedTopologyCleanupFailure {
                    stage: HvfArm64StablePausedTopologyCleanupStage::CoordinatorDispatch,
                    index: Some(installed.index),
                    source: Box::new(source),
                });
        }
    }
    if let Err(source) = coordinator.shutdown_after_failed_stable_import() {
        error
            .cleanup
            .push(HvfArm64StablePausedTopologyCleanupFailure {
                stage: HvfArm64StablePausedTopologyCleanupStage::TopologyShutdown,
                index: None,
                source: Box::new(source),
            });
    }
}

fn shutdown_unmodified_stable_import(
    topology: &HvfVcpuTopology<'_>,
    error: &mut HvfArm64StablePausedTopologyImportError,
) {
    if let Err(source) = topology.shutdown() {
        error
            .cleanup
            .push(HvfArm64StablePausedTopologyCleanupFailure {
                stage: HvfArm64StablePausedTopologyCleanupStage::TopologyShutdown,
                index: None,
                source: Box::new(source),
            });
    }
}

impl<'vm> Deref for HvfArm64BootVcpuSession<'vm> {
    type Target = HvfVcpuRunner<'vm>;

    fn deref(&self) -> &Self::Target {
        self.coordinator.primary_runner()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;

    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioDispatcher;

    use super::{
        HvfArm64BootVcpuSession, HvfArm64StablePausedTopologyCleanupStage, barrier_cpu_on_admission,
    };
    use crate::HvfVcpuRunStepOutcome;
    use crate::coordinator::{HvfVcpuRunControlReason, HvfVcpuRunCoordinator, HvfVcpuRunEvent};
    use crate::paused_topology::{
        HvfArm64CpuSuspendConvention, HvfArm64StableCpuSuspendState,
        HvfArm64StablePausedTopologyMember, HvfArm64StablePausedTopologyState,
        HvfArm64StableVcpuDisposition,
    };
    use crate::psci::{
        PsciCall, PsciCoordinatedDispatch, PsciCoordinatorRequest, PsciCpuOffBegin, PsciCpuOnBegin,
        PsciCpuPowerCoordinator, PsciCpuPowerState, PsciStatus, handle_coordinated_call,
    };
    use crate::runner::tests::{
        start_coordinated_psci_run_step_recording_runner,
        start_coordinated_psci_run_step_recording_runner_with_destroy,
        start_cpu_suspend_retained_runner, start_destroy_order_recording_runner,
        start_secondary_configure_recording_runner,
    };
    use crate::topology::HvfVcpuTopology;
    use crate::vcpu::{HvfArm64SecondaryBootRegisters, HvfRegister};

    const PSCI_CPU_ON_64: u64 = 0xc400_0003;
    const PSCI_CPU_SUSPEND: u64 = 0x8400_0001;
    const PSCI_CPU_OFF: u64 = 0x8400_0002;
    const SECONDARY_ENTRY: u64 = 0x8020_0000;
    const SECONDARY_CONTEXT: u64 = 0xfeed_face_cafe_beef;

    type CpuOnSessionFixture = (
        HvfArm64BootVcpuSession<'static>,
        mpsc::Receiver<HvfRegister>,
        mpsc::Receiver<(HvfRegister, u64)>,
        mpsc::Receiver<HvfArm64SecondaryBootRegisters>,
    );

    #[test]
    fn completed_pause_exports_live_cpu_suspend_architectural_state() {
        let arguments = [0x11, 0x22, 0x33];
        let (runner, _reads, writes, timer_samples, _ppi) =
            start_cpu_suspend_retained_runner(PSCI_CPU_SUSPEND, arguments, 1, 100, [Ok(100)]);
        let coordinator = HvfVcpuRunCoordinator::from_test_runners(
            vec![runner],
            vec![0],
            Arc::new(Mutex::new(MmioDispatcher::new())),
            &[0],
        )
        .expect("single test coordinator should build");
        let power = PsciCpuPowerCoordinator::new(&[0]).expect("power topology should build");
        let mut session = HvfArm64BootVcpuSession::new(coordinator, power, 27);

        let wrong_phase = session
            .capture_stable_paused_topology()
            .expect_err("running topology should not export");
        assert_eq!(wrong_phase.stage(), "coordinator pause validation");
        assert!(matches!(
            session.run_step(|_| true),
            Ok(HvfVcpuRunStepOutcome::CpuSuspend {
                index: 0,
                function_id: PSCI_CPU_SUSPEND,
                ..
            })
        ));
        session
            .control()
            .request_pause()
            .expect("idle pause should start")
            .wait()
            .expect("idle pause should complete");

        let pending = session.pending_cpu_suspends[0].take();
        let partial = session
            .capture_stable_paused_topology()
            .expect_err("partial CPU_SUSPEND ownership should not export");
        assert_eq!(partial.stage(), "member lifecycle agreement");
        assert_eq!(partial.index(), Some(0));
        session.pending_cpu_suspends[0] = pending;

        let stable = session
            .capture_stable_paused_topology()
            .expect("completed pause should export");
        assert_eq!(stable.virtual_timer_intid(), 27);
        assert_eq!(stable.members().len(), 1);
        let HvfArm64StableVcpuDisposition::Suspended(suspend) = stable.members()[0].disposition()
        else {
            panic!("live CPU_SUSPEND should export as suspended");
        };
        assert_eq!(suspend.convention(), HvfArm64CpuSuspendConvention::Call32);
        assert_eq!(suspend.arguments(), arguments);
        assert_eq!(suspend.return_pc(), 0x8000);
        assert_eq!(writes.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert_eq!(timer_samples.try_recv(), Err(mpsc::TryRecvError::Empty));
        session.shutdown().expect("session should shut down");
    }

    #[test]
    fn stable_paused_import_recaptures_equivalent_lifecycle_graph() {
        let arguments = [0x11, 0x22, 0x33];
        let stable_suspend = HvfArm64StableCpuSuspendState::new(
            HvfArm64CpuSuspendConvention::Call64,
            arguments,
            0x8000,
        )
        .expect("stable suspend should build");
        let stable = HvfArm64StablePausedTopologyState::new(
            27,
            vec![
                HvfArm64StablePausedTopologyMember::new(
                    0,
                    0,
                    HvfArm64StableVcpuDisposition::Suspended(stable_suspend),
                ),
                HvfArm64StablePausedTopologyMember::new(
                    1,
                    1,
                    HvfArm64StableVcpuDisposition::Offline,
                ),
                HvfArm64StablePausedTopologyMember::new(
                    2,
                    2,
                    HvfArm64StableVcpuDisposition::Runnable,
                ),
            ],
        )
        .expect("stable topology should build");
        let (primary, _, primary_writes) =
            start_coordinated_psci_run_step_recording_runner(0xc400_0001, arguments, 0, false);
        let (secondary, _, secondary_writes) =
            start_coordinated_psci_run_step_recording_runner(0, [0; 3], 0, false);
        let (runnable, _, runnable_writes) =
            start_coordinated_psci_run_step_recording_runner(0, [0; 3], 0, false);
        let topology =
            HvfVcpuTopology::from_test_parts(vec![primary, secondary, runnable], vec![0, 1, 2]);
        let mut session = HvfArm64BootVcpuSession::from_stable_paused_topology(
            topology,
            &stable,
            Arc::new(Mutex::new(MmioDispatcher::new())),
            27,
        )
        .expect("stable topology should import");

        assert_eq!(
            session
                .capture_stable_paused_topology()
                .expect("imported graph should immediately recapture"),
            stable
        );
        let formatted = format!("{session:?}");
        assert!(formatted.contains("<redacted>"));
        for raw in ["11", "22", "33", "8000"] {
            assert!(!formatted.contains(raw));
        }
        assert_eq!(primary_writes.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert_eq!(secondary_writes.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert_eq!(runnable_writes.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert!(session.coordinator.dispatch_online().is_err());
        session.shutdown().expect("session should shut down");
    }

    #[test]
    fn imported_stable_cpu_suspend_cancels_rearms_and_wakes_after_resume() {
        let arguments = [0x11, 0x22, 0x33];
        let stable = HvfArm64StablePausedTopologyState::new(
            27,
            vec![HvfArm64StablePausedTopologyMember::new(
                0,
                0,
                HvfArm64StableVcpuDisposition::Suspended(
                    HvfArm64StableCpuSuspendState::new(
                        HvfArm64CpuSuspendConvention::Call32,
                        arguments,
                        0x8000,
                    )
                    .expect("stable suspend should build"),
                ),
            )],
        )
        .expect("stable topology should build");
        let (runner, _reads, writes, timer_samples, ppi) = start_cpu_suspend_retained_runner(
            PSCI_CPU_SUSPEND,
            arguments,
            1,
            1_000_000_000,
            [Ok(1), Ok(1_000_000_000)],
        );
        let topology = HvfVcpuTopology::from_test_parts(vec![runner], vec![0]);
        let mut session = HvfArm64BootVcpuSession::from_stable_paused_topology(
            topology,
            &stable,
            Arc::new(Mutex::new(MmioDispatcher::new())),
            27,
        )
        .expect("stable topology should import");

        assert_eq!(timer_samples.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert_eq!(writes.try_recv(), Err(mpsc::TryRecvError::Empty));
        session.resume().expect("imported session should resume");

        let pause_control = session.control();
        thread::scope(|scope| {
            let step = scope.spawn(|| session.run_step(|_| true));
            timer_samples
                .recv()
                .expect("first retained wait should sample");
            let waiter = pause_control
                .request_pause()
                .expect("pause should cancel the retained wait");
            assert!(matches!(
                step.join().expect("session step should not panic"),
                Ok(HvfVcpuRunStepOutcome::Canceled)
            ));
            waiter.wait().expect("pause barrier should complete");
        });
        assert!(session.pending_cpu_suspends[0].is_some());
        assert_eq!(writes.try_recv(), Err(mpsc::TryRecvError::Empty));

        session
            .resume()
            .expect("explicit resume should rearm the imported suspension");
        assert!(matches!(
            session.run_step(|_| true),
            Ok(HvfVcpuRunStepOutcome::Hvc {
                function_id: PSCI_CPU_SUSPEND,
                return_value: 0,
                ..
            })
        ));
        timer_samples
            .recv()
            .expect("rearmed retained wait should sample again");
        assert_eq!(
            ppi.recv()
                .expect("timer wake should publish the configured PPI"),
            (27, true)
        );
        assert_eq!(
            writes
                .recv()
                .expect("timer wake should complete imported CPU_SUSPEND"),
            (HvfRegister::X0, 0)
        );
        assert!(session.pending_cpu_suspends[0].is_none());
        session.shutdown().expect("session should shut down");
    }

    #[test]
    fn stable_import_prevalidation_consumes_topology_and_retains_shutdown_failure() {
        let stable = HvfArm64StablePausedTopologyState::new(
            27,
            (0..3)
                .map(|index| {
                    HvfArm64StablePausedTopologyMember::new(
                        index,
                        index as u64,
                        HvfArm64StableVcpuDisposition::Runnable,
                    )
                })
                .collect(),
        )
        .expect("stable topology should build");
        let (destroyed_sender, destroyed_receiver) = mpsc::channel();
        let runners = (0..3)
            .map(|index| {
                start_destroy_order_recording_runner(index, index == 1, destroyed_sender.clone())
            })
            .collect();
        let topology = HvfVcpuTopology::from_test_parts(runners, vec![0, 1, 2]);

        let error = HvfArm64BootVcpuSession::from_stable_paused_topology(
            topology,
            &stable,
            Arc::new(Mutex::new(MmioDispatcher::new())),
            26,
        )
        .expect_err("destination PPI mismatch should fail");

        assert_eq!(error.stage(), "virtual timer PPI validation");
        assert_eq!(error.index(), None);
        assert_eq!(error.cleanup_failures().len(), 1);
        assert_eq!(
            error.cleanup_failures()[0].stage(),
            HvfArm64StablePausedTopologyCleanupStage::TopologyShutdown
        );
        assert_eq!(error.cleanup_failures()[0].index(), None);
        assert_eq!(destroyed_receiver.try_iter().collect::<Vec<_>>(), [2, 1, 0]);
    }

    #[test]
    fn stable_import_rejects_wrong_and_duplicate_destination_mpidrs_before_owner_access() {
        let stable = HvfArm64StablePausedTopologyState::new(
            27,
            vec![
                HvfArm64StablePausedTopologyMember::new(
                    0,
                    0,
                    HvfArm64StableVcpuDisposition::Runnable,
                ),
                HvfArm64StablePausedTopologyMember::new(
                    1,
                    1,
                    HvfArm64StableVcpuDisposition::Offline,
                ),
            ],
        )
        .expect("stable topology should build");

        for mpidrs in [vec![0, 0], vec![0, 2]] {
            let (destroyed_sender, destroyed_receiver) = mpsc::channel();
            let runners = (0..2)
                .map(|index| {
                    start_destroy_order_recording_runner(index, false, destroyed_sender.clone())
                })
                .collect();
            let topology = HvfVcpuTopology::from_test_parts(runners, mpidrs);

            let error = HvfArm64BootVcpuSession::from_stable_paused_topology(
                topology,
                &stable,
                Arc::new(Mutex::new(MmioDispatcher::new())),
                27,
            )
            .expect_err("destination MPIDR mismatch should fail");

            assert_eq!(error.stage(), "destination MPIDR validation");
            assert_eq!(error.index(), Some(1));
            assert!(error.cleanup_failures().is_empty());
            assert_eq!(destroyed_receiver.try_iter().collect::<Vec<_>>(), [1, 0]);
        }
    }

    #[test]
    fn stable_import_rejects_an_already_run_runnable_destination() {
        let stable = HvfArm64StablePausedTopologyState::new(
            27,
            vec![HvfArm64StablePausedTopologyMember::new(
                0,
                0,
                HvfArm64StableVcpuDisposition::Runnable,
            )],
        )
        .expect("stable topology should build");
        let (runner, _reads, _writes) =
            start_coordinated_psci_run_step_recording_runner(0x8400_0000, [0; 3], 0, false);
        runner
            .run_once_and_handle_mmio_coordinated(Arc::new(Mutex::new(MmioDispatcher::new())))
            .expect("test destination should run once");
        let topology = HvfVcpuTopology::from_test_parts(vec![runner], vec![0]);

        let error = HvfArm64BootVcpuSession::from_stable_paused_topology(
            topology,
            &stable,
            Arc::new(Mutex::new(MmioDispatcher::new())),
            27,
        )
        .expect_err("already-run destination should fail");

        assert_eq!(error.stage(), "destination owner readiness");
        assert_eq!(error.index(), Some(0));
    }

    #[test]
    fn stable_import_reverses_every_installed_failure_prefix_before_shutdown() {
        let arguments = [[0x11, 0x12, 0x13], [0x21, 0x22, 0x23], [0x31, 0x32, 0x33]];
        let stable = HvfArm64StablePausedTopologyState::new(
            27,
            arguments
                .iter()
                .copied()
                .enumerate()
                .map(|(index, arguments)| {
                    HvfArm64StablePausedTopologyMember::new(
                        index,
                        index as u64,
                        HvfArm64StableVcpuDisposition::Suspended(
                            HvfArm64StableCpuSuspendState::new(
                                HvfArm64CpuSuspendConvention::Call32,
                                arguments,
                                0x8000,
                            )
                            .expect("stable suspend should build"),
                        ),
                    )
                })
                .collect(),
        )
        .expect("stable topology should build");

        for failure_index in 0..3 {
            let (destroyed_sender, destroyed_receiver) = mpsc::channel();
            let mut read_receivers = Vec::new();
            let mut write_receivers = Vec::new();
            let mut runners = Vec::new();
            for (index, arguments) in arguments.iter().copied().enumerate() {
                let function_id = if index == failure_index {
                    HvfArm64CpuSuspendConvention::Call64.function_id()
                } else {
                    HvfArm64CpuSuspendConvention::Call32.function_id()
                };
                let (runner, reads, writes) =
                    start_coordinated_psci_run_step_recording_runner_with_destroy(
                        function_id,
                        arguments,
                        0,
                        false,
                        Some((
                            index,
                            failure_index == 2 && index == 1,
                            destroyed_sender.clone(),
                        )),
                    );
                runners.push(runner);
                read_receivers.push(reads);
                write_receivers.push(writes);
            }
            let topology = HvfVcpuTopology::from_test_parts(runners, vec![0, 1, 2]);

            let error = HvfArm64BootVcpuSession::from_stable_paused_topology(
                topology,
                &stable,
                Arc::new(Mutex::new(MmioDispatcher::new())),
                27,
            )
            .expect_err("injected register mismatch should fail import");

            assert_eq!(error.stage(), "runner CPU_SUSPEND install");
            assert_eq!(error.index(), Some(failure_index));
            assert_eq!(
                error.cleanup_failures().len(),
                usize::from(failure_index == 2)
            );
            if let Some(cleanup) = error.cleanup_failures().first() {
                assert_eq!(
                    cleanup.stage(),
                    HvfArm64StablePausedTopologyCleanupStage::TopologyShutdown
                );
                assert_eq!(cleanup.index(), None);
            }
            assert_eq!(destroyed_receiver.try_iter().collect::<Vec<_>>(), [2, 1, 0]);
            for (index, reads) in read_receivers.into_iter().enumerate() {
                assert_eq!(
                    reads.try_iter().count(),
                    usize::from(index <= failure_index) * 5
                );
            }
            for writes in write_receivers {
                assert_eq!(writes.try_recv(), Err(mpsc::TryRecvError::Disconnected));
            }
            let formatted = format!("{error:?}");
            assert!(formatted.contains("<redacted>"));
            for raw in ["11", "12", "13", "21", "22", "23", "31", "32", "33"] {
                assert!(!formatted.contains(raw));
            }
        }
    }

    fn cpu_on_session(fail_secondary_setup: bool) -> CpuOnSessionFixture {
        let (primary, reads, writes) = start_coordinated_psci_run_step_recording_runner(
            PSCI_CPU_ON_64,
            [1, SECONDARY_ENTRY, SECONDARY_CONTEXT],
            0,
            false,
        );
        let (secondary, configured) =
            start_secondary_configure_recording_runner(fail_secondary_setup);
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let coordinator = HvfVcpuRunCoordinator::from_test_runners(
            vec![primary, secondary],
            vec![0, 1],
            dispatcher,
            &[0],
        )
        .expect("test coordinator should build");
        let power =
            PsciCpuPowerCoordinator::new(&[0, 1]).expect("test power topology should build");
        (
            HvfArm64BootVcpuSession::new(coordinator, power, 27),
            reads,
            writes,
            configured,
        )
    }

    fn power_with_secondary_online() -> PsciCpuPowerCoordinator {
        let mut power =
            PsciCpuPowerCoordinator::new(&[0, 1]).expect("test power topology should build");
        let PsciCoordinatedDispatch::Coordinate(PsciCoordinatorRequest::CpuOn(request)) =
            handle_coordinated_call(PsciCall::from_arguments(
                PSCI_CPU_ON_64,
                [1, SECONDARY_ENTRY, SECONDARY_CONTEXT],
            ))
        else {
            panic!("test CPU_ON should decode");
        };
        let PsciCpuOnBegin::Pending(work) = power
            .begin_cpu_on(request, |_| true)
            .expect("test CPU_ON should begin")
        else {
            panic!("test secondary should begin pending");
        };
        power
            .finish_target_setup(work.token(), true)
            .expect("test secondary setup should finish");
        power
            .commit_caller_completion(work.token())
            .expect("test caller completion should commit");
        power
            .mark_target_entered(work.token())
            .expect("test secondary should enter");
        power
    }

    fn paused_two_member_session(
        power: PsciCpuPowerCoordinator,
        online_indexes: &[usize],
    ) -> HvfArm64BootVcpuSession<'static> {
        let (primary, _, _) = start_coordinated_psci_run_step_recording_runner(0, [0; 3], 0, false);
        let (secondary, _, _) =
            start_coordinated_psci_run_step_recording_runner(0, [0; 3], 0, false);
        let coordinator = HvfVcpuRunCoordinator::from_test_runners(
            vec![primary, secondary],
            vec![0, 1],
            Arc::new(Mutex::new(MmioDispatcher::new())),
            online_indexes,
        )
        .expect("two-member coordinator should build");
        let session = HvfArm64BootVcpuSession::new(coordinator, power, 27);
        session
            .control()
            .request_pause()
            .expect("idle pause should start")
            .wait()
            .expect("idle pause should complete");
        session
    }

    #[test]
    fn stable_export_rejects_staged_cpu_on_and_cpu_off_transactions() {
        let PsciCoordinatedDispatch::Coordinate(PsciCoordinatorRequest::CpuOn(request)) =
            handle_coordinated_call(PsciCall::from_arguments(
                PSCI_CPU_ON_64,
                [1, SECONDARY_ENTRY, SECONDARY_CONTEXT],
            ))
        else {
            panic!("test CPU_ON should decode");
        };
        let mut on_pending =
            PsciCpuPowerCoordinator::new(&[0, 1]).expect("power topology should build");
        assert!(matches!(
            on_pending
                .begin_cpu_on(request, |_| true)
                .expect("CPU_ON should begin"),
            PsciCpuOnBegin::Pending(_)
        ));
        let mut session = paused_two_member_session(on_pending, &[0]);
        let error = session
            .capture_stable_paused_topology()
            .expect_err("staged CPU_ON should reject export");
        assert_eq!(error.stage(), "transient PSCI power work");
        assert_eq!(error.index(), Some(1));
        session.shutdown().expect("session should shut down");

        let mut off_pending = power_with_secondary_online();
        assert!(matches!(
            off_pending.begin_cpu_off(1).expect("CPU_OFF should begin"),
            PsciCpuOffBegin::Pending(_)
        ));
        let mut session = paused_two_member_session(off_pending, &[0, 1]);
        let error = session
            .capture_stable_paused_topology()
            .expect_err("staged CPU_OFF should reject export");
        assert_eq!(error.stage(), "transient PSCI power work");
        assert_eq!(error.index(), Some(1));
        session.shutdown().expect("session should shut down");
    }

    #[test]
    fn barrier_policy_admits_rejects_or_abandons_cpu_on_by_reason() {
        assert_eq!(
            barrier_cpu_on_admission(HvfVcpuRunControlReason::Wakeup),
            Some(true)
        );
        assert_eq!(
            barrier_cpu_on_admission(HvfVcpuRunControlReason::Pause),
            Some(false)
        );
        assert_eq!(
            barrier_cpu_on_admission(HvfVcpuRunControlReason::Stop),
            None
        );
        assert_eq!(
            barrier_cpu_on_admission(HvfVcpuRunControlReason::Shutdown),
            None
        );
    }

    #[test]
    fn cpu_suspend_session_defers_success_until_timer_ppi_wakeup() {
        let arguments = [0xfeed_face, 0x1234_5678, u64::MAX];
        let (runner, reads, writes, timer_samples, ppi) =
            start_cpu_suspend_retained_runner(PSCI_CPU_SUSPEND, arguments, 1, 100, [Ok(100)]);
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let coordinator =
            HvfVcpuRunCoordinator::from_test_runners(vec![runner], vec![0], dispatcher, &[0])
                .expect("single test coordinator should build");
        let power = PsciCpuPowerCoordinator::new(&[0]).expect("test power topology should build");
        let mut session = HvfArm64BootVcpuSession::new(coordinator, power, 27);

        assert!(matches!(
            session.run_step(|_| true),
            Ok(HvfVcpuRunStepOutcome::CpuSuspend {
                index: 0,
                mpidr: 0,
                function_id: PSCI_CPU_SUSPEND,
                ..
            })
        ));
        assert_eq!(session.power.power_state(0), Some(PsciCpuPowerState::On));
        assert_eq!(writes.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert_eq!(timer_samples.try_recv(), Err(mpsc::TryRecvError::Empty));

        assert!(matches!(
            session.run_step(|_| true),
            Ok(HvfVcpuRunStepOutcome::Hvc {
                function_id: PSCI_CPU_SUSPEND,
                return_value: 0,
                ..
            })
        ));
        timer_samples
            .recv()
            .expect("suspended member should sample its retained timer");
        assert_eq!(
            ppi.recv()
                .expect("timer wake should publish the configured PPI"),
            (27, true)
        );
        assert_eq!(
            writes
                .recv()
                .expect("timer wake should complete CPU_SUSPEND X0"),
            (HvfRegister::X0, 0)
        );
        assert_eq!(
            reads.try_iter().collect::<Vec<_>>(),
            vec![
                HvfRegister::X0,
                HvfRegister::X1,
                HvfRegister::X2,
                HvfRegister::X3,
            ]
        );
        assert_eq!(session.power.power_state(0), Some(PsciCpuPowerState::On));
        assert!(session.pending_cpu_suspends[0].is_none());
        session.shutdown().expect("test session should shut down");
    }

    #[test]
    fn cpu_suspend_control_cancellation_rearms_without_completing_x0() {
        let (runner, _reads, writes, timer_samples, _ppi) =
            start_cpu_suspend_retained_runner(PSCI_CPU_SUSPEND, [1, 2, 3], 0, 100, [Ok(1), Ok(2)]);
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let coordinator =
            HvfVcpuRunCoordinator::from_test_runners(vec![runner], vec![0], dispatcher, &[0])
                .expect("single test coordinator should build");
        let power = PsciCpuPowerCoordinator::new(&[0]).expect("test power topology should build");
        let mut session = HvfArm64BootVcpuSession::new(coordinator, power, 27);
        assert!(matches!(
            session.run_step(|_| true),
            Ok(HvfVcpuRunStepOutcome::CpuSuspend { .. })
        ));

        let wakeup_control = session.control();
        thread::scope(|scope| {
            let step = scope.spawn(|| session.run_step(|_| true));
            timer_samples
                .recv()
                .expect("first retained wait should sample");
            let waiter = wakeup_control
                .request_wakeup()
                .expect("wakeup should cancel retained wait");
            assert!(matches!(
                step.join().expect("session step should not panic"),
                Ok(HvfVcpuRunStepOutcome::Canceled)
            ));
            waiter.wait().expect("wakeup barrier should complete");
        });
        assert!(session.pending_cpu_suspends[0].is_some());
        assert_eq!(writes.try_recv(), Err(mpsc::TryRecvError::Empty));

        let stop_control = session.control();
        thread::scope(|scope| {
            let step = scope.spawn(|| session.run_step(|_| true));
            timer_samples
                .recv()
                .expect("wakeup should rearm the retained wait");
            let waiter = stop_control
                .request_stop()
                .expect("stop should cancel retained wait");
            assert!(matches!(
                step.join().expect("session step should not panic"),
                Ok(HvfVcpuRunStepOutcome::Canceled)
            ));
            waiter.wait().expect("stop barrier should complete");
        });
        assert!(session.pending_cpu_suspends[0].is_some());
        assert_eq!(writes.try_recv(), Err(mpsc::TryRecvError::Empty));
        session.shutdown().expect("test session should shut down");
    }

    #[test]
    fn cpu_off_session_denies_the_last_online_cpu_with_a_normal_response() {
        let (runner, reads, writes) =
            start_coordinated_psci_run_step_recording_runner(PSCI_CPU_OFF, [u64::MAX; 3], 0, false);
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let coordinator =
            HvfVcpuRunCoordinator::from_test_runners(vec![runner], vec![0], dispatcher, &[0])
                .expect("single test coordinator should build");
        let power = PsciCpuPowerCoordinator::new(&[0]).expect("test power topology should build");
        let mut session = HvfArm64BootVcpuSession::new(coordinator, power, 27);

        assert!(matches!(
            session.run_step(|_| true),
            Ok(HvfVcpuRunStepOutcome::Hvc {
                function_id: PSCI_CPU_OFF,
                return_value,
                ..
            }) if return_value == PsciStatus::Denied.return_value()
        ));
        assert_eq!(reads.try_iter().collect::<Vec<_>>(), [HvfRegister::X0]);
        assert_eq!(
            writes.recv().expect("denied CPU_OFF should write X0"),
            (HvfRegister::X0, PsciStatus::Denied.return_value())
        );
        assert_eq!(session.power.power_state(0), Some(PsciCpuPowerState::On));
        session.shutdown().expect("test session should shut down");
    }

    #[test]
    fn cpu_off_session_removes_secondary_before_publishing_off() {
        let (primary, _configured) = start_secondary_configure_recording_runner(false);
        let (secondary, reads, writes) =
            start_coordinated_psci_run_step_recording_runner(PSCI_CPU_OFF, [u64::MAX; 3], 0, false);
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let coordinator = HvfVcpuRunCoordinator::from_test_runners(
            vec![primary, secondary],
            vec![0, 1],
            dispatcher,
            &[0, 1],
        )
        .expect("two-member test coordinator should build");
        let mut session =
            HvfArm64BootVcpuSession::new(coordinator, power_with_secondary_online(), 27);

        let mut observed = None;
        for _ in 0..4 {
            let outcome = session
                .run_step(|_| true)
                .expect("test topology step should succeed");
            if matches!(outcome, HvfVcpuRunStepOutcome::CpuOff { index: 1, .. }) {
                observed = Some(outcome);
                break;
            }
        }
        assert!(matches!(
            observed,
            Some(HvfVcpuRunStepOutcome::CpuOff {
                index: 1,
                mpidr: 1,
                function_id: PSCI_CPU_OFF,
                ..
            })
        ));
        assert_eq!(session.power.power_state(1), Some(PsciCpuPowerState::Off));
        assert_eq!(reads.try_iter().collect::<Vec<_>>(), [HvfRegister::X0]);
        assert_eq!(writes.try_recv(), Err(mpsc::TryRecvError::Empty));

        let _ = session
            .run_step(|_| true)
            .expect("remaining primary should continue after CPU1 off");
        assert!(reads.try_iter().next().is_none());
        session.shutdown().expect("test session should shut down");
    }

    #[test]
    fn cpu_on_session_configures_and_admits_target_before_success() {
        let (mut session, reads, writes, configured) = cpu_on_session(false);

        let outcome = session
            .run_step(|entry| entry == SECONDARY_ENTRY)
            .expect("CPU_ON should complete through the session adapter");

        assert!(matches!(
            outcome,
            HvfVcpuRunStepOutcome::Hvc {
                function_id: PSCI_CPU_ON_64,
                return_value: 0,
                ..
            }
        ));
        assert_eq!(
            reads.try_iter().collect::<Vec<_>>(),
            vec![
                HvfRegister::X0,
                HvfRegister::X1,
                HvfRegister::X2,
                HvfRegister::X3,
            ]
        );
        assert_eq!(
            configured
                .recv()
                .expect("secondary setup should be observed"),
            HvfArm64SecondaryBootRegisters::new(
                GuestAddress::new(SECONDARY_ENTRY),
                SECONDARY_CONTEXT,
            )
        );
        assert_eq!(
            writes.recv().expect("caller completion should be observed"),
            (HvfRegister::X0, 0)
        );
        let HvfVcpuRunEvent::Member(target) = session
            .coordinator
            .receive_event()
            .expect("admitted target should complete")
        else {
            panic!("expected target member completion");
        };
        assert_eq!(target.index(), 1);
        assert!(matches!(
            target.result(),
            Ok(crate::coordinator::HvfVcpuRunMemberOutcome::Handled(
                HvfVcpuRunStepOutcome::Canceled
            ))
        ));
        session.shutdown().expect("test session should shut down");
    }

    #[test]
    fn cpu_on_session_reports_internal_failure_without_target_admission() {
        let (mut session, _reads, writes, configured) = cpu_on_session(true);

        let outcome = session
            .run_step(|entry| entry == SECONDARY_ENTRY)
            .expect("setup failure should return a PSCI response");

        assert!(matches!(
            outcome,
            HvfVcpuRunStepOutcome::Hvc {
                function_id: PSCI_CPU_ON_64,
                return_value,
                ..
            } if return_value == PsciStatus::InternalFailure.return_value()
        ));
        assert_eq!(
            configured
                .recv()
                .expect("failed secondary setup should be observed"),
            HvfArm64SecondaryBootRegisters::new(
                GuestAddress::new(SECONDARY_ENTRY),
                SECONDARY_CONTEXT,
            )
        );
        assert_eq!(
            writes
                .recv()
                .expect("caller failure response should be observed"),
            (HvfRegister::X0, PsciStatus::InternalFailure.return_value())
        );
        session.shutdown().expect("test session should shut down");
    }
}
