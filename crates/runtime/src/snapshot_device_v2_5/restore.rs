//! Pathless destination planning for the native-v2 profile-2 block graph.

use std::fmt;
use std::time::{Duration, Instant};

use super::*;

use crate::block::async_executor::{
    BlockAsyncDriveGeneration, BlockAsyncRuntimeError, SharedBlockAsyncRuntime,
};
use crate::block::{
    PreparedBlockDevice, PreparedSnapshotBlockDeviceError, PreparedSnapshotBlockDeviceParts,
    VIRTIO_RING_FEATURE_EVENT_IDX, VIRTIO_RING_FEATURE_INDIRECT_DESC, VirtioBlockConfigSpace,
    VirtioBlockMmioHandler, VirtioBlockQueue, VirtioBlockRateLimiter, VirtioBlockRateLimiterState,
    VirtioBlockTokenBucketState, restore_prepared_block_mmio_handler,
};
use crate::interrupt::GuestInterruptLine;
use crate::memory::GuestMemory;
use crate::mmio::MmioRegion;
use crate::snapshot_device_v2::{
    SnapshotV2BlockBucketState, SnapshotV2RootTransportRestoreError, restore_mmio_transport_state,
};
use crate::virtio_mmio::VirtioMmioQueueState;

/// Complete pure destination proof for one detached profile-2 graph.
pub struct SnapshotV2MultiBlockRestorePlan {
    expected_drive_configs: DriveConfigs,
    records: Vec<SnapshotV2MultiBlockRecordPlan>,
}

struct SnapshotV2MultiBlockRecordPlan {
    key: SnapshotV2DeviceKey,
    drive_id: String,
    is_root: bool,
    io_engine: DriveIoEngine,
    cache_type: DriveCacheType,
    config_space: VirtioBlockConfigSpace,
    device_id: crate::block::VirtioBlockDeviceId,
    queue_ranges: Option<[GuestMemoryRange; 3]>,
    active_queue: Option<VirtioBlockQueue>,
    rate_limiter: Option<VirtioBlockRateLimiter>,
    retry: StorageRetryState,
    retry_deadline: Option<Instant>,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
}

impl SnapshotV2MultiBlockRestorePlan {
    /// Proves every record and queue against already-loaded guest memory.
    pub fn prepare(
        graph: SnapshotV2MultiBlockDeviceGraph,
        memory: &GuestMemory,
        now: Instant,
    ) -> Result<Self, SnapshotV2MultiBlockRestorePlanError> {
        Self::prepare_with_reserve(graph, memory, now, &mut SystemRestoreReserve)
    }

    fn prepare_with_reserve(
        graph: SnapshotV2MultiBlockDeviceGraph,
        memory: &GuestMemory,
        now: Instant,
        reserve: &mut impl RestoreReserve,
    ) -> Result<Self, SnapshotV2MultiBlockRestorePlanError> {
        validate_graph(&graph).map_err(|_| SnapshotV2MultiBlockRestorePlanError::InvalidGraph)?;
        let expected_drive_configs = graph
            .project_drive_configs()
            .map_err(|_| SnapshotV2MultiBlockRestorePlanError::Configuration)?;
        let SnapshotV2MultiBlockDeviceGraph { records, .. } = graph;
        let mut planned = Vec::new();
        reserve
            .reserve(&mut planned, records.len())
            .map_err(|()| SnapshotV2MultiBlockRestorePlanError::Allocation)?;

        for record in records {
            let SnapshotV2MultiBlockDeviceRecord {
                key,
                config,
                block,
                virtio,
                transport,
            } = record;
            let SnapshotV2MultiBlockConfig {
                drive_id,
                is_root,
                is_read_only,
                cache_type,
                io_engine,
                rate_limiter,
                ..
            } = config;
            let queue_state = *virtio
                .queues()
                .first()
                .ok_or(SnapshotV2MultiBlockRestorePlanError::InvalidGraph)?;
            let queue_ranges = queue_ranges(&queue_state)
                .map_err(|_| SnapshotV2MultiBlockRestorePlanError::InvalidGraph)?;
            if queue_ranges.is_some_and(|ranges| {
                ranges
                    .into_iter()
                    .any(|range| !range_is_wholly_contained(memory, range))
            }) {
                return Err(SnapshotV2MultiBlockRestorePlanError::QueueMemory);
            }

            let continuation = block.continuation;
            let retry = continuation.retry();
            let active_queue = continuation
                .active_queue()
                .map(|cursor| {
                    let queue = VirtioMmioQueueState::from_parts(
                        queue_state.max_size(),
                        queue_state.size(),
                        queue_state.ready(),
                        queue_state.descriptor_table(),
                        queue_state.driver_ring(),
                        queue_state.device_ring(),
                    );
                    let event_idx_enabled =
                        feature_enabled(virtio.driver_features(), VIRTIO_RING_FEATURE_EVENT_IDX);
                    let indirect_descriptors_enabled = feature_enabled(
                        virtio.driver_features(),
                        VIRTIO_RING_FEATURE_INDIRECT_DESC,
                    );
                    let queue = VirtioBlockQueue::from_snapshot_state(
                        &queue,
                        cursor,
                        event_idx_enabled,
                        indirect_descriptors_enabled,
                    )
                    .map_err(|_| SnapshotV2MultiBlockRestorePlanError::QueueContinuation)?;
                    queue
                        .validate_snapshot_state(memory, retry != StorageRetryState::None)
                        .map_err(|_| SnapshotV2MultiBlockRestorePlanError::QueueContinuation)?;
                    Ok(queue)
                })
                .transpose()?;
            let limiter_state = persisted_limiter_state(rate_limiter, continuation.limiter())?;
            let restored_limiter =
                VirtioBlockRateLimiter::from_persisted_state_at(rate_limiter, limiter_state, now)
                    .map_err(|_| SnapshotV2MultiBlockRestorePlanError::RateLimiter)?;
            let config_space =
                VirtioBlockConfigSpace::new(block.backing_bytes, is_read_only, cache_type);

            planned.push(SnapshotV2MultiBlockRecordPlan {
                key,
                drive_id,
                is_root,
                io_engine,
                cache_type,
                config_space,
                device_id: continuation.device_id(),
                queue_ranges,
                active_queue,
                rate_limiter: restored_limiter,
                retry,
                retry_deadline: restored_retry_deadline_at(retry, now),
                virtio,
                transport,
            });
        }

        Ok(Self {
            expected_drive_configs,
            records: planned,
        })
    }

    /// Returns the number of canonical block records in the plan.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Returns whether the validated plan is empty (always false for profile 2).
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Adopts one complete canonical backing batch into an unpublished bundle.
    pub fn prepare_backings(
        self,
        drive_configs: DriveConfigs,
        backings: Vec<crate::block::BlockFileBacking>,
    ) -> Result<PreparedSnapshotV2MultiBlockBundle, SnapshotV2MultiBlockBundleError> {
        self.prepare_backings_with(
            drive_configs,
            backings,
            &mut SystemRestoreReserve,
            SharedBlockAsyncRuntime::new,
            |device, runtime| device.bind_async_runtime(runtime.clone()),
        )
    }

