//! Live-state conversion boundary for the pure profile-2 artifact model.

use super::*;

use crate::block::{
    BlockCaptureIoEngine, VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE, VIRTIO_BLOCK_SECTOR_SHIFT,
};
use crate::snapshot_device_v2::{
    SnapshotV2DeviceGraphCaptureError, capture_limiter_state, capture_mmio_common,
    capture_mmio_transport, capture_pci_common, capture_pci_transport_parts,
};
use crate::storage_capture::{CaptureReadyBlockDeviceState, StorageTransportState};

impl SnapshotV2MultiBlockDeviceGraph {
    /// Validates profile-2 public configuration before live source mutation.
    pub fn preflight_capture_configs(
        compatibility_version: SnapshotFormatVersion,
        configs: &[crate::block::DriveConfig],
    ) -> Result<(), SnapshotV2MultiBlockDeviceGraphCaptureError> {
        preflight_capture_configs(compatibility_version, configs)
    }

    /// Converts one config-ordered live block vector into detached profile 2.
    ///
    /// Backing generation, host executor counters, and host file identity are
    /// deliberately discarded. The resulting graph retains only the stable
    /// semantic engine choice and exact guest-visible continuation.
    pub fn from_capture_ready_blocks(
        compatibility_version: SnapshotFormatVersion,
        states: &[CaptureReadyBlockDeviceState],
    ) -> Result<Self, SnapshotV2MultiBlockDeviceGraphCaptureError> {
        capture_multi_block_graph(compatibility_version, states)
    }
}

fn capture_multi_block_graph(
    compatibility_version: SnapshotFormatVersion,
    states: &[CaptureReadyBlockDeviceState],
) -> Result<SnapshotV2MultiBlockDeviceGraph, SnapshotV2MultiBlockDeviceGraphCaptureError> {
    let mut configs = Vec::new();
    configs
        .try_reserve_exact(states.len())
        .map_err(|_| SnapshotV2MultiBlockDeviceGraphCaptureError::Allocation)?;
    configs.extend(states.iter().map(CaptureReadyBlockDeviceState::config));
    preflight_capture_config_refs(compatibility_version, &configs)?;

    let mut records = Vec::new();
    records
        .try_reserve_exact(states.len())
        .map_err(|_| SnapshotV2MultiBlockDeviceGraphCaptureError::Allocation)?;
    let mut transport_kind = None;
    for (index, state) in states.iter().enumerate() {
        let (record_transport_kind, record) = capture_multi_block_record(index, state)?;
        match transport_kind {
            None => transport_kind = Some(record_transport_kind),
            Some(expected) if expected == record_transport_kind => {}
            Some(_) => {
                return Err(SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedInventory);
            }
        }
        records.push(record);
    }

    let root_key = records
        .first()
        .filter(|record| record.config.is_root)
        .map(|record| record.key);
    SnapshotV2MultiBlockDeviceGraph::try_from_parts(
        root_key,
        transport_kind.ok_or(SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedInventory)?,
        records,
    )
    .map_err(|_| SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidGraph)
}

pub(crate) fn capture_multi_block_record(
    index: usize,
    state: &CaptureReadyBlockDeviceState,
) -> Result<
    (
        SnapshotV2DeviceTransportKind,
        SnapshotV2MultiBlockDeviceRecord,
    ),
    SnapshotV2MultiBlockDeviceGraphCaptureError,
