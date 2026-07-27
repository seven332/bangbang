//! Pathless destination planning for the native-v2 profile-3 storage graph.

use std::fmt;
use std::time::{Duration, Instant};

use super::*;

use crate::block::async_executor::BlockAsyncRuntimeError;
use crate::block::{BlockFileBacking, DriveConfigs};
use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestMemory, GuestMemoryRange};
use crate::message_interrupt::GuestMessageInterruptRegistry;
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::pci::PciSbdf;
use crate::pmem::{
    PmemConfigInput, PmemConfigs, PmemFileBacking, PreparedPmemDevice,
    PreparedSnapshotPmemDeviceError, PreparedSnapshotPmemDeviceParts, VIRTIO_PMEM_DEVICE_ID,
    VIRTIO_PMEM_QUEUE_SIZES, VirtioPmemConfigSpace, VirtioPmemDevice, VirtioPmemMmioHandler,
    VirtioPmemRateLimiterState, VirtioPmemTokenBucketState,
};
use crate::snapshot_device_v2::{
    SnapshotV2RootTransportRestoreError, restore_mmio_transport_state_for_device,
};
use crate::snapshot_device_v2_5::{
    PreparedSnapshotV2MultiBlockBundle, PreparedSnapshotV2MultiBlockMmioBundle,
    PreparedSnapshotV2MultiBlockPciBundle, SnapshotV2MultiBlockBundleError,
    SnapshotV2MultiBlockCleanupError, SnapshotV2MultiBlockDeviceGraph,
    SnapshotV2MultiBlockMmioTransportError, SnapshotV2MultiBlockPciTransportError,
    SnapshotV2MultiBlockRestorePlan, SnapshotV2MultiBlockRestorePlanError,
};
use crate::virtio::VirtioDeviceType;
use crate::virtio_mmio::{
    VirtioMmioQueueState, VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError,
    VirtioMmioTransportStateError,
};
use crate::virtio_pci::{PreparedVirtioPciEndpoint, VirtioPciIdentity, VirtioPciTransportState};

/// Complete pure destination proof for one detached profile-3 graph.
pub struct SnapshotV2StorageRestorePlan {
    root_key: Option<SnapshotV2DeviceKey>,
    transport_kind: SnapshotV2DeviceTransportKind,
    block: Option<SnapshotV2StorageBlockRestorePlan>,
    pmem_configs: PmemConfigs,
    pmem_records: Vec<SnapshotV2PmemRecordPlan>,
}

struct SnapshotV2StorageBlockRestorePlan {
    configs: DriveConfigs,
    plan: SnapshotV2MultiBlockRestorePlan,
}

struct SnapshotV2PmemRecordPlan {
    key: SnapshotV2DeviceKey,
    pmem_id: String,
    is_root: bool,
    is_read_only: bool,
    rate_limiter: Option<PmemRateLimiterConfig>,
    expected_file_len: u64,
    expected_mapped_len: u64,
    guest_range: GuestMemoryRange,
    config_space: VirtioPmemConfigSpace,
    queue_ranges: Option<[GuestMemoryRange; 3]>,
    retry: StorageRetryState,
    retry_deadline: Option<Instant>,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
    device: VirtioPmemDevice,
}

impl SnapshotV2StorageRestorePlan {
    /// Proves every record and queue against already-loaded guest memory.
    ///
    /// This phase does not open, map, or otherwise resolve a backing selector.
    pub fn prepare(
        graph: SnapshotV2StorageDeviceGraph,
        memory: &GuestMemory,
        now: Instant,
    ) -> Result<Self, SnapshotV2StorageRestorePlanError> {
        Self::prepare_with_reserve(graph, memory, now, &mut SystemRestoreReserve)
    }

    fn prepare_with_reserve(
        graph: SnapshotV2StorageDeviceGraph,
        memory: &GuestMemory,
        now: Instant,
        reserve: &mut impl StorageRestoreReserve,
    ) -> Result<Self, SnapshotV2StorageRestorePlanError> {
        validate_graph(&graph).map_err(|_| SnapshotV2StorageRestorePlanError::InvalidGraph)?;
        let SnapshotV2StorageDeviceGraph {
            root_key,
            transport_kind,
            block_records,
            pmem_records,
        } = graph;

        let block = if block_records.is_empty() {
            None
        } else {
            let block_root = root_key.filter(|root| {
                block_records
                    .first()
                    .is_some_and(|record| record.key() == *root)
            });
            let block_graph = SnapshotV2MultiBlockDeviceGraph::try_from_parts(
                block_root,
                transport_kind,
                block_records,
            )
            .map_err(|_| SnapshotV2StorageRestorePlanError::InvalidGraph)?;
            let configs = block_graph
                .project_drive_configs()
                .map_err(|_| SnapshotV2StorageRestorePlanError::Configuration)?;
            let plan = SnapshotV2MultiBlockRestorePlan::prepare(block_graph, memory, now)
                .map_err(SnapshotV2StorageRestorePlanError::Block)?;
            Some(SnapshotV2StorageBlockRestorePlan { configs, plan })
        };

        let mut pmem_configs = PmemConfigs::new();
        reserve
            .reserve_pmem_configs(&mut pmem_configs, pmem_records.len())
            .map_err(|_| SnapshotV2StorageRestorePlanError::Allocation)?;
        let mut planned = Vec::new();
        reserve
            .reserve_vec(&mut planned, pmem_records.len())
            .map_err(|_| SnapshotV2StorageRestorePlanError::Allocation)?;

        for record in pmem_records {
            let SnapshotV2PmemDeviceRecord {
                key,
                config,
                pmem,
                virtio,
                transport,
            } = record;
            let SnapshotV2PmemConfig {
                pmem_id,
                is_root,
                is_read_only,
                rate_limiter,
                selector,
            } = config;
            let SnapshotV2PmemState {
                file_bytes,
                mapped_bytes,
                guest_range,
                config_space,
                active_queue,
                limiter,
                pending_rate_limited_queue,
                retry,
            } = pmem;

            if memory
                .regions()
                .iter()
                .any(|region| region.range().overlaps(guest_range))
            {
                return Err(SnapshotV2StorageRestorePlanError::PmemRange);
            }

            let queue_state = *virtio
                .queues()
                .first()
                .ok_or(SnapshotV2StorageRestorePlanError::InvalidGraph)?;
            let queue_ranges = queue_ranges(&queue_state)
                .map_err(|_| SnapshotV2StorageRestorePlanError::InvalidGraph)?;
            if queue_ranges.is_some_and(|ranges| {
                ranges
                    .into_iter()
                    .any(|range| !range_is_wholly_contained(memory, range))
            }) {
                return Err(SnapshotV2StorageRestorePlanError::QueueMemory);
            }
            let active_queue = active_queue
                .map(|cursor| {
                    let queue = VirtioMmioQueueState::from_parts(
                        queue_state.max_size(),
                        queue_state.size(),
                        queue_state.ready(),
                        queue_state.descriptor_table(),
                        queue_state.driver_ring(),
                        queue_state.device_ring(),
                    );
                    let queue =
                        crate::pmem::VirtioPmemQueue::from_snapshot_state(&queue, cursor)
                            .map_err(|_| SnapshotV2StorageRestorePlanError::QueueContinuation)?;
                    queue
                        .validate_snapshot_state(memory, retry != StorageRetryState::None)
                        .map_err(|_| SnapshotV2StorageRestorePlanError::QueueContinuation)?;
                    Ok(queue)
                })
                .transpose()?;
            let limiter_state = persisted_pmem_limiter_state(rate_limiter, limiter)?;
            let device = VirtioPmemDevice::from_snapshot_state_at(
                file_bytes,
                active_queue,
                rate_limiter,
                limiter_state,
                pending_rate_limited_queue,
                now,
            )
            .map_err(|_| SnapshotV2StorageRestorePlanError::RateLimiter)?;

            let mut input = PmemConfigInput::new(pmem_id.clone(), selector.clone())
                .with_root_device(is_root)
                .with_read_only(is_read_only);
            if let Some(rate_limiter) = rate_limiter {
                input = input.with_rate_limiter(rate_limiter);
            }
            pmem_configs
                .insert(input)
                .map_err(|_| SnapshotV2StorageRestorePlanError::Configuration)?;
            let projected = pmem_configs
                .as_slice()
                .last()
                .ok_or(SnapshotV2StorageRestorePlanError::Configuration)?;
            if projected.id() != pmem_id
                || projected.path_on_host() != selector
                || projected.root_device() != is_root
                || projected.read_only() != is_read_only
                || projected.rate_limiter() != rate_limiter
            {
                return Err(SnapshotV2StorageRestorePlanError::Configuration);
            }

            planned.push(SnapshotV2PmemRecordPlan {
                key,
                pmem_id,
                is_root,
                is_read_only,
                rate_limiter,
                expected_file_len: file_bytes,
                expected_mapped_len: mapped_bytes,
                guest_range,
                config_space,
                queue_ranges,
                retry,
                retry_deadline: restored_retry_deadline_at(retry, now),
                virtio,
                transport,
                device,
            });
        }

        if pmem_configs.as_slice().len() != planned.len() {
            return Err(SnapshotV2StorageRestorePlanError::Configuration);
        }

        Ok(Self {
            root_key,
            transport_kind,
            block,
            pmem_configs,
            pmem_records: planned,
        })
    }

