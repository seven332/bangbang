//! Live-state conversion boundary for the pure profile-3 artifact model.

use super::*;

use crate::block::DriveConfig;
use crate::pmem::{
    PmemConfig, PmemRateLimiterConfig, PmemTokenBucketConfig, VIRTIO_PMEM_DEVICE_ID,
    VirtioPmemRateLimiterState, VirtioPmemTokenBucketState,
};
use crate::snapshot_device_v2::{
    SnapshotV2DeviceGraphCaptureError, capture_mmio_common_for_device, capture_mmio_transport,
    capture_pci_common_for_device, capture_pci_transport_parts,
};
use crate::snapshot_device_v2_5::{
    NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    SnapshotV2MultiBlockDeviceGraphCaptureError, capture_multi_block_record,
    preflight_capture_config_refs,
};
use crate::storage_capture::{
    CaptureReadyBlockDeviceState, CaptureReadyPmemDeviceState, StorageTransportState,
};

impl SnapshotV2StorageDeviceGraph {
    /// Validates profile-3 public configuration before live source mutation.
    pub fn preflight_capture_configs(
        compatibility_version: SnapshotFormatVersion,
        drive_configs: &[DriveConfig],
        pmem_configs: &[PmemConfig],
    ) -> Result<(), SnapshotV2StorageDeviceGraphCaptureError> {
        let mut drive_refs = Vec::new();
        drive_refs
            .try_reserve_exact(drive_configs.len())
            .map_err(|_| SnapshotV2StorageDeviceGraphCaptureError::Allocation)?;
        drive_refs.extend(drive_configs.iter());
        let mut pmem_refs = Vec::new();
        pmem_refs
            .try_reserve_exact(pmem_configs.len())
            .map_err(|_| SnapshotV2StorageDeviceGraphCaptureError::Allocation)?;
        pmem_refs.extend(pmem_configs.iter());
        preflight_capture_config_refs_2_6(compatibility_version, &drive_refs, &pmem_refs)
    }

    /// Converts one complete config-ordered live storage inventory into profile 3.
    pub fn from_capture_ready_storage(
        compatibility_version: SnapshotFormatVersion,
        block_states: &[CaptureReadyBlockDeviceState],
        pmem_states: &[CaptureReadyPmemDeviceState],
    ) -> Result<Self, SnapshotV2StorageDeviceGraphCaptureError> {
        capture_storage_graph(compatibility_version, block_states, pmem_states)
    }
}

fn capture_storage_graph(
    compatibility_version: SnapshotFormatVersion,
    block_states: &[CaptureReadyBlockDeviceState],
    pmem_states: &[CaptureReadyPmemDeviceState],
) -> Result<SnapshotV2StorageDeviceGraph, SnapshotV2StorageDeviceGraphCaptureError> {
    let mut drive_refs = Vec::new();
    drive_refs
        .try_reserve_exact(block_states.len())
        .map_err(|_| SnapshotV2StorageDeviceGraphCaptureError::Allocation)?;
    drive_refs.extend(
        block_states
            .iter()
            .map(CaptureReadyBlockDeviceState::config),
    );
    let mut pmem_refs = Vec::new();
    pmem_refs
        .try_reserve_exact(pmem_states.len())
        .map_err(|_| SnapshotV2StorageDeviceGraphCaptureError::Allocation)?;
    pmem_refs.extend(pmem_states.iter().map(CaptureReadyPmemDeviceState::config));
    preflight_capture_config_refs_2_6(compatibility_version, &drive_refs, &pmem_refs)?;

    let mut block_records = Vec::new();
    block_records
        .try_reserve_exact(block_states.len())
        .map_err(|_| SnapshotV2StorageDeviceGraphCaptureError::Allocation)?;
    let mut pmem_records = Vec::new();
    pmem_records
        .try_reserve_exact(pmem_states.len())
        .map_err(|_| SnapshotV2StorageDeviceGraphCaptureError::Allocation)?;
    let mut transport_kind = None;

    for (index, state) in block_states.iter().enumerate() {
        let (record_transport, record) =
            capture_multi_block_record(index, state).map_err(map_block_capture_error)?;
        merge_transport_kind(&mut transport_kind, record_transport)?;
        block_records.push(record);
    }
    for (index, state) in pmem_states.iter().enumerate() {
        let (record_transport, record) = capture_pmem_record(index, state)?;
        merge_transport_kind(&mut transport_kind, record_transport)?;
        pmem_records.push(record);
    }

    let root_key = block_records
        .first()
        .filter(|record| record.is_root())
        .map(|record| record.key())
        .or_else(|| {
            pmem_records
                .first()
                .filter(|record| record.is_root())
                .map(SnapshotV2PmemDeviceRecord::key)
        });
    SnapshotV2StorageDeviceGraph::try_from_parts(
        root_key,
        transport_kind.ok_or(SnapshotV2StorageDeviceGraphCaptureError::UnsupportedInventory)?,
        block_records,
        pmem_records,
    )
    .map_err(|_| SnapshotV2StorageDeviceGraphCaptureError::InvalidGraph)
}

