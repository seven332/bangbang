//! Canonical detached native-v2 2.5 multi-block device-graph profile.

use std::fmt;

use crate::block::{
    DriveCacheType, DriveConfigInput, DriveConfigs, DriveIoEngine, DriveRateLimiterConfig,
    DriveTokenBucketConfig, VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE, VIRTIO_BLOCK_QUEUE_SIZE,
    VirtioBlockConfigSpace,
};
use crate::memory::{GuestMemoryError, GuestMemoryRange};
use crate::pci::{
    PCI_BAR64_SIZE, PCI_BAR64_START, PCI_BUS_ZERO, PCI_FIRST_ENDPOINT_DEVICE, PCI_FUNCTION_ZERO,
    PCI_LAST_ENDPOINT_DEVICE, PCI_SEGMENT_ZERO, PciBarAddressSpace, PciBarPrefetchable,
};
use crate::snapshot_device_v2::{
    NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, NATIVE_V2_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES,
    NATIVE_V2_DEVICE_GRAPH_MAX_PARTUUID_BYTES, NATIVE_V2_DEVICE_GRAPH_MAX_SELECTOR_BYTES,
    SnapshotV2BlockLimiterState, SnapshotV2BlockState, SnapshotV2DeviceKey,
    SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind, SnapshotV2InterruptIntent,
    SnapshotV2MmioDeviceState, SnapshotV2PciDeviceState, SnapshotV2PciMsixState,
    SnapshotV2VirtioQueueState, SnapshotV2VirtioState,
};
use crate::snapshot_format::SnapshotFormatVersion;
use crate::snapshot_restore::{
    MAX_SNAPSHOT_RESTORE_PUBLIC_ID_BYTES, MAX_SNAPSHOT_RESTORE_RESOURCES,
};
use crate::storage_capture::{StorageDeviceOrigin, StorageRetryState};
use crate::virtio::{
    VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DEVICE_NEEDS_RESET,
    VIRTIO_DEVICE_STATUS_DRIVER, VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FAILED,
    VIRTIO_DEVICE_STATUS_FEATURES_OK, VIRTIO_DEVICE_STATUS_INIT,
};
use crate::virtio_mmio::{VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VIRTIO_MMIO_VERSION_1_FEATURE};
use crate::virtio_pci::{
    VIRTIO_PCI_CAPABILITY_BAR_INDEX, VIRTIO_PCI_CAPABILITY_BAR_SIZE, VIRTIO_PCI_MAX_MSIX_VECTORS,
    VIRTIO_PCI_NO_VECTOR, VirtioPciEndpointPhase,
};

mod capture;
mod codec;

#[cfg(test)]
mod tests;

/// Exact compatibility context of the detached multi-block graph profile.
pub const NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION: SnapshotFormatVersion =
    SnapshotFormatVersion::new(2, 5, 0);

/// Maximum encoded size of one native-v2 2.5 multi-block graph.
pub const NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_BYTES: usize = 512 * 1024;

/// Maximum block-record count admitted by profile 2.
pub const NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_RECORDS: u16 = 64;

/// Maximum UTF-8 byte length of one public drive identifier.
pub const NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES: usize =
    NATIVE_V2_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES;

/// Maximum UTF-8 byte length of one optional partition identifier.
pub const NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_PARTUUID_BYTES: usize =
    NATIVE_V2_DEVICE_GRAPH_MAX_PARTUUID_BYTES;

/// Maximum UTF-8 byte length of one inert logical backing selector.
pub const NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_SELECTOR_BYTES: usize =
    NATIVE_V2_DEVICE_GRAPH_MAX_SELECTOR_BYTES;

/// Fixed profile-2 payload header size.
pub const NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_HEADER_BYTES: usize = 64;

/// Fixed encoded size of one profile-2 record-directory entry.
pub const NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_RECORD_ENTRY_BYTES: usize = 32;

/// Fixed encoded size of one profile-2 section-directory entry.
pub const NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_SECTION_ENTRY_BYTES: usize = 32;

const CONFIG_FIXED_BYTES: usize = 80;
const BLOCK_SECTION_BYTES: usize = 112;
const COMMON_FIXED_BYTES: usize = 32;
const COMMON_QUEUE_BYTES: usize = 32;
const MMIO_SECTION_BYTES: usize = 48;
const PCI_SECTION_BYTES: usize = 144;
const SECTION_COUNT_PER_RECORD: usize = 4;
const ALIGNMENT: usize = 8;
const DEVICE_KIND_BLOCK: u32 = 1;
const REDACTED: &str = "<redacted>";