    /// Returns the optional cross-storage root key.
    pub const fn root_key(&self) -> Option<SnapshotV2DeviceKey> {
        self.root_key
    }

    /// Returns the graph-wide retained transport kind.
    pub const fn transport_kind(&self) -> SnapshotV2DeviceTransportKind {
        self.transport_kind
    }

    /// Returns the number of canonical block records.
    pub fn block_len(&self) -> usize {
        self.block.as_ref().map_or(0, |block| block.plan.len())
    }

    /// Returns the number of canonical pmem records.
    pub fn pmem_len(&self) -> usize {
        self.pmem_records.len()
    }

    /// Returns the pure ordered pmem controller projection.
    pub const fn pmem_configs(&self) -> &PmemConfigs {
        &self.pmem_configs
    }

    /// Adopts complete canonical backing vectors into one unpublished bundle.
    ///
    /// Cancellation is checked before block construction, before every pmem
    /// mapping, and after the complete bundle is built.
    pub fn prepare_backings(
        self,
        block_backings: Vec<BlockFileBacking>,
        pmem_backings: Vec<PmemFileBacking>,
        cancelled: impl Fn() -> bool,
    ) -> Result<PreparedSnapshotV2StorageBundle, SnapshotV2StorageBundleError> {
        self.prepare_backings_with(
            block_backings,
            pmem_backings,
            cancelled,
            &mut SystemRestoreReserve,
            PreparedPmemDevice::from_snapshot_parts,
        )
    }

    fn prepare_backings_with(
        self,
        block_backings: Vec<BlockFileBacking>,
        pmem_backings: Vec<PmemFileBacking>,
        cancelled: impl Fn() -> bool,
        reserve: &mut impl StorageRestoreReserve,
        mut prepare_pmem: impl FnMut(
            PreparedSnapshotPmemDeviceParts,
        )
            -> Result<PreparedPmemDevice, PreparedSnapshotPmemDeviceError>,
    ) -> Result<PreparedSnapshotV2StorageBundle, SnapshotV2StorageBundleError> {
        let expected_block_count = self.block_len();
        if block_backings.len() != expected_block_count
            || pmem_backings.len() != self.pmem_records.len()
        {
            return Err(SnapshotV2StorageBundleError::new(
                SnapshotV2StorageBundleErrorKind::BackingCount,
                None,
            ));
        }
        for (record, backing) in self.pmem_records.iter().zip(&pmem_backings) {
            if backing.is_read_only() != record.is_read_only {
                return Err(SnapshotV2StorageBundleError::new(
                    SnapshotV2StorageBundleErrorKind::PmemBackingMode,
                    None,
                ));
            }
            if backing.len() != record.expected_file_len
                || aligned_pmem_mapping_len(backing.len()) != Some(record.expected_mapped_len)
                || record.guest_range.size() != record.expected_mapped_len
            {
                return Err(SnapshotV2StorageBundleError::new(
                    SnapshotV2StorageBundleErrorKind::PmemBackingGeometry,
                    None,
                ));
            }
        }

        let mut prepared_pmem = Vec::new();
        reserve
            .reserve_vec(&mut prepared_pmem, self.pmem_records.len())
            .map_err(|_| {
                SnapshotV2StorageBundleError::new(
                    SnapshotV2StorageBundleErrorKind::Allocation,
                    None,
                )
            })?;

        if cancelled() {
            return Err(SnapshotV2StorageBundleError::new(
                SnapshotV2StorageBundleErrorKind::Cancelled,
                None,
            ));
        }
        let block = match self.block {
            Some(block) => Some(
                block
                    .plan
                    .prepare_backings(block.configs, block_backings)
                    .map_err(|source| {
                        SnapshotV2StorageBundleError::new(
                            SnapshotV2StorageBundleErrorKind::Block(source),
                            None,
                        )
                    })?,
            ),
            None => {
                debug_assert!(block_backings.is_empty());
                None
            }
        };

        for (record, backing) in self.pmem_records.into_iter().zip(pmem_backings) {
            if cancelled() {
                release_pmem_records(&mut prepared_pmem);
                let cleanup = abort_block_bundle(block).err();
                return Err(SnapshotV2StorageBundleError::new(
                    SnapshotV2StorageBundleErrorKind::Cancelled,
                    cleanup,
                ));
            }
            let SnapshotV2PmemRecordPlan {
                key,
                pmem_id,
                is_root,
                is_read_only,
                rate_limiter,
                expected_file_len,
                expected_mapped_len,
                guest_range,
                config_space,
                queue_ranges,
                retry,
                retry_deadline,
                virtio,
                transport,
                device,
            } = record;
            let prepared_device = match prepare_pmem(PreparedSnapshotPmemDeviceParts {
                id: pmem_id,
                is_read_only,
                rate_limiter,
                expected_file_len,
                expected_mapped_len,
                guest_range,
                config_space,
                backing,
            }) {
                Ok(prepared) => prepared,
                Err(source) => {
                    release_pmem_records(&mut prepared_pmem);
                    let cleanup = abort_block_bundle(block).err();
                    return Err(SnapshotV2StorageBundleError::new(
                        SnapshotV2StorageBundleErrorKind::Pmem(source),
                        cleanup,
                    ));
                }
            };
            prepared_pmem.push(PreparedSnapshotV2PmemRecord {
                key,
                is_root,
                queue_ranges,
                retry,
                retry_deadline,
                virtio,
                transport,
                prepared_device,
                device,
            });
        }

        if cancelled() {
            release_pmem_records(&mut prepared_pmem);
            let cleanup = abort_block_bundle(block).err();
            return Err(SnapshotV2StorageBundleError::new(
                SnapshotV2StorageBundleErrorKind::Cancelled,
                cleanup,
            ));
        }

        Ok(PreparedSnapshotV2StorageBundle {
            root_key: self.root_key,
            transport_kind: self.transport_kind,
            block,
            pmem_configs: self.pmem_configs,
            pmem_records: prepared_pmem,
        })
    }
}

impl fmt::Debug for SnapshotV2StorageRestorePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2StorageRestorePlan")
            .field("block_count", &self.block_len())
            .field("pmem_count", &self.pmem_len())
            .field("transport", &self.transport_kind)
            .field("state", &"<redacted>")
            .finish()
    }
}

/// One exact unpublished profile-3 pmem owner.
pub struct PreparedSnapshotV2PmemRecord {
    key: SnapshotV2DeviceKey,
    is_root: bool,
    queue_ranges: Option<[GuestMemoryRange; 3]>,
    retry: StorageRetryState,
    retry_deadline: Option<Instant>,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
    prepared_device: PreparedPmemDevice,
    device: VirtioPmemDevice,
}