fn preflight_capture_config_refs_2_6(
    compatibility_version: SnapshotFormatVersion,
    drive_configs: &[&DriveConfig],
    pmem_configs: &[&PmemConfig],
) -> Result<(), SnapshotV2StorageDeviceGraphCaptureError> {
    if compatibility_version != NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION {
        return Err(SnapshotV2StorageDeviceGraphCaptureError::UnsupportedVersion);
    }
    let record_count = drive_configs
        .len()
        .checked_add(pmem_configs.len())
        .ok_or(SnapshotV2StorageDeviceGraphCaptureError::UnsupportedInventory)?;
    if record_count == 0 || record_count > usize::from(NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS) {
        return Err(SnapshotV2StorageDeviceGraphCaptureError::UnsupportedInventory);
    }
    if !drive_configs.is_empty() {
        preflight_capture_config_refs(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            drive_configs,
        )
        .map_err(map_block_capture_error)?;
    }

    for (index, config) in pmem_configs.iter().copied().enumerate() {
        validate_capture_string(
            config.id(),
            NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_PMEM_ID_BYTES,
        )?;
        if !config
            .id()
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
        {
            return Err(SnapshotV2StorageDeviceGraphCaptureError::InvalidString);
        }
        validate_capture_string(
            config.path_on_host(),
            NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_SELECTOR_BYTES,
        )?;
        if config.root_device() && index != 0 {
            return Err(SnapshotV2StorageDeviceGraphCaptureError::InvalidGraph);
        }
        if pmem_configs
            .iter()
            .copied()
            .take(index)
            .any(|candidate| candidate.id() == config.id())
        {
            return Err(SnapshotV2StorageDeviceGraphCaptureError::InvalidGraph);
        }
        validate_pmem_limiter_config(config.rate_limiter())?;
    }

    let block_root_count = drive_configs
        .iter()
        .filter(|config| config.is_root_device())
        .count();
    let pmem_root_count = pmem_configs
        .iter()
        .filter(|config| config.root_device())
        .count();
    if block_root_count + pmem_root_count > 1 {
        return Err(SnapshotV2StorageDeviceGraphCaptureError::InvalidGraph);
    }

    for (index, config) in drive_configs.iter().copied().enumerate() {
        let selector = config
            .path_on_host()
            .and_then(|path| path.to_str())
            .ok_or(SnapshotV2StorageDeviceGraphCaptureError::InvalidString)?;
        if drive_configs
            .iter()
            .copied()
            .skip(index + 1)
            .any(|candidate| {
                candidate.path_on_host().and_then(|path| path.to_str()) == Some(selector)
            })
            || pmem_configs
                .iter()
                .copied()
                .any(|candidate| candidate.path_on_host() == selector)
        {
            return Err(SnapshotV2StorageDeviceGraphCaptureError::InvalidGraph);
        }
    }
    for (index, config) in pmem_configs.iter().copied().enumerate() {
        if pmem_configs
            .iter()
            .copied()
            .skip(index + 1)
            .any(|candidate| candidate.path_on_host() == config.path_on_host())
        {
            return Err(SnapshotV2StorageDeviceGraphCaptureError::InvalidGraph);
        }
    }
    Ok(())
}

