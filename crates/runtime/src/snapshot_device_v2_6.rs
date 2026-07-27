//! Canonical detached native-v2 2.6 storage device-graph profile.
//!
//! This exact profile is an internal compatibility capability. The production
//! writer remains exact native-v2 2.5 until the complete pmem capture and
//! restore transaction is activated.

use std::fmt;

use crate::memory::GuestMemoryRange;
use crate::pmem::{
    PmemRateLimiterConfig, PmemTokenBucketConfig, VIRTIO_PMEM_ALIGNMENT,
    VIRTIO_PMEM_CONFIG_SPACE_SIZE, VIRTIO_PMEM_QUEUE_SIZE, VirtioPmemConfigSpace,
    VirtioPmemQueueState, aligned_pmem_mapping_len,
};
use crate::snapshot_device_v2::{
    SnapshotV2DeviceKey, SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind,
    SnapshotV2VirtioQueueState, SnapshotV2VirtioState,
};
use crate::snapshot_device_v2_5::{
    BLOCK_SECTION_BYTES, COMMON_FIXED_BYTES, COMMON_QUEUE_BYTES, CONFIG_FIXED_BYTES,
    MMIO_SECTION_BYTES, NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES,
    NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_PARTUUID_BYTES,
    NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_SELECTOR_BYTES, PCI_SECTION_BYTES,
    SnapshotV2MultiBlockDeviceRecord, queue_ranges, validate_mmio, validate_pci, validate_record,
    validate_virtio,
};
use crate::snapshot_format::SnapshotFormatVersion;
use crate::snapshot_restore::{
    MAX_SNAPSHOT_RESTORE_PUBLIC_ID_BYTES, MAX_SNAPSHOT_RESTORE_RESOURCES,
};
use crate::storage_capture::{StorageDeviceOrigin, StorageRetryState};

mod capture;
mod codec;
mod restore;

pub use restore::{
    PreparedSnapshotV2PmemRecord, PreparedSnapshotV2PmemRecordParts,
    PreparedSnapshotV2StorageBundle, PreparedSnapshotV2StorageBundleParts,
    PreparedSnapshotV2StorageMmioBundle, PreparedSnapshotV2StorageMmioBundleParts,
    PreparedSnapshotV2StorageMmioPmemRecord, PreparedSnapshotV2StorageMmioPmemRecordParts,
    SnapshotV2StorageBundleError, SnapshotV2StorageCleanupError,
    SnapshotV2StorageMmioTransportError, SnapshotV2StorageRestorePlan,
    SnapshotV2StorageRestorePlanError,
};

#[cfg(test)]
mod tests;

/// Exact compatibility context of the detached storage graph profile.
pub const NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION: SnapshotFormatVersion =
    SnapshotFormatVersion::new(2, 6, 0);

/// Maximum encoded size of one native-v2 2.6 storage graph.
pub const NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_BYTES: usize = 512 * 1024;

/// Maximum combined block and pmem record count admitted by profile 3.
pub const NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS: u16 = 64;

/// Maximum UTF-8 byte length of one public pmem identifier.
pub const NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_PMEM_ID_BYTES: usize = 255;

/// Maximum UTF-8 byte length of one inert logical backing selector.
pub const NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_SELECTOR_BYTES: usize =
    NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_SELECTOR_BYTES;

/// Fixed profile-3 payload header size.
pub const NATIVE_V2_STORAGE_DEVICE_GRAPH_HEADER_BYTES: usize = 64;

/// Fixed encoded size of one profile-3 record-directory entry.
pub const NATIVE_V2_STORAGE_DEVICE_GRAPH_RECORD_ENTRY_BYTES: usize = 32;

/// Fixed encoded size of one profile-3 section-directory entry.
pub const NATIVE_V2_STORAGE_DEVICE_GRAPH_SECTION_ENTRY_BYTES: usize = 32;

pub(crate) const PMEM_CONFIG_FIXED_BYTES: usize = 80;
pub(crate) const PMEM_SECTION_BYTES: usize = 128;
pub(crate) const SECTION_COUNT_PER_RECORD: usize = 4;
pub(crate) const ALIGNMENT: usize = 8;
pub(crate) const DEVICE_KIND_BLOCK: u32 = 1;
pub(crate) const DEVICE_KIND_PMEM: u32 = 2;

const DIRECTORY_BYTES_PER_RECORD: usize = NATIVE_V2_STORAGE_DEVICE_GRAPH_RECORD_ENTRY_BYTES
    + SECTION_COUNT_PER_RECORD * NATIVE_V2_STORAGE_DEVICE_GRAPH_SECTION_ENTRY_BYTES;