> {
    let config = capture_multi_block_config(state)?;
    let block = capture_multi_block_state(state, &config)?;
    let expected_features =
        VirtioBlockConfigSpace::new(block.backing_bytes, config.is_read_only, config.cache_type)
            .available_features();
    let (transport_kind, virtio, transport) = match state.transport() {
        StorageTransportState::Mmio(mmio) => (
            SnapshotV2DeviceTransportKind::Mmio,
            capture_mmio_common(mmio.transport(), expected_features).map_err(map_capture_error)?,
            SnapshotV2DeviceTransport::Mmio(
                capture_mmio_transport(mmio.region(), mmio.interrupt_line(), mmio.transport())
                    .map_err(map_capture_error)?,
            ),
        ),
        StorageTransportState::Pci(pci) => (
            SnapshotV2DeviceTransportKind::Pci,
            capture_pci_common(pci.transport(), expected_features).map_err(map_capture_error)?,
            SnapshotV2DeviceTransport::Pci(
                capture_pci_transport_parts(
                    pci.origin(),
                    pci.sbdf(),
                    pci.bar_range(),
                    pci.transport(),
                )
                .map_err(map_capture_error)?,
            ),
        ),
    };
    let instance = u32::try_from(index)
        .map_err(|_| SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedInventory)?;
    Ok((
        transport_kind,
        SnapshotV2MultiBlockDeviceRecord {
            key: SnapshotV2DeviceKey::block(instance),
            config,
            block,
            virtio,
            transport,
        },
    ))
}

fn preflight_capture_configs(
    compatibility_version: SnapshotFormatVersion,
    configs: &[crate::block::DriveConfig],
) -> Result<(), SnapshotV2MultiBlockDeviceGraphCaptureError> {
    let mut refs = Vec::new();
    refs.try_reserve_exact(configs.len())
        .map_err(|_| SnapshotV2MultiBlockDeviceGraphCaptureError::Allocation)?;
    refs.extend(configs.iter());
    preflight_capture_config_refs(compatibility_version, &refs)
}

pub(crate) fn preflight_capture_config_refs(
    compatibility_version: SnapshotFormatVersion,
    configs: &[&crate::block::DriveConfig],
) -> Result<(), SnapshotV2MultiBlockDeviceGraphCaptureError> {
    if compatibility_version != NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION {
        return Err(SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedVersion);
    }
    if configs.is_empty()
        || configs.len() > usize::from(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_RECORDS)
    {
        return Err(SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedInventory);
    }
    for (index, config) in configs.iter().copied().enumerate() {
        if config.is_vhost_user()
            || config.is_read_only().is_none()
            || config.io_engine().is_none()
            || (config.is_root_device() && index != 0)
            || configs
                .iter()
                .copied()
                .take(index)
                .any(|candidate| candidate.drive_id() == config.drive_id())
        {
            return Err(SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidGraph);
        }
        validate_capture_string(
            config.drive_id(),
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES,
        )?;
        if !config
            .drive_id()
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
        {
            return Err(SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidString);
        }
        if let Some(partuuid) = config.partuuid() {
            validate_capture_string(
                partuuid,
                NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_PARTUUID_BYTES,
            )?;
        }
        let selector = config
            .path_on_host()
            .and_then(|path| path.to_str())
            .ok_or(SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidString)?;
        validate_capture_string(
            selector,
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_SELECTOR_BYTES,
        )?;
        super::validate_limiter_config(config.rate_limiter())
            .map_err(|_| SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedConfiguration)?;
    }
    Ok(())
}

fn capture_multi_block_config(
    state: &CaptureReadyBlockDeviceState,
) -> Result<SnapshotV2MultiBlockConfig, SnapshotV2MultiBlockDeviceGraphCaptureError> {
    let config = state.config();
    let (Some(is_read_only), Some(io_engine)) = (config.is_read_only(), config.io_engine()) else {
        return Err(SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedConfiguration);
    };
    if config.is_vhost_user() {
        return Err(SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedConfiguration);
    }
    let drive_id = clone_capture_string(
        config.drive_id(),
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES,
    )?;
    if !drive_id
        .chars()
        .all(|character| character == '_' || character.is_alphanumeric())
    {
        return Err(SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidString);
    }
    let partuuid = config
        .partuuid()
        .map(|value| {
            clone_capture_string(value, NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_PARTUUID_BYTES)
        })
        .transpose()?;
    let selector = config
        .path_on_host()
        .and_then(|path| path.to_str())
        .ok_or(SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidString)
        .and_then(|value| {
            clone_capture_string(value, NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_SELECTOR_BYTES)
        })?;
    super::validate_limiter_config(config.rate_limiter())
        .map_err(|_| SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedConfiguration)?;

    Ok(SnapshotV2MultiBlockConfig {
        drive_id,
        partuuid,
        is_root: config.is_root_device(),
        is_read_only,
        cache_type: config.cache_type(),
        io_engine,
        rate_limiter: config.rate_limiter(),
        selector,
    })
}