fn capture_pmem_record(
    index: usize,
    state: &CaptureReadyPmemDeviceState,
) -> Result<
    (SnapshotV2DeviceTransportKind, SnapshotV2PmemDeviceRecord),
    SnapshotV2StorageDeviceGraphCaptureError,
> {
    let config = capture_pmem_config(state.config())?;
    let pmem = capture_pmem_state(state, &config)?;
    let expected_features = pmem.config_space.available_features();
    let (transport_kind, virtio, transport) = match state.transport() {
        StorageTransportState::Mmio(mmio) => (
            SnapshotV2DeviceTransportKind::Mmio,
            capture_mmio_common_for_device(
                mmio.transport(),
                VIRTIO_PMEM_DEVICE_ID,
                expected_features,
            )
            .map_err(map_common_capture_error)?,
            SnapshotV2DeviceTransport::Mmio(
                capture_mmio_transport(mmio.region(), mmio.interrupt_line(), mmio.transport())
                    .map_err(map_common_capture_error)?,
            ),
        ),
        StorageTransportState::Pci(pci) => (
            SnapshotV2DeviceTransportKind::Pci,
            capture_pci_common_for_device(
                pci.transport(),
                VIRTIO_PMEM_DEVICE_ID,
                expected_features,
            )
            .map_err(map_common_capture_error)?,
            SnapshotV2DeviceTransport::Pci(
                capture_pci_transport_parts(
                    pci.origin(),
                    pci.sbdf(),
                    pci.bar_range(),
                    pci.transport(),
                )
                .map_err(map_common_capture_error)?,
            ),
        ),
    };
    let instance = u32::try_from(index)
        .map_err(|_| SnapshotV2StorageDeviceGraphCaptureError::UnsupportedInventory)?;
    Ok((
        transport_kind,
        SnapshotV2PmemDeviceRecord {
            key: SnapshotV2DeviceKey::pmem(instance),
            config,
            pmem,
            virtio,
            transport,
        },
    ))
}

fn capture_pmem_config(
    config: &PmemConfig,
) -> Result<SnapshotV2PmemConfig, SnapshotV2StorageDeviceGraphCaptureError> {
    validate_capture_string(
        config.id(),
        NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_PMEM_ID_BYTES,
    )?;
    if !config
        .id()
        .chars()
        .all(|character| character == '_' || character.is_alphanumeric())
    {
        return Err(SnapshotV2StorageDeviceGraphCaptureError::InvalidString);
    }
    validate_capture_string(
        config.path_on_host(),
        NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_SELECTOR_BYTES,
    )?;
    validate_pmem_limiter_config(config.rate_limiter())?;
    Ok(SnapshotV2PmemConfig {
        pmem_id: clone_capture_string(
            config.id(),
            NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_PMEM_ID_BYTES,
        )?,
        is_root: config.root_device(),
        is_read_only: config.read_only(),
        rate_limiter: config.rate_limiter(),
        selector: clone_capture_string(
            config.path_on_host(),
            NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_SELECTOR_BYTES,
        )?,
    })
}