const MAX_BLOCK_CONFIG_BYTES: usize = align_const(
    CONFIG_FIXED_BYTES
        + NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES
        + NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_PARTUUID_BYTES
        + NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_SELECTOR_BYTES,
);
const MAX_PMEM_CONFIG_BYTES: usize = align_const(
    PMEM_CONFIG_FIXED_BYTES
        + NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_PMEM_ID_BYTES
        + NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_SELECTOR_BYTES,
);
const MAX_COMMON_BYTES: usize = align_const(COMMON_FIXED_BYTES + COMMON_QUEUE_BYTES + 2 + 2 * 4);
const MAX_CONFIG_BYTES: usize = const_max(MAX_BLOCK_CONFIG_BYTES, MAX_PMEM_CONFIG_BYTES);
const MAX_DEVICE_BYTES: usize = const_max(BLOCK_SECTION_BYTES, PMEM_SECTION_BYTES);
const MAX_TRANSPORT_BYTES: usize = const_max(MMIO_SECTION_BYTES, PCI_SECTION_BYTES);
const REDACTED: &str = "<redacted>";

/// Exact maximum byte count of every field admitted by the final profile-3 schema.
pub const NATIVE_V2_STORAGE_DEVICE_GRAPH_WORST_CASE_BYTES: usize =
    NATIVE_V2_STORAGE_DEVICE_GRAPH_HEADER_BYTES
        + NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS as usize
            * (DIRECTORY_BYTES_PER_RECORD
                + MAX_CONFIG_BYTES
                + MAX_DEVICE_BYTES
                + MAX_COMMON_BYTES
                + MAX_TRANSPORT_BYTES);

const fn align_const(value: usize) -> usize {
    (value + (ALIGNMENT - 1)) & !(ALIGNMENT - 1)
}

const fn const_max(left: usize, right: usize) -> usize {
    if left > right { left } else { right }
}

const _: () = assert!(
    NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION.major()
        == NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION.major()
);
const _: () = assert!(
    NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION.minor()
        == NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION.minor() + 1
);
const _: () = assert!(NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION.patch() == 0);
const _: () =
    assert!(NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS as usize <= MAX_SNAPSHOT_RESTORE_RESOURCES);
const _: () = assert!(
    NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_PMEM_ID_BYTES <= MAX_SNAPSHOT_RESTORE_PUBLIC_ID_BYTES
);
const _: () = assert!(
    NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS as usize * SECTION_COUNT_PER_RECORD
        <= u16::MAX as usize
);
const _: () = assert!(
    NATIVE_V2_STORAGE_DEVICE_GRAPH_WORST_CASE_BYTES <= NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_BYTES
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

/// Stable public configuration of one profile-3 regular-file pmem record.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2PmemConfig {
    pmem_id: String,
    is_root: bool,
    is_read_only: bool,
    rate_limiter: Option<PmemRateLimiterConfig>,
    selector: String,
}

impl SnapshotV2PmemConfig {
    /// Constructs a checked inert pmem configuration.
    pub fn try_new(
        pmem_id: impl Into<String>,
        is_root: bool,
        is_read_only: bool,
        rate_limiter: Option<PmemRateLimiterConfig>,
        selector: impl Into<String>,
    ) -> Result<Self, SnapshotV2StorageDeviceGraphBuildError> {
        let config = Self {
            pmem_id: pmem_id.into(),
            is_root,
            is_read_only,
            rate_limiter,
            selector: selector.into(),
        };
        validate_pmem_config(&config)
            .map(|()| config)
            .map_err(|_| SnapshotV2StorageDeviceGraphBuildError::InvalidGraph)
    }

    /// Returns the stable public pmem identifier.
    pub fn pmem_id(&self) -> &str {
        &self.pmem_id
    }

    /// Returns whether this record is the boot root.
    pub const fn is_root(&self) -> bool {
        self.is_root
    }

    /// Returns whether guest writes are prohibited.
    pub const fn is_read_only(&self) -> bool {
        self.is_read_only
    }