/// Consumed exact pmem owner metadata and detached runtime owners.
pub type PreparedSnapshotV2PmemRecordParts = (
    SnapshotV2DeviceKey,
    bool,
    Option<[GuestMemoryRange; 3]>,
    StorageRetryState,
    Option<Instant>,
    SnapshotV2VirtioState,
    SnapshotV2DeviceTransport,
    PreparedPmemDevice,
    VirtioPmemDevice,
);

impl PreparedSnapshotV2PmemRecord {
    pub const fn key(&self) -> SnapshotV2DeviceKey {
        self.key
    }

    pub fn pmem_id(&self) -> &str {
        self.prepared_device.id()
    }

    pub const fn is_root_device(&self) -> bool {
        self.is_root
    }

    pub const fn queue_ranges(&self) -> Option<[GuestMemoryRange; 3]> {
        self.queue_ranges
    }

    pub const fn retry(&self) -> StorageRetryState {
        self.retry
    }

    pub const fn retry_deadline(&self) -> Option<Instant> {
        self.retry_deadline
    }

    pub const fn virtio(&self) -> &SnapshotV2VirtioState {
        &self.virtio
    }

    pub const fn transport(&self) -> &SnapshotV2DeviceTransport {
        &self.transport
    }

    pub const fn prepared_device(&self) -> &PreparedPmemDevice {
        &self.prepared_device
    }

    pub const fn device(&self) -> &VirtioPmemDevice {
        &self.device
    }

    /// Consumes the complete detached pmem owner.
    pub fn into_parts(self) -> PreparedSnapshotV2PmemRecordParts {
        (
            self.key,
            self.is_root,
            self.queue_ranges,
            self.retry,
            self.retry_deadline,
            self.virtio,
            self.transport,
            self.prepared_device,
            self.device,
        )
    }
}

impl fmt::Debug for PreparedSnapshotV2PmemRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2PmemRecord")
            .field("state", &"<redacted>")
            .finish()
    }
}

/// One exact profile-3 pmem MMIO handler and its still-unpublished mapping
/// owner.
pub struct PreparedSnapshotV2StorageMmioPmemRecord {
    key: SnapshotV2DeviceKey,
    is_root: bool,
    retry: StorageRetryState,
    retry_deadline: Option<Instant>,
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    prepared_device: PreparedPmemDevice,
    handler: VirtioPmemMmioHandler,
}

/// Consumed exact pmem MMIO metadata, mapping owner, and handler.
pub type PreparedSnapshotV2StorageMmioPmemRecordParts = (
    SnapshotV2DeviceKey,
    bool,
    StorageRetryState,
    Option<Instant>,
    MmioRegion,
    GuestInterruptLine,
    PreparedPmemDevice,
    VirtioPmemMmioHandler,
);

impl PreparedSnapshotV2StorageMmioPmemRecord {
    pub const fn key(&self) -> SnapshotV2DeviceKey {
        self.key
    }

    pub fn pmem_id(&self) -> &str {
        self.prepared_device.id()
    }

    pub const fn is_root_device(&self) -> bool {
        self.is_root
    }

    pub const fn retry(&self) -> StorageRetryState {
        self.retry
    }

    pub const fn retry_deadline(&self) -> Option<Instant> {
        self.retry_deadline
    }

    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    pub const fn interrupt_line(&self) -> GuestInterruptLine {
        self.interrupt_line
    }

    pub const fn prepared_device(&self) -> &PreparedPmemDevice {
        &self.prepared_device
    }

    /// Borrows the reconstructed but still-unpublished MMIO handler.
    #[doc(hidden)]
    pub const fn handler(&self) -> &VirtioPmemMmioHandler {
        &self.handler
    }

    /// Consumes the unpublished handler and authoritative mapping owner.
    pub fn into_parts(self) -> PreparedSnapshotV2StorageMmioPmemRecordParts {
        (
            self.key,
            self.is_root,
            self.retry,
            self.retry_deadline,
            self.region,
            self.interrupt_line,
            self.prepared_device,
            self.handler,
        )
    }
}

impl fmt::Debug for PreparedSnapshotV2StorageMmioPmemRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2StorageMmioPmemRecord")
            .field("state", &"<redacted>")
            .finish()
    }
}

/// One exact detached profile-3 PCI pmem owner awaiting live route resources.
pub struct PreparedSnapshotV2StoragePciPmemRecord {
    key: SnapshotV2DeviceKey,
    is_root: bool,
    retry: StorageRetryState,
    retry_deadline: Option<Instant>,
    origin: StorageDeviceOrigin,
    sbdf: PciSbdf,
    bar_range: GuestMemoryRange,
    prepared_device: PreparedPmemDevice,
    config_space: VirtioPmemConfigSpace,
    device: VirtioPmemDevice,
    identity: VirtioPciIdentity,
    retained: VirtioPciTransportState,
}

impl PreparedSnapshotV2StoragePciPmemRecord {
    pub const fn key(&self) -> SnapshotV2DeviceKey {
        self.key
    }

    pub fn pmem_id(&self) -> &str {
        self.prepared_device.id()
    }

    pub const fn is_root_device(&self) -> bool {
        self.is_root
    }

    pub const fn retry(&self) -> StorageRetryState {
        self.retry
    }

    pub const fn retry_deadline(&self) -> Option<Instant> {
        self.retry_deadline
    }

    pub const fn origin(&self) -> StorageDeviceOrigin {
        self.origin
    }

    pub const fn sbdf(&self) -> PciSbdf {
        self.sbdf
    }

    pub const fn bar_range(&self) -> GuestMemoryRange {
        self.bar_range
    }

    pub const fn prepared_device(&self) -> &PreparedPmemDevice {
        &self.prepared_device
    }

    /// Completes retained endpoint preparation against the destination's
    /// fresh shared message registry without publishing live resources.
    pub fn prepare_endpoint(
        self,
        region_id: MmioRegionId,
        messages: GuestMessageInterruptRegistry,
    ) -> Result<PreparedSnapshotV2StoragePciPmemEndpoint, SnapshotV2RootTransportRestoreError> {
        let endpoint = PreparedVirtioPciEndpoint::new(
            self.identity,
            &VIRTIO_PMEM_QUEUE_SIZES,
            self.config_space,
            self.device,
            self.retained.is_device_activated(),
            false,
            &self.retained,
            self.sbdf,
            self.bar_range,
            region_id,
            messages,
        )
        .map_err(SnapshotV2RootTransportRestoreError::Pci)?;
        Ok(PreparedSnapshotV2StoragePciPmemEndpoint {
            key: self.key,
            is_root: self.is_root,
            retry: self.retry,
            retry_deadline: self.retry_deadline,
            origin: self.origin,
            prepared_device: self.prepared_device,
            endpoint,
        })
    }
}

impl fmt::Debug for PreparedSnapshotV2StoragePciPmemRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2StoragePciPmemRecord")
            .field("state", &"<redacted>")
            .finish()
    }
}

/// One fully checked, still-unpublished exact-2.6 PCI pmem endpoint and
/// authoritative mapping owner.
pub struct PreparedSnapshotV2StoragePciPmemEndpoint {
    key: SnapshotV2DeviceKey,
    is_root: bool,
    retry: StorageRetryState,
    retry_deadline: Option<Instant>,
    origin: StorageDeviceOrigin,
    prepared_device: PreparedPmemDevice,
    endpoint: PreparedVirtioPciEndpoint<VirtioPmemConfigSpace, VirtioPmemDevice>,
}

/// Consumed exact pmem owner metadata, mapping, and retained PCI endpoint.
pub type PreparedSnapshotV2StoragePciPmemEndpointParts = (
    SnapshotV2DeviceKey,
    bool,
    StorageRetryState,
    Option<Instant>,
    StorageDeviceOrigin,
    PreparedPmemDevice,
    PreparedVirtioPciEndpoint<VirtioPmemConfigSpace, VirtioPmemDevice>,
);