    fn prepare_backings_with(
        self,
        drive_configs: DriveConfigs,
        backings: Vec<crate::block::BlockFileBacking>,
        reserve: &mut impl RestoreReserve,
        runtime_factory: impl FnOnce() -> SharedBlockAsyncRuntime,
        mut bind_async: impl FnMut(
            &mut PreparedBlockDevice,
            &SharedBlockAsyncRuntime,
        )
            -> Result<Option<BlockAsyncDriveGeneration>, BlockAsyncRuntimeError>,
    ) -> Result<PreparedSnapshotV2MultiBlockBundle, SnapshotV2MultiBlockBundleError> {
        if drive_configs != self.expected_drive_configs
            || drive_configs.as_slice().len() != self.records.len()
        {
            return Err(SnapshotV2MultiBlockBundleError::Configuration);
        }
        if backings.len() != self.records.len() {
            return Err(SnapshotV2MultiBlockBundleError::BackingCount);
        }
        if drive_configs
            .as_slice()
            .iter()
            .zip(&self.records)
            .any(|(config, record)| {
                config.drive_id() != record.drive_id
                    || config.is_root_device() != record.is_root
                    || config.is_read_only() != Some(record.config_space.is_read_only())
                    || config.cache_type() != record.cache_type
                    || config.io_engine() != Some(record.io_engine)
            })
        {
            return Err(SnapshotV2MultiBlockBundleError::Configuration);
        }

        for (record, backing) in self.records.iter().zip(&backings) {
            PreparedBlockDevice::validate_snapshot_backing(backing, record.config_space)
                .map_err(SnapshotV2MultiBlockBundleError::Backing)?;
        }

        let async_count = self
            .records
            .iter()
            .filter(|record| record.io_engine == DriveIoEngine::Async)
            .count();
        let mut prepared = Vec::new();
        let mut retry_projection = Vec::new();
        let mut async_generations = Vec::new();
        reserve
            .reserve(&mut prepared, self.records.len())
            .and_then(|()| reserve.reserve(&mut retry_projection, self.records.len()))
            .and_then(|()| reserve.reserve(&mut async_generations, async_count))
            .map_err(|()| SnapshotV2MultiBlockBundleError::Allocation)?;

        for (record, backing) in self.records.into_iter().zip(backings) {
            let SnapshotV2MultiBlockRecordPlan {
                key,
                drive_id,
                is_root,
                io_engine,
                cache_type,
                config_space,
                device_id,
                queue_ranges,
                active_queue,
                rate_limiter,
                retry,
                retry_deadline,
                virtio,
                transport,
            } = record;
            let device =
                PreparedBlockDevice::from_snapshot_parts(PreparedSnapshotBlockDeviceParts {
                    drive_id,
                    is_root_device: is_root,
                    io_engine,
                    cache_type,
                    config_space,
                    backing,
                    device_id,
                    active_queue,
                    rate_limiter,
                    pending_rate_limited_queue: retry != StorageRetryState::None,
                })
                .map_err(SnapshotV2MultiBlockBundleError::Backing)?;
            retry_projection.push(SnapshotV2MultiBlockRetryProjection {
                key,
                retry,
                retry_deadline,
            });
            prepared.push(PreparedSnapshotV2MultiBlockRecord {
                key,
                queue_ranges,
                retry,
                retry_deadline,
                virtio,
                transport,
                async_generation: None,
                device,
            });
        }

        if async_count == 0 {
            return Ok(PreparedSnapshotV2MultiBlockBundle::new(
                drive_configs,
                prepared,
                None,
                retry_projection,
            ));
        }
        let runtime = runtime_factory();
        if !runtime
            .generation_count()
            .and_then(|count| {
                runtime
                    .outstanding_tasks()
                    .map(|tasks| count == 0 && tasks == 0)
            })
            .unwrap_or(false)
        {
            return Err(SnapshotV2MultiBlockBundleError::FreshRuntime);
        }

        for record in &mut prepared {
            if record.device.io_engine() != Some(DriveIoEngine::Async) {
                continue;
            }
            let generation = match bind_async(&mut record.device, &runtime) {
                Ok(Some(generation)) => generation,
                Ok(None) => {
                    let source = BlockAsyncRuntimeError::ExecutorInvariant;
                    return Err(async_binding_failure(source, &runtime, &async_generations));
                }
                Err(source) => {
                    return Err(async_binding_failure(source, &runtime, &async_generations));
                }
            };
            let expected_generation = u64::try_from(async_generations.len())
                .ok()
                .and_then(|index| index.checked_add(1));
            let fresh = expected_generation == Some(generation.value())
                && runtime
                    .pressure_pending(generation)
                    .map(|pending| !pending)
                    .unwrap_or(false)
                && runtime
                    .pop_completion(generation)
                    .map(|completion| completion.is_none())
                    .unwrap_or(false);
            if !fresh {
                async_generations.push(generation);
                return Err(async_binding_failure(
                    BlockAsyncRuntimeError::ExecutorInvariant,
                    &runtime,
                    &async_generations,
                ));
            }
            record.async_generation = Some(generation);
            async_generations.push(generation);
        }
        if async_generations.len() != async_count
            || runtime.generation_count().ok() != Some(async_count)
            || runtime.outstanding_tasks().ok() != Some(0)
        {
            return Err(async_binding_failure(
                BlockAsyncRuntimeError::ExecutorInvariant,
                &runtime,
                &async_generations,
            ));
        }

        Ok(PreparedSnapshotV2MultiBlockBundle::new(
            drive_configs,
            prepared,
            Some(runtime),
            retry_projection,
        ))
    }
}

impl fmt::Debug for SnapshotV2MultiBlockRestorePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2MultiBlockRestorePlan")
            .field("record_count", &self.records.len())
            .field("state", &"<redacted>")
            .finish()
    }
}

/// One exact unpublished profile-2 block owner.
pub struct PreparedSnapshotV2MultiBlockRecord {
    key: SnapshotV2DeviceKey,
    queue_ranges: Option<[GuestMemoryRange; 3]>,
    retry: StorageRetryState,
    retry_deadline: Option<Instant>,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
    async_generation: Option<BlockAsyncDriveGeneration>,
    device: PreparedBlockDevice,
}

impl PreparedSnapshotV2MultiBlockRecord {
    pub const fn key(&self) -> SnapshotV2DeviceKey {
        self.key
    }

    pub fn drive_id(&self) -> &str {
        self.device.drive_id()
    }

    pub const fn is_root_device(&self) -> bool {
        self.device.is_root_device()
    }

    pub const fn config_space(&self) -> VirtioBlockConfigSpace {
        self.device.config_space()
    }

    pub const fn device(&self) -> &PreparedBlockDevice {
        &self.device
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

    pub const fn async_generation(&self) -> Option<BlockAsyncDriveGeneration> {
        self.async_generation
    }
}

impl fmt::Debug for PreparedSnapshotV2MultiBlockRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2MultiBlockRecord")
            .field("state", &"<redacted>")
            .finish()
    }
}

/// One exact profile-2 MMIO handler prepared before live bus construction.
pub struct PreparedSnapshotV2MultiBlockMmioRecord {
    key: SnapshotV2DeviceKey,
    drive_id: String,
    is_root_device: bool,
    retry: StorageRetryState,
    retry_deadline: Option<Instant>,
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    async_generation: Option<BlockAsyncDriveGeneration>,
    handler: VirtioBlockMmioHandler,
}

impl PreparedSnapshotV2MultiBlockMmioRecord {
    pub const fn key(&self) -> SnapshotV2DeviceKey {
        self.key
    }

    pub fn drive_id(&self) -> &str {
        &self.drive_id
    }

    pub const fn is_root_device(&self) -> bool {
        self.is_root_device
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

    pub const fn async_generation(&self) -> Option<BlockAsyncDriveGeneration> {
        self.async_generation
    }

    /// Consumes the still-unpublished exact handler and its owner metadata.
    pub fn into_parts(
        self,
    ) -> (
        SnapshotV2DeviceKey,
        String,
        bool,
        StorageRetryState,
        Option<Instant>,
        MmioRegion,
        GuestInterruptLine,
        Option<BlockAsyncDriveGeneration>,
        VirtioBlockMmioHandler,
    ) {
        (
            self.key,
            self.drive_id,
            self.is_root_device,
            self.retry,
            self.retry_deadline,
            self.region,
            self.interrupt_line,
            self.async_generation,
            self.handler,
        )
    }
}

impl fmt::Debug for PreparedSnapshotV2MultiBlockMmioRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2MultiBlockMmioRecord")
            .field("state", &"<redacted>")
            .finish()
    }
}