    /// Returns the public rate-limiter configuration.
    pub const fn rate_limiter(&self) -> Option<PmemRateLimiterConfig> {
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

redacted_debug!(SnapshotV2PmemConfig, "SnapshotV2PmemConfig");

/// Live value of one configured pmem token bucket.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2PmemBucketState {
    budget: u64,
    remaining_burst: u64,
    age_nanos: u64,
}

impl SnapshotV2PmemBucketState {
    /// Constructs one persisted token-bucket value.
    pub const fn new(budget: u64, remaining_burst: u64, age_nanos: u64) -> Self {
        Self {
            budget,
            remaining_burst,
            age_nanos,
        }
    }

    /// Returns the current ordinary-token budget.
    pub const fn budget(self) -> u64 {
        self.budget
    }

    /// Returns the remaining one-time burst.
    pub const fn remaining_burst(self) -> u64 {
        self.remaining_burst
    }

    /// Returns the monotonic age captured as a duration.
    pub const fn age_nanos(self) -> u64 {
        self.age_nanos
    }
}

redacted_debug!(SnapshotV2PmemBucketState, "SnapshotV2PmemBucketState");

/// Persisted pmem rate-limiter state without duplicated configuration.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2PmemLimiterState {
    bandwidth: Option<SnapshotV2PmemBucketState>,
    ops: Option<SnapshotV2PmemBucketState>,
}

impl SnapshotV2PmemLimiterState {
    /// Constructs one inert limiter continuation.
    pub const fn new(
        bandwidth: Option<SnapshotV2PmemBucketState>,
        ops: Option<SnapshotV2PmemBucketState>,
    ) -> Self {
        Self { bandwidth, ops }
    }

    /// Returns the bandwidth bucket state.
    pub const fn bandwidth(self) -> Option<SnapshotV2PmemBucketState> {
        self.bandwidth
    }

    /// Returns the operations bucket state.
    pub const fn ops(self) -> Option<SnapshotV2PmemBucketState> {
        self.ops
    }

    /// Returns whether no bucket state is present.
    pub const fn is_empty(self) -> bool {
        self.bandwidth.is_none() && self.ops.is_none()
    }
}

redacted_debug!(SnapshotV2PmemLimiterState, "SnapshotV2PmemLimiterState");

/// Exact geometry and pmem-local continuation for one profile-3 record.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2PmemState {
    file_bytes: u64,
    mapped_bytes: u64,
    guest_range: GuestMemoryRange,
    config_space: VirtioPmemConfigSpace,
    active_queue: Option<VirtioPmemQueueState>,
    limiter: SnapshotV2PmemLimiterState,
    pending_rate_limited_queue: bool,
    retry: StorageRetryState,
}

impl SnapshotV2PmemState {
    /// Constructs checked pmem-local semantic state.
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        file_bytes: u64,
        mapped_bytes: u64,
        guest_range: GuestMemoryRange,
        config_space: VirtioPmemConfigSpace,
        active_queue: Option<VirtioPmemQueueState>,
        limiter: SnapshotV2PmemLimiterState,
        pending_rate_limited_queue: bool,
        retry: StorageRetryState,
    ) -> Result<Self, SnapshotV2StorageDeviceGraphBuildError> {
        let state = Self {
            file_bytes,
            mapped_bytes,
            guest_range,
            config_space,
            active_queue,
            limiter,
            pending_rate_limited_queue,
            retry,
        };
        validate_pmem_state_local(&state)
            .map(|()| state)
            .map_err(|_| SnapshotV2StorageDeviceGraphBuildError::InvalidGraph)
    }

    /// Returns the exact external regular-file byte length.
    pub const fn file_bytes(&self) -> u64 {
        self.file_bytes
    }

    /// Returns the checked 2-MiB-aligned mapping length.
    pub const fn mapped_bytes(&self) -> u64 {
        self.mapped_bytes
    }

    /// Returns the exact guest pmem range.
    pub const fn guest_range(&self) -> GuestMemoryRange {
        self.guest_range
    }

    /// Returns the exact guest-visible pmem config space.
    pub const fn config_space(&self) -> VirtioPmemConfigSpace {
        self.config_space
    }

    /// Returns the optional active queue cursors.
    pub const fn active_queue(&self) -> Option<VirtioPmemQueueState> {
        self.active_queue
    }

    /// Returns persisted rate-limiter state.
    pub const fn limiter(&self) -> SnapshotV2PmemLimiterState {
        self.limiter
    }

    /// Returns whether one rate-limited queue notification remains pending.
    pub const fn pending_rate_limited_queue(&self) -> bool {
        self.pending_rate_limited_queue
    }

    /// Returns the host-time-free retry disposition.
    pub const fn retry(&self) -> StorageRetryState {
        self.retry
    }
}