impl PreparedSnapshotV2StoragePciPmemEndpoint {
    pub const fn key(&self) -> SnapshotV2DeviceKey {
        self.key
    }

    pub fn pmem_id(&self) -> &str {
        self.prepared_device.id()
    }

    pub const fn prepared_device(&self) -> &PreparedPmemDevice {
        &self.prepared_device
    }

    pub fn into_parts(self) -> PreparedSnapshotV2StoragePciPmemEndpointParts {
        (
            self.key,
            self.is_root,
            self.retry,
            self.retry_deadline,
            self.origin,
            self.prepared_device,
            self.endpoint,
        )
    }
}

impl fmt::Debug for PreparedSnapshotV2StoragePciPmemEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2StoragePciPmemEndpoint")
            .field("state", &"<redacted>")
            .finish()
    }
}

/// Canonical move-only pathless profile-3 block-and-pmem vector.
pub struct PreparedSnapshotV2StorageBundle {
    root_key: Option<SnapshotV2DeviceKey>,
    transport_kind: SnapshotV2DeviceTransportKind,
    block: Option<PreparedSnapshotV2MultiBlockBundle>,
    pmem_configs: PmemConfigs,
    pmem_records: Vec<PreparedSnapshotV2PmemRecord>,
}

/// Consumed detached storage bundle parts for later transport reconstruction.
pub type PreparedSnapshotV2StorageBundleParts = (
    Option<SnapshotV2DeviceKey>,
    SnapshotV2DeviceTransportKind,
    Option<PreparedSnapshotV2MultiBlockBundle>,
    PmemConfigs,
    Vec<PreparedSnapshotV2PmemRecord>,
);

impl PreparedSnapshotV2StorageBundle {
    pub const fn root_key(&self) -> Option<SnapshotV2DeviceKey> {
        self.root_key
    }

    pub const fn transport_kind(&self) -> SnapshotV2DeviceTransportKind {
        self.transport_kind
    }

    pub const fn block_bundle(&self) -> Option<&PreparedSnapshotV2MultiBlockBundle> {
        self.block.as_ref()
    }

    pub const fn pmem_configs(&self) -> &PmemConfigs {
        &self.pmem_configs
    }

    pub fn pmem_records(&self) -> &[PreparedSnapshotV2PmemRecord] {
        &self.pmem_records
    }

    /// Reconstructs every exact MMIO handler without publishing a bus or
    /// registering a pmem mapping with a hypervisor.
    pub fn prepare_mmio_transport(
        self,
    ) -> Result<PreparedSnapshotV2StorageMmioBundle, SnapshotV2StorageMmioTransportError> {
        self.prepare_mmio_transport_with_reserve(&mut SystemRestoreReserve)
    }

    fn prepare_mmio_transport_with_reserve(
        mut self,
        reserve: &mut impl StorageRestoreReserve,
    ) -> Result<PreparedSnapshotV2StorageMmioBundle, SnapshotV2StorageMmioTransportError> {
        if self.transport_kind != SnapshotV2DeviceTransportKind::Mmio {
            return Err(
                self.mmio_transport_error(SnapshotV2StorageMmioTransportErrorKind::TransportPolicy)
            );
        }

        let block = match self.block.take() {
            Some(block) => match block.prepare_mmio_transport() {
                Ok(block) => Some(block),
                Err(source) => {
                    return Err(SnapshotV2StorageMmioTransportError::new(
                        SnapshotV2StorageMmioTransportErrorKind::Block(source),
                        None,
                    ));
                }
            },
            None => None,
        };

        let mut prepared_pmem = Vec::new();
        if reserve
            .reserve_vec(&mut prepared_pmem, self.pmem_records.len())
            .is_err()
        {
            return Err(storage_mmio_transport_error_after_block(
                SnapshotV2StorageMmioTransportErrorKind::Allocation,
                block,
            ));
        }

        let root_key = self.root_key;
        let pmem_configs = std::mem::take(&mut self.pmem_configs);
        let records = std::mem::take(&mut self.pmem_records);
        for record in records {
            let (
                key,
                is_root,
                _queue_ranges,
                retry,
                retry_deadline,
                virtio,
                transport,
                prepared_device,
                device,
            ) = record.into_parts();
            let SnapshotV2DeviceTransport::Mmio(mmio) = transport else {
                release_storage_mmio_pmem_records(&mut prepared_pmem);
                drop(prepared_device);
                return Err(storage_mmio_transport_error_after_block(
                    SnapshotV2StorageMmioTransportErrorKind::TransportPolicy,
                    block,
                ));
            };
            let retained = match restore_mmio_transport_state_for_device(
                VIRTIO_PMEM_DEVICE_ID,
                &virtio,
                &mmio,
            ) {
                Ok(retained) => retained,
                Err(source) => {
                    release_storage_mmio_pmem_records(&mut prepared_pmem);
                    drop(prepared_device);
                    return Err(storage_mmio_transport_error_after_block(
                        SnapshotV2StorageMmioTransportErrorKind::PmemState(source),
                        block,
                    ));
                }
            };
            let config_space = prepared_device.config_space();
            let activation_is_active = device.is_activated();
            let mut handler = match VirtioMmioRegisterHandler::with_device_config_and_activation(
                VIRTIO_PMEM_DEVICE_ID,
                config_space.available_features(),
                &VIRTIO_PMEM_QUEUE_SIZES,
                config_space,
                device,
            ) {
                Ok(handler) => handler,
                Err(source) => {
                    release_storage_mmio_pmem_records(&mut prepared_pmem);
                    drop(prepared_device);
                    return Err(storage_mmio_transport_error_after_block(
                        SnapshotV2StorageMmioTransportErrorKind::PmemHandler(source),
                        block,
                    ));
                }
            };
            if let Err(source) = handler.restore_transport_state(&retained, activation_is_active) {
                release_storage_mmio_pmem_records(&mut prepared_pmem);
                drop(prepared_device);
                drop(handler);
                return Err(storage_mmio_transport_error_after_block(
                    SnapshotV2StorageMmioTransportErrorKind::PmemTransport(source),
                    block,
                ));
            }
            prepared_pmem.push(PreparedSnapshotV2StorageMmioPmemRecord {
                key,
                is_root,
                retry,
                retry_deadline,
                region: mmio.region(),
                interrupt_line: mmio.interrupt_line(),
                prepared_device,
                handler,
            });
        }

        Ok(PreparedSnapshotV2StorageMmioBundle {
            root_key,
            block,
            pmem_configs,
            pmem_records: prepared_pmem,
        })
    }

    /// Reconstructs every exact PCI endpoint owner without publishing a BAR,
    /// function, route, dispatcher handler, or pmem mapping.
    pub fn prepare_pci_transport(
        self,
    ) -> Result<PreparedSnapshotV2StoragePciBundle, SnapshotV2StoragePciTransportError> {
        self.prepare_pci_transport_with_reserve(&mut SystemRestoreReserve)
    }