const MAX_CONFIG_BYTES: usize = align_const(
    CONFIG_FIXED_BYTES
        + NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES
        + NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_PARTUUID_BYTES
        + NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_SELECTOR_BYTES,
);
const MAX_COMMON_BYTES: usize = align_const(COMMON_FIXED_BYTES + COMMON_QUEUE_BYTES + 2 + 2 * 4);
const DIRECTORY_BYTES_PER_RECORD: usize = NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_RECORD_ENTRY_BYTES
    + SECTION_COUNT_PER_RECORD * NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_SECTION_ENTRY_BYTES;

/// Exact maximum byte count of every field admitted by the final profile-2 schema.
pub const NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_WORST_CASE_BYTES: usize =
    NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_HEADER_BYTES
        + NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_RECORDS as usize
            * (DIRECTORY_BYTES_PER_RECORD
                + MAX_CONFIG_BYTES
                + BLOCK_SECTION_BYTES
                + MAX_COMMON_BYTES
                + PCI_SECTION_BYTES);

const fn align_const(value: usize) -> usize {
    (value + (ALIGNMENT - 1)) & !(ALIGNMENT - 1)
}

const _: () = assert!(
    NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION.major()
        == NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION.major()
);
const _: () = assert!(
    NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION.minor()
        == NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION.minor() + 1
);
const _: () = assert!(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION.patch() == 0);
const _: () = assert!(
    NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_RECORDS as usize <= MAX_SNAPSHOT_RESTORE_RESOURCES
);
const _: () = assert!(
    NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES <= MAX_SNAPSHOT_RESTORE_PUBLIC_ID_BYTES
);
const _: () =
    assert!(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_RECORDS as usize * 4 <= u16::MAX as usize);
const _: () = assert!(
    NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_WORST_CASE_BYTES
        <= NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_BYTES
);

macro_rules! redacted_debug {
    ($type:ty, $name:literal) => {
        impl fmt::Debug for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct($name)
                    .field("state", &REDACTED)
                    .finish()
            }
        }
    };
}

/// Stable public configuration of one profile-2 regular-file block record.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2MultiBlockConfig {
    drive_id: String,
    partuuid: Option<String>,
    is_root: bool,
    is_read_only: bool,
    cache_type: DriveCacheType,
    io_engine: DriveIoEngine,
    rate_limiter: Option<DriveRateLimiterConfig>,
    selector: String,
}

impl SnapshotV2MultiBlockConfig {
    /// Returns the stable public drive identifier.
    pub fn drive_id(&self) -> &str {
        &self.drive_id
    }

    /// Returns the optional stable partition identifier.
    pub fn partuuid(&self) -> Option<&str> {
        self.partuuid.as_deref()
    }

    /// Returns whether this record is the boot root.
    pub const fn is_root(&self) -> bool {
        self.is_root
    }

    /// Returns whether the backing is read-only.
    pub const fn is_read_only(&self) -> bool {
        self.is_read_only
    }

    /// Returns the public cache policy.
    pub const fn cache_type(&self) -> DriveCacheType {
        self.cache_type
    }

    /// Returns the semantic file-engine choice.
    pub const fn io_engine(&self) -> DriveIoEngine {
        self.io_engine
    }

    /// Returns the public rate-limiter configuration.
    pub const fn rate_limiter(&self) -> Option<DriveRateLimiterConfig> {
        self.rate_limiter
    }

    /// Returns the inert logical backing selector.
    pub fn selector(&self) -> &str {
        &self.selector
    }

    /// Returns the closed profile backing-kind policy.
    pub const fn is_regular_file(&self) -> bool {
        true
    }
}

redacted_debug!(SnapshotV2MultiBlockConfig, "SnapshotV2MultiBlockConfig");

/// Exact byte geometry plus block-local continuation for one profile-2 record.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2MultiBlockState {
    backing_bytes: u64,
    continuation: SnapshotV2BlockState,
}

impl SnapshotV2MultiBlockState {
    /// Returns the exact external regular-file byte length.
    pub const fn backing_bytes(&self) -> u64 {
        self.backing_bytes
    }

    /// Returns the guest-visible block-local continuation.
    pub const fn continuation(&self) -> &SnapshotV2BlockState {
        &self.continuation
    }
}

redacted_debug!(SnapshotV2MultiBlockState, "SnapshotV2MultiBlockState");

/// One canonical profile-2 regular-file block record.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2MultiBlockDeviceRecord {
    key: SnapshotV2DeviceKey,
    config: SnapshotV2MultiBlockConfig,
    block: SnapshotV2MultiBlockState,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
}

impl SnapshotV2MultiBlockDeviceRecord {
    /// Returns the stable typed record key.
    pub const fn key(&self) -> SnapshotV2DeviceKey {
        self.key
    }

    /// Returns the stable regular-file configuration.
    pub const fn config(&self) -> &SnapshotV2MultiBlockConfig {
        &self.config
    }