redacted_debug!(SnapshotV2PmemState, "SnapshotV2PmemState");

/// One canonical profile-3 regular-file pmem record.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2PmemDeviceRecord {
    key: SnapshotV2DeviceKey,
    config: SnapshotV2PmemConfig,
    pmem: SnapshotV2PmemState,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
}

impl SnapshotV2PmemDeviceRecord {
    /// Constructs a checked pmem record before graph-wide conflict validation.
    pub fn try_new(
        instance: u32,
        config: SnapshotV2PmemConfig,
        pmem: SnapshotV2PmemState,
        virtio: SnapshotV2VirtioState,
        transport: SnapshotV2DeviceTransport,
    ) -> Result<Self, SnapshotV2StorageDeviceGraphBuildError> {
        let record = Self {
            key: SnapshotV2DeviceKey::pmem(instance),
            config,
            pmem,
            virtio,
            transport,
        };
        validate_pmem_record(&record)
            .map(|()| record)
            .map_err(|_| SnapshotV2StorageDeviceGraphBuildError::InvalidGraph)
    }

    pub(crate) const fn from_decoded_parts(
        key: SnapshotV2DeviceKey,
        config: SnapshotV2PmemConfig,
        pmem: SnapshotV2PmemState,
        virtio: SnapshotV2VirtioState,
        transport: SnapshotV2DeviceTransport,
    ) -> Self {
        Self {
            key,
            config,
            pmem,
            virtio,
            transport,
        }
    }

    /// Returns the stable typed record key.
    pub const fn key(&self) -> SnapshotV2DeviceKey {
        self.key
    }

    /// Returns stable public configuration.
    pub const fn config(&self) -> &SnapshotV2PmemConfig {
        &self.config
    }

    /// Returns exact pmem-local geometry and continuation.
    pub const fn pmem(&self) -> &SnapshotV2PmemState {
        &self.pmem
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

redacted_debug!(SnapshotV2PmemDeviceRecord, "SnapshotV2PmemDeviceRecord");

/// Fully validated detached native-v2 2.6 storage graph.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2StorageDeviceGraph {
    root_key: Option<SnapshotV2DeviceKey>,
    transport_kind: SnapshotV2DeviceTransportKind,
    block_records: Vec<SnapshotV2MultiBlockDeviceRecord>,
    pmem_records: Vec<SnapshotV2PmemDeviceRecord>,
}

impl SnapshotV2StorageDeviceGraph {
    /// Constructs one complete checked storage graph.
    pub fn try_from_parts(
        root_key: Option<SnapshotV2DeviceKey>,
        transport_kind: SnapshotV2DeviceTransportKind,
        block_records: Vec<SnapshotV2MultiBlockDeviceRecord>,
        pmem_records: Vec<SnapshotV2PmemDeviceRecord>,
    ) -> Result<Self, SnapshotV2StorageDeviceGraphBuildError> {
        let graph = Self {
            root_key,
            transport_kind,
            block_records,
            pmem_records,
        };
        validate_graph(&graph)
            .map(|()| graph)
            .map_err(|_| SnapshotV2StorageDeviceGraphBuildError::InvalidGraph)
    }

    /// Returns this graph's exact compatibility context.
    pub const fn compatibility_version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION
    }

    /// Returns the optional cross-storage root key.
    pub const fn root_key(&self) -> Option<SnapshotV2DeviceKey> {
        self.root_key
    }

    /// Returns the graph-wide transport kind.
    pub const fn transport_kind(&self) -> SnapshotV2DeviceTransportKind {
        self.transport_kind
    }

    /// Returns block records in canonical block configuration order.
    pub fn block_records(&self) -> &[SnapshotV2MultiBlockDeviceRecord] {
        &self.block_records
    }

    /// Returns pmem records in canonical pmem configuration order.
    pub fn pmem_records(&self) -> &[SnapshotV2PmemDeviceRecord] {
        &self.pmem_records
    }

    /// Returns the combined record count.
    pub fn record_count(&self) -> usize {
        self.block_records.len() + self.pmem_records.len()
    }

    /// Encodes this graph under the exact supplied compatibility context.
    pub fn encode(
        &self,
        compatibility_version: SnapshotFormatVersion,
    ) -> Result<Vec<u8>, SnapshotV2StorageDeviceGraphEncodeError> {
        codec::encode(compatibility_version, self)
    }

    /// Decodes and validates one exact profile-3 payload.
    pub fn decode(
        compatibility_version: SnapshotFormatVersion,
        bytes: &[u8],
    ) -> Result<Self, SnapshotV2StorageDeviceGraphDecodeError> {
        codec::decode(compatibility_version, bytes)
    }
}

