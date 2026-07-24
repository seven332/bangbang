//! Native-v1 snapshot load preparation and restore orchestration.

use std::fmt;
use std::fs::File;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use bangbang_pager::{
    MAX_FRAME_BYTES, MAX_IN_FLIGHT, PagerLimits, PagerOperations, PagerRegionId, PagerSessionId,
};
use bangbang_runtime::BackendError;
use bangbang_runtime::lazy_memory::{
    LazyGuestMemory, LazyGuestMemoryError, LazyGuestMemoryLimits, LazyGuestMemoryRegion,
    MAX_LAZY_MEMORY_WAITERS,
};
use bangbang_runtime::memory::{GuestMemory, GuestMemoryLayout, GuestMemoryRange};
use bangbang_runtime::rtc::RTC_MMIO_DEVICE_WINDOW_SIZE;
use bangbang_runtime::snapshot_artifact::{
    LoadedSnapshotArtifacts, PreparedSnapshotState, SnapshotArtifactLoadError,
    load_prepared_snapshot_memory_file, load_prepared_snapshot_memory_path,
};
use bangbang_runtime::snapshot_memory::{
    SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES, SnapshotMemoryBinding,
};
use bangbang_runtime::startup::{
    InstallSnapshotV1RuntimeError, InstalledSnapshotV1Runtime, InstalledSnapshotV1RuntimeParts,
    PrepareSnapshotV1DeviceProfileError, install_snapshot_v1_runtime,
    install_snapshot_v1_runtime_with_memory_owner, prepare_snapshot_v1_device_profile,
    prepare_snapshot_v1_device_profile_with_root_backing, prepare_snapshot_v1_root_backing_file,
};

use crate::backend::HvfBackend;
use crate::coordinator::HvfVcpuRunCoordinatorError;
use crate::gic::HvfGicError;
use crate::lazy_host_fault::{
    HvfLazyGuestMemoryConsumer, HvfLazyHostFaultBridge, HvfLazyHostFaultError,
};
use crate::lazy_pager::{HvfLazyPager, HvfLazyPagerError};
use crate::runner::HvfVcpuRunnerError;
use crate::snapshot_bundle::{HvfSnapshotV1Bundle, HvfSnapshotV1BundleError, HvfSnapshotV1State};
use crate::startup::{HvfArm64BootTimeIdentityRestoreError, HvfArm64BootVmGenIdRestoreError};

const REDACTED: &str = "<redacted>";

/// A complete native-v1 load value prepared without constructing an HVF VM.
pub struct PreparedHvfSnapshotV1Load {
    state: HvfSnapshotV1State,
    runtime: PreparedHvfSnapshotV1Runtime,
}

enum PreparedHvfSnapshotV1Runtime {
    Eager(InstalledSnapshotV1Runtime),
    Lazy(InstalledSnapshotV1Runtime<HvfLazyGuestMemoryConsumer>),
}

/// Borrowed memory-owner-independent values from a prepared native-v1 runtime.
pub struct PreparedHvfSnapshotV1RuntimeRef<'a> {
    pub mmio_dispatcher: &'a bangbang_runtime::mmio::MmioDispatcher,
    pub runtime_resources: &'a bangbang_runtime::startup::Arm64BootRuntimeResources,
    pub drive_config: &'a bangbang_runtime::block::DriveConfig,
    pub block_retry: &'a bangbang_runtime::snapshot_device::SnapshotV1BlockRetryState,
    pub serial_output: &'a bangbang_runtime::serial::SharedSerialOutput,
    pub serial_output_buffer: &'a bangbang_runtime::serial::SharedSerialOutputBuffer,
}

impl fmt::Debug for PreparedHvfSnapshotV1RuntimeRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PreparedHvfSnapshotV1RuntimeRef(<redacted>)")
    }
}

impl<'a, MemoryOwner> From<&'a InstalledSnapshotV1Runtime<MemoryOwner>>
    for PreparedHvfSnapshotV1RuntimeRef<'a>
{
    fn from(runtime: &'a InstalledSnapshotV1Runtime<MemoryOwner>) -> Self {
        Self {
            mmio_dispatcher: &runtime.mmio_dispatcher,
            runtime_resources: &runtime.runtime_resources,
            drive_config: &runtime.drive_config,
            block_retry: &runtime.block_retry,
            serial_output: &runtime.serial_output,
            serial_output_buffer: &runtime.serial_output_buffer,
        }
    }
}

pub(crate) enum PreparedHvfSnapshotV1RuntimeMemory {
    Eager(GuestMemory),
    Lazy(HvfLazyGuestMemoryConsumer),
}

/// Decoded native-v1 state retained before exact memory/root adoption.
pub struct PreparedHvfSnapshotV1State {
    record: bangbang_runtime::snapshot_commit::SnapshotCommitRecord,
    state: HvfSnapshotV1State,
}

impl PreparedHvfSnapshotV1State {
    /// Decodes and destination-validates state without loading guest memory.
    pub fn from_prepared_state(
        prepared: PreparedSnapshotState,
    ) -> Result<Self, PrepareHvfSnapshotV1LoadError> {
        let record = prepared.into_record();
        let bundle = HvfSnapshotV1Bundle::try_from_commit_record(record.clone())
            .map_err(PrepareHvfSnapshotV1LoadError::Bundle)?;
        validate_destination_cache(bundle.state())?;
        Ok(Self {
            record,
            state: bundle.into_state(),
        })
    }