    /// Returns exact byte geometry and block-local continuation.
    pub const fn block(&self) -> &SnapshotV2MultiBlockState {
        &self.block
    }

    /// Returns common virtio continuation state.
    pub const fn virtio(&self) -> &SnapshotV2VirtioState {
        &self.virtio
    }

    /// Returns exact tagged transport state.
    pub const fn transport(&self) -> &SnapshotV2DeviceTransport {
        &self.transport
    }

    /// Returns the root role cross-checked against the graph header.
    pub const fn is_root(&self) -> bool {
        self.config.is_root
    }
}

redacted_debug!(
    SnapshotV2MultiBlockDeviceRecord,
    "SnapshotV2MultiBlockDeviceRecord"
);

/// Fully validated detached native-v2 2.5 multi-block graph.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2MultiBlockDeviceGraph {
    root_key: Option<SnapshotV2DeviceKey>,
    transport_kind: SnapshotV2DeviceTransportKind,
    records: Vec<SnapshotV2MultiBlockDeviceRecord>,
}

impl SnapshotV2MultiBlockDeviceGraph {
    /// Returns the exact compatibility context of this graph profile.
    pub const fn compatibility_version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION
    }

    /// Returns the optional root selector.
    pub const fn root_key(&self) -> Option<SnapshotV2DeviceKey> {
        self.root_key
    }

    /// Returns the graph-wide transport kind.
    pub const fn transport_kind(&self) -> SnapshotV2DeviceTransportKind {
        self.transport_kind
    }

    /// Returns records in canonical configuration order.
    pub fn records(&self) -> &[SnapshotV2MultiBlockDeviceRecord] {
        &self.records
    }

    /// Builds the exact controller projection without mutating a live
    /// controller or consuming any restore authority.
    pub fn project_drive_configs(
        &self,
    ) -> Result<DriveConfigs, SnapshotV2MultiBlockDriveConfigsError> {
        let mut configs = DriveConfigs::new();
        for record in &self.records {
            let config = record.config();
            let mut input = DriveConfigInput::new(
                config.drive_id(),
                config.drive_id(),
                config.selector(),
                config.is_root(),
            )
            .with_is_read_only(config.is_read_only())
            .with_cache_type(config.cache_type())
            .with_io_engine(config.io_engine());
            if let Some(partuuid) = config.partuuid() {
                input = input.with_partuuid(partuuid);
            }
            if let Some(rate_limiter) = config.rate_limiter() {
                input = input.with_rate_limiter(rate_limiter);
            }
            configs
                .insert(input)
                .map_err(|_| SnapshotV2MultiBlockDriveConfigsError)?;
        }
        if configs.as_slice().len() != self.records.len()
            || configs
                .as_slice()
                .iter()
                .zip(&self.records)
                .any(|(config, record)| {
                    config.drive_id() != record.config().drive_id()
                        || config.is_root_device() != record.is_root()
                        || config.is_read_only() != Some(record.config().is_read_only())
                        || config.partuuid() != record.config().partuuid()
                        || config.cache_type() != record.config().cache_type()
                        || config.io_engine() != Some(record.config().io_engine())
                        || config.rate_limiter() != record.config().rate_limiter()
                        || config.path_on_host().and_then(|path| path.to_str())
                            != Some(record.config().selector())
                })
        {
            return Err(SnapshotV2MultiBlockDriveConfigsError);
        }
        Ok(configs)
    }

    /// Consumes a trusted record vector only after complete graph validation.
    pub(crate) fn try_from_parts(
        root_key: Option<SnapshotV2DeviceKey>,
        transport_kind: SnapshotV2DeviceTransportKind,
        records: Vec<SnapshotV2MultiBlockDeviceRecord>,
    ) -> Result<Self, SnapshotV2MultiBlockDeviceGraphBuildError> {
        let graph = Self {
            root_key,
            transport_kind,
            records,
        };
        validate_graph(&graph)
            .map(|()| graph)
            .map_err(|_| SnapshotV2MultiBlockDeviceGraphBuildError::InvalidGraph)
    }

    /// Encodes this graph under the exact supplied compatibility context.
    pub fn encode(
        &self,
        compatibility_version: SnapshotFormatVersion,
    ) -> Result<Vec<u8>, SnapshotV2MultiBlockDeviceGraphEncodeError> {
        codec::encode(compatibility_version, self)
    }

    /// Decodes and validates one exact profile-2 payload.
    pub fn decode(
        compatibility_version: SnapshotFormatVersion,
        bytes: &[u8],
    ) -> Result<Self, SnapshotV2MultiBlockDeviceGraphDecodeError> {
        codec::decode(compatibility_version, bytes)
    }
}