impl fmt::Debug for SnapshotV2StorageDeviceGraph {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2StorageDeviceGraph")
            .field("block_record_count", &self.block_records.len())
            .field("pmem_record_count", &self.pmem_records.len())
            .field("transport", &self.transport_kind)
            .field("state", &REDACTED)
            .finish()
    }
}

/// Failure while building one trusted profile-3 graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2StorageDeviceGraphBuildError {
    /// The supplied records do not satisfy the complete profile.
    InvalidGraph,
}

impl fmt::Display for SnapshotV2StorageDeviceGraphBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("native-v2 storage device graph is invalid")
    }
}

impl std::error::Error for SnapshotV2StorageDeviceGraphBuildError {}

/// Failure while converting one complete live storage inventory into profile 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2StorageDeviceGraphCaptureError {
    /// The supplied compatibility context is not exact 2.6.
    UnsupportedVersion,
    /// The combined inventory is empty, too large, or transport-mixed.
    UnsupportedInventory,
    /// A block or pmem configuration lies outside profile 3.
    UnsupportedConfiguration,
    /// Bounded live string metadata is invalid.
    InvalidString,
    /// Repeated block-local facts disagree.
    InconsistentBlockState,
    /// Repeated pmem-local facts disagree.
    InconsistentPmemState,
    /// Common virtio continuation is invalid.
    InvalidVirtioState,
    /// MMIO placement or selectors are invalid.
    InvalidMmioState,
    /// PCI placement, configuration, or MSI-X state is invalid.
    InvalidPciState,
    /// The complete config-ordered graph violates profile 3.
    InvalidGraph,
    /// A bounded artifact allocation failed.
    Allocation,
}

impl fmt::Display for SnapshotV2StorageDeviceGraphCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => {
                "native-v2 storage graph compatibility version is unsupported"
            }
            Self::UnsupportedInventory => {
                "live storage inventory is outside the native-v2 storage profile"
            }
            Self::UnsupportedConfiguration => {
                "live storage configuration is outside the native-v2 storage profile"
            }
            Self::InvalidString => "live storage string metadata is invalid",
            Self::InconsistentBlockState => "live block continuation state is inconsistent",
            Self::InconsistentPmemState => "live pmem continuation state is inconsistent",
            Self::InvalidVirtioState => "live common virtio state is invalid",
            Self::InvalidMmioState => "live virtio-mmio state is invalid",
            Self::InvalidPciState => "live virtio-pci state is invalid",
            Self::InvalidGraph => "captured native-v2 storage graph is invalid",
            Self::Allocation => "failed to allocate a native-v2 storage graph",
        })
    }
}

impl std::error::Error for SnapshotV2StorageDeviceGraphCaptureError {}

/// Failure while encoding one validated profile-3 graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2StorageDeviceGraphEncodeError {
    /// The supplied compatibility context is not exact 2.6.
    UnsupportedVersion,
    /// The in-memory graph violates profile 3.
    InvalidGraph,
    /// Checked encoded length exceeds 512 KiB.
    TooLarge,
    /// Bounded output storage could not be allocated.
    Allocation,
}

impl fmt::Display for SnapshotV2StorageDeviceGraphEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => {
                "native-v2 storage device graph compatibility version is unsupported"
            }
            Self::InvalidGraph => "native-v2 storage device graph is invalid",
            Self::TooLarge => "native-v2 storage device graph exceeds 512 KiB",
            Self::Allocation => "failed to allocate encoded native-v2 storage device graph",
        })
    }
}

impl std::error::Error for SnapshotV2StorageDeviceGraphEncodeError {}

/// Failure while decoding one untrusted profile-3 payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2StorageDeviceGraphDecodeError {
    /// The supplied compatibility context is not exact 2.6.
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

impl fmt::Display for SnapshotV2StorageDeviceGraphDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => {
                "native-v2 storage device graph compatibility version is unsupported"
            }
            Self::TooSmall => "native-v2 storage device graph is smaller than 64 bytes",
            Self::TooLarge => "native-v2 storage device graph exceeds 512 KiB",
            Self::InvalidMagic => "native-v2 storage device graph magic is invalid",
            Self::UnsupportedProfile => "native-v2 storage device graph profile is unsupported",
            Self::NonzeroReserved => "native-v2 storage device graph reserved bytes are nonzero",
            Self::InvalidStructure => "native-v2 storage device graph structure is noncanonical",
            Self::Truncated => "native-v2 storage device graph is truncated",
            Self::InvalidValue => "native-v2 storage device graph scalar value is invalid",
            Self::InvalidString => "native-v2 storage device graph string metadata is invalid",
            Self::Allocation => "failed to allocate decoded native-v2 storage device graph",
            Self::InvalidGraph => "native-v2 storage device graph semantics are invalid",
        })
    }
}