fn capture_pmem_state(
    state: &CaptureReadyPmemDeviceState,
    config: &SnapshotV2PmemConfig,
) -> Result<SnapshotV2PmemState, SnapshotV2StorageDeviceGraphCaptureError> {
    let backing = state.backing();
    let mapping = state.mapping();
    let device = state.device();
    let guest_range = state.guest_range();
    let expected_mapped = align_pmem_length(backing.len())
        .ok_or(SnapshotV2StorageDeviceGraphCaptureError::InconsistentPmemState)?;
    let expected_config_space =
        VirtioPmemConfigSpace::new(guest_range.start().raw_value(), guest_range.size());
    if !backing.is_regular_file()
        || backing.is_empty()
        || mapping.file_len() != backing.len()
        || mapping.mapped_len() != expected_mapped
        || mapping.mapped_len() != guest_range.size()
        || mapping.is_read_only() != config.is_read_only
        || device.file_len() != backing.len()
        || device.config_space() != expected_config_space
    {
        return Err(SnapshotV2StorageDeviceGraphCaptureError::InconsistentPmemState);
    }
    let limiter = capture_pmem_limiter_state(config.rate_limiter, device.rate_limiter())?;
    let pmem = SnapshotV2PmemState {
        file_bytes: backing.len(),
        mapped_bytes: mapping.mapped_len(),
        guest_range,
        config_space: device.config_space(),
        active_queue: device.active_queue(),
        limiter,
        pending_rate_limited_queue: device.pending_rate_limited_queue(),
        retry: state.retry(),
    };
    validate_pmem_state_local(&pmem)
        .and_then(|()| validate_pmem_limiter_relationship(config.rate_limiter, pmem.limiter))
        .map_err(|_| SnapshotV2StorageDeviceGraphCaptureError::InconsistentPmemState)?;
    Ok(pmem)
}

fn capture_pmem_limiter_state(
    config: Option<PmemRateLimiterConfig>,
    state: VirtioPmemRateLimiterState,
) -> Result<SnapshotV2PmemLimiterState, SnapshotV2StorageDeviceGraphCaptureError> {
    Ok(SnapshotV2PmemLimiterState::new(
        capture_pmem_bucket_state(
            config.and_then(PmemRateLimiterConfig::bandwidth),
            state.bandwidth(),
        )?,
        capture_pmem_bucket_state(config.and_then(PmemRateLimiterConfig::ops), state.ops())?,
    ))
}

fn capture_pmem_bucket_state(
    config: Option<PmemTokenBucketConfig>,
    state: Option<VirtioPmemTokenBucketState>,
) -> Result<Option<SnapshotV2PmemBucketState>, SnapshotV2StorageDeviceGraphCaptureError> {
    match (config, state) {
        (Some(config), Some(state))
            if pmem_token_bucket_is_enabled(config)
                && state.config() == config
                && state.budget() <= config.size()
                && state.remaining_burst() <= config.one_time_burst().unwrap_or(0) =>
        {
            Ok(Some(SnapshotV2PmemBucketState::new(
                state.budget(),
                state.remaining_burst(),
                state.age_nanos(),
            )))
        }
        (None, None) => Ok(None),
        _ => Err(SnapshotV2StorageDeviceGraphCaptureError::InconsistentPmemState),
    }
}

fn validate_pmem_limiter_config(
    config: Option<PmemRateLimiterConfig>,
) -> Result<(), SnapshotV2StorageDeviceGraphCaptureError> {
    let Some(config) = config else {
        return Ok(());
    };
    if !config.is_configured()
        || [config.bandwidth(), config.ops()]
            .into_iter()
            .flatten()
            .any(|bucket| !pmem_token_bucket_is_enabled(bucket))
    {
        return Err(SnapshotV2StorageDeviceGraphCaptureError::UnsupportedConfiguration);
    }
    Ok(())
}

fn merge_transport_kind(
    expected: &mut Option<SnapshotV2DeviceTransportKind>,
    candidate: SnapshotV2DeviceTransportKind,
) -> Result<(), SnapshotV2StorageDeviceGraphCaptureError> {
    match expected {
        None => *expected = Some(candidate),
        Some(current) if *current == candidate => {}
        Some(_) => return Err(SnapshotV2StorageDeviceGraphCaptureError::UnsupportedInventory),
    }
    Ok(())
}