/// Failure while projecting a validated profile-2 graph into controller
/// configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2MultiBlockDriveConfigsError;

impl fmt::Display for SnapshotV2MultiBlockDriveConfigsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("native-v2 multi-block drive projection is invalid")
    }
}

impl std::error::Error for SnapshotV2MultiBlockDriveConfigsError {}

impl fmt::Debug for SnapshotV2MultiBlockDeviceGraph {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2MultiBlockDeviceGraph")
            .field("record_count", &self.records.len())
            .field("transport", &self.transport_kind)
            .field("state", &REDACTED)
            .finish()
    }
}

/// Failure while building one trusted profile-2 graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2MultiBlockDeviceGraphBuildError {
    /// The supplied records do not satisfy the complete profile.
    InvalidGraph,
}

impl fmt::Display for SnapshotV2MultiBlockDeviceGraphBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("native-v2 multi-block device graph is invalid")
    }
}

impl std::error::Error for SnapshotV2MultiBlockDeviceGraphBuildError {}

/// Failure while converting live capture-ready blocks into profile 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2MultiBlockDeviceGraphCaptureError {
    /// The supplied compatibility context is not exact 2.5.
    UnsupportedVersion,
    /// The selected block inventory is empty, too large, or transport-mixed.
    UnsupportedInventory,
    /// A drive lies outside the regular-file Sync/Async profile.
    UnsupportedConfiguration,
    /// A bounded string is empty, non-UTF-8, or too large.
    InvalidString,
    /// Repeated block-local facts disagree.
    InconsistentBlockState,
    /// Common virtio continuation is invalid.
    InvalidVirtioState,
    /// MMIO placement or selectors are invalid.
    InvalidMmioState,
    /// PCI placement, configuration, or MSI-X state is invalid.
    InvalidPciState,
    /// The complete config-ordered graph violates profile 2.
    InvalidGraph,
    /// A bounded artifact allocation failed.
    Allocation,
}

impl fmt::Display for SnapshotV2MultiBlockDeviceGraphCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => {
                "native-v2 multi-block graph compatibility version is unsupported"
            }
            Self::UnsupportedInventory => {
                "live block inventory is outside the native-v2 multi-block profile"
            }
            Self::UnsupportedConfiguration => {
                "live block configuration is outside the native-v2 multi-block profile"
            }
            Self::InvalidString => "live block string metadata is invalid",
            Self::InconsistentBlockState => "live block continuation state is inconsistent",
            Self::InvalidVirtioState => "live common virtio state is invalid",
            Self::InvalidMmioState => "live virtio-mmio state is invalid",
            Self::InvalidPciState => "live virtio-pci state is invalid",
            Self::InvalidGraph => "captured native-v2 multi-block graph is invalid",
            Self::Allocation => "failed to allocate a native-v2 multi-block graph",
        })
    }
}

impl std::error::Error for SnapshotV2MultiBlockDeviceGraphCaptureError {}

/// Failure while encoding one validated profile-2 graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2MultiBlockDeviceGraphEncodeError {
    /// The supplied compatibility context is not exact 2.5.
    UnsupportedVersion,
    /// The in-memory graph violates profile 2.
    InvalidGraph,
    /// Checked encoded length exceeds 512 KiB.
    TooLarge,
    /// Bounded output storage could not be allocated.
    Allocation,
}

impl fmt::Display for SnapshotV2MultiBlockDeviceGraphEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => {
                "native-v2 multi-block device graph compatibility version is unsupported"
            }
            Self::InvalidGraph => "native-v2 multi-block device graph is invalid",
            Self::TooLarge => "native-v2 multi-block device graph exceeds 512 KiB",
            Self::Allocation => "failed to allocate encoded native-v2 multi-block device graph",
        })
    }
}

impl std::error::Error for SnapshotV2MultiBlockDeviceGraphEncodeError {}

/// Failure while decoding one untrusted profile-2 payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2MultiBlockDeviceGraphDecodeError {
    /// The supplied compatibility context is not exact 2.5.
    UnsupportedVersion,
    /// The payload is shorter than its fixed header.
    TooSmall,
    /// The payload exceeds the 512-KiB profile maximum.
    TooLarge,
    /// The device-family magic is invalid.
    InvalidMagic,
    /// Profile, transport, root, or count fields are unsupported.
    UnsupportedProfile,
    /// Reserved bytes or canonical padding are nonzero.
    NonzeroReserved,
    /// Header or directory bounds are noncanonical.
    InvalidStructure,
    /// A declared fixed or variable field is truncated.
    Truncated,
    /// A scalar tag, boolean, or count is invalid.
    InvalidValue,
    /// Bounded string metadata or UTF-8 is invalid.
    InvalidString,
    /// Bounded owned state could not be allocated.
    Allocation,
    /// Decoded values violate complete graph semantics.
    InvalidGraph,
}