fn capture_multi_block_state(
    state: &CaptureReadyBlockDeviceState,
    config: &SnapshotV2MultiBlockConfig,
) -> Result<SnapshotV2MultiBlockState, SnapshotV2MultiBlockDeviceGraphCaptureError> {
    let device = state.device().clone();
    let backing = device.backing();
    let expected_config_space =
        VirtioBlockConfigSpace::new(backing.len(), config.is_read_only, config.cache_type);
    if !backing.kind().is_regular_file()
        || device.config_space() != expected_config_space
        || device.config_space().config_len() != VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE
        || device.config_space().capacity_sectors() != backing.len() >> VIRTIO_BLOCK_SECTOR_SHIFT
    {
        return Err(SnapshotV2MultiBlockDeviceGraphCaptureError::InconsistentBlockState);
    }
    match (config.io_engine, device.io_engine()) {
        (DriveIoEngine::Sync, BlockCaptureIoEngine::Sync) => {}
        (DriveIoEngine::Async, BlockCaptureIoEngine::Async(async_state))
            if async_state.cache_type() == config.cache_type
                && async_state.admission_stopped()
                && async_state.owned_operations() == 0
                && async_state.parked_host_completions() == 0
                && async_state.final_completions() == 0 => {}
        (DriveIoEngine::Sync | DriveIoEngine::Async, _) => {
            return Err(SnapshotV2MultiBlockDeviceGraphCaptureError::InconsistentBlockState);
        }
    }

    let limiter = capture_limiter_state(config.rate_limiter, device.rate_limiter())
        .map_err(|_| SnapshotV2MultiBlockDeviceGraphCaptureError::InconsistentBlockState)?;
    let active_queue = device.active_queue();
    let retry = state.retry();
    validate_block_retry_state(active_queue.is_some(), limiter, retry)
        .map_err(|_| SnapshotV2MultiBlockDeviceGraphCaptureError::InconsistentBlockState)?;
    Ok(SnapshotV2MultiBlockState {
        backing_bytes: backing.len(),
        continuation: SnapshotV2BlockState::from_parts(
            expected_config_space.capacity_sectors(),
            device.device_id(),
            active_queue,
            limiter,
            retry,
        ),
    })
}

fn clone_capture_string(
    value: &str,
    maximum: usize,
) -> Result<String, SnapshotV2MultiBlockDeviceGraphCaptureError> {
    validate_capture_string(value, maximum)?;
    let mut owned = String::new();
    owned
        .try_reserve_exact(value.len())
        .map_err(|_| SnapshotV2MultiBlockDeviceGraphCaptureError::Allocation)?;
    owned.push_str(value);
    Ok(owned)
}

fn validate_capture_string(
    value: &str,
    maximum: usize,
) -> Result<(), SnapshotV2MultiBlockDeviceGraphCaptureError> {
    if value.is_empty() || value.len() > maximum {
        Err(SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidString)
    } else {
        Ok(())
    }
}