fn clone_capture_string(
    value: &str,
    maximum: usize,
) -> Result<String, SnapshotV2StorageDeviceGraphCaptureError> {
    validate_capture_string(value, maximum)?;
    let mut owned = String::new();
    owned
        .try_reserve_exact(value.len())
        .map_err(|_| SnapshotV2StorageDeviceGraphCaptureError::Allocation)?;
    owned.push_str(value);
    Ok(owned)
}

fn validate_capture_string(
    value: &str,
    maximum: usize,
) -> Result<(), SnapshotV2StorageDeviceGraphCaptureError> {
    if value.is_empty() || value.len() > maximum {
        Err(SnapshotV2StorageDeviceGraphCaptureError::InvalidString)
    } else {
        Ok(())
    }
}

const fn map_block_capture_error(
    error: SnapshotV2MultiBlockDeviceGraphCaptureError,
) -> SnapshotV2StorageDeviceGraphCaptureError {
    match error {
        SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedVersion => {
            SnapshotV2StorageDeviceGraphCaptureError::UnsupportedVersion
        }
        SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedInventory => {
            SnapshotV2StorageDeviceGraphCaptureError::UnsupportedInventory
        }
        SnapshotV2MultiBlockDeviceGraphCaptureError::UnsupportedConfiguration => {
            SnapshotV2StorageDeviceGraphCaptureError::UnsupportedConfiguration
        }
        SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidString => {
            SnapshotV2StorageDeviceGraphCaptureError::InvalidString
        }
        SnapshotV2MultiBlockDeviceGraphCaptureError::InconsistentBlockState => {
            SnapshotV2StorageDeviceGraphCaptureError::InconsistentBlockState
        }
        SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidVirtioState => {
            SnapshotV2StorageDeviceGraphCaptureError::InvalidVirtioState
        }
        SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidMmioState => {
            SnapshotV2StorageDeviceGraphCaptureError::InvalidMmioState
        }
        SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidPciState => {
            SnapshotV2StorageDeviceGraphCaptureError::InvalidPciState
        }
        SnapshotV2MultiBlockDeviceGraphCaptureError::InvalidGraph => {
            SnapshotV2StorageDeviceGraphCaptureError::InvalidGraph
        }
        SnapshotV2MultiBlockDeviceGraphCaptureError::Allocation => {
            SnapshotV2StorageDeviceGraphCaptureError::Allocation
        }
    }
}