impl fmt::Display for SnapshotV2MultiBlockDeviceGraphDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => {
                "native-v2 multi-block device graph compatibility version is unsupported"
            }
            Self::TooSmall => "native-v2 multi-block device graph is smaller than 64 bytes",
            Self::TooLarge => "native-v2 multi-block device graph exceeds 512 KiB",
            Self::InvalidMagic => "native-v2 multi-block device graph magic is invalid",
            Self::UnsupportedProfile => "native-v2 multi-block device graph profile is unsupported",
            Self::NonzeroReserved => {
                "native-v2 multi-block device graph reserved bytes are nonzero"
            }
            Self::InvalidStructure => {
                "native-v2 multi-block device graph structure is noncanonical"
            }
            Self::Truncated => "native-v2 multi-block device graph is truncated",
            Self::InvalidValue => "native-v2 multi-block device graph scalar value is invalid",
            Self::InvalidString => "native-v2 multi-block device graph string metadata is invalid",
            Self::Allocation => "failed to allocate decoded native-v2 multi-block device graph",
            Self::InvalidGraph => "native-v2 multi-block device graph semantics are invalid",
        })
    }
}

impl std::error::Error for SnapshotV2MultiBlockDeviceGraphDecodeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphValidationError {
    Root,
    Configuration,
    Block,
    Virtio,
    Mmio,
    Pci,
    Conflict,
}

pub(crate) fn validate_graph(
    graph: &SnapshotV2MultiBlockDeviceGraph,
) -> Result<(), GraphValidationError> {
    if graph.records.is_empty()
        || graph.records.len() > usize::from(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_RECORDS)
    {
        return Err(GraphValidationError::Root);
    }
    match graph.root_key {
        None if graph.records.iter().all(|record| !record.config.is_root) => {}
        Some(root)
            if root.kind() == DEVICE_KIND_BLOCK
                && root.instance() == 0
                && graph
                    .records
                    .first()
                    .is_some_and(|record| record.config.is_root)
                && graph
                    .records
                    .iter()
                    .skip(1)
                    .all(|record| !record.config.is_root) => {}
        _ => return Err(GraphValidationError::Root),
    }

    for (index, record) in graph.records.iter().enumerate() {
        let instance = u32::try_from(index).map_err(|_| GraphValidationError::Root)?;
        if record.key.kind() != DEVICE_KIND_BLOCK
            || record.key.instance() != instance
            || record.transport.kind() != graph.transport_kind
        {
            return Err(GraphValidationError::Root);
        }
        validate_config(&record.config)?;
        validate_record(record)?;
        if record.config.is_root
            && matches!(
                record.transport,
                SnapshotV2DeviceTransport::Pci(ref state)
                    if state.origin() != StorageDeviceOrigin::Startup
            )
        {
            return Err(GraphValidationError::Pci);
        }
    }

    for (index, record) in graph.records.iter().enumerate() {
        let following = index.checked_add(1).ok_or(GraphValidationError::Conflict)?;
        for other in graph.records.iter().skip(following) {
            if record.config.drive_id == other.config.drive_id {
                return Err(GraphValidationError::Conflict);
            }
            if placement(record).overlaps(placement(other)) {
                return Err(GraphValidationError::Conflict);
            }
            match (&record.transport, &other.transport) {
                (
                    SnapshotV2DeviceTransport::Mmio(first),
                    SnapshotV2DeviceTransport::Mmio(second),
                ) if first.region().id() == second.region().id()
                    || first.interrupt_line() == second.interrupt_line() =>
                {
                    return Err(GraphValidationError::Conflict);
                }
                (SnapshotV2DeviceTransport::Pci(first), SnapshotV2DeviceTransport::Pci(second))
                    if first.sbdf() == second.sbdf() =>
                {
                    return Err(GraphValidationError::Conflict);
                }
                (SnapshotV2DeviceTransport::Mmio(_), SnapshotV2DeviceTransport::Mmio(_))
                | (SnapshotV2DeviceTransport::Pci(_), SnapshotV2DeviceTransport::Pci(_)) => {}
                _ => return Err(GraphValidationError::Conflict),
            }
            let first_queue = record
                .virtio
                .queues()
                .first()
                .ok_or(GraphValidationError::Virtio)?;
            let second_queue = other
                .virtio
                .queues()
                .first()
                .ok_or(GraphValidationError::Virtio)?;
            if matches!(
                (queue_ranges(first_queue)?, queue_ranges(second_queue)?),
                (Some(first), Some(second))
                    if first
                        .iter()
                        .any(|left| second.iter().any(|right| left.overlaps(*right)))
            ) {
                return Err(GraphValidationError::Conflict);
            }
        }
        if let Some(ranges) = queue_ranges(
            record
                .virtio
                .queues()
                .first()
                .ok_or(GraphValidationError::Virtio)?,
        )? {
            for range in ranges {
                if graph
                    .records
                    .iter()
                    .any(|candidate| range.overlaps(placement(candidate)))
                {
                    return Err(GraphValidationError::Conflict);
                }
            }
        }
    }
    Ok(())
}