/// Move-only exact profile-2 MMIO vector awaiting one private dispatcher.
pub struct PreparedSnapshotV2MultiBlockMmioBundle {
    drive_configs: DriveConfigs,
    records: Vec<PreparedSnapshotV2MultiBlockMmioRecord>,
    async_runtime: Option<SharedBlockAsyncRuntime>,
    async_generations: Vec<BlockAsyncDriveGeneration>,
}

impl PreparedSnapshotV2MultiBlockMmioBundle {
    pub const fn drive_configs(&self) -> &DriveConfigs {
        &self.drive_configs
    }

    pub fn records(&self) -> &[PreparedSnapshotV2MultiBlockMmioRecord] {
        &self.records
    }

    pub const fn async_runtime(&self) -> Option<&SharedBlockAsyncRuntime> {
        self.async_runtime.as_ref()
    }

    /// Transfers every exact handler, generation owner, and controller value.
    pub fn into_parts(
        mut self,
    ) -> (
        DriveConfigs,
        Vec<PreparedSnapshotV2MultiBlockMmioRecord>,
        Option<SharedBlockAsyncRuntime>,
        Vec<BlockAsyncDriveGeneration>,
    ) {
        (
            std::mem::take(&mut self.drive_configs),
            std::mem::take(&mut self.records),
            self.async_runtime.take(),
            std::mem::take(&mut self.async_generations),
        )
    }

    /// Explicitly releases every transferred fresh Async generation.
    pub fn abort(mut self) -> Result<(), SnapshotV2MultiBlockCleanupError> {
        let result =
            cleanup_async_generations(self.async_runtime.as_ref(), &self.async_generations);
        self.async_generations.clear();
        self.async_runtime = None;
        result
    }
}

impl Drop for PreparedSnapshotV2MultiBlockMmioBundle {
    fn drop(&mut self) {
        let _ = cleanup_async_generations(self.async_runtime.as_ref(), &self.async_generations);
    }
}

impl fmt::Debug for PreparedSnapshotV2MultiBlockMmioBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2MultiBlockMmioBundle")
            .field("record_count", &self.records.len())
            .field("has_async_runtime", &self.async_runtime.is_some())
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Debug)]
enum SnapshotV2MultiBlockMmioTransportErrorKind {
    TransportPolicy,
    Allocation,
    AsyncBinding,
    Transport(SnapshotV2RootTransportRestoreError),
}

/// Redacted failure while consuming profile-2 devices into exact MMIO handlers.
pub struct SnapshotV2MultiBlockMmioTransportError {
    kind: SnapshotV2MultiBlockMmioTransportErrorKind,
    cleanup: Option<SnapshotV2MultiBlockCleanupError>,
}

impl SnapshotV2MultiBlockMmioTransportError {
    fn new(
        kind: SnapshotV2MultiBlockMmioTransportErrorKind,
        cleanup: Option<SnapshotV2MultiBlockCleanupError>,
    ) -> Self {
        Self { kind, cleanup }
    }

    pub const fn cleanup_failed(&self) -> bool {
        self.cleanup.is_some()
    }
}

impl fmt::Debug for SnapshotV2MultiBlockMmioTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            SnapshotV2MultiBlockMmioTransportErrorKind::TransportPolicy => "transport-policy",
            SnapshotV2MultiBlockMmioTransportErrorKind::Allocation => "allocation",
            SnapshotV2MultiBlockMmioTransportErrorKind::AsyncBinding => "async-binding",
            SnapshotV2MultiBlockMmioTransportErrorKind::Transport(_) => "transport",
        };
        formatter
            .debug_struct("SnapshotV2MultiBlockMmioTransportError")
            .field("kind", &kind)
            .field("cleanup_failed", &self.cleanup_failed())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for SnapshotV2MultiBlockMmioTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.kind {
            SnapshotV2MultiBlockMmioTransportErrorKind::TransportPolicy => {
                "snapshot multi-block MMIO transport policy is invalid"
            }
            SnapshotV2MultiBlockMmioTransportErrorKind::Allocation => {
                "snapshot multi-block MMIO transport allocation failed"
            }
            SnapshotV2MultiBlockMmioTransportErrorKind::AsyncBinding => {
                "snapshot multi-block MMIO Async ownership is invalid"
            }
            SnapshotV2MultiBlockMmioTransportErrorKind::Transport(_) => {
                "snapshot multi-block MMIO handler reconstruction failed"
            }
        })?;
        if self.cleanup_failed() {
            formatter.write_str("; Async cleanup also failed")?;
        }
        Ok(())
    }
}

impl std::error::Error for SnapshotV2MultiBlockMmioTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            SnapshotV2MultiBlockMmioTransportErrorKind::Transport(source) => Some(source),
            SnapshotV2MultiBlockMmioTransportErrorKind::TransportPolicy
            | SnapshotV2MultiBlockMmioTransportErrorKind::Allocation
            | SnapshotV2MultiBlockMmioTransportErrorKind::AsyncBinding => self
                .cleanup
                .as_ref()
                .map(|source| source as &(dyn std::error::Error + 'static)),
        }
    }
}

/// Host-time projection of one record retry disposition.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2MultiBlockRetryProjection {
    key: SnapshotV2DeviceKey,
    retry: StorageRetryState,
    retry_deadline: Option<Instant>,
}

impl SnapshotV2MultiBlockRetryProjection {
    pub const fn key(self) -> SnapshotV2DeviceKey {
        self.key
    }

    pub const fn retry(self) -> StorageRetryState {
        self.retry
    }

    pub const fn retry_deadline(self) -> Option<Instant> {
        self.retry_deadline
    }
}

impl fmt::Debug for SnapshotV2MultiBlockRetryProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2MultiBlockRetryProjection")
            .field("state", &"<redacted>")
            .finish()
    }
}

/// Canonical move-only pathless profile-2 block vector.
pub struct PreparedSnapshotV2MultiBlockBundle {
    drive_configs: DriveConfigs,
    records: Vec<PreparedSnapshotV2MultiBlockRecord>,
    async_runtime: Option<SharedBlockAsyncRuntime>,
    retry_projection: Vec<SnapshotV2MultiBlockRetryProjection>,
}

impl PreparedSnapshotV2MultiBlockBundle {
    fn new(
        drive_configs: DriveConfigs,
        records: Vec<PreparedSnapshotV2MultiBlockRecord>,
        async_runtime: Option<SharedBlockAsyncRuntime>,
        retry_projection: Vec<SnapshotV2MultiBlockRetryProjection>,
    ) -> Self {
        Self {
            drive_configs,
            records,
            async_runtime,
            retry_projection,
        }
    }

    pub const fn drive_configs(&self) -> &DriveConfigs {
        &self.drive_configs
    }

    pub fn records(&self) -> &[PreparedSnapshotV2MultiBlockRecord] {
        &self.records
    }

    pub const fn async_runtime(&self) -> Option<&SharedBlockAsyncRuntime> {
        self.async_runtime.as_ref()
    }

    pub fn retry_projection(&self) -> &[SnapshotV2MultiBlockRetryProjection] {
        &self.retry_projection
    }