    fn prepare_pci_transport_with_reserve(
        mut self,
        reserve: &mut impl StorageRestoreReserve,
    ) -> Result<PreparedSnapshotV2StoragePciBundle, SnapshotV2StoragePciTransportError> {
        if self.transport_kind != SnapshotV2DeviceTransportKind::Pci {
            return Err(
                self.pci_transport_error(SnapshotV2StoragePciTransportErrorKind::TransportPolicy)
            );
        }

        let block = match self.block.take() {
            Some(block) => match block.prepare_pci_transport() {
                Ok(block) => Some(block),
                Err(source) => {
                    return Err(SnapshotV2StoragePciTransportError::new(
                        SnapshotV2StoragePciTransportErrorKind::Block(source),
                        None,
                    ));
                }
            },
            None => None,
        };

        let mut prepared_pmem = Vec::new();
        if reserve
            .reserve_vec(&mut prepared_pmem, self.pmem_records.len())
            .is_err()
        {
            return Err(storage_pci_transport_error_after_block(
                SnapshotV2StoragePciTransportErrorKind::Allocation,
                block,
            ));
        }

        let root_key = self.root_key;
        let pmem_configs = std::mem::take(&mut self.pmem_configs);
        let records = std::mem::take(&mut self.pmem_records);
        for record in records {
            let (
                key,
                is_root,
                _queue_ranges,
                retry,
                retry_deadline,
                virtio,
                transport,
                prepared_device,
                device,
            ) = record.into_parts();
            let SnapshotV2DeviceTransport::Pci(pci) = transport else {
                release_storage_pci_pmem_records(&mut prepared_pmem);
                drop(prepared_device);
                return Err(storage_pci_transport_error_after_block(
                    SnapshotV2StoragePciTransportErrorKind::TransportPolicy,
                    block,
                ));
            };
            let device_type = match VirtioDeviceType::new(VIRTIO_PMEM_DEVICE_ID) {
                Ok(device_type) => device_type,
                Err(source) => {
                    release_storage_pci_pmem_records(&mut prepared_pmem);
                    drop(prepared_device);
                    return Err(storage_pci_transport_error_after_block(
                        SnapshotV2StoragePciTransportErrorKind::PmemState(
                            SnapshotV2RootTransportRestoreError::DeviceType(source),
                        ),
                        block,
                    ));
                }
            };
            let identity = VirtioPciIdentity::new(device_type, virtio.available_features())
                .with_config_generation(virtio.config_generation());
            let retained = match VirtioPciTransportState::from_snapshot_v2_parts(
                identity, &virtio, &pci, false,
            ) {
                Ok(retained) => retained,
                Err(source) => {
                    release_storage_pci_pmem_records(&mut prepared_pmem);
                    drop(prepared_device);
                    return Err(storage_pci_transport_error_after_block(
                        SnapshotV2StoragePciTransportErrorKind::PmemState(
                            SnapshotV2RootTransportRestoreError::Pci(source),
                        ),
                        block,
                    ));
                }
            };
            let config_space = prepared_device.config_space();
            prepared_pmem.push(PreparedSnapshotV2StoragePciPmemRecord {
                key,
                is_root,
                retry,
                retry_deadline,
                origin: pci.origin(),
                sbdf: pci.sbdf(),
                bar_range: pci.bar_range(),
                prepared_device,
                config_space,
                device,
                identity,
                retained,
            });
        }

        Ok(PreparedSnapshotV2StoragePciBundle {
            root_key,
            block,
            pmem_configs,
            pmem_records: prepared_pmem,
        })
    }

    /// Transfers every detached storage owner to the transport layer.
    pub fn into_parts(mut self) -> PreparedSnapshotV2StorageBundleParts {
        (
            self.root_key,
            self.transport_kind,
            self.block.take(),
            std::mem::take(&mut self.pmem_configs),
            std::mem::take(&mut self.pmem_records),
        )
    }

    /// Explicitly releases pmem owners in reverse, followed by block Async
    /// generations in reverse.
    pub fn abort(mut self) -> Result<(), SnapshotV2StorageCleanupError> {
        release_pmem_records(&mut self.pmem_records);
        self.block
            .take()
            .map_or(Ok(()), PreparedSnapshotV2MultiBlockBundle::abort)
            .map_err(SnapshotV2StorageCleanupError::new)
    }

    fn mmio_transport_error(
        self,
        kind: SnapshotV2StorageMmioTransportErrorKind,
    ) -> SnapshotV2StorageMmioTransportError {
        let cleanup = self
            .abort()
            .err()
            .map(SnapshotV2StorageCleanupError::into_source);
        SnapshotV2StorageMmioTransportError::new(kind, cleanup)
    }

    fn pci_transport_error(
        self,
        kind: SnapshotV2StoragePciTransportErrorKind,
    ) -> SnapshotV2StoragePciTransportError {
        let cleanup = self
            .abort()
            .err()
            .map(SnapshotV2StorageCleanupError::into_source);
        SnapshotV2StoragePciTransportError::new(kind, cleanup)
    }
}

impl Drop for PreparedSnapshotV2StorageBundle {
    fn drop(&mut self) {
        release_pmem_records(&mut self.pmem_records);
        if let Some(block) = self.block.take() {
            let _ = block.abort();
        }
    }
}

impl fmt::Debug for PreparedSnapshotV2StorageBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2StorageBundle")
            .field(
                "block_count",
                &self.block.as_ref().map_or(0, |block| block.records().len()),
            )
            .field("pmem_count", &self.pmem_records.len())
            .field("transport", &self.transport_kind)
            .field("state", &"<redacted>")
            .finish()
    }
}

/// Move-only exact profile-3 MMIO storage product awaiting one private
/// dispatcher and hypervisor mapping transaction.
pub struct PreparedSnapshotV2StorageMmioBundle {
    root_key: Option<SnapshotV2DeviceKey>,
    block: Option<PreparedSnapshotV2MultiBlockMmioBundle>,
    pmem_configs: PmemConfigs,
    pmem_records: Vec<PreparedSnapshotV2StorageMmioPmemRecord>,
}

/// Consumed exact MMIO storage bundle parts.
pub type PreparedSnapshotV2StorageMmioBundleParts = (
    Option<SnapshotV2DeviceKey>,
    Option<PreparedSnapshotV2MultiBlockMmioBundle>,
    PmemConfigs,
    Vec<PreparedSnapshotV2StorageMmioPmemRecord>,
);

impl PreparedSnapshotV2StorageMmioBundle {
    pub const fn root_key(&self) -> Option<SnapshotV2DeviceKey> {
        self.root_key
    }

    pub const fn block_bundle(&self) -> Option<&PreparedSnapshotV2MultiBlockMmioBundle> {
        self.block.as_ref()
    }

    pub const fn pmem_configs(&self) -> &PmemConfigs {
        &self.pmem_configs
    }

    pub fn pmem_records(&self) -> &[PreparedSnapshotV2StorageMmioPmemRecord] {
        &self.pmem_records
    }

    pub fn into_parts(mut self) -> PreparedSnapshotV2StorageMmioBundleParts {
        (
            self.root_key,
            self.block.take(),
            std::mem::take(&mut self.pmem_configs),
            std::mem::take(&mut self.pmem_records),
        )
    }

    pub fn abort(mut self) -> Result<(), SnapshotV2StorageCleanupError> {
        release_storage_mmio_pmem_records(&mut self.pmem_records);
        self.block
            .take()
            .map_or(Ok(()), PreparedSnapshotV2MultiBlockMmioBundle::abort)
            .map_err(SnapshotV2StorageCleanupError::new)
    }
}

impl Drop for PreparedSnapshotV2StorageMmioBundle {
    fn drop(&mut self) {
        release_storage_mmio_pmem_records(&mut self.pmem_records);
        if let Some(block) = self.block.take() {
            let _ = block.abort();
        }
    }
}

impl fmt::Debug for PreparedSnapshotV2StorageMmioBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2StorageMmioBundle")
            .field(
                "block_count",
                &self
                    .block
                    .as_ref()
                    .map_or(0, |bundle| bundle.records().len()),
            )
            .field("pmem_count", &self.pmem_records.len())
            .field("state", &"<redacted>")
            .finish()
    }
}

/// Move-only exact profile-3 PCI storage product awaiting one private
/// heterogeneous manager and hypervisor mapping transaction.
pub struct PreparedSnapshotV2StoragePciBundle {
    root_key: Option<SnapshotV2DeviceKey>,
    block: Option<PreparedSnapshotV2MultiBlockPciBundle>,
    pmem_configs: PmemConfigs,
    pmem_records: Vec<PreparedSnapshotV2StoragePciPmemRecord>,
}

/// Consumed exact PCI storage bundle parts.
pub type PreparedSnapshotV2StoragePciBundleParts = (
    Option<SnapshotV2DeviceKey>,
    Option<PreparedSnapshotV2MultiBlockPciBundle>,
    PmemConfigs,
    Vec<PreparedSnapshotV2StoragePciPmemRecord>,
);