fn validate_config(config: &SnapshotV2MultiBlockConfig) -> Result<(), GraphValidationError> {
    validate_nonempty_string(
        &config.drive_id,
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES,
    )
    .map_err(|()| GraphValidationError::Configuration)?;
    if !config
        .drive_id
        .chars()
        .all(|character| character == '_' || character.is_alphanumeric())
    {
        return Err(GraphValidationError::Configuration);
    }
    if let Some(partuuid) = config.partuuid.as_deref() {
        validate_nonempty_string(
            partuuid,
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_PARTUUID_BYTES,
        )
        .map_err(|()| GraphValidationError::Configuration)?;
    }
    validate_nonempty_string(
        &config.selector,
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_SELECTOR_BYTES,
    )
    .map_err(|()| GraphValidationError::Configuration)?;
    validate_limiter_config(config.rate_limiter)
}

fn validate_record(record: &SnapshotV2MultiBlockDeviceRecord) -> Result<(), GraphValidationError> {
    let block = &record.block.continuation;
    if block.capacity_sectors() != record.block.backing_bytes >> 9
        || block.device_id().as_bytes().iter().all(|byte| *byte == 0)
    {
        return Err(GraphValidationError::Block);
    }
    validate_limiter_relationship(record.config.rate_limiter, block.limiter())?;
    if block.retry() != StorageRetryState::None
        && (block.active_queue().is_none() || block.limiter().is_empty())
    {
        return Err(GraphValidationError::Block);
    }
    if matches!(
        block.retry(),
        StorageRetryState::After { remaining_nanos: 0 }
    ) {
        return Err(GraphValidationError::Block);
    }
    if block.retry() != StorageRetryState::None
        && block
            .active_queue()
            .is_some_and(|queue| queue.next_available() == queue.next_used())
    {
        return Err(GraphValidationError::Block);
    }

    let expected_features = VirtioBlockConfigSpace::new(
        record.block.backing_bytes,
        record.config.is_read_only,
        record.config.cache_type,
    )
    .available_features();
    validate_virtio(&record.virtio, expected_features)?;
    if block.active_queue().is_some() != record.virtio.is_activated() {
        return Err(GraphValidationError::Block);
    }
    let queue = record
        .virtio
        .queues()
        .first()
        .ok_or(GraphValidationError::Virtio)?;
    if block.active_queue().is_some_and(|cursor| {
        cursor.next_available().wrapping_sub(cursor.next_used()) > queue.size()
    }) {
        return Err(GraphValidationError::Block);
    }
    match &record.transport {
        SnapshotV2DeviceTransport::Mmio(state) => validate_mmio(state),
        SnapshotV2DeviceTransport::Pci(state) => validate_pci(state),
    }
}

fn validate_limiter_config(
    config: Option<DriveRateLimiterConfig>,
) -> Result<(), GraphValidationError> {
    if let Some(config) = config {
        for bucket in [config.bandwidth(), config.ops()].into_iter().flatten() {
            if !token_bucket_is_enabled(bucket) {
                return Err(GraphValidationError::Configuration);
            }
        }
        if !config.is_configured() {
            return Err(GraphValidationError::Configuration);
        }
    }
    Ok(())
}

fn validate_limiter_relationship(
    config: Option<DriveRateLimiterConfig>,
    state: SnapshotV2BlockLimiterState,
) -> Result<(), GraphValidationError> {
    validate_bucket_relationship(
        config.and_then(DriveRateLimiterConfig::bandwidth),
        state.bandwidth(),
    )?;
    validate_bucket_relationship(config.and_then(DriveRateLimiterConfig::ops), state.ops())
}

fn validate_bucket_relationship(
    config: Option<DriveTokenBucketConfig>,
    state: Option<crate::snapshot_device_v2::SnapshotV2BlockBucketState>,
) -> Result<(), GraphValidationError> {
    match (config, state) {
        (Some(config), Some(state))
            if token_bucket_is_enabled(config)
                && state.budget() <= config.size()
                && state.remaining_burst() <= config.one_time_burst().unwrap_or(0) =>
        {
            Ok(())
        }
        (None, None) => Ok(()),
        _ => Err(GraphValidationError::Block),
    }
}