    /// Returns the persisted root-backing selector to the authority owner.
    pub fn root_backing_path(&self) -> &Path {
        self.state.device().root_block().path()
    }

    /// Validates and retains exact lazy topology before a peer is acquired.
    pub fn prepare_lazy(
        self,
    ) -> Result<PreparedHvfSnapshotV1LazyState, PrepareHvfSnapshotV1LoadError> {
        let topology =
            PreparedHvfSnapshotV1LazyTopology::from_binding(self.record.memory_binding())?;
        validate_platform_composition_ranges(&self.state, topology.layout.ranges().iter().copied())
            .map_err(PrepareHvfSnapshotV1LoadError::Platform)?;
        Ok(PreparedHvfSnapshotV1LazyState {
            state: self.state,
            topology,
        })
    }

    /// Loads exact memory against the retained commit without re-decoding state.
    pub fn load_memory_file(
        self,
        memory: File,
    ) -> Result<PreparedHvfSnapshotV1Memory, SnapshotArtifactLoadError> {
        let artifacts = load_prepared_snapshot_memory_file(
            PreparedSnapshotState::from_record(self.record),
            memory,
        )?;
        let (_, memory) = artifacts.into_parts();
        Ok(PreparedHvfSnapshotV1Memory {
            state: self.state,
            memory,
        })
    }

    /// Opens and loads an ordinary memory path against the retained commit.
    pub fn load_memory_path(
        self,
        memory: &Path,
    ) -> Result<PreparedHvfSnapshotV1Memory, SnapshotArtifactLoadError> {
        let artifacts = load_prepared_snapshot_memory_path(
            PreparedSnapshotState::from_record(self.record),
            memory,
        )?;
        let (_, memory) = artifacts.into_parts();
        Ok(PreparedHvfSnapshotV1Memory {
            state: self.state,
            memory,
        })
    }
}

/// State and topology validated before consuming one pager connection.
pub struct PreparedHvfSnapshotV1LazyState {
    state: HvfSnapshotV1State,
    topology: PreparedHvfSnapshotV1LazyTopology,
}

impl PreparedHvfSnapshotV1LazyState {
    /// Returns the persisted root-backing selector to the authority owner.
    pub fn root_backing_path(&self) -> &Path {
        self.state.device().root_block().path()
    }

    /// Validates and retains the exact root backing before peer acquisition.
    pub fn prepare_root_backing(
        self,
        root_backing: Option<File>,
    ) -> Result<PreparedHvfSnapshotV1LazyLoad, PrepareHvfSnapshotV1LoadError> {
        let root_backing = prepare_snapshot_v1_root_backing_file(self.state.device(), root_backing)
            .map_err(PrepareHvfSnapshotV1LoadError::Device)?;
        Ok(PreparedHvfSnapshotV1LazyLoad {
            state: self.state,
            topology: self.topology,
            root_backing,
        })
    }
}

/// State, topology, and root backing validated before consuming one pager.
pub struct PreparedHvfSnapshotV1LazyLoad {
    state: HvfSnapshotV1State,
    topology: PreparedHvfSnapshotV1LazyTopology,
    root_backing: File,
}

impl PreparedHvfSnapshotV1LazyLoad {
    /// Completes demand-backed preparation over one already-connected peer.
    pub fn finish(
        self,
        stream: UnixStream,
        timeout: Duration,
        now: Instant,
    ) -> Result<PreparedHvfSnapshotV1Load, PrepareHvfSnapshotV1LoadError> {
        let memory = Arc::new(
            LazyGuestMemory::new(self.topology.limits, self.topology.regions)
                .map_err(PrepareHvfSnapshotV1LoadError::LazyMemory)?,
        );
        let pager = Arc::new(
            HvfLazyPager::connect_with_session(
                Arc::clone(&memory),
                stream,
                timeout,
                self.topology.session,
            )
            .map_err(PrepareHvfSnapshotV1LoadError::LazyPager)?,
        );
        let prepared = (|| {
            let bridge = HvfLazyHostFaultBridge::install(
                Arc::clone(&memory),
                Arc::<HvfLazyPager>::clone(&pager),
            )
            .map_err(PrepareHvfSnapshotV1LoadError::LazyHostFault)?;
            let mut consumer = bridge
                .into_guest_memory_consumer()
                .map_err(PrepareHvfSnapshotV1LoadError::LazyHostFault)?;
            consumer.cancel_source_on_drop();
            validate_platform_composition(&self.state, consumer.memory())
                .map_err(PrepareHvfSnapshotV1LoadError::Platform)?;
            let profile = prepare_snapshot_v1_device_profile_with_root_backing(
                self.state.device(),
                consumer.memory(),
                now,
                Some(self.root_backing),
            )
            .map_err(PrepareHvfSnapshotV1LoadError::Device)?;
            let runtime = install_snapshot_v1_runtime_with_memory_owner(
                profile,
                self.state.machine(),
                consumer,
                self.topology.layout,
                self.state.compatibility().rtc_mmio_layout(),
            )
            .map_err(PrepareHvfSnapshotV1LoadError::Install)?;
            Ok(PreparedHvfSnapshotV1Load {
                state: self.state,
                runtime: PreparedHvfSnapshotV1Runtime::Lazy(runtime),
            })
        })();
        if prepared.is_err() {
            let _ = pager.cancel();
        }
        prepared
    }
}