impl std::error::Error for SnapshotV2StorageDeviceGraphDecodeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphValidationError {
    Root,
    Configuration,
    Pmem,
    Virtio,
    Mmio,
    Pci,
    Conflict,
}

pub(crate) fn validate_graph(
    graph: &SnapshotV2StorageDeviceGraph,
) -> Result<(), GraphValidationError> {
    let record_count = graph
        .block_records
        .len()
        .checked_add(graph.pmem_records.len())
        .ok_or(GraphValidationError::Root)?;
    if record_count == 0 || record_count > usize::from(NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS) {
        return Err(GraphValidationError::Root);
    }
    validate_root(graph)?;

    for (index, record) in graph.block_records.iter().enumerate() {
        if record.key().kind() != DEVICE_KIND_BLOCK
            || record.key().instance()
                != u32::try_from(index).map_err(|_| GraphValidationError::Root)?
            || record.transport().kind() != graph.transport_kind
        {
            return Err(GraphValidationError::Root);
        }
        validate_record(record).map_err(|_| GraphValidationError::Configuration)?;
        if record.is_root()
            && matches!(
                record.transport(),
                SnapshotV2DeviceTransport::Pci(state)
                    if state.origin() != StorageDeviceOrigin::Startup
            )
        {
            return Err(GraphValidationError::Pci);
        }
    }
    for (index, record) in graph.pmem_records.iter().enumerate() {
        if record.key.kind() != DEVICE_KIND_PMEM
            || record.key.instance()
                != u32::try_from(index).map_err(|_| GraphValidationError::Root)?
            || record.transport.kind() != graph.transport_kind
        {
            return Err(GraphValidationError::Root);
        }
        validate_pmem_record(record)?;
        if record.is_root()
            && matches!(
                record.transport(),
                SnapshotV2DeviceTransport::Pci(state)
                    if state.origin() != StorageDeviceOrigin::Startup
            )
        {
            return Err(GraphValidationError::Pci);
        }
    }

    for (index, record) in graph.block_records.iter().enumerate() {
        if graph
            .block_records
            .iter()
            .skip(index + 1)
            .any(|other| record.config().drive_id() == other.config().drive_id())
        {
            return Err(GraphValidationError::Conflict);
        }
    }
    for (index, record) in graph.pmem_records.iter().enumerate() {
        if graph
            .pmem_records
            .iter()
            .skip(index + 1)
            .any(|other| record.config.pmem_id == other.config.pmem_id)
        {
            return Err(GraphValidationError::Conflict);
        }
    }

    for left_index in 0..record_count {
        let left = storage_record_at(graph, left_index).ok_or(GraphValidationError::Conflict)?;
        for right_index in left_index + 1..record_count {
            let right =
                storage_record_at(graph, right_index).ok_or(GraphValidationError::Conflict)?;
            validate_record_pair(left, right)?;
        }
        validate_record_against_product(graph, left)?;
    }
    Ok(())
}

fn validate_root(graph: &SnapshotV2StorageDeviceGraph) -> Result<(), GraphValidationError> {
    let block_root_count = graph
        .block_records
        .iter()
        .filter(|record| record.is_root())
        .count();
    let pmem_root_count = graph
        .pmem_records
        .iter()
        .filter(|record| record.is_root())
        .count();
    match graph.root_key {
        None if block_root_count == 0 && pmem_root_count == 0 => Ok(()),
        Some(key)
            if key == SnapshotV2DeviceKey::block(0)
                && block_root_count == 1
                && graph
                    .block_records
                    .first()
                    .is_some_and(SnapshotV2MultiBlockDeviceRecord::is_root)
                && pmem_root_count == 0 =>
        {
            Ok(())
        }
        Some(key)
            if key == SnapshotV2DeviceKey::pmem(0)
                && pmem_root_count == 1
                && graph
                    .pmem_records
                    .first()
                    .is_some_and(SnapshotV2PmemDeviceRecord::is_root)
                && block_root_count == 0 =>
        {
            Ok(())
        }
        _ => Err(GraphValidationError::Root),
    }
}