fn validate_virtio(
    state: &SnapshotV2VirtioState,
    expected_features: u64,
) -> Result<(), GraphValidationError> {
    if state.available_features() != expected_features
        || state.driver_features() & !state.available_features() != 0
        || state.queues().len() != 1
        || state.pending_notifications().len() > 1
        || state.interrupt_intents().len() > 2
    {
        return Err(GraphValidationError::Virtio);
    }
    let healthy_driver_ok = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK
        | VIRTIO_DEVICE_STATUS_DRIVER_OK;
    let healthy_statuses = [
        VIRTIO_DEVICE_STATUS_INIT,
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
            | VIRTIO_DEVICE_STATUS_DRIVER
            | VIRTIO_DEVICE_STATUS_FEATURES_OK,
        healthy_driver_ok,
    ];
    if !healthy_statuses.contains(&state.status())
        || state.status() & (VIRTIO_DEVICE_STATUS_FAILED | VIRTIO_DEVICE_STATUS_DEVICE_NEEDS_RESET)
            != 0
    {
        return Err(GraphValidationError::Virtio);
    }
    if state.status() & VIRTIO_DEVICE_STATUS_DRIVER == 0 && state.driver_features() != 0 {
        return Err(GraphValidationError::Virtio);
    }
    if state.status() & VIRTIO_DEVICE_STATUS_FEATURES_OK != 0
        && state.driver_features() & VIRTIO_MMIO_VERSION_1_FEATURE == 0
    {
        return Err(GraphValidationError::Virtio);
    }
    if state.is_activated() != (state.status() == healthy_driver_ok) {
        return Err(GraphValidationError::Virtio);
    }
    let queue = state.queues().first().ok_or(GraphValidationError::Virtio)?;
    validate_queue(queue)?;
    if state.is_activated() && !queue.ready() {
        return Err(GraphValidationError::Virtio);
    }
    if state.status() & VIRTIO_DEVICE_STATUS_FEATURES_OK == 0
        && (queue.size() != 0
            || queue.ready()
            || queue.descriptor_table().raw_value() != 0
            || queue.driver_ring().raw_value() != 0
            || queue.device_ring().raw_value() != 0)
    {
        return Err(GraphValidationError::Virtio);
    }
    if !state.is_activated() && !state.pending_notifications().is_empty() {
        return Err(GraphValidationError::Virtio);
    }
    if state
        .pending_notifications()
        .iter()
        .copied()
        .any(|index| index != 0)
    {
        return Err(GraphValidationError::Virtio);
    }
    if !state
        .interrupt_intents()
        .windows(2)
        .all(|window| matches!(window, [first, second] if first < second))
        || state.interrupt_intents().iter().any(|intent| {
            matches!(
                intent,
                SnapshotV2InterruptIntent::Queue { queue_index } if *queue_index != 0
            )
        })
    {
        return Err(GraphValidationError::Virtio);
    }
    Ok(())
}

fn validate_queue(queue: &SnapshotV2VirtioQueueState) -> Result<(), GraphValidationError> {
    if queue.max_size() != VIRTIO_BLOCK_QUEUE_SIZE
        || (queue.size() != 0
            && (!queue.size().is_power_of_two() || queue.size() > queue.max_size()))
        || (queue.ready() && queue.size() == 0)
        || !queue
            .descriptor_table()
            .raw_value()
            .is_multiple_of(crate::virtio_queue::VIRTQUEUE_DESCRIPTOR_ALIGNMENT)
        || !queue
            .driver_ring()
            .raw_value()
            .is_multiple_of(crate::virtio_queue::VIRTQUEUE_AVAILABLE_RING_ALIGNMENT)
        || !queue
            .device_ring()
            .raw_value()
            .is_multiple_of(crate::virtio_queue::VIRTQUEUE_USED_RING_ALIGNMENT)
    {
        return Err(GraphValidationError::Virtio);
    }
    if let Some(ranges) = queue_ranges(queue)?
        && (ranges[0].overlaps(ranges[1])
            || ranges[0].overlaps(ranges[2])
            || ranges[1].overlaps(ranges[2]))
    {
        return Err(GraphValidationError::Virtio);
    }
    Ok(())
}

fn queue_ranges(
    queue: &SnapshotV2VirtioQueueState,
) -> Result<Option<[GuestMemoryRange; 3]>, GraphValidationError> {
    if queue.size() == 0 {
        return Ok(None);
    }
    let descriptor_size = u64::from(queue.size())
        .checked_mul(16)
        .ok_or(GraphValidationError::Virtio)?;
    let available_size = u64::from(queue.size())
        .checked_mul(2)
        .and_then(|size| size.checked_add(6))
        .ok_or(GraphValidationError::Virtio)?;
    let used_size = u64::from(queue.size())
        .checked_mul(8)
        .and_then(|size| size.checked_add(6))
        .ok_or(GraphValidationError::Virtio)?;
    Ok(Some([
        GuestMemoryRange::new(queue.descriptor_table(), descriptor_size).map_err(range_error)?,
        GuestMemoryRange::new(queue.driver_ring(), available_size).map_err(range_error)?,
        GuestMemoryRange::new(queue.device_ring(), used_size).map_err(range_error)?,
    ]))
}