impl PreparedSnapshotV2StoragePciBundle {
    pub const fn root_key(&self) -> Option<SnapshotV2DeviceKey> {
        self.root_key
    }

    pub const fn block_bundle(&self) -> Option<&PreparedSnapshotV2MultiBlockPciBundle> {
        self.block.as_ref()
    }

    pub const fn pmem_configs(&self) -> &PmemConfigs {
        &self.pmem_configs
    }

    pub fn pmem_records(&self) -> &[PreparedSnapshotV2StoragePciPmemRecord] {
        &self.pmem_records
    }

    pub fn into_parts(mut self) -> PreparedSnapshotV2StoragePciBundleParts {
        (
            self.root_key,
            self.block.take(),
            std::mem::take(&mut self.pmem_configs),
            std::mem::take(&mut self.pmem_records),
        )
    }

    pub fn abort(mut self) -> Result<(), SnapshotV2StorageCleanupError> {
        release_storage_pci_pmem_records(&mut self.pmem_records);
        self.block
            .take()
            .map_or(Ok(()), PreparedSnapshotV2MultiBlockPciBundle::abort)
            .map_err(SnapshotV2StorageCleanupError::new)
    }
}

impl Drop for PreparedSnapshotV2StoragePciBundle {
    fn drop(&mut self) {
        release_storage_pci_pmem_records(&mut self.pmem_records);
        if let Some(block) = self.block.take() {
            let _ = block.abort();
        }
    }
}

impl fmt::Debug for PreparedSnapshotV2StoragePciBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2StoragePciBundle")
            .field(
                "block_count",
                &self
                    .block
                    .as_ref()
                    .map_or(0, |bundle| bundle.records().len()),
            )
            .field("pmem_count", &self.pmem_records.len())
            .field("state", &"<redacted>")
            .finish()
    }
}

enum SnapshotV2StorageMmioTransportErrorKind {
    TransportPolicy,
    Allocation,
    Block(SnapshotV2MultiBlockMmioTransportError),
    PmemState(SnapshotV2RootTransportRestoreError),
    PmemHandler(VirtioMmioRegisterHandlerError),
    PmemTransport(VirtioMmioTransportStateError),
}

/// Redacted failure while consuming profile-3 storage owners into exact MMIO
/// handlers.
pub struct SnapshotV2StorageMmioTransportError {
    kind: Box<SnapshotV2StorageMmioTransportErrorKind>,
    cleanup: Option<SnapshotV2MultiBlockCleanupError>,
}

impl SnapshotV2StorageMmioTransportError {
    fn new(
        kind: SnapshotV2StorageMmioTransportErrorKind,
        cleanup: Option<SnapshotV2MultiBlockCleanupError>,
    ) -> Self {
        Self {
            kind: Box::new(kind),
            cleanup,
        }
    }

    pub fn cleanup_failed(&self) -> bool {
        self.cleanup.is_some()
            || matches!(
                self.kind.as_ref(),
                SnapshotV2StorageMmioTransportErrorKind::Block(source)
                    if source.cleanup_failed()
            )
    }
}

impl fmt::Debug for SnapshotV2StorageMmioTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind.as_ref() {
            SnapshotV2StorageMmioTransportErrorKind::TransportPolicy => "transport-policy",
            SnapshotV2StorageMmioTransportErrorKind::Allocation => "allocation",
            SnapshotV2StorageMmioTransportErrorKind::Block(_) => "block",
            SnapshotV2StorageMmioTransportErrorKind::PmemState(_) => "pmem-state",
            SnapshotV2StorageMmioTransportErrorKind::PmemHandler(_) => "pmem-handler",
            SnapshotV2StorageMmioTransportErrorKind::PmemTransport(_) => "pmem-transport",
        };
        formatter
            .debug_struct("SnapshotV2StorageMmioTransportError")
            .field("kind", &kind)
            .field("cleanup_failed", &self.cleanup_failed())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for SnapshotV2StorageMmioTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.kind.as_ref() {
            SnapshotV2StorageMmioTransportErrorKind::TransportPolicy => {
                "snapshot storage MMIO transport policy is invalid"
            }
            SnapshotV2StorageMmioTransportErrorKind::Allocation => {
                "snapshot storage MMIO transport allocation failed"
            }
            SnapshotV2StorageMmioTransportErrorKind::Block(_) => {
                "snapshot storage block MMIO reconstruction failed"
            }
            SnapshotV2StorageMmioTransportErrorKind::PmemState(_) => {
                "snapshot storage pmem MMIO retained state is invalid"
            }
            SnapshotV2StorageMmioTransportErrorKind::PmemHandler(_) => {
                "snapshot storage pmem MMIO handler construction failed"
            }
            SnapshotV2StorageMmioTransportErrorKind::PmemTransport(_) => {
                "snapshot storage pmem MMIO transport reconstruction failed"
            }
        })?;
        if self.cleanup_failed() {
            formatter.write_str("; cleanup also failed")?;
        }
        Ok(())
    }
}

impl std::error::Error for SnapshotV2StorageMmioTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self.kind.as_ref() {
            SnapshotV2StorageMmioTransportErrorKind::Block(source) => Some(source),
            SnapshotV2StorageMmioTransportErrorKind::PmemState(source) => Some(source),
            SnapshotV2StorageMmioTransportErrorKind::PmemHandler(source) => Some(source),
            SnapshotV2StorageMmioTransportErrorKind::PmemTransport(source) => Some(source),
            SnapshotV2StorageMmioTransportErrorKind::TransportPolicy
            | SnapshotV2StorageMmioTransportErrorKind::Allocation => self
                .cleanup
                .as_ref()
                .map(|source| source as &(dyn std::error::Error + 'static)),
        }
    }
}

enum SnapshotV2StoragePciTransportErrorKind {
    TransportPolicy,
    Allocation,
    Block(SnapshotV2MultiBlockPciTransportError),
    PmemState(SnapshotV2RootTransportRestoreError),
}

/// Redacted failure while consuming profile-3 storage owners into exact PCI
/// endpoint state.
pub struct SnapshotV2StoragePciTransportError {
    kind: Box<SnapshotV2StoragePciTransportErrorKind>,
    cleanup: Option<SnapshotV2MultiBlockCleanupError>,
}

impl SnapshotV2StoragePciTransportError {
    fn new(
        kind: SnapshotV2StoragePciTransportErrorKind,
        cleanup: Option<SnapshotV2MultiBlockCleanupError>,
    ) -> Self {
        Self {
            kind: Box::new(kind),
            cleanup,
        }
    }

    pub fn cleanup_failed(&self) -> bool {
        self.cleanup.is_some()
            || matches!(
                self.kind.as_ref(),
                SnapshotV2StoragePciTransportErrorKind::Block(source)
                    if source.cleanup_failed()
            )
    }
}

impl fmt::Debug for SnapshotV2StoragePciTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind.as_ref() {
            SnapshotV2StoragePciTransportErrorKind::TransportPolicy => "transport-policy",
            SnapshotV2StoragePciTransportErrorKind::Allocation => "allocation",
            SnapshotV2StoragePciTransportErrorKind::Block(_) => "block",
            SnapshotV2StoragePciTransportErrorKind::PmemState(_) => "pmem-state",
        };
        formatter
            .debug_struct("SnapshotV2StoragePciTransportError")
            .field("kind", &kind)
            .field("cleanup_failed", &self.cleanup_failed())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for SnapshotV2StoragePciTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.kind.as_ref() {
            SnapshotV2StoragePciTransportErrorKind::TransportPolicy => {
                "snapshot storage PCI transport policy is invalid"
            }
            SnapshotV2StoragePciTransportErrorKind::Allocation => {
                "snapshot storage PCI transport allocation failed"
            }
            SnapshotV2StoragePciTransportErrorKind::Block(_) => {
                "snapshot storage block PCI reconstruction failed"
            }
            SnapshotV2StoragePciTransportErrorKind::PmemState(_) => {
                "snapshot storage pmem PCI retained state is invalid"
            }
        })?;
        if self.cleanup_failed() {
            formatter.write_str("; cleanup also failed")?;
        }
        Ok(())
    }
}