fn validate_pmem_config(config: &SnapshotV2PmemConfig) -> Result<(), GraphValidationError> {
    if config.pmem_id.is_empty()
        || config.pmem_id.len() > NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_PMEM_ID_BYTES
        || !config
            .pmem_id
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
        || config.selector.is_empty()
        || config.selector.len() > NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_SELECTOR_BYTES
    {
        return Err(GraphValidationError::Configuration);
    }
    if let Some(limiter) = config.rate_limiter {
        for bucket in [limiter.bandwidth(), limiter.ops()].into_iter().flatten() {
            if !pmem_token_bucket_is_enabled(bucket) {
                return Err(GraphValidationError::Configuration);
            }
        }
        if !limiter.is_configured() {
            return Err(GraphValidationError::Configuration);
        }
    }
    Ok(())
}

fn validate_pmem_state_local(state: &SnapshotV2PmemState) -> Result<(), GraphValidationError> {
    let expected_mapped =
        aligned_pmem_mapping_len(state.file_bytes).ok_or(GraphValidationError::Pmem)?;
    if state.file_bytes == 0
        || state.mapped_bytes != expected_mapped
        || state.guest_range.size() != state.mapped_bytes
        || state
            .guest_range
            .validate_alignment(VIRTIO_PMEM_ALIGNMENT)
            .is_err()
        || state.config_space.start() != state.guest_range.start().raw_value()
        || state.config_space.size() != state.guest_range.size()
        || matches!(state.retry, StorageRetryState::After { remaining_nanos: 0 })
    {
        return Err(GraphValidationError::Pmem);
    }
    if state.pending_rate_limited_queue
        && (state.active_queue.is_none()
            || state.limiter.is_empty()
            || state.retry == StorageRetryState::None
            || state
                .active_queue
                .is_some_and(|queue| queue.next_available() == queue.next_used()))
    {
        return Err(GraphValidationError::Pmem);
    }
    Ok(())
}

fn validate_pmem_record(record: &SnapshotV2PmemDeviceRecord) -> Result<(), GraphValidationError> {
    validate_pmem_config(&record.config)?;
    validate_pmem_state_local(&record.pmem)?;
    validate_pmem_limiter_relationship(record.config.rate_limiter, record.pmem.limiter)?;
    validate_virtio(
        &record.virtio,
        record.pmem.config_space.available_features(),
    )
    .map_err(|_| GraphValidationError::Virtio)?;
    if record.pmem.active_queue.is_some() != record.virtio.is_activated() {
        return Err(GraphValidationError::Pmem);
    }
    let queue = record
        .virtio
        .queues()
        .first()
        .ok_or(GraphValidationError::Virtio)?;
    if queue.max_size() != VIRTIO_PMEM_QUEUE_SIZE
        || record.pmem.active_queue.is_some_and(|cursor| {
            cursor.next_available().wrapping_sub(cursor.next_used()) > queue.size()
        })
    {
        return Err(GraphValidationError::Pmem);
    }
    match &record.transport {
        SnapshotV2DeviceTransport::Mmio(state) => {
            validate_mmio(state).map_err(|_| GraphValidationError::Mmio)
        }
        SnapshotV2DeviceTransport::Pci(state) => {
            validate_pci(state).map_err(|_| GraphValidationError::Pci)
        }
    }
}

fn validate_pmem_limiter_relationship(
    config: Option<PmemRateLimiterConfig>,
    state: SnapshotV2PmemLimiterState,
) -> Result<(), GraphValidationError> {
    for (config, state) in [
        (
            config.and_then(PmemRateLimiterConfig::bandwidth),
            state.bandwidth,
        ),
        (config.and_then(PmemRateLimiterConfig::ops), state.ops),
    ] {
        match (config, state) {
            (Some(config), Some(state))
                if pmem_token_bucket_is_enabled(config)
                    && state.budget <= config.size()
                    && state.remaining_burst <= config.one_time_burst().unwrap_or(0) => {}
            (None, None) => {}
            _ => return Err(GraphValidationError::Pmem),
        }
    }
    Ok(())
}

fn pmem_token_bucket_is_enabled(config: PmemTokenBucketConfig) -> bool {
    config.size() != 0
        && config
            .refill_time()
            .checked_mul(1_000_000)
            .is_some_and(|nanos| nanos != 0)
}