impl fmt::Debug for PreparedHvfSnapshotV1LazyState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedHvfSnapshotV1LazyState")
            .field("profile", &"native-v1-lazy")
            .field("state", &REDACTED)
            .finish()
    }
}

impl fmt::Debug for PreparedHvfSnapshotV1LazyLoad {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedHvfSnapshotV1LazyLoad")
            .field("profile", &"native-v1-lazy")
            .field("state", &REDACTED)
            .finish()
    }
}

struct PreparedHvfSnapshotV1LazyTopology {
    limits: LazyGuestMemoryLimits,
    regions: Vec<LazyGuestMemoryRegion>,
    layout: GuestMemoryLayout,
    session: PagerSessionId,
}

impl PreparedHvfSnapshotV1LazyTopology {
    fn from_binding(
        binding: &SnapshotMemoryBinding,
    ) -> Result<Self, PrepareHvfSnapshotV1LoadError> {
        let page_size = u32::try_from(
            crate::memory::host_page_size()
                .map_err(|_| PrepareHvfSnapshotV1LoadError::LazyTopology)?,
        )
        .map_err(|_| PrepareHvfSnapshotV1LoadError::LazyTopology)?;
        let region_count = u16::try_from(binding.ranges().len())
            .map_err(|_| PrepareHvfSnapshotV1LoadError::LazyTopology)?;
        let pager = PagerLimits::new(
            page_size,
            region_count,
            MAX_IN_FLIGHT,
            u32::try_from(MAX_FRAME_BYTES)
                .map_err(|_| PrepareHvfSnapshotV1LoadError::LazyTopology)?,
            PagerOperations::v1(),
        )
        .map_err(|_| PrepareHvfSnapshotV1LoadError::LazyTopology)?;
        let page_size_u64 = u64::from(page_size);
        let total_pages = binding
            .data_length()
            .checked_div(page_size_u64)
            .filter(|_| binding.data_length().is_multiple_of(page_size_u64))
            .ok_or(PrepareHvfSnapshotV1LoadError::LazyTopology)?;
        let limits = LazyGuestMemoryLimits::new(pager, total_pages, MAX_LAZY_MEMORY_WAITERS)
            .map_err(PrepareHvfSnapshotV1LoadError::LazyMemory)?;
        let mut regions = Vec::new();
        regions
            .try_reserve_exact(binding.ranges().len())
            .map_err(|_| PrepareHvfSnapshotV1LoadError::LazyTopology)?;
        let mut ranges = Vec::new();
        ranges
            .try_reserve_exact(binding.ranges().len())
            .map_err(|_| PrepareHvfSnapshotV1LoadError::LazyTopology)?;
        let header = u64::try_from(SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES)
            .map_err(|_| PrepareHvfSnapshotV1LoadError::LazyTopology)?;
        for (index, range) in binding.ranges().iter().copied().enumerate() {
            let id = u32::try_from(index)
                .ok()
                .and_then(|index| index.checked_add(1))
                .and_then(|id| PagerRegionId::new(id).ok())
                .ok_or(PrepareHvfSnapshotV1LoadError::LazyTopology)?;
            let source_offset = range
                .file_offset()
                .checked_sub(header)
                .ok_or(PrepareHvfSnapshotV1LoadError::LazyTopology)?;
            let guest = range.range();
            regions.push(
                LazyGuestMemoryRegion::new(id, guest, source_offset, page_size)
                    .map_err(PrepareHvfSnapshotV1LoadError::LazyMemory)?,
            );
            ranges.push(guest);
        }
        let layout = GuestMemoryLayout::new(ranges)
            .map_err(|_| PrepareHvfSnapshotV1LoadError::LazyTopology)?;
        let session = PagerSessionId::from_bytes(binding.pager_v1_session_bytes())
            .map_err(|_| PrepareHvfSnapshotV1LoadError::LazyTopology)?;
        Ok(Self {
            limits,
            regions,
            layout,
            session,
        })
    }
}

impl fmt::Debug for PreparedHvfSnapshotV1State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedHvfSnapshotV1State")
            .field("profile", &"native-v1")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Decoded state plus bound anonymous memory awaiting root-backing adoption.
pub struct PreparedHvfSnapshotV1Memory {
    state: HvfSnapshotV1State,
    memory: GuestMemory,
}

impl PreparedHvfSnapshotV1Memory {
    /// Completes off-side preparation with an optional exact root backing.
    pub fn finish(
        self,
        root_backing: Option<File>,
        now: Instant,
    ) -> Result<PreparedHvfSnapshotV1Load, PrepareHvfSnapshotV1LoadError> {
        validate_platform_composition(&self.state, &self.memory)
            .map_err(PrepareHvfSnapshotV1LoadError::Platform)?;
        let profile = prepare_snapshot_v1_device_profile_with_root_backing(
            self.state.device(),
            &self.memory,
            now,
            root_backing,
        )
        .map_err(PrepareHvfSnapshotV1LoadError::Device)?;
        let runtime = install_snapshot_v1_runtime(
            profile,
            self.state.machine(),
            self.memory,
            self.state.compatibility().rtc_mmio_layout(),
        )
        .map_err(PrepareHvfSnapshotV1LoadError::Install)?;
        Ok(PreparedHvfSnapshotV1Load {
            state: self.state,
            runtime: PreparedHvfSnapshotV1Runtime::Eager(runtime),
        })
    }
}