const fn map_common_capture_error(
    error: SnapshotV2DeviceGraphCaptureError,
) -> SnapshotV2StorageDeviceGraphCaptureError {
    match error {
        SnapshotV2DeviceGraphCaptureError::UnsupportedVersion => {
            SnapshotV2StorageDeviceGraphCaptureError::UnsupportedVersion
        }
        SnapshotV2DeviceGraphCaptureError::UnsupportedConfiguration => {
            SnapshotV2StorageDeviceGraphCaptureError::UnsupportedConfiguration
        }
        SnapshotV2DeviceGraphCaptureError::InvalidString => {
            SnapshotV2StorageDeviceGraphCaptureError::InvalidString
        }
        SnapshotV2DeviceGraphCaptureError::InconsistentBlockState => {
            SnapshotV2StorageDeviceGraphCaptureError::InconsistentPmemState
        }
        SnapshotV2DeviceGraphCaptureError::InvalidVirtioState => {
            SnapshotV2StorageDeviceGraphCaptureError::InvalidVirtioState
        }
        SnapshotV2DeviceGraphCaptureError::InvalidMmioState => {
            SnapshotV2StorageDeviceGraphCaptureError::InvalidMmioState
        }
        SnapshotV2DeviceGraphCaptureError::InvalidPciState => {
            SnapshotV2StorageDeviceGraphCaptureError::InvalidPciState
        }
        SnapshotV2DeviceGraphCaptureError::Allocation => {
            SnapshotV2StorageDeviceGraphCaptureError::Allocation
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

    use crate::block::{
        DriveCacheType, DriveConfigInput, DriveIoEngine, PreparedBlockDevice,
        VIRTIO_BLOCK_DEVICE_ID, VIRTIO_BLOCK_QUEUE_SIZES,
    };
    use crate::interrupt::GuestInterruptLine;
    use crate::memory::{GuestAddress, GuestMemoryRange};
    use crate::mmio::{MmioRegion, MmioRegionId};
    use crate::pmem::{
        PmemConfigInput, PmemFileBacking, PreparedPmemDevice, VIRTIO_PMEM_ALIGNMENT,
        VIRTIO_PMEM_QUEUE_SIZES, VirtioPmemDevice,
    };
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
                "bangbang-snapshot-v2-6-capture-{name}-{}-{sequence}",
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

    fn block_config(path: &Path, is_root: bool) -> DriveConfig {
        DriveConfigInput::new("rootfs", "rootfs", path, is_root)
            .with_is_read_only(false)
            .with_cache_type(DriveCacheType::Unsafe)
            .with_io_engine(DriveIoEngine::Sync)
            .validate()
            .expect("profile-3 block config should validate")
    }

    fn pmem_config(
        id: &str,
        path: &Path,
        is_root: bool,
        is_read_only: bool,
        rate_limiter: Option<PmemRateLimiterConfig>,
    ) -> PmemConfig {
        let mut input = PmemConfigInput::new(id, path.to_string_lossy().into_owned())
            .with_root_device(is_root)
            .with_read_only(is_read_only);
        if let Some(rate_limiter) = rate_limiter {
            input = input.with_rate_limiter(rate_limiter);
        }
        PmemConfig::try_from(input).expect("profile-3 pmem config should validate")
    }

    fn mmio_region(index: u32) -> MmioRegion {
        MmioRegion::new(
            MmioRegionId::new(u64::from(index) + 1),
            GuestAddress::new(0xd000_0000 + u64::from(index) * VIRTIO_MMIO_DEVICE_WINDOW_SIZE),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("profile-3 MMIO region should validate")
    }

    fn capture_mmio_block(config: &DriveConfig, index: u32) -> CaptureReadyBlockDeviceState {
        let prepared = PreparedBlockDevice::from_config_with_backing(config, None)
            .expect("profile-3 block should prepare");
        let (_, _, config_space, device) = prepared.into_parts();
        let handler = VirtioMmioRegisterHandler::with_device_config_and_activation(
            VIRTIO_BLOCK_DEVICE_ID,
            config_space.available_features(),
            &VIRTIO_BLOCK_QUEUE_SIZES,
            config_space,
            device,
        )
        .expect("profile-3 block MMIO handler should build");
        let captured = handler
            .capture_block_device_state_at(config, Instant::now())
            .expect("profile-3 block state should capture");
        CaptureReadyBlockDeviceState::new(
            config.clone(),
            StorageTransportState::Mmio(StorageMmioTransportState::new(
                mmio_region(index),
                GuestInterruptLine::new(index + 32)
                    .expect("profile-3 block interrupt should validate"),
                handler.transport_state(),
            )),
            StorageRetryState::None,
            captured,
        )
    }

    fn capture_mmio_pmem(config: &PmemConfig, index: u32) -> CaptureReadyPmemDeviceState {
        let prepared = PreparedPmemDevice::from_config_with_backing_and_reserved_ranges(
            config,
            PmemFileBacking::open(config).expect("profile-3 pmem backing should open"),
            &[],
        )
        .expect("profile-3 pmem should prepare");
        let (_, backing, mapping, guest_range, config_space, rate_limiter) = prepared.into_parts();
        let handler = VirtioMmioRegisterHandler::with_device_config_and_activation(
            VIRTIO_PMEM_DEVICE_ID,
            config_space.available_features(),
            &VIRTIO_PMEM_QUEUE_SIZES,
            config_space,
            VirtioPmemDevice::with_rate_limiter(mapping.file_len(), rate_limiter),
        )
        .expect("profile-3 pmem MMIO handler should build");
        let captured = handler
            .capture_pmem_device_state_at(mapping.file_len(), rate_limiter, Instant::now())
            .expect("profile-3 pmem state should capture");
        CaptureReadyPmemDeviceState::new(
            config.clone(),
            guest_range,
            backing
                .capture_identity()
                .expect("profile-3 pmem identity should capture"),
            mapping.capture_identity(),
            StorageTransportState::Mmio(StorageMmioTransportState::new(
                mmio_region(index),
                GuestInterruptLine::new(index + 32)
                    .expect("profile-3 pmem interrupt should validate"),
                handler.transport_state(),
            )),
            StorageRetryState::None,
            captured,
        )
    }

    fn limiter_config() -> PmemRateLimiterConfig {
        PmemRateLimiterConfig::new(
            Some(PmemTokenBucketConfig::new(1_000_000, Some(4096), 10)),
            Some(PmemTokenBucketConfig::new(1_000, None, 20)),
        )
    }

    #[test]
    fn live_mixed_mmio_inventory_converts_to_exact_profile_3() {
        let block_file = TempFile::new("mixed-root.img", 4096);
        let pmem_file = TempFile::new("mixed-pmem.img", VIRTIO_PMEM_ALIGNMENT + 513);
        let block = block_config(block_file.path(), true);
        let pmem = pmem_config(
            "pmem_0",
            pmem_file.path(),
            false,
            false,
            Some(limiter_config()),
        );
        let block_state = capture_mmio_block(&block, 0);
        let pmem_state = capture_mmio_pmem(&pmem, 1);

        SnapshotV2StorageDeviceGraph::preflight_capture_configs(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            std::slice::from_ref(&block),
            std::slice::from_ref(&pmem),
        )
        .expect("mixed profile-3 config should preflight");
        let graph = SnapshotV2StorageDeviceGraph::from_capture_ready_storage(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &[block_state],
            &[pmem_state],
        )
        .expect("mixed live inventory should convert");

        assert_eq!(graph.root_key(), Some(SnapshotV2DeviceKey::block(0)));
        assert_eq!(graph.transport_kind(), SnapshotV2DeviceTransportKind::Mmio);
        assert_eq!(graph.block_records().len(), 1);
        assert_eq!(graph.pmem_records().len(), 1);
        let record = &graph.pmem_records()[0];
        assert_eq!(record.config().pmem_id(), "pmem_0");
        assert!(!record.config().is_read_only());
        assert_eq!(record.pmem().file_bytes(), VIRTIO_PMEM_ALIGNMENT + 513);
        assert_eq!(record.pmem().mapped_bytes(), VIRTIO_PMEM_ALIGNMENT * 2);
        assert_eq!(
            record.pmem().guest_range().size(),
            record.pmem().mapped_bytes()
        );
        assert_eq!(
            record.pmem().config_space().size(),
            record.pmem().mapped_bytes()
        );
        assert!(record.pmem().limiter().bandwidth().is_some());
        assert!(record.pmem().limiter().ops().is_some());
        assert_eq!(record.pmem().retry(), StorageRetryState::None);
        assert!(matches!(
            record.transport(),
            SnapshotV2DeviceTransport::Mmio(_)
        ));

        let bytes = graph
            .encode(NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION)
            .expect("captured profile-3 graph should encode");
        assert_eq!(
            SnapshotV2StorageDeviceGraph::decode(
                NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &bytes,
            )
            .expect("captured profile-3 graph should decode"),
            graph
        );
    }

    #[test]
    fn live_read_only_pmem_root_converts_without_blocks() {
        let file = TempFile::new("pmem-root.img", 4097);
        let config = pmem_config("pmem_root", file.path(), true, true, None);
        let state = capture_mmio_pmem(&config, 0);

        let graph = SnapshotV2StorageDeviceGraph::from_capture_ready_storage(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &[],
            &[state],
        )
        .expect("pmem-only live inventory should convert");

        assert_eq!(graph.root_key(), Some(SnapshotV2DeviceKey::pmem(0)));
        assert!(graph.block_records().is_empty());
        assert_eq!(graph.pmem_records().len(), 1);
        assert!(graph.pmem_records()[0].config().is_read_only());
        assert_eq!(graph.pmem_records()[0].pmem().file_bytes(), 4097);
        assert_eq!(
            graph.pmem_records()[0].pmem().mapped_bytes(),
            VIRTIO_PMEM_ALIGNMENT
        );
    }

    #[test]
    fn profile_3_preflight_rejects_wrong_version_empty_cross_roots_and_selectors() {
        assert_eq!(
            SnapshotV2StorageDeviceGraph::preflight_capture_configs(
                NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &[],
                &[],
            ),
            Err(SnapshotV2StorageDeviceGraphCaptureError::UnsupportedVersion)
        );
        assert_eq!(
            SnapshotV2StorageDeviceGraph::preflight_capture_configs(
                NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &[],
                &[],
            ),
            Err(SnapshotV2StorageDeviceGraphCaptureError::UnsupportedInventory)
        );

        let block_file = TempFile::new("preflight-block.img", 4096);
        let pmem_file = TempFile::new("preflight-pmem.img", 4096);
        let block_root = block_config(block_file.path(), true);
        let pmem_root = pmem_config("pmem_root", pmem_file.path(), true, false, None);
        assert_eq!(
            SnapshotV2StorageDeviceGraph::preflight_capture_configs(
                NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &[block_root],
                &[pmem_root],
            ),
            Err(SnapshotV2StorageDeviceGraphCaptureError::InvalidGraph)
        );

        let block = block_config(block_file.path(), false);
        let same_selector = pmem_config("pmem_data", block_file.path(), false, false, None);
        assert_eq!(
            SnapshotV2StorageDeviceGraph::preflight_capture_configs(
                NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &[block],
                &[same_selector],
            ),
            Err(SnapshotV2StorageDeviceGraphCaptureError::InvalidGraph)
        );

        let first = pmem_config("pmem_first", block_file.path(), false, false, None);
        let late_root = pmem_config("pmem_root", pmem_file.path(), true, false, None);
        assert_eq!(
            SnapshotV2StorageDeviceGraph::preflight_capture_configs(
                NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &[],
                &[first, late_root],
            ),
            Err(SnapshotV2StorageDeviceGraphCaptureError::InvalidGraph)
        );
    }

    #[test]
    fn profile_3_conversion_rejects_mapping_access_mismatch() {
        let file = TempFile::new("mapping-access.img", 4096);
        let writable = pmem_config("pmem_0", file.path(), false, false, None);
        let state = capture_mmio_pmem(&writable, 0);
        let read_only = pmem_config("pmem_0", file.path(), false, true, None);
        let mismatched = CaptureReadyPmemDeviceState::new(
            read_only,
            state.guest_range(),
            state.backing(),
            state.mapping().clone(),
            state.transport().clone(),
            state.retry(),
            state.device().clone(),
        );

        assert_eq!(
            SnapshotV2StorageDeviceGraph::from_capture_ready_storage(
                NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &[],
                &[mismatched],
            ),
            Err(SnapshotV2StorageDeviceGraphCaptureError::InconsistentPmemState)
        );
    }

    #[test]
    fn pmem_guest_range_is_aligned_for_capture_fixture() {
        let file = TempFile::new("range-alignment.img", 4096);
        let config = pmem_config("pmem_0", file.path(), false, false, None);
        let state = capture_mmio_pmem(&config, 0);
        let range = state.guest_range();

        assert_eq!(range.start().raw_value() % VIRTIO_PMEM_ALIGNMENT, 0);
        assert_eq!(range.size(), VIRTIO_PMEM_ALIGNMENT);
        assert_eq!(
            GuestMemoryRange::new(range.start(), range.size())
                .expect("captured range should remain valid"),
            range
        );
    }
}