const fn map_capture_error(
    error: SnapshotV2DeviceGraphCaptureError,
) -> SnapshotV2MultiBlockDeviceGraphCaptureError {
    match error {
        SnapshotV2DeviceGraphCaptureError::UnsupportedVersion => {
            SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedVersion
        }
        SnapshotV2DeviceGraphCaptureError::UnsupportedConfiguration => {
            SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedConfiguration
        }
        SnapshotV2DeviceGraphCaptureError::InvalidString => {
            SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidString
        }
        SnapshotV2DeviceGraphCaptureError::InconsistentBlockState => {
            SnapshotV2MultiBlockDeviceGraphCaptureError::InconsistentBlockState
        }
        SnapshotV2DeviceGraphCaptureError::InvalidVirtioState => {
            SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidVirtioState
        }
        SnapshotV2DeviceGraphCaptureError::InvalidMmioState => {
            SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidMmioState
        }
        SnapshotV2DeviceGraphCaptureError::InvalidPciState => {
            SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidPciState
        }
        SnapshotV2DeviceGraphCaptureError::Allocation => {
            SnapshotV2MultiBlockDeviceGraphCaptureError::Allocation
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs::{self, OpenOptions};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    use crate::block::async_executor::{BlockAsyncDriveGeneration, SharedBlockAsyncRuntime};
    use crate::block::{
        DriveConfig, DriveConfigInput, PreparedBlockDevice, VIRTIO_BLOCK_DEVICE_ID,
        VIRTIO_BLOCK_QUEUE_SIZES,
    };
    use crate::interrupt::GuestInterruptLine;
    use crate::memory::{GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange};
    use crate::mmio::{MmioRegion, MmioRegionId};
    use crate::storage_capture::{StorageMmioTransportState, StorageRetryState};
    use crate::virtio_mmio::{VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioRegisterHandler};

    static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {
        fn new(name: &str, len: u64) -> Self {
            let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-snapshot-v2-5-capture-{name}-{}-{sequence}",
                std::process::id(),
            ));
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("capture backing should create");
            file.set_len(len).expect("capture backing should resize");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn config(
        drive_id: &str,
        path: &Path,
        is_root: bool,
        is_read_only: bool,
        cache_type: DriveCacheType,
        io_engine: DriveIoEngine,
    ) -> DriveConfig {
        DriveConfigInput::new(drive_id, drive_id, path, is_root)
            .with_is_read_only(is_read_only)
            .with_cache_type(cache_type)
            .with_io_engine(io_engine)
            .validate()
            .expect("profile-2 capture config should validate")
    }

    fn guest_memory() -> GuestMemory {
        let range = GuestMemoryRange::new(GuestAddress::new(0), 0x1_0000)
            .expect("capture memory range should validate");
        GuestMemory::allocate(
            &GuestMemoryLayout::new(vec![range]).expect("capture memory layout should validate"),
        )
        .expect("capture guest memory should allocate")
    }

    fn capture_mmio(
        config: &DriveConfig,
        index: u32,
        runtime: &SharedBlockAsyncRuntime,
    ) -> (
        CaptureReadyBlockDeviceState,
        Option<BlockAsyncDriveGeneration>,
    ) {
        let mut prepared = PreparedBlockDevice::from_config_with_backing(config, None)
            .expect("profile-2 block should prepare");
        let generation = prepared
            .bind_async_runtime(runtime.clone())
            .expect("profile-2 Async binding should succeed");
        let (_, _, config_space, device) = prepared.into_parts();
        let handler = VirtioMmioRegisterHandler::with_device_config_and_activation(
            VIRTIO_BLOCK_DEVICE_ID,
            config_space.available_features(),
            &VIRTIO_BLOCK_QUEUE_SIZES,
            config_space,
            device,
        )
        .expect("profile-2 MMIO handler should build");
        if let Some(generation) = generation {
            runtime
                .stop_generations(&[generation])
                .expect("profile-2 Async admission should stop");
            runtime
                .drain_stopped_generation(generation, &mut guest_memory())
                .expect("profile-2 Async generation should drain");
        }
        let captured = handler
            .capture_block_device_state_at(config, Instant::now())
            .expect("profile-2 block state should capture");
        let region = MmioRegion::new(
            MmioRegionId::new(u64::from(index) + 1),
            GuestAddress::new(0xd000_0000 + u64::from(index) * VIRTIO_MMIO_DEVICE_WINDOW_SIZE),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("profile-2 MMIO region should validate");
        (
            CaptureReadyBlockDeviceState::new(
                config.clone(),
                StorageTransportState::Mmio(StorageMmioTransportState::new(
                    region,
                    GuestInterruptLine::new(index + 32)
                        .expect("profile-2 interrupt should validate"),
                    handler.transport_state(),
                )),
                StorageRetryState::None,
                captured,
            ),
            generation,
        )
    }

    #[test]
    fn live_mmio_sync_and_async_vector_converts_to_semantic_profile() {
        let root_file = TempFile::new("root.img", 4096);
        let data_file = TempFile::new("data.img", 8193);
        let root = config(
            "rootfs",
            root_file.path(),
            true,
            false,
            DriveCacheType::Unsafe,
            DriveIoEngine::Sync,
        );
        let data = config(
            "data_1",
            data_file.path(),
            false,
            false,
            DriveCacheType::Writeback,
            DriveIoEngine::Async,
        );
        let runtime = SharedBlockAsyncRuntime::new();
        let (root_state, root_generation) = capture_mmio(&root, 0, &runtime);
        assert_eq!(root_generation, None);
        let (data_state, data_generation) = capture_mmio(&data, 1, &runtime);
        let data_generation = data_generation.expect("Async drive should own a generation");

        let graph = SnapshotV2MultiBlockDeviceGraph::from_capture_ready_blocks(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &[root_state, data_state],
        )
        .expect("live config-ordered block vector should convert");
        assert_eq!(graph.root_key(), Some(SnapshotV2DeviceKey::block(0)));
        assert_eq!(graph.transport_kind(), SnapshotV2DeviceTransportKind::Mmio);
        assert_eq!(graph.records().len(), 2);
        assert_eq!(graph.records()[0].config().drive_id(), "rootfs");
        assert!(!graph.records()[0].config().is_read_only());
        assert_eq!(graph.records()[0].config().io_engine(), DriveIoEngine::Sync);
        assert_eq!(graph.records()[0].block().backing_bytes(), 4096);
        assert_eq!(graph.records()[1].config().drive_id(), "data_1");
        assert_eq!(
            graph.records()[1].config().io_engine(),
            DriveIoEngine::Async
        );
        assert_eq!(graph.records()[1].block().backing_bytes(), 8193);
        assert_eq!(
            SnapshotV2MultiBlockDeviceGraph::decode(
                NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &graph
                    .encode(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION)
                    .expect("converted graph should encode"),
            )
            .expect("converted graph should decode"),
            graph
        );

        runtime
            .unbind_quiesced(data_generation)
            .expect("captured Async generation should unbind");
        assert!(
            runtime
                .shutdown_if_idle()
                .expect("capture runtime should stop")
        );
    }

    #[test]
    fn conversion_rejects_wrong_version_and_empty_inventory() {
        assert_eq!(
            SnapshotV2MultiBlockDeviceGraph::from_capture_ready_blocks(
                SnapshotFormatVersion::new(2, 4, 0),
                &[],
            ),
            Err(SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedVersion)
        );
        assert_eq!(
            SnapshotV2MultiBlockDeviceGraph::from_capture_ready_blocks(
                NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &[],
            ),
            Err(SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedInventory)
        );

        let file = TempFile::new("preflight.img", 4096);
        let rootless = config(
            "data_0",
            file.path(),
            false,
            true,
            DriveCacheType::Unsafe,
            DriveIoEngine::Sync,
        );
        assert_eq!(
            SnapshotV2MultiBlockDeviceGraph::preflight_capture_configs(
                NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                std::slice::from_ref(&rootless),
            ),
            Ok(())
        );
        assert_eq!(
            SnapshotV2MultiBlockDeviceGraph::preflight_capture_configs(
                NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &[rootless.clone(), rootless],
            ),
            Err(SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidGraph)
        );

        let late_root = config(
            "rootfs",
            file.path(),
            true,
            true,
            DriveCacheType::Unsafe,
            DriveIoEngine::Sync,
        );
        let first_data = config(
            "data_1",
            file.path(),
            false,
            true,
            DriveCacheType::Unsafe,
            DriveIoEngine::Sync,
        );
        assert_eq!(
            SnapshotV2MultiBlockDeviceGraph::preflight_capture_configs(
                NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &[first_data, late_root],
            ),
            Err(SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidGraph)
        );
    }
}