impl fmt::Debug for PreparedHvfSnapshotV1Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedHvfSnapshotV1Memory")
            .field("profile", &"native-v1")
            .field("state", &REDACTED)
            .finish()
    }
}

impl PreparedHvfSnapshotV1Load {
    /// Decode, cross-validate, prepare, and install all off-side load owners.
    pub fn from_loaded_artifacts(
        artifacts: LoadedSnapshotArtifacts,
        now: Instant,
    ) -> Result<Self, PrepareHvfSnapshotV1LoadError> {
        let (record, memory) = artifacts.into_parts();
        let bundle = HvfSnapshotV1Bundle::try_from_commit_record(record)
            .map_err(PrepareHvfSnapshotV1LoadError::Bundle)?;
        validate_platform_composition(bundle.state(), &memory)
            .map_err(PrepareHvfSnapshotV1LoadError::Platform)?;

        validate_destination_cache(bundle.state())?;

        let profile = prepare_snapshot_v1_device_profile(bundle.state().device(), &memory, now)
            .map_err(PrepareHvfSnapshotV1LoadError::Device)?;
        let state = bundle.into_state();
        let runtime = install_snapshot_v1_runtime(
            profile,
            state.machine(),
            memory,
            state.compatibility().rtc_mmio_layout(),
        )
        .map_err(PrepareHvfSnapshotV1LoadError::Install)?;

        Ok(Self {
            state,
            runtime: PreparedHvfSnapshotV1Runtime::Eager(runtime),
        })
    }

    pub const fn state(&self) -> &HvfSnapshotV1State {
        &self.state
    }

    pub fn runtime(&self) -> PreparedHvfSnapshotV1RuntimeRef<'_> {
        match &self.runtime {
            PreparedHvfSnapshotV1Runtime::Eager(runtime) => runtime.into(),
            PreparedHvfSnapshotV1Runtime::Lazy(runtime) => runtime.into(),
        }
    }

    pub fn drive_config(&self) -> &bangbang_runtime::block::DriveConfig {
        self.runtime().drive_config
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        HvfSnapshotV1State,
        PreparedHvfSnapshotV1RuntimeMemory,
        InstalledSnapshotV1RuntimeParts,
    ) {
        let (memory, runtime) = match self.runtime {
            PreparedHvfSnapshotV1Runtime::Eager(runtime) => {
                let (memory, runtime) = runtime.into_parts();
                (PreparedHvfSnapshotV1RuntimeMemory::Eager(memory), runtime)
            }
            PreparedHvfSnapshotV1Runtime::Lazy(runtime) => {
                let (memory, runtime) = runtime.into_parts();
                (PreparedHvfSnapshotV1RuntimeMemory::Lazy(memory), runtime)
            }
        };
        (self.state, memory, runtime)
    }
}

fn validate_destination_cache(
    state: &HvfSnapshotV1State,
) -> Result<(), PrepareHvfSnapshotV1LoadError> {
    let destination_cache = HvfBackend::arm64_vcpu_cache_manifest()
        .map_err(PrepareHvfSnapshotV1LoadError::CacheQuery)?;
    if destination_cache != state.compatibility().cache_manifest() {
        return Err(PrepareHvfSnapshotV1LoadError::CacheMismatch);
    }
    Ok(())
}

impl fmt::Debug for PreparedHvfSnapshotV1Load {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedHvfSnapshotV1Load")
            .field("profile", &"native-v1")
            .field("state", &REDACTED)
            .finish()
    }
}

#[derive(Debug)]
pub enum PrepareHvfSnapshotV1LoadError {
    Bundle(HvfSnapshotV1BundleError),
    Platform(HvfSnapshotV1PlatformError),
    CacheQuery(BackendError),
    CacheMismatch,
    LazyTopology,
    LazyMemory(LazyGuestMemoryError),
    LazyPager(HvfLazyPagerError),
    LazyHostFault(HvfLazyHostFaultError),
    Device(PrepareSnapshotV1DeviceProfileError),
    Install(InstallSnapshotV1RuntimeError),
}

impl fmt::Display for PrepareHvfSnapshotV1LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bundle(source) => write!(f, "native-v1 bundle preparation failed: {source}"),
            Self::Platform(source) => {
                write!(f, "native-v1 platform composition is invalid: {source}")
            }
            Self::CacheQuery(_) => f.write_str("native-v1 destination cache manifest query failed"),
            Self::CacheMismatch => {
                f.write_str("native-v1 destination cache manifest is incompatible")
            }
            Self::LazyTopology => f.write_str("native-v1 lazy memory topology is incompatible"),
            Self::LazyMemory(_) => f.write_str("native-v1 lazy memory construction failed"),
            Self::LazyPager(_) => f.write_str("native-v1 bangbang-pager-v1 negotiation failed"),
            Self::LazyHostFault(_) => {
                f.write_str("native-v1 lazy host fault bridge preparation failed")
            }
            Self::Device(source) => write!(f, "native-v1 device preparation failed: {source}"),
            Self::Install(source) => write!(f, "native-v1 runtime installation failed: {source}"),
        }
    }
}

impl std::error::Error for PrepareHvfSnapshotV1LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Bundle(source) => Some(source),
            Self::Platform(source) => Some(source),
            Self::CacheQuery(source) => Some(source),
            Self::LazyMemory(source) => Some(source),
            Self::LazyPager(source) => Some(source),
            Self::LazyHostFault(source) => Some(source),
            Self::Device(source) => Some(source),
            Self::Install(source) => Some(source),
            Self::CacheMismatch | Self::LazyTopology => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfSnapshotV1PlatformError {
    InterruptRange,
    InterruptAssignment,
    RegionIdConflict,
    RegionOverflow,
    MmioOverlap,
    MmioMemoryOverlap,
}