    /// Reconstructs the complete exact MMIO vector without publishing a bus.
    pub fn prepare_mmio_transport(
        mut self,
    ) -> Result<PreparedSnapshotV2MultiBlockMmioBundle, SnapshotV2MultiBlockMmioTransportError>
    {
        let mut async_generations = Vec::new();
        if async_generations
            .try_reserve_exact(self.records.len())
            .is_err()
        {
            return Err(
                self.mmio_transport_error(SnapshotV2MultiBlockMmioTransportErrorKind::Allocation)
            );
        }
        if !validate_mmio_async_ownership(
            &self.records,
            self.async_runtime.as_ref(),
            &mut async_generations,
        ) {
            return Err(
                self.mmio_transport_error(SnapshotV2MultiBlockMmioTransportErrorKind::AsyncBinding)
            );
        }
        if self
            .records
            .iter()
            .any(|record| !matches!(record.transport, SnapshotV2DeviceTransport::Mmio(_)))
        {
            return Err(self.mmio_transport_error(
                SnapshotV2MultiBlockMmioTransportErrorKind::TransportPolicy,
            ));
        }

        let mut prepared = Vec::new();
        if prepared.try_reserve_exact(self.records.len()).is_err() {
            return Err(
                self.mmio_transport_error(SnapshotV2MultiBlockMmioTransportErrorKind::Allocation)
            );
        }

        let drive_configs = std::mem::take(&mut self.drive_configs);
        let records = std::mem::take(&mut self.records);
        let async_runtime = self.async_runtime.take();
        self.retry_projection.clear();
        let result = (|| {
            for record in records {
                let PreparedSnapshotV2MultiBlockRecord {
                    key,
                    queue_ranges: _,
                    retry,
                    retry_deadline,
                    virtio,
                    transport,
                    async_generation,
                    device,
                } = record;
                let SnapshotV2DeviceTransport::Mmio(mmio) = transport else {
                    return Err(SnapshotV2MultiBlockMmioTransportErrorKind::TransportPolicy);
                };
                let retained = restore_mmio_transport_state(&virtio, &mmio)
                    .map_err(SnapshotV2MultiBlockMmioTransportErrorKind::Transport)?;
                let (drive_id, is_root_device, config_space, device) = device.into_parts();
                let handler = restore_prepared_block_mmio_handler(config_space, device, &retained)
                    .map_err(|source| {
                        SnapshotV2MultiBlockMmioTransportErrorKind::Transport(
                            SnapshotV2RootTransportRestoreError::Mmio(source),
                        )
                    })?;
                prepared.push(PreparedSnapshotV2MultiBlockMmioRecord {
                    key,
                    drive_id,
                    is_root_device,
                    retry,
                    retry_deadline,
                    region: mmio.region(),
                    interrupt_line: mmio.interrupt_line(),
                    async_generation,
                    handler,
                });
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok(PreparedSnapshotV2MultiBlockMmioBundle {
                drive_configs,
                records: prepared,
                async_runtime,
                async_generations,
            }),
            Err(kind) => {
                drop(prepared);
                let cleanup =
                    cleanup_async_generations(async_runtime.as_ref(), &async_generations).err();
                Err(SnapshotV2MultiBlockMmioTransportError::new(kind, cleanup))
            }
        }
    }

    fn mmio_transport_error(
        self,
        kind: SnapshotV2MultiBlockMmioTransportErrorKind,
    ) -> SnapshotV2MultiBlockMmioTransportError {
        let cleanup = self.abort().err();
        SnapshotV2MultiBlockMmioTransportError::new(kind, cleanup)
    }

    /// Explicitly releases every fresh Async generation in reverse order.
    pub fn abort(mut self) -> Result<(), SnapshotV2MultiBlockCleanupError> {
        let result = cleanup_async_bundle(&mut self.records, self.async_runtime.as_ref());
        self.async_runtime = None;
        result
    }
}

impl Drop for PreparedSnapshotV2MultiBlockBundle {
    fn drop(&mut self) {
        let _ = cleanup_async_bundle(&mut self.records, self.async_runtime.as_ref());
    }
}

impl fmt::Debug for PreparedSnapshotV2MultiBlockBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2MultiBlockBundle")
            .field("record_count", &self.records.len())
            .field("has_async_runtime", &self.async_runtime.is_some())
            .field("state", &"<redacted>")
            .finish()
    }
}

/// Failure while proving a profile-2 graph against loaded destination state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2MultiBlockRestorePlanError {
    InvalidGraph,
    Configuration,
    Allocation,
    QueueMemory,
    QueueContinuation,
    RateLimiter,
}

impl fmt::Display for SnapshotV2MultiBlockRestorePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidGraph => "native-v2 multi-block graph is invalid",
            Self::Configuration => "native-v2 multi-block configuration is invalid",
            Self::Allocation => "native-v2 multi-block restore plan allocation failed",
            Self::QueueMemory => "native-v2 multi-block queue memory is invalid",
            Self::QueueContinuation => "native-v2 multi-block queue continuation is invalid",
            Self::RateLimiter => "native-v2 multi-block rate limiter is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2MultiBlockRestorePlanError {}

/// Failure while adopting one complete backing batch.
pub enum SnapshotV2MultiBlockBundleError {
    Configuration,
    BackingCount,
    Allocation,
    Backing(PreparedSnapshotBlockDeviceError),
    FreshRuntime,
    AsyncBinding {
        source: BlockAsyncRuntimeError,
        cleanup: Option<BlockAsyncRuntimeError>,
    },
}

impl SnapshotV2MultiBlockBundleError {
    pub const fn cleanup_failed(&self) -> bool {
        matches!(
            self,
            Self::AsyncBinding {
                cleanup: Some(_),
                ..
            }
        )
    }
}

impl fmt::Debug for SnapshotV2MultiBlockBundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Configuration => "Configuration",
            Self::BackingCount => "BackingCount",
            Self::Allocation => "Allocation",
            Self::Backing(_) => "Backing",
            Self::FreshRuntime => "FreshRuntime",
            Self::AsyncBinding { .. } => "AsyncBinding",
        };
        formatter
            .debug_struct("SnapshotV2MultiBlockBundleError")
            .field("kind", &kind)
            .field("cleanup_failed", &self.cleanup_failed())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for SnapshotV2MultiBlockBundleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Configuration => "snapshot multi-block drive configuration is inconsistent",
            Self::BackingCount => "snapshot multi-block backing count is inconsistent",
            Self::Allocation => "snapshot multi-block bundle allocation failed",
            Self::Backing(_) => "snapshot multi-block backing is inconsistent",
            Self::FreshRuntime => "snapshot multi-block Async runtime is not fresh",
            Self::AsyncBinding { .. } => "snapshot multi-block Async binding failed",
        })?;
        if self.cleanup_failed() {
            formatter.write_str("; Async cleanup also failed")?;
        }
        Ok(())
    }
}

impl std::error::Error for SnapshotV2MultiBlockBundleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backing(source) => Some(source),
            Self::AsyncBinding { source, .. } => Some(source),
            Self::Configuration | Self::BackingCount | Self::Allocation | Self::FreshRuntime => {
                None
            }
        }
    }
}

/// Failure while explicitly releasing a prepared pathless bundle.
pub struct SnapshotV2MultiBlockCleanupError {
    source: BlockAsyncRuntimeError,
    additional_failures: usize,
}

impl SnapshotV2MultiBlockCleanupError {
    pub const fn additional_failures(&self) -> usize {
        self.additional_failures
    }
}

impl fmt::Debug for SnapshotV2MultiBlockCleanupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2MultiBlockCleanupError")
            .field("additional_failures", &self.additional_failures)
            .field("state", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for SnapshotV2MultiBlockCleanupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("snapshot multi-block Async cleanup failed")
    }
}

impl std::error::Error for SnapshotV2MultiBlockCleanupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn async_binding_failure(
    source: BlockAsyncRuntimeError,
    runtime: &SharedBlockAsyncRuntime,
    generations: &[BlockAsyncDriveGeneration],
) -> SnapshotV2MultiBlockBundleError {
    SnapshotV2MultiBlockBundleError::AsyncBinding {
        source,
        cleanup: discard_generations(runtime, generations),
    }
}

fn validate_mmio_async_ownership(
    records: &[PreparedSnapshotV2MultiBlockRecord],
    runtime: Option<&SharedBlockAsyncRuntime>,
    generations: &mut Vec<BlockAsyncDriveGeneration>,
) -> bool {
    for record in records {
        match (
            record.device.device().io_engine(),
            record.async_generation,
            record.device.device().async_binding(),
            runtime,
        ) {
            (Some(DriveIoEngine::Sync), None, None, _) => {}
            (
                Some(DriveIoEngine::Async),
                Some(generation),
                Some((bound_runtime, bound_generation)),
                Some(runtime),
            ) if generation == bound_generation
                && runtime.same_runtime(&bound_runtime)
                && !generations.contains(&generation)
                && runtime
                    .pressure_pending(generation)
                    .is_ok_and(|pending| !pending)
                && runtime
                    .pop_completion(generation)
                    .is_ok_and(|completion| completion.is_none()) =>
            {
                generations.push(generation);
            }
            _ => return false,
        }
    }
    match runtime {
        Some(runtime) => {
            !generations.is_empty()
                && runtime.generation_count().ok() == Some(generations.len())
                && runtime.outstanding_tasks().ok() == Some(0)
        }
        None => generations.is_empty(),
    }
}