#[derive(Clone, Copy)]
enum StorageRecordRef<'a> {
    Block(&'a SnapshotV2MultiBlockDeviceRecord),
    Pmem(&'a SnapshotV2PmemDeviceRecord),
}

impl<'a> StorageRecordRef<'a> {
    fn selector(self) -> &'a str {
        match self {
            Self::Block(record) => record.config().selector(),
            Self::Pmem(record) => record.config.selector(),
        }
    }

    const fn transport(self) -> &'a SnapshotV2DeviceTransport {
        match self {
            Self::Block(record) => record.transport(),
            Self::Pmem(record) => record.transport(),
        }
    }

    fn queue(self) -> Result<&'a SnapshotV2VirtioQueueState, GraphValidationError> {
        let queues = match self {
            Self::Block(record) => record.virtio().queues(),
            Self::Pmem(record) => record.virtio.queues(),
        };
        queues.first().ok_or(GraphValidationError::Virtio)
    }

    const fn pmem_range(self) -> Option<GuestMemoryRange> {
        match self {
            Self::Block(_) => None,
            Self::Pmem(record) => Some(record.pmem.guest_range),
        }
    }
}

fn storage_record_at(
    graph: &SnapshotV2StorageDeviceGraph,
    index: usize,
) -> Option<StorageRecordRef<'_>> {
    if let Some(record) = graph.block_records.get(index) {
        return Some(StorageRecordRef::Block(record));
    }
    graph
        .pmem_records
        .get(index.checked_sub(graph.block_records.len())?)
        .map(StorageRecordRef::Pmem)
}

fn validate_record_pair(
    left: StorageRecordRef<'_>,
    right: StorageRecordRef<'_>,
) -> Result<(), GraphValidationError> {
    if left.selector() == right.selector()
        || placement(left).overlaps(placement(right))
        || matches!(
            (left.pmem_range(), right.pmem_range()),
            (Some(left), Some(right)) if left.overlaps(right)
        )
    {
        return Err(GraphValidationError::Conflict);
    }
    match (left.transport(), right.transport()) {
        (SnapshotV2DeviceTransport::Mmio(first), SnapshotV2DeviceTransport::Mmio(second))
            if first.region().id() == second.region().id()
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
    if ranges_overlap(queue_ranges_for(left)?, queue_ranges_for(right)?) {
        return Err(GraphValidationError::Conflict);
    }
    for pmem_range in [left.pmem_range(), right.pmem_range()]
        .into_iter()
        .flatten()
    {
        let other = if left.pmem_range() == Some(pmem_range) {
            right
        } else {
            left
        };
        if placement(other).overlaps(pmem_range)
            || queue_ranges_for(other)?
                .is_some_and(|ranges| ranges.iter().any(|range| range.overlaps(pmem_range)))
        {
            return Err(GraphValidationError::Conflict);
        }
    }
    Ok(())
}

fn validate_record_against_product(
    graph: &SnapshotV2StorageDeviceGraph,
    record: StorageRecordRef<'_>,
) -> Result<(), GraphValidationError> {
    if let Some(ranges) = queue_ranges_for(record)? {
        for range in ranges {
            if (0..graph.record_count()).any(|index| {
                storage_record_at(graph, index)
                    .is_some_and(|candidate| range.overlaps(placement(candidate)))
            }) {
                return Err(GraphValidationError::Conflict);
            }
            if graph
                .pmem_records
                .iter()
                .any(|pmem| range.overlaps(pmem.pmem.guest_range))
            {
                return Err(GraphValidationError::Conflict);
            }
        }
    }
    if let Some(pmem_range) = record.pmem_range()
        && placement(record).overlaps(pmem_range)
    {
        return Err(GraphValidationError::Conflict);
    }
    Ok(())
}

fn queue_ranges_for(
    record: StorageRecordRef<'_>,
) -> Result<Option<[GuestMemoryRange; 3]>, GraphValidationError> {
    queue_ranges(record.queue()?).map_err(|_| GraphValidationError::Virtio)
}

fn ranges_overlap(
    left: Option<[GuestMemoryRange; 3]>,
    right: Option<[GuestMemoryRange; 3]>,
) -> bool {
    matches!(
        (left, right),
        (Some(left), Some(right))
            if left
                .iter()
                .any(|left| right.iter().any(|right| left.overlaps(*right)))
    )
}

fn placement(record: StorageRecordRef<'_>) -> GuestMemoryRange {
    match record.transport() {
        SnapshotV2DeviceTransport::Mmio(state) => state.region().range(),
        SnapshotV2DeviceTransport::Pci(state) => state.bar_range(),
    }
}