impl fmt::Display for HvfSnapshotV1PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InterruptRange => "GIC SPI range does not contain the baseline device lines",
            Self::InterruptAssignment => "baseline device interrupt assignment is noncanonical",
            Self::RegionIdConflict => "baseline device MMIO region IDs conflict",
            Self::RegionOverflow => "baseline platform MMIO range overflows",
            Self::MmioOverlap => "baseline platform MMIO ranges overlap",
            Self::MmioMemoryOverlap => "baseline device MMIO overlaps guest memory",
        };
        f.write_str(message)
    }
}

impl std::error::Error for HvfSnapshotV1PlatformError {}

/// Stable construction or restore stage for a native-v1 destination VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfSnapshotV1RestoreStage {
    CreateVm,
    CreateGic,
    ValidateGic,
    EnableDirtyTracking,
    MapMemory,
    StartRunner,
    StartBlockRetryScheduler,
    RestoreRunnerState,
    ReplaceVmGenId,
    RestoreTimeIdentity,
    AssembleSession,
}

impl fmt::Display for HvfSnapshotV1RestoreStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::CreateVm => "VM creation",
            Self::CreateGic => "GIC creation",
            Self::ValidateGic => "GIC compatibility validation",
            Self::EnableDirtyTracking => "dirty tracking initialization",
            Self::MapMemory => "guest-memory mapping",
            Self::StartRunner => "vCPU runner startup",
            Self::StartBlockRetryScheduler => "block retry scheduler startup",
            Self::RestoreRunnerState => "aggregate runner restore",
            Self::ReplaceVmGenId => "VMGenID replacement",
            Self::RestoreTimeIdentity => "VMClock and VMGenID restore",
            Self::AssembleSession => "restored session assembly",
        };
        f.write_str(name)
    }
}

/// Whether a failed load can safely retry in the same process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfSnapshotV1RestoreDisposition {
    Retryable,
    Terminal,
}

/// Value-free cleanup evidence retained with a failed restore.
#[derive(Debug)]
pub struct HvfSnapshotV1RestoreCleanup {
    scheduler_failed: bool,
    runner: Option<Box<HvfVcpuRunnerError>>,
    coordinator: Option<Box<HvfVcpuRunCoordinatorError>>,
    backend: Option<BackendError>,
}

impl HvfSnapshotV1RestoreCleanup {
    pub(crate) fn new(
        scheduler_failed: bool,
        runner: Option<HvfVcpuRunnerError>,
        backend: Option<BackendError>,
    ) -> Self {
        Self {
            scheduler_failed,
            runner: runner.map(Box::new),
            coordinator: None,
            backend,
        }
    }

    pub(crate) fn with_coordinator(
        scheduler_failed: bool,
        coordinator: Option<HvfVcpuRunCoordinatorError>,
        backend: Option<BackendError>,
    ) -> Self {
        Self {
            scheduler_failed,
            runner: None,
            coordinator: coordinator.map(Box::new),
            backend,
        }
    }

    pub const fn is_complete(&self) -> bool {
        !self.scheduler_failed
            && self.runner.is_none()
            && self.coordinator.is_none()
            && self.backend.is_none()
    }

    pub const fn scheduler_failed(&self) -> bool {
        self.scheduler_failed
    }

    pub fn runner_error(&self) -> Option<&HvfVcpuRunnerError> {
        self.runner.as_deref()
    }

    pub fn coordinator_error(&self) -> Option<&HvfVcpuRunCoordinatorError> {
        self.coordinator.as_deref()
    }

    pub const fn backend_error(&self) -> Option<&BackendError> {
        self.backend.as_ref()
    }
}

#[derive(Debug)]
pub enum HvfSnapshotV1RestoreFailure {
    Backend(BackendError),
    Gic(HvfGicError),
    GicMetadataMismatch,
    DirtyTracking,
    MemoryMapping,
    Runner(Box<HvfVcpuRunnerError>),
    Coordinator(Box<HvfVcpuRunCoordinatorError>),
    Scheduler(std::io::ErrorKind),
    VmGenId(Box<HvfArm64BootVmGenIdRestoreError>),
    TimeIdentity(Box<HvfArm64BootTimeIdentityRestoreError>),
    InvalidRuntime,
}

impl fmt::Display for HvfSnapshotV1RestoreFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(_) => f.write_str("native-v1 HVF backend operation failed"),
            Self::Gic(_) => f.write_str("native-v1 GIC operation failed"),
            Self::GicMetadataMismatch => {
                f.write_str("native-v1 destination GIC metadata is incompatible")
            }
            Self::DirtyTracking => f.write_str("native-v1 dirty tracking initialization failed"),
            Self::MemoryMapping => f.write_str("native-v1 guest-memory mapping failed"),
            Self::Runner(source) => write!(f, "native-v1 runner operation failed: {source}"),
            Self::Coordinator(source) => {
                write!(f, "native-v1 vCPU coordinator assembly failed: {source}")
            }
            Self::Scheduler(kind) => {
                write!(f, "native-v1 scheduler startup failed with {kind:?}")
            }
            Self::VmGenId(source) => write!(f, "native-v1 VMGenID restore failed: {source}"),
            Self::TimeIdentity(source) => {
                write!(f, "native-v1 time/identity restore failed: {source}")
            }
            Self::InvalidRuntime => f.write_str("native-v1 installed runtime is invalid"),
        }
    }
}