fn cleanup_async_bundle(
    records: &mut [PreparedSnapshotV2MultiBlockRecord],
    runtime: Option<&SharedBlockAsyncRuntime>,
) -> Result<(), SnapshotV2MultiBlockCleanupError> {
    let Some(runtime) = runtime else {
        return Ok(());
    };
    let mut first = None;
    let mut additional_failures = 0_usize;
    for record in records.iter_mut().rev() {
        let generation = record
            .device
            .device()
            .async_binding()
            .and_then(|(bound_runtime, generation)| {
                runtime.same_runtime(&bound_runtime).then_some(generation)
            })
            .or_else(|| record.async_generation.take());
        record.async_generation = None;
        let Some(generation) = generation else {
            continue;
        };
        if let Err(source) = runtime.discard_generation_without_guest_memory(generation) {
            if first.is_none() {
                first = Some(source);
            } else {
                additional_failures = additional_failures.saturating_add(1);
            }
        }
    }
    let shutdown = match runtime.shutdown_if_idle() {
        Ok(true) => None,
        Ok(false) => Some(BlockAsyncRuntimeError::ExecutorInvariant),
        Err(source) => Some(source),
    };
    if let Some(source) = shutdown {
        if first.is_none() {
            first = Some(source);
        } else {
            additional_failures = additional_failures.saturating_add(1);
        }
    }
    match first {
        Some(source) => Err(SnapshotV2MultiBlockCleanupError {
            source,
            additional_failures,
        }),
        None => Ok(()),
    }
}

fn cleanup_async_generations(
    runtime: Option<&SharedBlockAsyncRuntime>,
    generations: &[BlockAsyncDriveGeneration],
) -> Result<(), SnapshotV2MultiBlockCleanupError> {
    let Some(runtime) = runtime else {
        return Ok(());
    };
    let mut first = None;
    let mut additional_failures = 0_usize;
    for generation in generations.iter().rev().copied() {
        if let Err(source) = runtime.discard_generation_without_guest_memory(generation) {
            if first.is_none() {
                first = Some(source);
            } else {
                additional_failures = additional_failures.saturating_add(1);
            }
        }
    }
    let shutdown = match runtime.shutdown_if_idle() {
        Ok(true) => None,
        Ok(false) => Some(BlockAsyncRuntimeError::ExecutorInvariant),
        Err(source) => Some(source),
    };
    if let Some(source) = shutdown {
        if first.is_none() {
            first = Some(source);
        } else {
            additional_failures = additional_failures.saturating_add(1);
        }
    }
    match first {
        Some(source) => Err(SnapshotV2MultiBlockCleanupError {
            source,
            additional_failures,
        }),
        None => Ok(()),
    }
}

fn discard_generations(
    runtime: &SharedBlockAsyncRuntime,
    generations: &[BlockAsyncDriveGeneration],
) -> Option<BlockAsyncRuntimeError> {
    let mut first = None;
    for generation in generations.iter().rev().copied() {
        if let Err(source) = runtime.discard_generation_without_guest_memory(generation)
            && first.is_none()
        {
            first = Some(source);
        }
    }
    match runtime.shutdown_if_idle() {
        Ok(true) => {}
        Ok(false) if first.is_none() => {
            first = Some(BlockAsyncRuntimeError::ExecutorInvariant);
        }
        Err(source) if first.is_none() => {
            first = Some(source);
        }
        Ok(false) | Err(_) => {}
    }
    first
}

fn range_is_wholly_contained(memory: &GuestMemory, range: GuestMemoryRange) -> bool {
    memory.regions().iter().any(|region| {
        let region = region.range();
        region.start().raw_value() <= range.start().raw_value()
            && range.end_exclusive().raw_value() <= region.end_exclusive().raw_value()
    })
}