impl std::error::Error for SnapshotV2StoragePciTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self.kind.as_ref() {
            SnapshotV2StoragePciTransportErrorKind::Block(source) => Some(source),
            SnapshotV2StoragePciTransportErrorKind::PmemState(source) => Some(source),
            SnapshotV2StoragePciTransportErrorKind::TransportPolicy
            | SnapshotV2StoragePciTransportErrorKind::Allocation => self
                .cleanup
                .as_ref()
                .map(|source| source as &(dyn std::error::Error + 'static)),
        }
    }
}

/// Failure while proving a profile-3 graph against loaded destination state.
#[derive(Debug)]
pub enum SnapshotV2StorageRestorePlanError {
    InvalidGraph,
    Configuration,
    Allocation,
    QueueMemory,
    QueueContinuation,
    RateLimiter,
    PmemRange,
    Block(SnapshotV2MultiBlockRestorePlanError),
}

impl fmt::Display for SnapshotV2StorageRestorePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidGraph => "native-v2 storage graph is invalid",
            Self::Configuration => "native-v2 storage configuration is invalid",
            Self::Allocation => "native-v2 storage restore plan allocation failed",
            Self::QueueMemory => "native-v2 storage queue memory is invalid",
            Self::QueueContinuation => "native-v2 storage queue continuation is invalid",
            Self::RateLimiter => "native-v2 storage rate limiter is invalid",
            Self::PmemRange => "native-v2 storage pmem range overlaps loaded memory",
            Self::Block(_) => "native-v2 storage block restore plan is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2StorageRestorePlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Block(source) => Some(source),
            Self::InvalidGraph
            | Self::Configuration
            | Self::Allocation
            | Self::QueueMemory
            | Self::QueueContinuation
            | Self::RateLimiter
            | Self::PmemRange => None,
        }
    }
}

enum SnapshotV2StorageBundleErrorKind {
    BackingCount,
    PmemBackingMode,
    PmemBackingGeometry,
    Allocation,
    Cancelled,
    Block(SnapshotV2MultiBlockBundleError),
    Pmem(PreparedSnapshotPmemDeviceError),
}

/// Failure while adopting one complete profile-3 backing batch.
pub struct SnapshotV2StorageBundleError {
    kind: SnapshotV2StorageBundleErrorKind,
    cleanup: Option<SnapshotV2MultiBlockCleanupError>,
}

impl SnapshotV2StorageBundleError {
    fn new(
        kind: SnapshotV2StorageBundleErrorKind,
        cleanup: Option<SnapshotV2MultiBlockCleanupError>,
    ) -> Self {
        Self { kind, cleanup }
    }

    pub const fn cleanup_failed(&self) -> bool {
        self.cleanup.is_some()
            || matches!(
                &self.kind,
                SnapshotV2StorageBundleErrorKind::Pmem(source) if source.cleanup_failed()
            )
    }

    /// Returns whether a clean retry may be attempted.
    pub const fn is_retryable(&self) -> bool {
        if self.cleanup_failed() {
            return false;
        }
        match &self.kind {
            SnapshotV2StorageBundleErrorKind::Allocation
            | SnapshotV2StorageBundleErrorKind::Cancelled => true,
            SnapshotV2StorageBundleErrorKind::Pmem(source) => source.is_retryable(),
            SnapshotV2StorageBundleErrorKind::Block(
                SnapshotV2MultiBlockBundleError::Allocation
                | SnapshotV2MultiBlockBundleError::AsyncBinding {
                    source:
                        BlockAsyncRuntimeError::MetadataAllocation
                        | BlockAsyncRuntimeError::BuildExecutor(_)
                        | BlockAsyncRuntimeError::DriveBuild(_),
                    cleanup: None,
                },
            ) => true,
            SnapshotV2StorageBundleErrorKind::BackingCount
            | SnapshotV2StorageBundleErrorKind::PmemBackingMode
            | SnapshotV2StorageBundleErrorKind::PmemBackingGeometry
            | SnapshotV2StorageBundleErrorKind::Block(_) => false,
        }
    }

    pub const fn is_cancelled(&self) -> bool {
        matches!(self.kind, SnapshotV2StorageBundleErrorKind::Cancelled)
    }
}

impl fmt::Debug for SnapshotV2StorageBundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            SnapshotV2StorageBundleErrorKind::BackingCount => "BackingCount",
            SnapshotV2StorageBundleErrorKind::PmemBackingMode => "PmemBackingMode",
            SnapshotV2StorageBundleErrorKind::PmemBackingGeometry => "PmemBackingGeometry",
            SnapshotV2StorageBundleErrorKind::Allocation => "Allocation",
            SnapshotV2StorageBundleErrorKind::Cancelled => "Cancelled",
            SnapshotV2StorageBundleErrorKind::Block(_) => "Block",
            SnapshotV2StorageBundleErrorKind::Pmem(_) => "Pmem",
        };
        formatter
            .debug_struct("SnapshotV2StorageBundleError")
            .field("kind", &kind)
            .field("retryable", &self.is_retryable())
            .field("cleanup_failed", &self.cleanup_failed())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for SnapshotV2StorageBundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.kind {
            SnapshotV2StorageBundleErrorKind::BackingCount => {
                "snapshot storage backing count is inconsistent"
            }
            SnapshotV2StorageBundleErrorKind::PmemBackingMode => {
                "snapshot storage pmem backing access mode is inconsistent"
            }
            SnapshotV2StorageBundleErrorKind::PmemBackingGeometry => {
                "snapshot storage pmem backing geometry is inconsistent"
            }
            SnapshotV2StorageBundleErrorKind::Allocation => {
                "snapshot storage bundle allocation failed"
            }
            SnapshotV2StorageBundleErrorKind::Cancelled => {
                "snapshot storage bundle construction was cancelled"
            }
            SnapshotV2StorageBundleErrorKind::Block(_) => {
                "snapshot storage block bundle construction failed"
            }
            SnapshotV2StorageBundleErrorKind::Pmem(_) => {
                "snapshot storage pmem bundle construction failed"
            }
        })?;
        if self.cleanup_failed() {
            formatter.write_str("; cleanup also failed")?;
        }
        Ok(())
    }
}

impl std::error::Error for SnapshotV2StorageBundleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            SnapshotV2StorageBundleErrorKind::Block(source) => Some(source),
            SnapshotV2StorageBundleErrorKind::Pmem(source) => Some(source),
            SnapshotV2StorageBundleErrorKind::BackingCount
            | SnapshotV2StorageBundleErrorKind::PmemBackingMode
            | SnapshotV2StorageBundleErrorKind::PmemBackingGeometry
            | SnapshotV2StorageBundleErrorKind::Allocation
            | SnapshotV2StorageBundleErrorKind::Cancelled => self
                .cleanup
                .as_ref()
                .map(|source| source as &(dyn std::error::Error + 'static)),
        }
    }
}

/// Failure while explicitly releasing a prepared pathless storage bundle.
pub struct SnapshotV2StorageCleanupError {
    source: SnapshotV2MultiBlockCleanupError,
}

impl SnapshotV2StorageCleanupError {
    const fn new(source: SnapshotV2MultiBlockCleanupError) -> Self {
        Self { source }
    }

    fn into_source(self) -> SnapshotV2MultiBlockCleanupError {
        self.source
    }
}

impl fmt::Debug for SnapshotV2StorageCleanupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2StorageCleanupError")
            .field("state", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for SnapshotV2StorageCleanupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("snapshot storage block Async cleanup failed")
    }
}

impl std::error::Error for SnapshotV2StorageCleanupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