impl std::error::Error for HvfSnapshotV1RestoreFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(source) => Some(source),
            Self::Gic(source) => Some(source),
            Self::Runner(source) => Some(source.as_ref()),
            Self::Coordinator(source) => Some(source.as_ref()),
            Self::VmGenId(source) => Some(source.as_ref()),
            Self::TimeIdentity(source) => Some(source.as_ref()),
            Self::GicMetadataMismatch
            | Self::DirtyTracking
            | Self::MemoryMapping
            | Self::Scheduler(_)
            | Self::InvalidRuntime => None,
        }
    }
}

impl HvfSnapshotV1RestoreFailure {
    const fn destination_committed(&self) -> bool {
        match self {
            Self::VmGenId(source) => source.is_committed(),
            Self::TimeIdentity(source) => source.is_committed(),
            Self::Backend(_)
            | Self::Gic(_)
            | Self::GicMetadataMismatch
            | Self::DirtyTracking
            | Self::MemoryMapping
            | Self::Runner(_)
            | Self::Coordinator(_)
            | Self::Scheduler(_)
            | Self::InvalidRuntime => false,
        }
    }
}

/// Redacted restore failure with explicit same-process cleanup disposition.
#[derive(Debug)]
pub struct HvfSnapshotV1RestoreError {
    stage: HvfSnapshotV1RestoreStage,
    failure: HvfSnapshotV1RestoreFailure,
    cleanup: HvfSnapshotV1RestoreCleanup,
    destination_committed: bool,
}

impl HvfSnapshotV1RestoreError {
    pub(crate) const fn new(
        stage: HvfSnapshotV1RestoreStage,
        failure: HvfSnapshotV1RestoreFailure,
        cleanup: HvfSnapshotV1RestoreCleanup,
    ) -> Self {
        let destination_committed = failure.destination_committed();
        Self {
            stage,
            failure,
            cleanup,
            destination_committed,
        }
    }

    pub(crate) const fn new_after_destination_commit(
        stage: HvfSnapshotV1RestoreStage,
        failure: HvfSnapshotV1RestoreFailure,
        cleanup: HvfSnapshotV1RestoreCleanup,
    ) -> Self {
        Self {
            stage,
            failure,
            cleanup,
            destination_committed: true,
        }
    }

    pub const fn stage(&self) -> HvfSnapshotV1RestoreStage {
        self.stage
    }

    pub const fn failure(&self) -> &HvfSnapshotV1RestoreFailure {
        &self.failure
    }

    pub const fn cleanup(&self) -> &HvfSnapshotV1RestoreCleanup {
        &self.cleanup
    }

    pub const fn destination_committed(&self) -> bool {
        self.destination_committed
    }

    pub const fn disposition(&self) -> HvfSnapshotV1RestoreDisposition {
        if self.cleanup.is_complete() && !self.destination_committed {
            HvfSnapshotV1RestoreDisposition::Retryable
        } else {
            HvfSnapshotV1RestoreDisposition::Terminal
        }
    }
}

impl fmt::Display for HvfSnapshotV1RestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "native-v1 restore failed during {}: {}; disposition={:?}; destination_committed={}",
            self.stage,
            self.failure,
            self.disposition(),
            self.destination_committed
        )
    }
}

impl std::error::Error for HvfSnapshotV1RestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.failure)
    }
}

fn validate_platform_composition(
    state: &HvfSnapshotV1State,
    memory: &GuestMemory,
) -> Result<(), HvfSnapshotV1PlatformError> {
    validate_platform_composition_ranges(
        state,
        memory.regions().iter().map(|region| region.range()),
    )
}

fn validate_platform_composition_ranges(
    state: &HvfSnapshotV1State,
    memory_ranges: impl Iterator<Item = GuestMemoryRange> + Clone,
) -> Result<(), HvfSnapshotV1PlatformError> {
    let compatibility = state.compatibility();
    let gic = compatibility.gic_metadata();
    let device = state.device();
    let root = device.root_block();

    let spi_end = gic
        .spi_interrupt_range
        .base
        .checked_add(gic.spi_interrupt_range.count)
        .ok_or(HvfSnapshotV1PlatformError::InterruptRange)?;
    let expected = [
        gic.spi_interrupt_range.base,
        gic.spi_interrupt_range
            .base
            .checked_add(1)
            .ok_or(HvfSnapshotV1PlatformError::InterruptRange)?,
        gic.spi_interrupt_range
            .base
            .checked_add(2)
            .ok_or(HvfSnapshotV1PlatformError::InterruptRange)?,
        gic.spi_interrupt_range
            .base
            .checked_add(3)
            .ok_or(HvfSnapshotV1PlatformError::InterruptRange)?,
    ];
    if expected[3] >= spi_end {
        return Err(HvfSnapshotV1PlatformError::InterruptRange);
    }
    let actual = [
        root.mmio().interrupt_line().raw_value(),
        device.serial_mmio().interrupt_line().raw_value(),
        device.vmgenid().interrupt_line().raw_value(),
        device.vmclock().interrupt_line().raw_value(),
    ];
    if actual != expected {
        return Err(HvfSnapshotV1PlatformError::InterruptAssignment);
    }

    let rtc = compatibility.rtc_mmio_layout();
    let region_ids = [
        root.mmio().region().id(),
        device.serial_mmio().region().id(),
        rtc.region_id(),
    ];
    if region_ids[0] == region_ids[1]
        || region_ids[0] == region_ids[2]
        || region_ids[1] == region_ids[2]
    {
        return Err(HvfSnapshotV1PlatformError::RegionIdConflict);
    }

    let block = range(
        root.mmio().region().range().start().raw_value(),
        root.mmio().region().range().size(),
    )?;
    let serial = range(
        device.serial_mmio().region().range().start().raw_value(),
        device.serial_mmio().region().range().size(),
    )?;
    let rtc = range(rtc.base().raw_value(), RTC_MMIO_DEVICE_WINDOW_SIZE)?;
    let distributor = range(gic.distributor.base, gic.distributor.size)?;
    let redistributor = range(gic.redistributor.region.base, gic.redistributor.region.size)?;
    let platform = [block, serial, rtc, distributor, redistributor];
    for (index, first) in platform.iter().enumerate() {
        if platform
            .iter()
            .skip(index + 1)
            .any(|second| overlaps(*first, *second))
        {
            return Err(HvfSnapshotV1PlatformError::MmioOverlap);
        }
    }
    for device_range in platform {
        if memory_ranges.clone().any(|range| {
            let guest = (range.start().raw_value(), range.end_exclusive().raw_value());
            overlaps(device_range, guest)
        }) {
            return Err(HvfSnapshotV1PlatformError::MmioMemoryOverlap);
        }
    }
    Ok(())
}