const fn feature_enabled(features: u64, feature: u32) -> bool {
    features & (1_u64 << feature) != 0
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

fn persisted_limiter_state(
    config: Option<DriveRateLimiterConfig>,
    state: SnapshotV2BlockLimiterState,
) -> Result<VirtioBlockRateLimiterState, SnapshotV2MultiBlockRestorePlanError> {
    Ok(VirtioBlockRateLimiterState::new(
        persisted_bucket_state(
            config.and_then(DriveRateLimiterConfig::bandwidth),
            state.bandwidth(),
        )?,
        persisted_bucket_state(config.and_then(DriveRateLimiterConfig::ops), state.ops())?,
    ))
}

fn persisted_bucket_state(
    config: Option<DriveTokenBucketConfig>,
    state: Option<SnapshotV2BlockBucketState>,
) -> Result<Option<VirtioBlockTokenBucketState>, SnapshotV2MultiBlockRestorePlanError> {
    match (config, state) {
        (Some(config), Some(state)) => Ok(Some(VirtioBlockTokenBucketState::new(
            config,
            state.budget(),
            state.remaining_burst(),
            state.age_nanos(),
        ))),
        (None, None) => Ok(None),
        _ => Err(SnapshotV2MultiBlockRestorePlanError::InvalidGraph),
    }
}

trait RestoreReserve {
    fn reserve<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()>;
}

struct SystemRestoreReserve;

impl RestoreReserve for SystemRestoreReserve {
    fn reserve<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()> {
        values.try_reserve_exact(additional).map_err(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs::{self, OpenOptions};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::memory::{GuestAddress, GuestMemoryLayout};

    static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

    struct TempBacking {
        path: PathBuf,
    }

    impl TempBacking {
        fn new(name: &str, len: u64) -> Self {
            let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-profile-2-restore-{name}-{}-{sequence}",
                std::process::id()
            ));
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("test backing should create");
            file.set_len(len).expect("test backing should resize");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempBacking {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    struct FailingReserve {
        calls: usize,
        fail_at: usize,
    }

    impl RestoreReserve for FailingReserve {
        fn reserve<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()> {
            let current = self.calls;
            self.calls = self.calls.saturating_add(1);
            if current == self.fail_at {
                Err(())
            } else {
                values.try_reserve_exact(additional).map_err(|_| ())
            }
        }
    }

    fn memory_for(graph: &SnapshotV2MultiBlockDeviceGraph) -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), 0x80_0000)
                .expect("test memory range should validate"),
        ])
        .expect("test memory layout should validate");
        let mut memory = GuestMemory::allocate(&layout).expect("test memory should allocate");
        for record in graph.records() {
            let Some(cursor) = record.block().continuation().active_queue() else {
                continue;
            };
            let queue = record
                .virtio()
                .queues()
                .first()
                .expect("fixture queue should exist");
            let available_index =
                if record.block().continuation().retry() == StorageRetryState::None {
                    cursor.next_available()
                } else {
                    cursor.next_available().wrapping_add(1)
                };
            memory
                .write_slice(
                    &available_index.to_le_bytes(),
                    GuestAddress::new(queue.driver_ring().raw_value() + 2),
                )
                .expect("available cursor should write");
            memory
                .write_slice(
                    &cursor.next_used().to_le_bytes(),
                    GuestAddress::new(queue.device_ring().raw_value() + 2),
                )
                .expect("used cursor should write");
        }
        memory
    }

    fn open_backings(
        graph: &SnapshotV2MultiBlockDeviceGraph,
    ) -> (Vec<TempBacking>, Vec<crate::block::BlockFileBacking>) {
        let files: Vec<_> = graph
            .records()
            .iter()
            .enumerate()
            .map(|(index, record)| {
                TempBacking::new(&index.to_string(), record.block().backing_bytes())
            })
            .collect();
        let backings = files
            .iter()
            .zip(graph.records())
            .map(|(file, record)| {
                crate::block::BlockFileBacking::open_snapshot(
                    file.path(),
                    record.config().is_read_only(),
                )
                .expect("snapshot backing should open")
                .0
            })
            .collect();
        (files, backings)
    }

    #[test]
    fn exact_mixed_bundle_is_pathless_and_binds_one_fresh_runtime() {
        for transport in [
            SnapshotV2DeviceTransportKind::Mmio,
            SnapshotV2DeviceTransportKind::Pci,
        ] {
            let graph = crate::snapshot_device_v2_5::tests::fixture_graph(transport, true);
            let expected = graph.clone();
            let memory = memory_for(&graph);
            let now = Instant::now();
            let configs = graph
                .project_drive_configs()
                .expect("fixture configs should project");
            let (_files, backings) = open_backings(&graph);
            let bundle = SnapshotV2MultiBlockRestorePlan::prepare(graph, &memory, now)
                .expect("fixture graph should prepare")
                .prepare_backings(configs.clone(), backings)
                .expect("fixture backings should prepare");

            assert_eq!(bundle.drive_configs(), &configs);
            assert_eq!(bundle.records().len(), expected.records().len());
            assert_eq!(bundle.retry_projection().len(), expected.records().len());
            let runtime = bundle
                .async_runtime()
                .expect("mixed fixture should own one Async runtime");
            assert_eq!(runtime.generation_count().expect("runtime should lock"), 1);
            assert_eq!(runtime.outstanding_tasks().expect("runtime should lock"), 0);

            for ((prepared, expected), retry) in bundle
                .records()
                .iter()
                .zip(expected.records())
                .zip(bundle.retry_projection())
            {
                assert_eq!(prepared.key(), expected.key());
                assert_eq!(prepared.drive_id(), expected.config().drive_id());
                assert_eq!(prepared.is_root_device(), expected.is_root());
                assert_eq!(
                    prepared.config_space(),
                    VirtioBlockConfigSpace::new(
                        expected.block().backing_bytes(),
                        expected.config().is_read_only(),
                        expected.config().cache_type(),
                    )
                );
                assert_eq!(prepared.virtio(), expected.virtio());
                assert_eq!(prepared.transport(), expected.transport());
                assert_eq!(prepared.retry(), expected.block().continuation().retry());
                assert_eq!(retry.key(), expected.key());
                assert_eq!(retry.retry(), prepared.retry());
                assert_eq!(retry.retry_deadline(), prepared.retry_deadline());
                assert_eq!(
                    prepared.device().device().io_engine(),
                    Some(expected.config().io_engine())
                );
                assert_eq!(
                    prepared.device().cache_type(),
                    expected.config().cache_type()
                );
                assert_eq!(
                    prepared
                        .device()
                        .device()
                        .backing()
                        .expect("prepared device should retain its backing")
                        .is_read_only(),
                    expected.config().is_read_only()
                );
                assert_eq!(
                    prepared.device().device().device_id(),
                    expected.block().continuation().device_id()
                );
                assert_eq!(
                    prepared
                        .device()
                        .device()
                        .active_queue()
                        .map(VirtioBlockQueue::snapshot_state),
                    expected.block().continuation().active_queue()
                );
                assert_eq!(
                    prepared.device().device().has_pending_rate_limited_queue(),
                    prepared.retry() != StorageRetryState::None
                );
                let limiter = prepared
                    .device()
                    .device()
                    .snapshot_rate_limiter_state_at(expected.config().rate_limiter(), now)
                    .expect("restored limiter should recapture");
                assert_eq!(
                    limiter.bandwidth().map(|bucket| (
                        bucket.config(),
                        bucket.budget(),
                        bucket.remaining_burst(),
                        bucket.age_nanos(),
                    )),
                    expected
                        .block()
                        .continuation()
                        .limiter()
                        .bandwidth()
                        .zip(
                            expected
                                .config()
                                .rate_limiter()
                                .and_then(DriveRateLimiterConfig::bandwidth)
                        )
                        .map(|(state, config)| (
                            config,
                            state.budget(),
                            state.remaining_burst(),
                            state.age_nanos(),
                        ))
                );
                assert_eq!(
                    limiter.ops().map(|bucket| (
                        bucket.config(),
                        bucket.budget(),
                        bucket.remaining_burst(),
                        bucket.age_nanos(),
                    )),
                    expected
                        .block()
                        .continuation()
                        .limiter()
                        .ops()
                        .zip(
                            expected
                                .config()
                                .rate_limiter()
                                .and_then(DriveRateLimiterConfig::ops)
                        )
                        .map(|(state, config)| (
                            config,
                            state.budget(),
                            state.remaining_burst(),
                            state.age_nanos(),
                        ))
                );
                match expected.config().io_engine() {
                    DriveIoEngine::Sync => assert_eq!(prepared.async_generation(), None),
                    DriveIoEngine::Async => {
                        let generation = prepared
                            .async_generation()
                            .expect("Async record should own a generation");
                        assert_eq!(generation.value(), 1);
                        assert!(
                            !runtime
                                .pressure_pending(generation)
                                .expect("runtime should lock")
                        );
                        assert!(
                            runtime
                                .pop_completion(generation)
                                .expect("runtime should lock")
                                .is_none()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn consuming_mmio_handoff_preserves_exact_transport_and_async_identity() {
        let graph = crate::snapshot_device_v2_5::tests::fixture_graph(
            SnapshotV2DeviceTransportKind::Mmio,
            true,
        );
        let expected = graph.clone();
        let memory = memory_for(&graph);
        let now = Instant::now();
        let configs = graph
            .project_drive_configs()
            .expect("fixture configs should project");
        let (_files, backings) = open_backings(&graph);
        let bundle = SnapshotV2MultiBlockRestorePlan::prepare(graph, &memory, now)
            .expect("fixture graph should prepare")
            .prepare_backings(configs.clone(), backings)
            .expect("fixture backings should prepare");
        let expected_runtime = bundle
            .async_runtime()
            .expect("mixed fixture should own one runtime")
            .clone();

        let mmio = bundle
            .prepare_mmio_transport()
            .expect("exact MMIO vector should reconstruct");
        assert_eq!(mmio.drive_configs(), &configs);
        assert_eq!(mmio.records().len(), expected.records().len());
        assert!(
            mmio.async_runtime()
                .is_some_and(|runtime| runtime.same_runtime(&expected_runtime))
        );
        let (returned_configs, records, runtime, generations) = mmio.into_parts();
        assert_eq!(returned_configs, configs);
        assert_eq!(generations.len(), 1);
        for (record, expected) in records.into_iter().zip(expected.records()) {
            let SnapshotV2DeviceTransport::Mmio(expected_mmio) = expected.transport() else {
                panic!("fixture transport should be MMIO");
            };
            let expected_transport = restore_mmio_transport_state(expected.virtio(), expected_mmio)
                .expect("retained transport should restore");
            let (
                key,
                drive_id,
                is_root,
                retry,
                retry_deadline,
                region,
                interrupt_line,
                generation,
                handler,
            ) = record.into_parts();
            assert_eq!(key, expected.key());
            assert_eq!(drive_id, expected.config().drive_id());
            assert_eq!(is_root, expected.is_root());
            assert_eq!(retry, expected.block().continuation().retry());
            assert_eq!(retry_deadline.is_some(), retry != StorageRetryState::None);
            assert_eq!(region, expected_mmio.region());
            assert_eq!(interrupt_line, expected_mmio.interrupt_line());
            assert_eq!(handler.transport_state(), expected_transport);
            match expected.config().io_engine() {
                DriveIoEngine::Sync => {
                    assert_eq!(generation, None);
                    assert!(handler.block_async_binding().is_none());
                }
                DriveIoEngine::Async => {
                    let generation = generation.expect("Async generation should transfer");
                    let (handler_runtime, handler_generation) = handler
                        .block_async_binding()
                        .expect("Async handler should retain its binding");
                    assert_eq!(handler_generation, generation);
                    assert!(handler_runtime.same_runtime(&expected_runtime));
                }
            }
        }
        cleanup_async_generations(runtime.as_ref(), &generations)
            .expect("transferred Async runtime should cleanly release");
        assert_eq!(
            expected_runtime
                .generation_count()
                .expect("runtime should remain observable"),
            0
        );
    }

    #[test]
    fn consuming_mmio_handoff_rejects_foreign_transport_and_invalid_generation_set() {
        let pci_graph = crate::snapshot_device_v2_5::tests::fixture_graph(
            SnapshotV2DeviceTransportKind::Pci,
            true,
        );
        let memory = memory_for(&pci_graph);
        let configs = pci_graph
            .project_drive_configs()
            .expect("fixture configs should project");
        let (_files, backings) = open_backings(&pci_graph);
        let bundle = SnapshotV2MultiBlockRestorePlan::prepare(pci_graph, &memory, Instant::now())
            .expect("PCI graph should prepare")
            .prepare_backings(configs, backings)
            .expect("PCI bundle should prepare");
        let runtime = bundle
            .async_runtime()
            .expect("mixed fixture should own one runtime")
            .clone();
        let error = bundle
            .prepare_mmio_transport()
            .expect_err("PCI vector must not enter the MMIO handoff");
        assert!(!error.cleanup_failed());
        assert_eq!(runtime.generation_count().expect("runtime should lock"), 0);

        let graph = crate::snapshot_device_v2_5::tests::fixture_graph(
            SnapshotV2DeviceTransportKind::Mmio,
            true,
        );
        let memory = memory_for(&graph);
        let configs = graph
            .project_drive_configs()
            .expect("fixture configs should project");
        let (_files, backings) = open_backings(&graph);
        let mut bundle = SnapshotV2MultiBlockRestorePlan::prepare(graph, &memory, Instant::now())
            .expect("MMIO graph should prepare")
            .prepare_backings(configs, backings)
            .expect("MMIO bundle should prepare");
        let runtime = bundle
            .async_runtime()
            .expect("mixed fixture should own one runtime")
            .clone();
        let async_record = bundle
            .records
            .iter_mut()
            .find(|record| record.async_generation.is_some())
            .expect("fixture should contain one Async record");
        async_record.async_generation = None;
        let error = bundle
            .prepare_mmio_transport()
            .expect_err("missing generation ownership must be rejected");
        assert!(!error.cleanup_failed());
        assert_eq!(runtime.generation_count().expect("runtime should lock"), 0);
    }

    #[test]
    fn rootless_and_maximum_vectors_preserve_exact_projection() {
        for (count, with_root) in [(2, false), (64, true)] {
            let graph = crate::snapshot_device_v2_5::tests::boundary_graph(count, with_root);
            let memory = memory_for(&graph);
            let configs = graph
                .project_drive_configs()
                .expect("boundary configs should project");
            let (_files, backings) = open_backings(&graph);
            let bundle = SnapshotV2MultiBlockRestorePlan::prepare(graph, &memory, Instant::now())
                .expect("boundary graph should prepare")
                .prepare_backings(configs.clone(), backings)
                .expect("boundary backings should prepare");
            assert_eq!(bundle.records().len(), count);
            assert_eq!(bundle.drive_configs(), &configs);
            assert_eq!(bundle.drive_configs().has_root_device(), with_root);
            let async_count = configs
                .as_slice()
                .iter()
                .filter(|config| config.io_engine() == Some(DriveIoEngine::Async))
                .count();
            assert_eq!(bundle.async_runtime().is_some(), async_count != 0);
            if let Some(runtime) = bundle.async_runtime() {
                assert_eq!(
                    runtime.generation_count().expect("runtime should lock"),
                    async_count
                );
            }
        }
    }

    #[test]
    fn sync_only_vector_never_creates_an_async_runtime() {
        let mut graph = crate::snapshot_device_v2_5::tests::boundary_graph(2, false);
        for record in &mut graph.records {
            record.config.io_engine = DriveIoEngine::Sync;
        }
        validate_graph(&graph).expect("Sync-only graph should validate");
        let memory = memory_for(&graph);
        let configs = graph
            .project_drive_configs()
            .expect("Sync-only configs should project");
        let (_files, backings) = open_backings(&graph);
        let bundle = SnapshotV2MultiBlockRestorePlan::prepare(graph, &memory, Instant::now())
            .expect("Sync-only graph should prepare")
            .prepare_backings_with(
                configs,
                backings,
                &mut SystemRestoreReserve,
                || panic!("Sync-only preparation must not create an Async runtime"),
                |device, runtime| device.bind_async_runtime(runtime.clone()),
            )
            .expect("Sync-only backings should prepare");
        assert!(bundle.async_runtime().is_none());
        assert!(
            bundle
                .records()
                .iter()
                .all(|record| record.async_generation().is_none())
        );
    }

    #[test]
    fn loaded_memory_containment_and_ring_cursors_fail_before_adoption() {
        let mut graph = crate::snapshot_device_v2_5::tests::fixture_graph(
            SnapshotV2DeviceTransportKind::Mmio,
            true,
        );
        let queue = graph.records[0].virtio.queues()[0];
        graph.records[0].virtio = crate::snapshot_device_v2_5::tests::replace_queue(
            &graph.records[0].virtio,
            SnapshotV2VirtioQueueState::from_parts(
                queue.max_size(),
                queue.size(),
                queue.ready(),
                GuestAddress::new(0xf800),
                queue.driver_ring(),
                queue.device_ring(),
            ),
        );
        validate_graph(&graph).expect("cross-region graph should remain structural");
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), 0x1_0000)
                .expect("first region should validate"),
            GuestMemoryRange::new(GuestAddress::new(0x1_0000), 0x7f_0000)
                .expect("second region should validate"),
        ])
        .expect("split memory layout should validate");
        let memory = GuestMemory::allocate(&layout).expect("split memory should allocate");
        assert_eq!(
            SnapshotV2MultiBlockRestorePlan::prepare(graph, &memory, Instant::now())
                .expect_err("cross-region queue must reject"),
            SnapshotV2MultiBlockRestorePlanError::QueueMemory
        );

        let graph = crate::snapshot_device_v2_5::tests::fixture_graph(
            SnapshotV2DeviceTransportKind::Mmio,
            true,
        );
        let mut memory = memory_for(&graph);
        let queue = graph.records()[0].virtio().queues()[0];
        memory
            .write_slice(
                &7_u16.to_le_bytes(),
                GuestAddress::new(queue.device_ring().raw_value() + 2),
            )
            .expect("mismatched cursor should write");
        assert_eq!(
            SnapshotV2MultiBlockRestorePlan::prepare(graph, &memory, Instant::now())
                .expect_err("cursor mismatch must reject"),
            SnapshotV2MultiBlockRestorePlanError::QueueContinuation
        );
    }

    #[test]
    fn complete_backing_and_configuration_preflight_is_all_or_nothing() {
        let graph = crate::snapshot_device_v2_5::tests::fixture_graph(
            SnapshotV2DeviceTransportKind::Mmio,
            true,
        );
        let memory = memory_for(&graph);
        let configs = graph
            .project_drive_configs()
            .expect("fixture configs should project");
        let plan = SnapshotV2MultiBlockRestorePlan::prepare(graph.clone(), &memory, Instant::now())
            .expect("fixture graph should prepare");
        let (_files, mut backings) = open_backings(&graph);
        backings.pop();
        assert!(matches!(
            plan.prepare_backings(configs.clone(), backings),
            Err(SnapshotV2MultiBlockBundleError::BackingCount)
        ));

        let plan = SnapshotV2MultiBlockRestorePlan::prepare(graph.clone(), &memory, Instant::now())
            .expect("fixture graph should prepare");
        let wrong_configs = crate::snapshot_device_v2_5::tests::fixture_graph(
            SnapshotV2DeviceTransportKind::Mmio,
            false,
        )
        .project_drive_configs()
        .expect("different fixture configs should project");
        let (_files, backings) = open_backings(&graph);
        assert!(matches!(
            plan.prepare_backings(wrong_configs, backings),
            Err(SnapshotV2MultiBlockBundleError::Configuration)
        ));

        let plan = SnapshotV2MultiBlockRestorePlan::prepare(graph.clone(), &memory, Instant::now())
            .expect("fixture graph should prepare");
        let (mut files, mut backings) = open_backings(&graph);
        let wrong = TempBacking::new("wrong-geometry", 4096);
        backings[1] = crate::block::BlockFileBacking::open_snapshot(
            wrong.path(),
            graph.records()[1].config().is_read_only(),
        )
        .expect("wrong backing should open")
        .0;
        files.push(wrong);
        assert!(matches!(
            plan.prepare_backings(configs, backings),
            Err(SnapshotV2MultiBlockBundleError::Backing(
                PreparedSnapshotBlockDeviceError::GeometryMismatch
            ))
        ));

        let plan = SnapshotV2MultiBlockRestorePlan::prepare(graph.clone(), &memory, Instant::now())
            .expect("fixture graph should prepare");
        let (_files, mut backings) = open_backings(&graph);
        backings[0] = crate::block::BlockFileBacking::open_snapshot(_files[0].path(), false)
            .expect("opposite-access backing should open")
            .0;
        let configs = graph
            .project_drive_configs()
            .expect("fixture configs should project");
        assert!(matches!(
            plan.prepare_backings(configs, backings),
            Err(SnapshotV2MultiBlockBundleError::Backing(
                PreparedSnapshotBlockDeviceError::BackingModeMismatch
            ))
        ));
    }

    #[test]
    fn allocation_and_partial_async_binding_failures_release_every_generation() {
        let graph = crate::snapshot_device_v2_5::tests::fixture_graph(
            SnapshotV2DeviceTransportKind::Mmio,
            false,
        );
        let memory = memory_for(&graph);
        let mut successful = FailingReserve {
            calls: 0,
            fail_at: usize::MAX,
        };
        SnapshotV2MultiBlockRestorePlan::prepare_with_reserve(
            graph.clone(),
            &memory,
            Instant::now(),
            &mut successful,
        )
        .expect("unfailing reserve should prepare");
        assert_eq!(successful.calls, 1);
        let mut failing = FailingReserve {
            calls: 0,
            fail_at: 0,
        };
        assert_eq!(
            SnapshotV2MultiBlockRestorePlan::prepare_with_reserve(
                graph.clone(),
                &memory,
                Instant::now(),
                &mut failing,
            )
            .expect_err("plan allocation should fail"),
            SnapshotV2MultiBlockRestorePlanError::Allocation
        );

        let configs = graph
            .project_drive_configs()
            .expect("fixture configs should project");
        let (_files, backings) = open_backings(&graph);
        let plan = SnapshotV2MultiBlockRestorePlan::prepare(graph.clone(), &memory, Instant::now())
            .expect("fixture graph should prepare");
        for fail_at in 0..3 {
            let mut reserve = FailingReserve { calls: 0, fail_at };
            let runtime = SharedBlockAsyncRuntime::new();
            assert!(matches!(
                plan_from_clone(&plan).prepare_backings_with(
                    configs.clone(),
                    clone_backings(&graph, &_files),
                    &mut reserve,
                    || runtime,
                    |device, runtime| device.bind_async_runtime(runtime.clone()),
                ),
                Err(SnapshotV2MultiBlockBundleError::Allocation)
            ));
        }
        drop(backings);

        let mut async_graph = graph;
        async_graph.records[0].config.io_engine = DriveIoEngine::Async;
        async_graph.records[1].config.io_engine = DriveIoEngine::Async;
        validate_graph(&async_graph).expect("all-Async graph should validate");
        let memory = memory_for(&async_graph);
        let configs = async_graph
            .project_drive_configs()
            .expect("Async configs should project");
        let (_files, backings) = open_backings(&async_graph);
        let runtime = SharedBlockAsyncRuntime::new();
        let observed = runtime.clone();
        let mut bind_index = 0_usize;
        let error = SnapshotV2MultiBlockRestorePlan::prepare(async_graph, &memory, Instant::now())
            .expect("Async graph should prepare")
            .prepare_backings_with(
                configs,
                backings,
                &mut SystemRestoreReserve,
                || runtime,
                |device, runtime| {
                    let current = bind_index;
                    bind_index = bind_index.saturating_add(1);
                    if current == 1 {
                        Err(BlockAsyncRuntimeError::MetadataAllocation)
                    } else {
                        device.bind_async_runtime(runtime.clone())
                    }
                },
            )
            .expect_err("second Async binding should fail");
        assert!(matches!(
            error,
            SnapshotV2MultiBlockBundleError::AsyncBinding { cleanup: None, .. }
        ));
        assert_eq!(
            observed
                .generation_count()
                .expect("cleaned runtime should lock"),
            0
        );
        assert!(
            observed
                .shutdown_if_idle()
                .expect("cleaned runtime should stop")
        );
    }

    #[test]
    fn diagnostics_and_restore_source_are_redacted_and_path_free() {
        let graph = crate::snapshot_device_v2_5::tests::fixture_graph(
            SnapshotV2DeviceTransportKind::Pci,
            true,
        );
        let memory = memory_for(&graph);
        let configs = graph
            .project_drive_configs()
            .expect("fixture configs should project");
        let (_files, backings) = open_backings(&graph);
        let plan = SnapshotV2MultiBlockRestorePlan::prepare(graph, &memory, Instant::now())
            .expect("fixture graph should prepare");
        let rendered_plan = format!("{plan:?}");
        let bundle = plan
            .prepare_backings(configs, backings)
            .expect("fixture bundle should prepare");
        let rendered = format!("{bundle:?}\n{:?}", bundle.records()[0]);
        for secret in ["rootfs", "logical-selector-0", "1111-2222"] {
            assert!(!rendered_plan.contains(secret));
            assert!(!rendered.contains(secret));
        }
        assert!(rendered.contains("<redacted>"));

        let source = include_str!("restore.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source prefix should exist");
        for forbidden in ["std::fs", "OpenOptions", "path_on_host()", "PathBuf"] {
            assert!(!source.contains(forbidden));
        }
    }

    fn plan_from_clone(plan: &SnapshotV2MultiBlockRestorePlan) -> SnapshotV2MultiBlockRestorePlan {
        SnapshotV2MultiBlockRestorePlan {
            expected_drive_configs: plan.expected_drive_configs.clone(),
            records: plan
                .records
                .iter()
                .map(|record| SnapshotV2MultiBlockRecordPlan {
                    key: record.key,
                    drive_id: record.drive_id.clone(),
                    is_root: record.is_root,
                    io_engine: record.io_engine,
                    cache_type: record.cache_type,
                    config_space: record.config_space,
                    device_id: record.device_id,
                    queue_ranges: record.queue_ranges,
                    active_queue: record.active_queue.clone(),
                    rate_limiter: record.rate_limiter.clone(),
                    retry: record.retry,
                    retry_deadline: record.retry_deadline,
                    virtio: record.virtio.clone(),
                    transport: record.transport.clone(),
                })
                .collect(),
        }
    }

    fn clone_backings(
        graph: &SnapshotV2MultiBlockDeviceGraph,
        files: &[TempBacking],
    ) -> Vec<crate::block::BlockFileBacking> {
        files
            .iter()
            .zip(graph.records())
            .map(|(file, record)| {
                crate::block::BlockFileBacking::open_snapshot(
                    file.path(),
                    record.config().is_read_only(),
                )
                .expect("snapshot backing should reopen")
                .0
            })
            .collect()
    }
}