trait StorageRestoreReserve {
    fn reserve_vec<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()>;

    fn reserve_pmem_configs(
        &mut self,
        configs: &mut PmemConfigs,
        additional: usize,
    ) -> Result<(), ()>;
}

struct SystemRestoreReserve;

impl StorageRestoreReserve for SystemRestoreReserve {
    fn reserve_vec<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()> {
        values.try_reserve_exact(additional).map_err(|_| ())
    }

    fn reserve_pmem_configs(
        &mut self,
        configs: &mut PmemConfigs,
        additional: usize,
    ) -> Result<(), ()> {
        configs.try_reserve_exact(additional).map_err(|_| ())
    }
}

#[cfg(test)]
struct FailingStorageRestoreReserve {
    calls: usize,
    fail_at: usize,
}

#[cfg(test)]
impl FailingStorageRestoreReserve {
    const fn new(fail_at: usize) -> Self {
        Self { calls: 0, fail_at }
    }

    fn should_fail(&mut self) -> bool {
        let call = self.calls;
        self.calls = self.calls.saturating_add(1);
        call == self.fail_at
    }
}

#[cfg(test)]
impl StorageRestoreReserve for FailingStorageRestoreReserve {
    fn reserve_vec<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()> {
        if self.should_fail() {
            Err(())
        } else {
            values.try_reserve_exact(additional).map_err(|_| ())
        }
    }

    fn reserve_pmem_configs(
        &mut self,
        configs: &mut PmemConfigs,
        additional: usize,
    ) -> Result<(), ()> {
        if self.should_fail() {
            Err(())
        } else {
            configs.try_reserve_exact(additional).map_err(|_| ())
        }
    }
}

#[cfg(test)]
pub(super) fn prepare_with_failing_reserve_for_test(
    graph: SnapshotV2StorageDeviceGraph,
    memory: &GuestMemory,
    now: Instant,
    fail_at: usize,
) -> Result<SnapshotV2StorageRestorePlan, SnapshotV2StorageRestorePlanError> {
    SnapshotV2StorageRestorePlan::prepare_with_reserve(
        graph,
        memory,
        now,
        &mut FailingStorageRestoreReserve::new(fail_at),
    )
}

#[cfg(test)]
pub(super) fn prepare_backings_with_failing_reserve_for_test(
    plan: SnapshotV2StorageRestorePlan,
    block_backings: Vec<BlockFileBacking>,
    pmem_backings: Vec<PmemFileBacking>,
    fail_at: usize,
) -> Result<PreparedSnapshotV2StorageBundle, SnapshotV2StorageBundleError> {
    plan.prepare_backings_with(
        block_backings,
        pmem_backings,
        || false,
        &mut FailingStorageRestoreReserve::new(fail_at),
        PreparedPmemDevice::from_snapshot_parts,
    )
}

#[cfg(test)]
pub(super) fn prepare_backings_with_pmem_fault_for_test(
    plan: SnapshotV2StorageRestorePlan,
    block_backings: Vec<BlockFileBacking>,
    pmem_backings: Vec<PmemFileBacking>,
    fail_at: usize,
) -> Result<PreparedSnapshotV2StorageBundle, SnapshotV2StorageBundleError> {
    let mut pmem_calls = 0_usize;
    plan.prepare_backings_with(
        block_backings,
        pmem_backings,
        || false,
        &mut SystemRestoreReserve,
        |parts| {
            let call = pmem_calls;
            pmem_calls = pmem_calls.saturating_add(1);
            if call == fail_at {
                Err(PreparedSnapshotPmemDeviceError::Configuration)
            } else {
                PreparedPmemDevice::from_snapshot_parts(parts)
            }
        },
    )
}

#[cfg(test)]
pub(super) fn prepare_mmio_transport_with_failing_reserve_for_test(
    bundle: PreparedSnapshotV2StorageBundle,
) -> Result<PreparedSnapshotV2StorageMmioBundle, SnapshotV2StorageMmioTransportError> {
    bundle.prepare_mmio_transport_with_reserve(&mut FailingStorageRestoreReserve::new(0))
}

#[cfg(test)]
pub(super) fn prepare_pci_transport_with_failing_reserve_for_test(
    bundle: PreparedSnapshotV2StorageBundle,
) -> Result<PreparedSnapshotV2StoragePciBundle, SnapshotV2StoragePciTransportError> {
    bundle.prepare_pci_transport_with_reserve(&mut FailingStorageRestoreReserve::new(0))
}

fn release_pmem_records(records: &mut Vec<PreparedSnapshotV2PmemRecord>) {
    while records.pop().is_some() {}
}

fn release_storage_mmio_pmem_records(records: &mut Vec<PreparedSnapshotV2StorageMmioPmemRecord>) {
    while records.pop().is_some() {}
}

fn release_storage_pci_pmem_records(records: &mut Vec<PreparedSnapshotV2StoragePciPmemRecord>) {
    while records.pop().is_some() {}
}

fn storage_mmio_transport_error_after_block(
    kind: SnapshotV2StorageMmioTransportErrorKind,
    block: Option<PreparedSnapshotV2MultiBlockMmioBundle>,
) -> SnapshotV2StorageMmioTransportError {
    let cleanup = block
        .map(PreparedSnapshotV2MultiBlockMmioBundle::abort)
        .transpose()
        .err();
    SnapshotV2StorageMmioTransportError::new(kind, cleanup)
}

fn storage_pci_transport_error_after_block(
    kind: SnapshotV2StoragePciTransportErrorKind,
    block: Option<PreparedSnapshotV2MultiBlockPciBundle>,
) -> SnapshotV2StoragePciTransportError {
    let cleanup = block
        .map(PreparedSnapshotV2MultiBlockPciBundle::abort)
        .transpose()
        .err();
    SnapshotV2StoragePciTransportError::new(kind, cleanup)
}

fn abort_block_bundle(
    block: Option<PreparedSnapshotV2MultiBlockBundle>,
) -> Result<(), SnapshotV2MultiBlockCleanupError> {
    block.map_or(Ok(()), PreparedSnapshotV2MultiBlockBundle::abort)
}

fn range_is_wholly_contained(memory: &GuestMemory, range: GuestMemoryRange) -> bool {
    memory.regions().iter().any(|region| {
        let region = region.range();
        region.start().raw_value() <= range.start().raw_value()
            && range.end_exclusive().raw_value() <= region.end_exclusive().raw_value()
    })
}

fn restored_retry_deadline_at(retry: StorageRetryState, now: Instant) -> Option<Instant> {
    match retry {
        StorageRetryState::None => None,
        StorageRetryState::Immediate => Some(now),
        StorageRetryState::After { remaining_nanos } => Some(
            now.checked_add(Duration::from_nanos(remaining_nanos))
                .unwrap_or(now),
        ),
    }
}

fn persisted_pmem_limiter_state(
    config: Option<PmemRateLimiterConfig>,
    state: SnapshotV2PmemLimiterState,
) -> Result<VirtioPmemRateLimiterState, SnapshotV2StorageRestorePlanError> {
    Ok(VirtioPmemRateLimiterState::new(
        persisted_pmem_bucket_state(
            config.and_then(PmemRateLimiterConfig::bandwidth),
            state.bandwidth(),
        )?,
        persisted_pmem_bucket_state(config.and_then(PmemRateLimiterConfig::ops), state.ops())?,
    ))
}

fn persisted_pmem_bucket_state(
    config: Option<PmemTokenBucketConfig>,
    state: Option<SnapshotV2PmemBucketState>,
) -> Result<Option<VirtioPmemTokenBucketState>, SnapshotV2StorageRestorePlanError> {
    match (config, state) {
        (Some(config), Some(state)) => Ok(Some(VirtioPmemTokenBucketState::new(
            config,
            state.budget(),
            state.remaining_burst(),
            state.age_nanos(),
        ))),
        (None, None) => Ok(None),
        _ => Err(SnapshotV2StorageRestorePlanError::InvalidGraph),
    }
}