fn range(start: u64, size: u64) -> Result<(u64, u64), HvfSnapshotV1PlatformError> {
    start
        .checked_add(size)
        .map(|end| (start, end))
        .ok_or(HvfSnapshotV1PlatformError::RegionOverflow)
}

const fn overlaps(first: (u64, u64), second: (u64, u64)) -> bool {
    first.0 < second.1 && second.0 < first.1
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use bangbang_pager::PagerRegionId;
    use bangbang_runtime::memory::{GuestAddress, GuestMemoryLayout, GuestMemoryRange, aarch64};
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::rtc::RtcMmioLayout;
    use bangbang_runtime::snapshot_memory::write_snapshot_memory_image;
    use bangbang_runtime::vmclock::VmClockRestoreUpdateError;

    use super::{
        HvfSnapshotV1PlatformError, HvfSnapshotV1RestoreCleanup, HvfSnapshotV1RestoreDisposition,
        HvfSnapshotV1RestoreError, HvfSnapshotV1RestoreFailure, HvfSnapshotV1RestoreStage,
        PreparedHvfSnapshotV1LazyTopology, validate_platform_composition,
    };
    use crate::snapshot_bundle::{
        HvfSnapshotV1CompatibilityState, HvfSnapshotV1State, tests::fixture,
    };
    use crate::startup::{HvfArm64BootTimeIdentityRestoreError, HvfArm64BootVmClockRestoreError};

    fn memory_for(state: &HvfSnapshotV1State) -> bangbang_runtime::memory::GuestMemory {
        let size = state.machine().mem_size_mib() * 1024 * 1024;
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(aarch64::DRAM_MEM_START), size)
                .expect("fixture memory range should validate"),
        ])
        .expect("fixture memory layout should validate");
        bangbang_runtime::memory::GuestMemory::allocate(&layout)
            .expect("fixture memory should allocate")
    }

    fn with_platform(
        state: HvfSnapshotV1State,
        gic: crate::gic::HvfGicMetadata,
        rtc: RtcMmioLayout,
    ) -> HvfSnapshotV1State {
        let (machine, compatibility, vcpu, interrupts, device) = state.into_parts();
        HvfSnapshotV1State::new(
            machine,
            HvfSnapshotV1CompatibilityState::new(
                compatibility.identification(),
                compatibility.optional_sve_sme_identification(),
                compatibility.cache_manifest(),
                compatibility.primary_mpidr(),
                gic,
                rtc,
            ),
            vcpu,
            interrupts,
            device,
        )
    }

    #[test]
    fn lazy_topology_uses_host_pages_and_zero_based_memory_source_offsets() {
        let page_size =
            crate::memory::host_page_size().expect("test host page size should validate");
        let first = GuestMemoryRange::new(GuestAddress::new(aarch64::DRAM_MEM_START), page_size)
            .expect("first range should validate");
        let second = GuestMemoryRange::new(
            GuestAddress::new(aarch64::DRAM_MEM_START + page_size * 2),
            page_size,
        )
        .expect("second range should validate");
        let layout = GuestMemoryLayout::new(vec![first, second])
            .expect("fixture memory layout should validate");
        let memory = bangbang_runtime::memory::GuestMemory::allocate(&layout)
            .expect("fixture memory should allocate");
        let mut image = Cursor::new(Vec::new());
        let binding = write_snapshot_memory_image(&memory, &mut image)
            .expect("fixture memory image should write");

        let topology = PreparedHvfSnapshotV1LazyTopology::from_binding(&binding)
            .expect("bound topology should prepare");
        assert_eq!(u64::from(topology.limits.pager().page_size()), page_size);
        assert_eq!(topology.limits.pager().region_count(), 2);
        assert_eq!(topology.limits.max_pages(), 2);
        assert_eq!(topology.layout.ranges(), [first, second]);
        assert_eq!(
            topology.regions[0].id(),
            PagerRegionId::new(1).expect("region ID should validate")
        );
        assert_eq!(topology.regions[0].source_offset(), 0);
        assert_eq!(
            topology.regions[1].id(),
            PagerRegionId::new(2).expect("region ID should validate")
        );
        assert_eq!(topology.regions[1].source_offset(), page_size);
        assert_eq!(
            topology.session.as_bytes(),
            &binding.pager_v1_session_bytes()
        );
    }

    #[test]
    fn platform_validation_accepts_baseline_and_rejects_rtc_memory_overlap() {
        let baseline = fixture();
        let memory = memory_for(&baseline);
        validate_platform_composition(&baseline, &memory)
            .expect("baseline platform should validate");

        let compatibility = baseline.compatibility();
        let overlapping = with_platform(
            baseline.clone(),
            compatibility.gic_metadata(),
            RtcMmioLayout::new(
                GuestAddress::new(aarch64::DRAM_MEM_START),
                compatibility.rtc_mmio_layout().region_id(),
            ),
        );
        assert_eq!(
            validate_platform_composition(&overlapping, &memory),
            Err(HvfSnapshotV1PlatformError::MmioMemoryOverlap)
        );
    }

    #[test]
    fn platform_validation_rejects_gic_memory_overlap() {
        let baseline = fixture();
        let memory = memory_for(&baseline);
        let compatibility = baseline.compatibility();
        let mut gic = compatibility.gic_metadata();
        let rtc = compatibility.rtc_mmio_layout();
        gic.distributor.base = aarch64::DRAM_MEM_START;
        let overlapping = with_platform(baseline, gic, rtc);

        assert_eq!(
            validate_platform_composition(&overlapping, &memory),
            Err(HvfSnapshotV1PlatformError::MmioMemoryOverlap)
        );
    }

    #[test]
    fn platform_validation_rejects_rtc_overlap_and_region_id_conflict() {
        let baseline = fixture();
        let memory = memory_for(&baseline);
        let compatibility = baseline.compatibility();
        let gic = compatibility.gic_metadata();
        let rtc = compatibility.rtc_mmio_layout();
        let root = baseline.device().root_block().mmio();

        let overlapping = with_platform(
            baseline.clone(),
            gic,
            RtcMmioLayout::new(root.region().range().start(), MmioRegionId::new(10)),
        );
        assert_eq!(
            validate_platform_composition(&overlapping, &memory),
            Err(HvfSnapshotV1PlatformError::MmioOverlap)
        );

        let conflicting = with_platform(
            baseline,
            gic,
            RtcMmioLayout::new(rtc.base(), root.region().id()),
        );
        assert_eq!(
            validate_platform_composition(&conflicting, &memory),
            Err(HvfSnapshotV1PlatformError::RegionIdConflict)
        );
    }

    #[test]
    fn restore_disposition_uses_cleanup_and_destination_commit_evidence() {
        let retryable = HvfSnapshotV1RestoreError::new(
            HvfSnapshotV1RestoreStage::AssembleSession,
            HvfSnapshotV1RestoreFailure::InvalidRuntime,
            HvfSnapshotV1RestoreCleanup::new(false, None, None),
        );
        assert_eq!(
            retryable.disposition(),
            HvfSnapshotV1RestoreDisposition::Retryable
        );

        let terminal = HvfSnapshotV1RestoreError::new(
            HvfSnapshotV1RestoreStage::AssembleSession,
            HvfSnapshotV1RestoreFailure::InvalidRuntime,
            HvfSnapshotV1RestoreCleanup::new(
                false,
                None,
                Some(bangbang_runtime::BackendError::InvalidState(
                    "injected cleanup failure",
                )),
            ),
        );
        assert_eq!(
            terminal.disposition(),
            HvfSnapshotV1RestoreDisposition::Terminal
        );

        let committed = HvfSnapshotV1RestoreError::new(
            HvfSnapshotV1RestoreStage::RestoreTimeIdentity,
            HvfSnapshotV1RestoreFailure::TimeIdentity(Box::new(
                HvfArm64BootTimeIdentityRestoreError::VmClock {
                    source: HvfArm64BootVmClockRestoreError::Update {
                        source: VmClockRestoreUpdateError::InvalidRange,
                    },
                },
            )),
            HvfSnapshotV1RestoreCleanup::new(false, None, None),
        );
        assert_eq!(
            committed.disposition(),
            HvfSnapshotV1RestoreDisposition::Terminal
        );
        assert!(committed.destination_committed());

        let later_failure = HvfSnapshotV1RestoreError::new_after_destination_commit(
            HvfSnapshotV1RestoreStage::AssembleSession,
            HvfSnapshotV1RestoreFailure::InvalidRuntime,
            HvfSnapshotV1RestoreCleanup::new(false, None, None),
        );
        assert_eq!(
            later_failure.disposition(),
            HvfSnapshotV1RestoreDisposition::Terminal
        );
        assert!(later_failure.destination_committed());

        let mapping = HvfSnapshotV1RestoreError::new(
            HvfSnapshotV1RestoreStage::MapMemory,
            HvfSnapshotV1RestoreFailure::MemoryMapping,
            HvfSnapshotV1RestoreCleanup::new(false, None, None),
        );
        let diagnostics = format!("{mapping:?} {mapping}");
        assert!(diagnostics.contains("MemoryMapping"));
        assert!(!diagnostics.contains("0x"));
    }
}