fn range_error(_: GuestMemoryError) -> GraphValidationError {
    GraphValidationError::Virtio
}

fn validate_mmio(state: &SnapshotV2MmioDeviceState) -> Result<(), GraphValidationError> {
    if state.device_feature_select() > 1
        || state.driver_feature_select() > 1
        || state.queue_select() != 0
        || state.region().id().raw_value() == 0
        || state.region().range().size() != VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        || state
            .region()
            .range()
            .validate_alignment(VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
            .is_err()
        || state.interrupt_line().raw_value() < 32
    {
        Err(GraphValidationError::Mmio)
    } else {
        Ok(())
    }
}

fn validate_pci(state: &SnapshotV2PciDeviceState) -> Result<(), GraphValidationError> {
    const WRITABLE_OFFSETS: [u16; 4] = [0x04, 0x05, 0x0c, 0x3c];
    let aperture_end = PCI_BAR64_START
        .checked_add(PCI_BAR64_SIZE)
        .ok_or(GraphValidationError::Pci)?;
    if state.phase() != VirtioPciEndpointPhase::Active
        || state.sbdf().segment() != PCI_SEGMENT_ZERO
        || state.sbdf().bus() != PCI_BUS_ZERO
        || !(PCI_FIRST_ENDPOINT_DEVICE..=PCI_LAST_ENDPOINT_DEVICE).contains(&state.sbdf().device())
        || state.sbdf().function() != PCI_FUNCTION_ZERO
        || state.bar_index() != VIRTIO_PCI_CAPABILITY_BAR_INDEX
        || state.bar_address_space() != PciBarAddressSpace::Memory64
        || state.bar_prefetchable() != PciBarPrefetchable::No
        || state.bar_range().size() != VIRTIO_PCI_CAPABILITY_BAR_SIZE
        || state
            .bar_range()
            .validate_alignment(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .is_err()
        || state.bar_range().start().raw_value() < PCI_BAR64_START
        || state.bar_range().end_exclusive().raw_value() > aperture_end
        || state.device_feature_select() > 1
        || state.driver_feature_select() > 1
        || state.queue_select() != 0
        || state.writable_bytes().len() != WRITABLE_OFFSETS.len()
        || state.bar_probes().len() != 2
    {
        return Err(GraphValidationError::Pci);
    }
    if state
        .writable_bytes()
        .iter()
        .map(|byte| byte.offset())
        .ne(WRITABLE_OFFSETS)
        || state
            .bar_probes()
            .iter()
            .map(|probe| probe.index())
            .ne([0, 1])
    {
        return Err(GraphValidationError::Pci);
    }
    validate_msix(state.msix())
}

fn validate_msix(state: &SnapshotV2PciMsixState) -> Result<(), GraphValidationError> {
    if state.entries().len() != 2
        || state.entries().len() > VIRTIO_PCI_MAX_MSIX_VECTORS
        || state.pending_words().len() != 1
        || state.queue_vectors().len() != 1
        || state
            .pending_words()
            .first()
            .copied()
            .is_none_or(|pending| pending & !0b11 != 0)
        || !valid_msix_vector(state.config_vector(), state.entries().len())
        || state
            .queue_vectors()
            .iter()
            .copied()
            .any(|vector| !valid_msix_vector(vector, state.entries().len()))
        || state
            .entries()
            .iter()
            .any(|entry| entry.vector_control() & !1 != 0)
    {
        Err(GraphValidationError::Pci)
    } else {
        Ok(())
    }
}

fn valid_msix_vector(vector: u16, count: usize) -> bool {
    vector == VIRTIO_PCI_NO_VECTOR || usize::from(vector) < count
}

fn placement(record: &SnapshotV2MultiBlockDeviceRecord) -> GuestMemoryRange {
    match &record.transport {
        SnapshotV2DeviceTransport::Mmio(state) => state.region().range(),
        SnapshotV2DeviceTransport::Pci(state) => state.bar_range(),
    }
}

fn validate_nonempty_string(value: &str, maximum: usize) -> Result<(), ()> {
    if value.is_empty() || value.len() > maximum {
        Err(())
    } else {
        Ok(())
    }
}

fn token_bucket_is_enabled(config: DriveTokenBucketConfig) -> bool {
    config.size() != 0
        && config
            .refill_time()
            .checked_mul(1_000_000)
            .is_some_and(|nanos| nanos != 0)
}
