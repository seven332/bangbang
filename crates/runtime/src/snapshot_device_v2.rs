//! Canonical native-v2 device-graph artifact model and payload codec.

use std::fmt;
use std::mem::size_of;
use std::time::{Duration, Instant};

use crate::block::{
    BlockCaptureIoEngine, BlockFileBacking, DriveCacheType, DriveConfig, DriveConfigError,
    DriveConfigInput, DriveIoEngine, DriveRateLimiterConfig, DriveTokenBucketConfig,
    VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE, VIRTIO_BLOCK_DEVICE_ID, VIRTIO_BLOCK_ID_BYTES,
    VIRTIO_BLOCK_QUEUE_SIZE, VIRTIO_BLOCK_QUEUE_SIZES, VIRTIO_BLOCK_SECTOR_SHIFT,
    VIRTIO_RING_FEATURE_EVENT_IDX, VIRTIO_RING_FEATURE_INDIRECT_DESC, VirtioBlockConfigSpace,
    VirtioBlockDevice, VirtioBlockDeviceId, VirtioBlockMmioHandler, VirtioBlockQueue,
    VirtioBlockRateLimiter, VirtioBlockRateLimiterState, VirtioBlockRuntimeStateError,
    VirtioBlockTokenBucketState, restore_prepared_block_mmio_handler,
};
use crate::interrupt::{DeviceInterruptKind, DeviceInterruptStatus, GuestInterruptLine};
use crate::memory::{GuestAddress, GuestMemory, GuestMemoryRange};
use crate::message_interrupt::GuestMessageInterruptRegistry;
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::pci::{
    PCI_BAR64_SIZE, PCI_BAR64_START, PCI_BUS_ZERO, PCI_FIRST_ENDPOINT_DEVICE, PCI_FUNCTION_ZERO,
    PCI_LAST_ENDPOINT_DEVICE, PCI_SEGMENT_ZERO, PciBarAddressSpace, PciBarPrefetchable, PciSbdf,
    PciType0GuestState,
};
use crate::snapshot_format::SnapshotFormatVersion;
use crate::snapshot_format_v2::NATIVE_V2_SNAPSHOT_VERSION;
use crate::storage_capture::{
    CaptureReadyBlockDeviceState, StorageDeviceOrigin, StorageRetryState, StorageTransportState,
};
use crate::virtio::{
    VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DEVICE_NEEDS_RESET,
    VIRTIO_DEVICE_STATUS_DRIVER, VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FAILED,
    VIRTIO_DEVICE_STATUS_FEATURES_OK, VIRTIO_DEVICE_STATUS_INIT, VirtioDeviceType,
    VirtioDeviceTypeError, VirtioInterruptIntent,
};
use crate::virtio_mmio::{
    VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VIRTIO_MMIO_VENDOR_ID, VIRTIO_MMIO_VERSION_1_FEATURE,
    VirtioMmioDeviceRegisters, VirtioMmioQueueState, VirtioMmioTransportState,
};
use crate::virtio_pci::{
    PreparedVirtioPciEndpoint, VIRTIO_PCI_CAPABILITY_BAR_INDEX, VIRTIO_PCI_CAPABILITY_BAR_SIZE,
    VIRTIO_PCI_MAX_MSIX_VECTORS, VIRTIO_PCI_NO_VECTOR, VirtioPciEndpointError,
    VirtioPciEndpointPhase, VirtioPciIdentity, VirtioPciMsixState, VirtioPciTransportState,
};

/// Exact outer native-v2 version understood by the first device-graph codec.
pub const NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION: SnapshotFormatVersion =
    SnapshotFormatVersion::new(2, 4, 0);

const _: () = assert!(
    NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION.major() == NATIVE_V2_SNAPSHOT_VERSION.major()
        && NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION.minor()
            == NATIVE_V2_SNAPSHOT_VERSION.minor()
        && NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION.patch()
            == NATIVE_V2_SNAPSHOT_VERSION.patch()
);

/// Maximum encoded size of one native-v2 2.4 device graph.
pub const NATIVE_V2_DEVICE_GRAPH_MAX_BYTES: usize = 64 * 1024;

/// Maximum UTF-8 byte length of one public drive identifier.
pub const NATIVE_V2_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES: usize = 255;

/// Maximum UTF-8 byte length of one optional partition identifier.
pub const NATIVE_V2_DEVICE_GRAPH_MAX_PARTUUID_BYTES: usize = 255;

/// Maximum UTF-8 byte length of one inert logical backing selector.
pub const NATIVE_V2_DEVICE_GRAPH_MAX_SELECTOR_BYTES: usize = 4096;

/// Fixed device-graph payload header size.
pub const NATIVE_V2_DEVICE_GRAPH_HEADER_BYTES: usize = 64;

/// Fixed encoded size of one record-directory entry.
pub const NATIVE_V2_DEVICE_GRAPH_RECORD_ENTRY_BYTES: usize = 32;

/// Fixed encoded size of one section-directory entry.
pub const NATIVE_V2_DEVICE_GRAPH_SECTION_ENTRY_BYTES: usize = 32;

const DEVICE_GRAPH_MAGIC: [u8; 8] = *b"BANGD2A\0";
const DEVICE_GRAPH_PROFILE: u16 = 1;
const DEVICE_GRAPH_FLAGS: u32 = 0;
const DEVICE_GRAPH_ALIGNMENT: usize = 8;
const DEVICE_GRAPH_RECORD_COUNT: u16 = 1;
const DEVICE_GRAPH_SECTION_COUNT: u16 = 4;
const DEVICE_GRAPH_SECTION_COUNT_USIZE: usize = 4;
const DEVICE_GRAPH_SECTION_COUNT_U32: u32 = 4;
const DEVICE_GRAPH_RECORD_DIRECTORY_OFFSET: usize = NATIVE_V2_DEVICE_GRAPH_HEADER_BYTES;
const DEVICE_GRAPH_SECTION_DIRECTORY_OFFSET: usize =
    DEVICE_GRAPH_RECORD_DIRECTORY_OFFSET + NATIVE_V2_DEVICE_GRAPH_RECORD_ENTRY_BYTES;
const DEVICE_GRAPH_PAYLOAD_OFFSET: usize = DEVICE_GRAPH_SECTION_DIRECTORY_OFFSET
    + NATIVE_V2_DEVICE_GRAPH_SECTION_ENTRY_BYTES * DEVICE_GRAPH_SECTION_COUNT_USIZE;

const HEADER_MAGIC_OFFSET: usize = 0;
const HEADER_BYTES_OFFSET: usize = 8;
const HEADER_PROFILE_OFFSET: usize = 10;
const HEADER_TRANSPORT_OFFSET: usize = 12;
const HEADER_RECORD_COUNT_OFFSET: usize = 14;
const HEADER_SECTION_COUNT_OFFSET: usize = 16;
const HEADER_RESERVED_OFFSET: usize = 18;
const HEADER_FLAGS_OFFSET: usize = 20;
const HEADER_TOTAL_LENGTH_OFFSET: usize = 24;
const HEADER_ROOT_KIND_OFFSET: usize = 32;
const HEADER_ROOT_INSTANCE_OFFSET: usize = 36;
const HEADER_RECORD_DIRECTORY_OFFSET_OFFSET: usize = 40;
const HEADER_SECTION_DIRECTORY_OFFSET_OFFSET: usize = 48;
const HEADER_PAYLOAD_OFFSET_OFFSET: usize = 56;

const RECORD_KIND_OFFSET: usize = 0;
const RECORD_INSTANCE_OFFSET: usize = 4;
const RECORD_FIRST_SECTION_OFFSET: usize = 8;
const RECORD_SECTION_COUNT_OFFSET: usize = 12;
const RECORD_RESERVED_OFFSET: usize = 16;

const SECTION_RECORD_INDEX_OFFSET: usize = 0;
const SECTION_KIND_OFFSET: usize = 4;
const SECTION_FLAGS_OFFSET: usize = 6;
const SECTION_RESERVED_OFFSET: usize = 8;
const SECTION_PAYLOAD_OFFSET: usize = 16;
const SECTION_LENGTH_OFFSET: usize = 24;

const CONFIG_FIXED_BYTES: usize = 80;
const BLOCK_SECTION_BYTES: usize = 104;
const COMMON_FIXED_BYTES: usize = 32;
const COMMON_QUEUE_BYTES: usize = 32;
const MMIO_SECTION_BYTES: usize = 48;
const PCI_FIXED_BYTES: usize = 72;
const PCI_WRITABLE_ENTRY_BYTES: usize = 4;
const PCI_BAR_PROBE_ENTRY_BYTES: usize = 4;
const PCI_MSIX_ENTRY_BYTES: usize = 16;
const PCI_PENDING_WORD_BYTES: usize = 8;
const PCI_QUEUE_VECTOR_BYTES: usize = 2;

const DEVICE_KIND_BLOCK: u32 = 1;
const DEVICE_INSTANCE_ROOT: u32 = 0;
const SECTION_KIND_CONFIG: u16 = 1;
const SECTION_KIND_BLOCK: u16 = 2;
const SECTION_KIND_COMMON: u16 = 3;
const SECTION_KIND_TRANSPORT: u16 = 4;
const TRANSPORT_KIND_MMIO: u16 = 1;
const TRANSPORT_KIND_PCI: u16 = 2;
const CACHE_UNSAFE: u8 = 0;
const CACHE_WRITEBACK: u8 = 1;
const ENGINE_SYNC: u8 = 1;
const RETRY_NONE: u8 = 0;
const RETRY_IMMEDIATE: u8 = 1;
const RETRY_AFTER: u8 = 2;
const INTERRUPT_QUEUE: u8 = 1;
const INTERRUPT_CONFIGURATION: u8 = 2;
const PCI_PHASE_ACTIVE: u8 = 1;
const PCI_ORIGIN_STARTUP: u8 = 1;
const PCI_BAR_MEMORY64: u8 = 2;
const PCI_BAR_NOT_PREFETCHABLE: u8 = 0;
const PCI_GENERIC_WRITABLE_BYTES: [(u16, u8); 4] =
    [(0x04, 0xff), (0x05, 0xff), (0x0c, 0xff), (0x3c, 0xff)];
const REDACTED: &str = "<redacted>";

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

/// Canonical typed identity of one device-graph record.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SnapshotV2DeviceKey {
    kind: u32,
    instance: u32,
}

impl SnapshotV2DeviceKey {
    /// Returns the stable record kind.
    pub const fn kind(self) -> u32 {
        self.kind
    }

    /// Returns the stable kind-local instance.
    pub const fn instance(self) -> u32 {
        self.instance
    }
}

redacted_debug!(SnapshotV2DeviceKey, "SnapshotV2DeviceKey");

/// Transport profile selected by the complete singleton graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2DeviceTransportKind {
    /// Modern virtio-mmio placement.
    Mmio,
    /// Modern, non-transitional virtio-pci placement.
    Pci,
}

/// One stable device-graph interrupt intent.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SnapshotV2InterruptIntent {
    /// Completion on one concrete virtqueue.
    Queue { queue_index: u16 },
    /// Device configuration changed.
    Configuration,
}

impl fmt::Debug for SnapshotV2InterruptIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SnapshotV2InterruptIntent(<redacted>)")
    }
}

/// Canonical public configuration of the first root block record.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2RootBlockConfig {
    drive_id: String,
    partuuid: Option<String>,
    cache_type: DriveCacheType,
    rate_limiter: Option<DriveRateLimiterConfig>,
    selector: String,
}

impl SnapshotV2RootBlockConfig {
    /// Returns the stable public drive identifier.
    pub fn drive_id(&self) -> &str {
        &self.drive_id
    }

    /// Returns the optional stable partition identifier.
    pub fn partuuid(&self) -> Option<&str> {
        self.partuuid.as_deref()
    }

    /// Returns the public cache policy.
    pub const fn cache_type(&self) -> DriveCacheType {
        self.cache_type
    }

    /// Returns the public rate-limiter configuration.
    pub const fn rate_limiter(&self) -> Option<DriveRateLimiterConfig> {
        self.rate_limiter
    }

    /// Returns the inert logical backing selector.
    pub fn selector(&self) -> &str {
        &self.selector
    }

    /// Returns the fixed read-only policy.
    pub const fn is_read_only(&self) -> bool {
        true
    }

    /// Returns the fixed synchronous I/O policy.
    pub const fn io_engine(&self) -> DriveIoEngine {
        DriveIoEngine::Sync
    }
}

redacted_debug!(SnapshotV2RootBlockConfig, "SnapshotV2RootBlockConfig");

/// Live value of one configured block token bucket.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2BlockBucketState {
    budget: u64,
    remaining_burst: u64,
    age_nanos: u64,
}

impl SnapshotV2BlockBucketState {
    /// Returns the current ordinary-token budget.
    pub const fn budget(self) -> u64 {
        self.budget
    }

    /// Returns the remaining one-time burst.
    pub const fn remaining_burst(self) -> u64 {
        self.remaining_burst
    }

    /// Returns the monotonic age at capture.
    pub const fn age_nanos(self) -> u64 {
        self.age_nanos
    }
}

redacted_debug!(SnapshotV2BlockBucketState, "SnapshotV2BlockBucketState");

/// Live block rate-limiter state without duplicated configuration.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2BlockLimiterState {
    bandwidth: Option<SnapshotV2BlockBucketState>,
    ops: Option<SnapshotV2BlockBucketState>,
}

impl SnapshotV2BlockLimiterState {
    /// Returns the live bandwidth bucket.
    pub const fn bandwidth(self) -> Option<SnapshotV2BlockBucketState> {
        self.bandwidth
    }

    /// Returns the live operations bucket.
    pub const fn ops(self) -> Option<SnapshotV2BlockBucketState> {
        self.ops
    }

    /// Returns whether neither bucket is present.
    pub const fn is_empty(self) -> bool {
        self.bandwidth.is_none() && self.ops.is_none()
    }
}

redacted_debug!(SnapshotV2BlockLimiterState, "SnapshotV2BlockLimiterState");

/// Guest-visible block-local continuation state.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2BlockState {
    capacity_sectors: u64,
    device_id: VirtioBlockDeviceId,
    active_queue: Option<crate::block::VirtioBlockQueueState>,
    limiter: SnapshotV2BlockLimiterState,
    retry: StorageRetryState,
}

impl SnapshotV2BlockState {
    /// Returns the guest-visible capacity in 512-byte sectors.
    pub const fn capacity_sectors(&self) -> u64 {
        self.capacity_sectors
    }

    /// Returns the exact guest-visible 20-byte block identifier.
    pub const fn device_id(&self) -> VirtioBlockDeviceId {
        self.device_id
    }

    /// Returns the optional active request cursor.
    pub const fn active_queue(&self) -> Option<crate::block::VirtioBlockQueueState> {
        self.active_queue
    }

    /// Returns the live limiter state.
    pub const fn limiter(&self) -> SnapshotV2BlockLimiterState {
        self.limiter
    }

    /// Returns the host-time-free retry disposition.
    pub const fn retry(&self) -> StorageRetryState {
        self.retry
    }
}

redacted_debug!(SnapshotV2BlockState, "SnapshotV2BlockState");

/// One canonical virtqueue state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2VirtioQueueState {
    max_size: u16,
    size: u16,
    ready: bool,
    descriptor_table: GuestAddress,
    driver_ring: GuestAddress,
    device_ring: GuestAddress,
}

impl SnapshotV2VirtioQueueState {
    /// Returns the device queue maximum.
    pub const fn max_size(self) -> u16 {
        self.max_size
    }

    /// Returns the guest-selected queue size.
    pub const fn size(self) -> u16 {
        self.size
    }

    /// Returns whether the queue is ready.
    pub const fn ready(self) -> bool {
        self.ready
    }

    /// Returns the descriptor-table address.
    pub const fn descriptor_table(self) -> GuestAddress {
        self.descriptor_table
    }

    /// Returns the driver-ring address.
    pub const fn driver_ring(self) -> GuestAddress {
        self.driver_ring
    }

    /// Returns the device-ring address.
    pub const fn device_ring(self) -> GuestAddress {
        self.device_ring
    }
}

redacted_debug!(SnapshotV2VirtioQueueState, "SnapshotV2VirtioQueueState");

/// Canonical transport-neutral virtio continuation state.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2VirtioState {
    available_features: u64,
    driver_features: u64,
    config_generation: u32,
    status: u32,
    activated: bool,
    queues: Vec<SnapshotV2VirtioQueueState>,
    pending_notifications: Vec<u16>,
    interrupt_intents: Vec<SnapshotV2InterruptIntent>,
}

impl SnapshotV2VirtioState {
    /// Returns available virtio features.
    pub const fn available_features(&self) -> u64 {
        self.available_features
    }

    /// Returns guest-negotiated virtio features.
    pub const fn driver_features(&self) -> u64 {
        self.driver_features
    }

    /// Returns the device configuration generation.
    pub const fn config_generation(&self) -> u32 {
        self.config_generation
    }

    /// Returns the virtio device status.
    pub const fn status(&self) -> u32 {
        self.status
    }

    /// Returns whether device activation completed.
    pub const fn is_activated(&self) -> bool {
        self.activated
    }

    /// Returns canonical queue state.
    pub fn queues(&self) -> &[SnapshotV2VirtioQueueState] {
        &self.queues
    }

    /// Returns sorted pending notification queue indices.
    pub fn pending_notifications(&self) -> &[u16] {
        &self.pending_notifications
    }

    /// Returns sorted unique pending interrupt intents.
    pub fn interrupt_intents(&self) -> &[SnapshotV2InterruptIntent] {
        &self.interrupt_intents
    }
}

redacted_debug!(SnapshotV2VirtioState, "SnapshotV2VirtioState");

/// Canonical virtio-mmio selectors and placement.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2MmioDeviceState {
    device_feature_select: u32,
    driver_feature_select: u32,
    queue_select: u32,
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
}

impl SnapshotV2MmioDeviceState {
    /// Returns the selected device feature word.
    pub const fn device_feature_select(&self) -> u32 {
        self.device_feature_select
    }

    /// Returns the selected driver feature word.
    pub const fn driver_feature_select(&self) -> u32 {
        self.driver_feature_select
    }

    /// Returns the selected queue.
    pub const fn queue_select(&self) -> u32 {
        self.queue_select
    }

    /// Returns the exact MMIO region.
    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    /// Returns the exact interrupt line.
    pub const fn interrupt_line(&self) -> GuestInterruptLine {
        self.interrupt_line
    }
}

redacted_debug!(SnapshotV2MmioDeviceState, "SnapshotV2MmioDeviceState");

/// One generic guest-writable PCI configuration byte.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2PciWritableByte {
    offset: u16,
    value: u8,
}

impl SnapshotV2PciWritableByte {
    /// Returns the PCI configuration-space byte offset.
    pub const fn offset(self) -> u16 {
        self.offset
    }

    /// Returns the writable bits at that offset.
    pub const fn value(self) -> u8 {
        self.value
    }
}

redacted_debug!(SnapshotV2PciWritableByte, "SnapshotV2PciWritableByte");

/// One configured PCI BAR register's one-shot probe state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2PciBarProbeState {
    index: u8,
    pending: bool,
}

impl SnapshotV2PciBarProbeState {
    /// Returns the BAR register index.
    pub const fn index(self) -> u8 {
        self.index
    }

    /// Returns whether the next read reports the BAR size.
    pub const fn pending(self) -> bool {
        self.pending
    }
}

redacted_debug!(SnapshotV2PciBarProbeState, "SnapshotV2PciBarProbeState");

/// One exact guest-programmed MSI-X table entry.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2PciMsixTableEntry {
    message_address_low: u32,
    message_address_high: u32,
    message_data: u32,
    vector_control: u32,
}

impl SnapshotV2PciMsixTableEntry {
    /// Returns the low message-address word.
    pub const fn message_address_low(self) -> u32 {
        self.message_address_low
    }

    /// Returns the high message-address word.
    pub const fn message_address_high(self) -> u32 {
        self.message_address_high
    }

    /// Returns the message data.
    pub const fn message_data(self) -> u32 {
        self.message_data
    }

    /// Returns guest vector-control bits.
    pub const fn vector_control(self) -> u32 {
        self.vector_control
    }
}

redacted_debug!(SnapshotV2PciMsixTableEntry, "SnapshotV2PciMsixTableEntry");

/// Exact MSI-X continuation state without live route authority.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2PciMsixState {
    entries: Vec<SnapshotV2PciMsixTableEntry>,
    pending_words: Vec<u64>,
    enabled: bool,
    function_masked: bool,
    config_vector: u16,
    queue_vectors: Vec<u16>,
    pending_transition_observed: bool,
}

impl SnapshotV2PciMsixState {
    /// Returns canonical MSI-X table entries.
    pub fn entries(&self) -> &[SnapshotV2PciMsixTableEntry] {
        &self.entries
    }

    /// Returns canonical pending-bit-array words.
    pub fn pending_words(&self) -> &[u64] {
        &self.pending_words
    }

    /// Returns whether MSI-X is enabled.
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Returns whether the function mask is set.
    pub const fn function_masked(&self) -> bool {
        self.function_masked
    }

    /// Returns the configuration vector or the no-vector sentinel.
    pub const fn config_vector(&self) -> u16 {
        self.config_vector
    }

    /// Returns one vector per virtqueue.
    pub fn queue_vectors(&self) -> &[u16] {
        &self.queue_vectors
    }

    /// Returns whether a pending-to-deliverable transition was observed.
    pub const fn pending_transition_observed(&self) -> bool {
        self.pending_transition_observed
    }
}

redacted_debug!(SnapshotV2PciMsixState, "SnapshotV2PciMsixState");

/// Canonical virtio-pci selectors, placement, guest configuration, and MSI-X.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2PciDeviceState {
    phase: VirtioPciEndpointPhase,
    origin: StorageDeviceOrigin,
    sbdf: PciSbdf,
    bar_index: u8,
    bar_address_space: PciBarAddressSpace,
    bar_prefetchable: PciBarPrefetchable,
    bar_range: GuestMemoryRange,
    device_feature_select: u32,
    driver_feature_select: u32,
    queue_select: u16,
    pci_cfg_bar: u8,
    pci_cfg_offset: u32,
    pci_cfg_length: u32,
    writable_bytes: Vec<SnapshotV2PciWritableByte>,
    bar_probes: Vec<SnapshotV2PciBarProbeState>,
    msix: SnapshotV2PciMsixState,
}

impl SnapshotV2PciDeviceState {
    /// Returns the retained endpoint phase.
    pub const fn phase(&self) -> VirtioPciEndpointPhase {
        self.phase
    }

    /// Returns startup/runtime placement origin.
    pub const fn origin(&self) -> StorageDeviceOrigin {
        self.origin
    }

    /// Returns the exact PCI function identity.
    pub const fn sbdf(&self) -> PciSbdf {
        self.sbdf
    }

    /// Returns the capability BAR register index.
    pub const fn bar_index(&self) -> u8 {
        self.bar_index
    }

    /// Returns the BAR address-space kind.
    pub const fn bar_address_space(&self) -> PciBarAddressSpace {
        self.bar_address_space
    }

    /// Returns the BAR prefetchability policy.
    pub const fn bar_prefetchable(&self) -> PciBarPrefetchable {
        self.bar_prefetchable
    }

    /// Returns the exact capability BAR range.
    pub const fn bar_range(&self) -> GuestMemoryRange {
        self.bar_range
    }

    /// Returns the selected device feature word.
    pub const fn device_feature_select(&self) -> u32 {
        self.device_feature_select
    }

    /// Returns the selected driver feature word.
    pub const fn driver_feature_select(&self) -> u32 {
        self.driver_feature_select
    }

    /// Returns the selected queue.
    pub const fn queue_select(&self) -> u16 {
        self.queue_select
    }

    /// Returns the PCI capability BAR selector.
    pub const fn pci_cfg_bar(&self) -> u8 {
        self.pci_cfg_bar
    }

    /// Returns the PCI capability offset selector.
    pub const fn pci_cfg_offset(&self) -> u32 {
        self.pci_cfg_offset
    }

    /// Returns the PCI capability access-length selector.
    pub const fn pci_cfg_length(&self) -> u32 {
        self.pci_cfg_length
    }

    /// Returns the exact generic guest-writable PCI bytes.
    pub fn writable_bytes(&self) -> &[SnapshotV2PciWritableByte] {
        &self.writable_bytes
    }

    /// Returns the exact BAR probe state.
    pub fn bar_probes(&self) -> &[SnapshotV2PciBarProbeState] {
        &self.bar_probes
    }

    /// Returns exact MSI-X state.
    pub const fn msix(&self) -> &SnapshotV2PciMsixState {
        &self.msix
    }
}

redacted_debug!(SnapshotV2PciDeviceState, "SnapshotV2PciDeviceState");

/// Tagged transport-specific continuation state.
#[derive(Clone, PartialEq, Eq)]
pub enum SnapshotV2DeviceTransport {
    /// Exact virtio-mmio selectors and placement.
    Mmio(SnapshotV2MmioDeviceState),
    /// Exact modern virtio-pci selectors and placement.
    Pci(SnapshotV2PciDeviceState),
}

impl SnapshotV2DeviceTransport {
    /// Returns the graph transport kind.
    pub const fn kind(&self) -> SnapshotV2DeviceTransportKind {
        match self {
            Self::Mmio(_) => SnapshotV2DeviceTransportKind::Mmio,
            Self::Pci(_) => SnapshotV2DeviceTransportKind::Pci,
        }
    }
}

impl fmt::Debug for SnapshotV2DeviceTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mmio(_) => formatter.write_str("SnapshotV2DeviceTransport::Mmio(<redacted>)"),
            Self::Pci(_) => formatter.write_str("SnapshotV2DeviceTransport::Pci(<redacted>)"),
        }
    }
}

/// One canonical root block record.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2DeviceRecord {
    key: SnapshotV2DeviceKey,
    config: SnapshotV2RootBlockConfig,
    block: SnapshotV2BlockState,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
}

impl SnapshotV2DeviceRecord {
    /// Returns the stable typed key.
    pub const fn key(&self) -> SnapshotV2DeviceKey {
        self.key
    }

    /// Returns the public root-block configuration.
    pub const fn config(&self) -> &SnapshotV2RootBlockConfig {
        &self.config
    }

    /// Returns block-local continuation state.
    pub const fn block(&self) -> &SnapshotV2BlockState {
        &self.block
    }

    /// Returns common virtio state.
    pub const fn virtio(&self) -> &SnapshotV2VirtioState {
        &self.virtio
    }

    /// Returns tagged transport state.
    pub const fn transport(&self) -> &SnapshotV2DeviceTransport {
        &self.transport
    }
}

redacted_debug!(SnapshotV2DeviceRecord, "SnapshotV2DeviceRecord");

/// Fully validated native-v2 2.4 singleton device graph.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2DeviceGraph {
    root_key: SnapshotV2DeviceKey,
    record: SnapshotV2DeviceRecord,
}

impl SnapshotV2DeviceGraph {
    /// Returns the exact compatibility version of this graph profile.
    pub const fn compatibility_version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION
    }

    /// Returns the sole root-record key.
    pub const fn root_key(&self) -> SnapshotV2DeviceKey {
        self.root_key
    }

    /// Returns the sole canonical record.
    pub const fn record(&self) -> &SnapshotV2DeviceRecord {
        &self.record
    }

    /// Returns the graph-wide transport profile.
    pub const fn transport_kind(&self) -> SnapshotV2DeviceTransportKind {
        self.record.transport.kind()
    }

    /// Returns whether the sole record is selected as root.
    pub const fn record_is_root(&self) -> bool {
        self.root_key.kind == self.record.key.kind
            && self.root_key.instance == self.record.key.instance
    }

    /// Converts one capture-ready runtime root into the stable graph model.
    pub fn from_capture_ready_root(
        outer_version: SnapshotFormatVersion,
        state: &CaptureReadyBlockDeviceState,
    ) -> Result<Self, SnapshotV2DeviceGraphCaptureError> {
        capture_device_graph(outer_version, state)
    }

    /// Encodes this graph using the exact supplied outer compatibility context.
    pub fn encode(
        &self,
        outer_version: SnapshotFormatVersion,
    ) -> Result<Vec<u8>, SnapshotV2DeviceGraphEncodeError> {
        encode_snapshot_v2_device_graph(outer_version, self)
    }

    /// Decodes and validates one graph using its exact outer compatibility context.
    pub fn decode(
        outer_version: SnapshotFormatVersion,
        bytes: &[u8],
    ) -> Result<Self, SnapshotV2DeviceGraphDecodeError> {
        decode_snapshot_v2_device_graph(outer_version, bytes)
    }
}

redacted_debug!(SnapshotV2DeviceGraph, "SnapshotV2DeviceGraph");

/// Proof that one validated 2.4 root graph can continue against loaded memory.
///
/// The selector remains inert data until the destination authority layer
/// resolves it. No path is retained after [`Self::prepare_backing`] succeeds.
pub struct SnapshotV2RootRestorePlan {
    selector: String,
    drive_id: String,
    partuuid: Option<String>,
    cache_type: DriveCacheType,
    rate_limiter_config: Option<DriveRateLimiterConfig>,
    capacity_sectors: u64,
    device_id: VirtioBlockDeviceId,
    queue_ranges: Option<[GuestMemoryRange; 3]>,
    active_queue: Option<VirtioBlockQueue>,
    rate_limiter: Option<VirtioBlockRateLimiter>,
    retry: StorageRetryState,
    retry_deadline: Option<Instant>,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
}

impl SnapshotV2RootRestorePlan {
    /// Validates a complete graph against already-loaded guest memory.
    ///
    /// Queue geometry must be wholly contained by individual memory regions;
    /// an otherwise readable range spanning adjacent regions is rejected.
    /// Active queue cursors and limiter time state are also restored here so
    /// all untrusted continuation data is rejected before backing access.
    pub fn prepare(
        graph: SnapshotV2DeviceGraph,
        memory: &GuestMemory,
        now: Instant,
    ) -> Result<Self, SnapshotV2RootRestorePlanError> {
        validate_graph(&graph).map_err(|_| SnapshotV2RootRestorePlanError::InvalidGraph)?;

        let SnapshotV2DeviceGraph { record, .. } = graph;
        let SnapshotV2DeviceRecord {
            config,
            block,
            virtio,
            transport,
            ..
        } = record;
        let SnapshotV2RootBlockConfig {
            drive_id,
            partuuid,
            cache_type,
            rate_limiter,
            selector,
        } = config;
        let SnapshotV2BlockState {
            capacity_sectors,
            device_id,
            active_queue,
            limiter,
            retry,
        } = block;
        let queue_state = *virtio
            .queues
            .first()
            .ok_or(SnapshotV2RootRestorePlanError::InvalidGraph)?;

        let queue_ranges =
            queue_ranges(&queue_state).map_err(|_| SnapshotV2RootRestorePlanError::InvalidGraph)?;
        if let Some(ranges) = queue_ranges
            && ranges
                .into_iter()
                .any(|range| !range_is_wholly_contained(memory, range))
        {
            return Err(SnapshotV2RootRestorePlanError::QueueMemory);
        }

        let active_queue = active_queue
            .map(|cursor| {
                let queue = VirtioMmioQueueState::from_parts(
                    queue_state.max_size,
                    queue_state.size,
                    queue_state.ready,
                    queue_state.descriptor_table,
                    queue_state.driver_ring,
                    queue_state.device_ring,
                );
                let event_idx_enabled =
                    feature_enabled(virtio.driver_features, VIRTIO_RING_FEATURE_EVENT_IDX);
                let indirect_descriptors_enabled =
                    feature_enabled(virtio.driver_features, VIRTIO_RING_FEATURE_INDIRECT_DESC);
                let queue = VirtioBlockQueue::from_snapshot_state(
                    &queue,
                    cursor,
                    event_idx_enabled,
                    indirect_descriptors_enabled,
                )
                .map_err(|_| SnapshotV2RootRestorePlanError::QueueContinuation)?;
                queue
                    .validate_snapshot_state(memory, retry != StorageRetryState::None)
                    .map_err(|_| SnapshotV2RootRestorePlanError::QueueContinuation)?;
                Ok(queue)
            })
            .transpose()?;

        let rate_limiter_config = rate_limiter;
        let limiter = persisted_limiter_state(rate_limiter_config, limiter)?;
        let rate_limiter =
            VirtioBlockRateLimiter::from_persisted_state_at(rate_limiter_config, limiter, now)
                .map_err(|_| SnapshotV2RootRestorePlanError::RateLimiter)?;
        let retry_deadline = restored_retry_deadline_at(retry, now);

        Ok(Self {
            selector,
            drive_id,
            partuuid,
            cache_type,
            rate_limiter_config,
            capacity_sectors,
            device_id,
            queue_ranges,
            active_queue,
            rate_limiter,
            retry,
            retry_deadline,
            virtio,
            transport,
        })
    }

    /// Returns the inert, untrusted backing selector.
    pub fn selector(&self) -> &str {
        &self.selector
    }

    /// Reconstructs the validated public controller projection before backing
    /// authority consumes the inert selector.
    pub fn drive_config(&self) -> Result<DriveConfig, DriveConfigError> {
        let mut input = DriveConfigInput::new(
            self.drive_id.clone(),
            self.drive_id.clone(),
            self.selector.clone(),
            true,
        )
        .with_is_read_only(true)
        .with_cache_type(self.cache_type)
        .with_io_engine(DriveIoEngine::Sync);
        if let Some(partuuid) = &self.partuuid {
            input = input.with_partuuid(partuuid.clone());
        }
        if let Some(rate_limiter) = self.rate_limiter_config {
            input = input.with_rate_limiter(rate_limiter);
        }
        input.validate()
    }

    /// Returns the stable public drive identifier.
    pub fn drive_id(&self) -> &str {
        &self.drive_id
    }

    /// Returns the optional root partition identifier.
    pub fn partuuid(&self) -> Option<&str> {
        self.partuuid.as_deref()
    }

    /// Returns the graph-selected transport kind.
    pub const fn transport_kind(&self) -> SnapshotV2DeviceTransportKind {
        self.transport.kind()
    }

    /// Returns the validated common virtio continuation.
    pub const fn virtio(&self) -> &SnapshotV2VirtioState {
        &self.virtio
    }

    /// Returns the validated transport continuation.
    pub const fn transport(&self) -> &SnapshotV2DeviceTransport {
        &self.transport
    }

    /// Returns the canonical guest-visible capacity.
    pub const fn capacity_sectors(&self) -> u64 {
        self.capacity_sectors
    }

    /// Returns the validated descriptor, available, and used ring ranges.
    pub const fn queue_ranges(&self) -> Option<[GuestMemoryRange; 3]> {
        self.queue_ranges
    }

    /// Binds one already-authorized backing and removes all path-shaped data.
    pub fn prepare_backing(
        self,
        backing: BlockFileBacking,
    ) -> Result<PreparedSnapshotV2RootBlock, SnapshotV2RootBackingError> {
        if !backing.kind().is_regular_file() || !backing.is_read_only() {
            return Err(SnapshotV2RootBackingError::UnsupportedBacking);
        }
        let config_space = VirtioBlockConfigSpace::from_backing(&backing, self.cache_type);
        if config_space.config_len() != VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE
            || config_space.capacity_sectors() != self.capacity_sectors
            || !config_space.is_read_only()
            || config_space.cache_type() != self.cache_type
            || config_space.available_features() != self.virtio.available_features
        {
            return Err(SnapshotV2RootBackingError::GeometryMismatch);
        }

        let device = VirtioBlockDevice::from_snapshot_parts(
            backing,
            self.device_id,
            self.active_queue,
            self.rate_limiter,
            self.retry != StorageRetryState::None,
        );
        Ok(PreparedSnapshotV2RootBlock {
            config_space,
            device,
            retry_deadline: self.retry_deadline,
            continuation: SnapshotV2RootContinuation {
                drive_id: self.drive_id,
                partuuid: self.partuuid,
                retry: self.retry,
                virtio: self.virtio,
                transport: self.transport,
            },
        })
    }
}

redacted_debug!(SnapshotV2RootRestorePlan, "SnapshotV2RootRestorePlan");

/// Pathless root-block owner prepared for later endpoint reconstruction.
pub struct PreparedSnapshotV2RootBlock {
    config_space: VirtioBlockConfigSpace,
    device: VirtioBlockDevice,
    retry_deadline: Option<Instant>,
    continuation: SnapshotV2RootContinuation,
}

impl PreparedSnapshotV2RootBlock {
    /// Returns the canonical block configuration space.
    pub const fn config_space(&self) -> VirtioBlockConfigSpace {
        self.config_space
    }

    /// Returns the prepared pathless block device.
    pub const fn device(&self) -> &VirtioBlockDevice {
        &self.device
    }

    /// Returns the remaining guest-visible continuation.
    pub const fn continuation(&self) -> &SnapshotV2RootContinuation {
        &self.continuation
    }

    /// Returns the absolute destination retry deadline computed from the
    /// restore plan's monotonic-time baseline.
    pub const fn retry_deadline(&self) -> Option<Instant> {
        self.retry_deadline
    }

    /// Separates the prepared device, retry deadline, and value-only
    /// continuation.
    pub fn into_parts(
        self,
    ) -> (
        VirtioBlockConfigSpace,
        VirtioBlockDevice,
        Option<Instant>,
        SnapshotV2RootContinuation,
    ) {
        (
            self.config_space,
            self.device,
            self.retry_deadline,
            self.continuation,
        )
    }

    /// Reconstructs the retained transport without publishing any live bus,
    /// interrupt, BAR, or function resource.
    pub fn prepare_transport(
        self,
    ) -> Result<PreparedSnapshotV2RootTransport, SnapshotV2RootTransportRestoreError> {
        let (config_space, device, retry_deadline, continuation) = self.into_parts();
        let SnapshotV2RootContinuation {
            drive_id,
            partuuid,
            retry,
            virtio,
            transport,
        } = continuation;
        match transport {
            SnapshotV2DeviceTransport::Mmio(mmio) => {
                let retained = restore_mmio_transport_state(&virtio, &mmio)?;
                let handler = restore_prepared_block_mmio_handler(config_space, device, &retained)
                    .map_err(SnapshotV2RootTransportRestoreError::Mmio)?;
                Ok(PreparedSnapshotV2RootTransport::Mmio(
                    PreparedSnapshotV2MmioRoot {
                        drive_id,
                        partuuid,
                        retry,
                        retry_deadline,
                        region: mmio.region(),
                        interrupt_line: mmio.interrupt_line(),
                        handler,
                    },
                ))
            }
            SnapshotV2DeviceTransport::Pci(pci) => {
                let device_type = VirtioDeviceType::new(VIRTIO_BLOCK_DEVICE_ID)
                    .map_err(SnapshotV2RootTransportRestoreError::DeviceType)?;
                let identity = VirtioPciIdentity::new(device_type, virtio.available_features())
                    .with_config_generation(virtio.config_generation());
                let retained =
                    VirtioPciTransportState::from_snapshot_v2_parts(identity, &virtio, &pci, false)
                        .map_err(SnapshotV2RootTransportRestoreError::Pci)?;
                Ok(PreparedSnapshotV2RootTransport::Pci(
                    PreparedSnapshotV2PciRoot {
                        drive_id,
                        partuuid,
                        retry,
                        retry_deadline,
                        origin: pci.origin(),
                        sbdf: pci.sbdf(),
                        bar_range: pci.bar_range(),
                        config_space,
                        device,
                        identity,
                        retained,
                    },
                ))
            }
        }
    }
}

redacted_debug!(PreparedSnapshotV2RootBlock, "PreparedSnapshotV2RootBlock");

/// One fully reconstructed, still-unpublished exact root transport.
pub enum PreparedSnapshotV2RootTransport {
    /// Checked virtio-mmio handler with exact placement and SPI metadata.
    Mmio(PreparedSnapshotV2MmioRoot),
    /// Checked retained virtio-pci state awaiting live route publication.
    Pci(PreparedSnapshotV2PciRoot),
}

impl PreparedSnapshotV2RootTransport {
    /// Returns the selected transport kind.
    pub const fn kind(&self) -> SnapshotV2DeviceTransportKind {
        match self {
            Self::Mmio(_) => SnapshotV2DeviceTransportKind::Mmio,
            Self::Pci(_) => SnapshotV2DeviceTransportKind::Pci,
        }
    }

    /// Returns the absolute destination retry deadline computed from the
    /// restore plan's monotonic-time baseline.
    pub const fn retry_deadline(&self) -> Option<Instant> {
        match self {
            Self::Mmio(root) => root.retry_deadline,
            Self::Pci(root) => root.retry_deadline,
        }
    }
}

impl fmt::Debug for PreparedSnapshotV2RootTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2RootTransport")
            .field("kind", &self.kind())
            .field("state", &"<redacted>")
            .finish()
    }
}

/// Exact unpublished MMIO root handler and owner metadata.
pub struct PreparedSnapshotV2MmioRoot {
    drive_id: String,
    partuuid: Option<String>,
    retry: StorageRetryState,
    retry_deadline: Option<Instant>,
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    handler: VirtioBlockMmioHandler,
}

impl PreparedSnapshotV2MmioRoot {
    /// Returns the stable drive identifier.
    pub fn drive_id(&self) -> &str {
        &self.drive_id
    }

    /// Returns the optional root partition identifier.
    pub fn partuuid(&self) -> Option<&str> {
        self.partuuid.as_deref()
    }

    /// Returns the retained retry disposition.
    pub const fn retry(&self) -> StorageRetryState {
        self.retry
    }

    /// Returns the exact MMIO placement.
    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    /// Returns the exact guest interrupt line.
    pub const fn interrupt_line(&self) -> GuestInterruptLine {
        self.interrupt_line
    }

    /// Consumes the unpublished handler and metadata.
    pub fn into_parts(
        self,
    ) -> (
        String,
        Option<String>,
        StorageRetryState,
        MmioRegion,
        GuestInterruptLine,
        VirtioBlockMmioHandler,
    ) {
        (
            self.drive_id,
            self.partuuid,
            self.retry,
            self.region,
            self.interrupt_line,
            self.handler,
        )
    }
}

redacted_debug!(PreparedSnapshotV2MmioRoot, "PreparedSnapshotV2MmioRoot");

/// Exact unpublished PCI root state awaiting destination-owned resources.
pub struct PreparedSnapshotV2PciRoot {
    drive_id: String,
    partuuid: Option<String>,
    retry: StorageRetryState,
    retry_deadline: Option<Instant>,
    origin: StorageDeviceOrigin,
    sbdf: PciSbdf,
    bar_range: GuestMemoryRange,
    config_space: VirtioBlockConfigSpace,
    device: VirtioBlockDevice,
    identity: VirtioPciIdentity,
    retained: VirtioPciTransportState,
}

impl PreparedSnapshotV2PciRoot {
    /// Returns the stable drive identifier.
    pub fn drive_id(&self) -> &str {
        &self.drive_id
    }

    /// Returns the optional root partition identifier.
    pub fn partuuid(&self) -> Option<&str> {
        self.partuuid.as_deref()
    }

    /// Returns the retained retry disposition.
    pub const fn retry(&self) -> StorageRetryState {
        self.retry
    }

    /// Returns the retained startup/runtime origin.
    pub const fn origin(&self) -> StorageDeviceOrigin {
        self.origin
    }

    /// Returns the exact PCI function identity.
    pub const fn sbdf(&self) -> PciSbdf {
        self.sbdf
    }

    /// Returns the exact capability BAR range.
    pub const fn bar_range(&self) -> GuestMemoryRange {
        self.bar_range
    }

    /// Completes checked endpoint preparation against one live route registry.
    pub fn prepare_endpoint(
        self,
        region_id: MmioRegionId,
        messages: GuestMessageInterruptRegistry,
    ) -> Result<PreparedSnapshotV2PciRootEndpoint, SnapshotV2RootTransportRestoreError> {
        let endpoint = PreparedVirtioPciEndpoint::new(
            self.identity,
            &VIRTIO_BLOCK_QUEUE_SIZES,
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
        Ok(PreparedSnapshotV2PciRootEndpoint {
            drive_id: self.drive_id,
            partuuid: self.partuuid,
            retry: self.retry,
            origin: self.origin,
            endpoint,
        })
    }
}

redacted_debug!(PreparedSnapshotV2PciRoot, "PreparedSnapshotV2PciRoot");

/// Checked root endpoint plus process-visible owner metadata.
pub struct PreparedSnapshotV2PciRootEndpoint {
    drive_id: String,
    partuuid: Option<String>,
    retry: StorageRetryState,
    origin: StorageDeviceOrigin,
    endpoint: PreparedVirtioPciEndpoint<VirtioBlockConfigSpace, VirtioBlockDevice>,
}

impl PreparedSnapshotV2PciRootEndpoint {
    /// Consumes the endpoint and owner metadata.
    pub fn into_parts(
        self,
    ) -> (
        String,
        Option<String>,
        StorageRetryState,
        StorageDeviceOrigin,
        PreparedVirtioPciEndpoint<VirtioBlockConfigSpace, VirtioBlockDevice>,
    ) {
        (
            self.drive_id,
            self.partuuid,
            self.retry,
            self.origin,
            self.endpoint,
        )
    }
}

redacted_debug!(
    PreparedSnapshotV2PciRootEndpoint,
    "PreparedSnapshotV2PciRootEndpoint"
);

/// Failure while converting exact stable root state into a runtime transport.
#[derive(Debug)]
pub enum SnapshotV2RootTransportRestoreError {
    /// Allocation failed while rebuilding bounded transport state.
    Allocation,
    /// The fixed virtio-block device identity could not be represented.
    DeviceType(VirtioDeviceTypeError),
    /// The checked MMIO handler rejected retained state.
    Mmio(VirtioBlockRuntimeStateError),
    /// The checked PCI retained-state path rejected retained state.
    Pci(VirtioPciEndpointError),
}

impl fmt::Display for SnapshotV2RootTransportRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allocation => formatter.write_str("native-v2 root transport allocation failed"),
            Self::DeviceType(_) => {
                formatter.write_str("native-v2 root transport device identity is invalid")
            }
            Self::Mmio(_) => formatter.write_str("native-v2 MMIO root reconstruction failed"),
            Self::Pci(_) => formatter.write_str("native-v2 PCI root reconstruction failed"),
        }
    }
}

impl std::error::Error for SnapshotV2RootTransportRestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DeviceType(source) => Some(source),
            Self::Mmio(source) => Some(source),
            Self::Pci(source) => Some(source),
            Self::Allocation => None,
        }
    }
}

fn restore_mmio_transport_state(
    common: &SnapshotV2VirtioState,
    mmio: &SnapshotV2MmioDeviceState,
) -> Result<VirtioMmioTransportState, SnapshotV2RootTransportRestoreError> {
    let device = VirtioMmioDeviceRegisters::with_vendor_id_and_config_generation(
        VIRTIO_BLOCK_DEVICE_ID,
        VIRTIO_MMIO_VENDOR_ID,
        common.available_features(),
        common.config_generation(),
    )
    .with_runtime_state(
        [mmio.device_feature_select(), mmio.driver_feature_select()],
        common.driver_features(),
        common.status(),
    );
    let mut queues = Vec::new();
    queues
        .try_reserve_exact(common.queues().len())
        .map_err(|_| SnapshotV2RootTransportRestoreError::Allocation)?;
    for queue in common.queues() {
        queues.push(VirtioMmioQueueState::from_parts(
            queue.max_size(),
            queue.size(),
            queue.ready(),
            queue.descriptor_table(),
            queue.driver_ring(),
            queue.device_ring(),
        ));
    }
    let mut pending_notifications = Vec::new();
    pending_notifications
        .try_reserve_exact(common.queues().len())
        .map_err(|_| SnapshotV2RootTransportRestoreError::Allocation)?;
    pending_notifications.resize(common.queues().len(), false);
    for queue_index in common.pending_notifications().iter().copied() {
        let pending = pending_notifications
            .get_mut(usize::from(queue_index))
            .ok_or(SnapshotV2RootTransportRestoreError::Allocation)?;
        *pending = true;
    }
    let mut interrupt_status = DeviceInterruptStatus::empty();
    for intent in common.interrupt_intents() {
        interrupt_status.insert(match intent {
            SnapshotV2InterruptIntent::Queue { .. } => DeviceInterruptKind::Queue,
            SnapshotV2InterruptIntent::Configuration => DeviceInterruptKind::Config,
        });
    }
    Ok(VirtioMmioTransportState::from_parts(
        device,
        mmio.queue_select(),
        queues,
        pending_notifications,
        interrupt_status,
        common.is_activated(),
        true,
    ))
}

/// Pathless value state needed to reconstruct the root transport owner.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2RootContinuation {
    drive_id: String,
    partuuid: Option<String>,
    retry: StorageRetryState,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
}

impl SnapshotV2RootContinuation {
    /// Returns the stable public drive identifier.
    pub fn drive_id(&self) -> &str {
        &self.drive_id
    }

    /// Returns the optional root partition identifier.
    pub fn partuuid(&self) -> Option<&str> {
        self.partuuid.as_deref()
    }

    /// Returns the retained retry disposition.
    pub const fn retry(&self) -> StorageRetryState {
        self.retry
    }

    /// Returns the common virtio continuation.
    pub const fn virtio(&self) -> &SnapshotV2VirtioState {
        &self.virtio
    }

    /// Returns the tagged transport continuation.
    pub const fn transport(&self) -> &SnapshotV2DeviceTransport {
        &self.transport
    }
}

redacted_debug!(SnapshotV2RootContinuation, "SnapshotV2RootContinuation");

/// Failure while proving a root graph against loaded destination state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2RootRestorePlanError {
    /// The graph no longer satisfies the exact 2.4 singleton profile.
    InvalidGraph,
    /// A queue range is not wholly contained by one guest-memory region.
    QueueMemory,
    /// Active guest queue cursors or pending work are inconsistent.
    QueueContinuation,
    /// Persisted limiter state cannot be anchored at destination time.
    RateLimiter,
}

impl fmt::Display for SnapshotV2RootRestorePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGraph => formatter.write_str("native-v2 root graph is invalid"),
            Self::QueueMemory => formatter.write_str("native-v2 root queue memory is invalid"),
            Self::QueueContinuation => {
                formatter.write_str("native-v2 root queue continuation is invalid")
            }
            Self::RateLimiter => formatter.write_str("native-v2 root rate limiter is invalid"),
        }
    }
}

impl std::error::Error for SnapshotV2RootRestorePlanError {}

/// Failure while binding an authorized backing to a validated root plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2RootBackingError {
    /// Only regular, read-only backings are admitted.
    UnsupportedBacking,
    /// Guest-visible backing geometry or features differ from the graph.
    GeometryMismatch,
}

impl fmt::Display for SnapshotV2RootBackingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedBacking => {
                formatter.write_str("native-v2 root backing is unsupported")
            }
            Self::GeometryMismatch => {
                formatter.write_str("native-v2 root backing geometry is inconsistent")
            }
        }
    }
}

impl std::error::Error for SnapshotV2RootBackingError {}

/// Failure while converting detached runtime state into an artifact graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2DeviceGraphCaptureError {
    /// The outer version does not select the 2.4 graph profile.
    UnsupportedVersion,
    /// Public root configuration is outside the first profile.
    UnsupportedConfiguration,
    /// A bounded string is empty, non-UTF-8, or too large.
    InvalidString,
    /// Repeated configuration or block-local facts disagree.
    InconsistentBlockState,
    /// Common virtio state is invalid or inconsistent.
    InvalidVirtioState,
    /// MMIO placement or policy is invalid.
    InvalidMmioState,
    /// PCI placement, configuration, or MSI-X state is invalid.
    InvalidPciState,
    /// A bounded artifact collection could not be allocated.
    Allocation,
}

impl fmt::Display for SnapshotV2DeviceGraphCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion => {
                formatter.write_str("native-v2 device graph compatibility version is unsupported")
            }
            Self::UnsupportedConfiguration => {
                formatter.write_str("root block configuration is outside the device graph profile")
            }
            Self::InvalidString => formatter.write_str("root block string metadata is invalid"),
            Self::InconsistentBlockState => {
                formatter.write_str("captured root block state is inconsistent")
            }
            Self::InvalidVirtioState => {
                formatter.write_str("captured common virtio state is invalid")
            }
            Self::InvalidMmioState => formatter.write_str("captured virtio-mmio state is invalid"),
            Self::InvalidPciState => formatter.write_str("captured virtio-pci state is invalid"),
            Self::Allocation => formatter.write_str("failed to allocate a native-v2 device graph"),
        }
    }
}

impl std::error::Error for SnapshotV2DeviceGraphCaptureError {}

/// Failure while encoding a validated device graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2DeviceGraphEncodeError {
    /// The outer version does not select the 2.4 graph profile.
    UnsupportedVersion,
    /// The in-memory graph violates the exact profile.
    InvalidGraph,
    /// Encoded length exceeds the device-graph maximum.
    TooLarge,
    /// A bounded output allocation failed.
    Allocation,
}

impl fmt::Display for SnapshotV2DeviceGraphEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion => {
                formatter.write_str("native-v2 device graph compatibility version is unsupported")
            }
            Self::InvalidGraph => formatter.write_str("native-v2 device graph is invalid"),
            Self::TooLarge => formatter.write_str("native-v2 device graph exceeds 64 KiB"),
            Self::Allocation => {
                formatter.write_str("failed to allocate encoded native-v2 device graph")
            }
        }
    }
}

impl std::error::Error for SnapshotV2DeviceGraphEncodeError {}

/// Failure while decoding a native-v2 device-graph payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2DeviceGraphDecodeError {
    /// The outer version does not select the 2.4 graph profile.
    UnsupportedVersion,
    /// The payload is shorter than its fixed header.
    TooSmall,
    /// The payload exceeds the 64 KiB graph bound.
    TooLarge,
    /// The graph magic is invalid.
    InvalidMagic,
    /// Header profile, transport, count, or flag fields are unsupported.
    UnsupportedProfile,
    /// A reserved field or terminal padding byte is nonzero.
    NonzeroReserved,
    /// Header or directory bounds are noncanonical.
    InvalidStructure,
    /// A declared field or section is truncated.
    Truncated,
    /// A scalar discriminant or boolean is invalid.
    InvalidValue,
    /// A bounded string is empty, non-UTF-8, or too large.
    InvalidString,
    /// A bounded decoded collection could not be allocated.
    Allocation,
    /// Decoded semantics violate the exact one-root profile.
    InvalidGraph,
}

impl fmt::Display for SnapshotV2DeviceGraphDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion => {
                formatter.write_str("native-v2 device graph compatibility version is unsupported")
            }
            Self::TooSmall => {
                formatter.write_str("native-v2 device graph is smaller than 64 bytes")
            }
            Self::TooLarge => formatter.write_str("native-v2 device graph exceeds 64 KiB"),
            Self::InvalidMagic => formatter.write_str("native-v2 device graph magic is invalid"),
            Self::UnsupportedProfile => {
                formatter.write_str("native-v2 device graph profile is unsupported")
            }
            Self::NonzeroReserved => {
                formatter.write_str("native-v2 device graph reserved bytes are nonzero")
            }
            Self::InvalidStructure => {
                formatter.write_str("native-v2 device graph structure is noncanonical")
            }
            Self::Truncated => formatter.write_str("native-v2 device graph is truncated"),
            Self::InvalidValue => {
                formatter.write_str("native-v2 device graph scalar value is invalid")
            }
            Self::InvalidString => {
                formatter.write_str("native-v2 device graph string metadata is invalid")
            }
            Self::Allocation => {
                formatter.write_str("failed to allocate decoded native-v2 device graph")
            }
            Self::InvalidGraph => {
                formatter.write_str("native-v2 device graph semantics are invalid")
            }
        }
    }
}

impl std::error::Error for SnapshotV2DeviceGraphDecodeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraphValidationError {
    Root,
    Configuration,
    Block,
    Virtio,
    Mmio,
    Pci,
}

fn capture_device_graph(
    outer_version: SnapshotFormatVersion,
    state: &CaptureReadyBlockDeviceState,
) -> Result<SnapshotV2DeviceGraph, SnapshotV2DeviceGraphCaptureError> {
    validate_compatibility_version(outer_version)
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::UnsupportedVersion)?;
    let config = capture_root_config(state.config())?;
    let block = capture_block_state(state, &config)?;
    let expected_features = expected_block_features(config.cache_type());

    let (virtio, transport) = match state.transport() {
        StorageTransportState::Mmio(mmio) => {
            let virtio = capture_mmio_common(mmio.transport(), expected_features)?;
            let transport =
                capture_mmio_transport(mmio.region(), mmio.interrupt_line(), mmio.transport())?;
            (virtio, SnapshotV2DeviceTransport::Mmio(transport))
        }
        StorageTransportState::Pci(pci) => {
            let virtio = capture_pci_common(pci.transport(), expected_features)?;
            let transport =
                capture_pci_transport(pci.origin(), pci.sbdf(), pci.bar_range(), pci.transport())?;
            (virtio, SnapshotV2DeviceTransport::Pci(transport))
        }
    };

    let key = SnapshotV2DeviceKey {
        kind: DEVICE_KIND_BLOCK,
        instance: DEVICE_INSTANCE_ROOT,
    };
    let graph = SnapshotV2DeviceGraph {
        root_key: key,
        record: SnapshotV2DeviceRecord {
            key,
            config,
            block,
            virtio,
            transport,
        },
    };
    validate_graph(&graph).map_err(capture_validation_error)?;
    Ok(graph)
}

fn capture_root_config(
    config: &DriveConfig,
) -> Result<SnapshotV2RootBlockConfig, SnapshotV2DeviceGraphCaptureError> {
    if !config.is_root_device()
        || config.is_vhost_user()
        || config.is_read_only() != Some(true)
        || config.io_engine() != Some(DriveIoEngine::Sync)
    {
        return Err(SnapshotV2DeviceGraphCaptureError::UnsupportedConfiguration);
    }
    validate_nonempty_string(config.drive_id(), NATIVE_V2_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES)
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::InvalidString)?;
    if let Some(partuuid) = config.partuuid() {
        validate_nonempty_string(partuuid, NATIVE_V2_DEVICE_GRAPH_MAX_PARTUUID_BYTES)
            .map_err(|_| SnapshotV2DeviceGraphCaptureError::InvalidString)?;
    }
    let selector = config
        .path_on_host()
        .and_then(|path| path.to_str())
        .ok_or(SnapshotV2DeviceGraphCaptureError::InvalidString)?;
    validate_nonempty_string(selector, NATIVE_V2_DEVICE_GRAPH_MAX_SELECTOR_BYTES)
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::InvalidString)?;
    validate_limiter_config(config.rate_limiter())
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::UnsupportedConfiguration)?;

    let mut drive_id = String::new();
    drive_id
        .try_reserve_exact(config.drive_id().len())
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::Allocation)?;
    drive_id.push_str(config.drive_id());
    let partuuid = match config.partuuid() {
        Some(value) => {
            let mut owned = String::new();
            owned
                .try_reserve_exact(value.len())
                .map_err(|_| SnapshotV2DeviceGraphCaptureError::Allocation)?;
            owned.push_str(value);
            Some(owned)
        }
        None => None,
    };
    let mut selector_owned = String::new();
    selector_owned
        .try_reserve_exact(selector.len())
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::Allocation)?;
    selector_owned.push_str(selector);

    Ok(SnapshotV2RootBlockConfig {
        drive_id,
        partuuid,
        cache_type: config.cache_type(),
        rate_limiter: config.rate_limiter(),
        selector: selector_owned,
    })
}

fn capture_block_state(
    state: &CaptureReadyBlockDeviceState,
    config: &SnapshotV2RootBlockConfig,
) -> Result<SnapshotV2BlockState, SnapshotV2DeviceGraphCaptureError> {
    let device = state.device();
    let config_space = device.config_space();
    let backing = device.backing();
    if !matches!(device.io_engine(), BlockCaptureIoEngine::Sync)
        || config_space.config_len() != VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE
        || !config_space.is_read_only()
        || config_space.cache_type() != config.cache_type
        || !backing.kind().is_regular_file()
        || backing.len() >> VIRTIO_BLOCK_SECTOR_SHIFT != config_space.capacity_sectors()
    {
        return Err(SnapshotV2DeviceGraphCaptureError::InconsistentBlockState);
    }
    let limiter = capture_limiter_state(config.rate_limiter, device.rate_limiter())
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::InconsistentBlockState)?;
    let active_queue = device.active_queue();
    let retry = state.retry();
    if retry != StorageRetryState::None && (active_queue.is_none() || limiter.is_empty()) {
        return Err(SnapshotV2DeviceGraphCaptureError::InconsistentBlockState);
    }
    if matches!(retry, StorageRetryState::After { remaining_nanos: 0 }) {
        return Err(SnapshotV2DeviceGraphCaptureError::InconsistentBlockState);
    }
    Ok(SnapshotV2BlockState {
        capacity_sectors: config_space.capacity_sectors(),
        device_id: device.device_id(),
        active_queue,
        limiter,
        retry,
    })
}

fn capture_limiter_state(
    config: Option<DriveRateLimiterConfig>,
    state: VirtioBlockRateLimiterState,
) -> Result<SnapshotV2BlockLimiterState, ()> {
    if config.is_none() && !state.is_empty() {
        return Err(());
    }
    Ok(SnapshotV2BlockLimiterState {
        bandwidth: capture_bucket_state(
            config.and_then(DriveRateLimiterConfig::bandwidth),
            state.bandwidth(),
        )?,
        ops: capture_bucket_state(config.and_then(DriveRateLimiterConfig::ops), state.ops())?,
    })
}

fn capture_bucket_state(
    config: Option<DriveTokenBucketConfig>,
    state: Option<VirtioBlockTokenBucketState>,
) -> Result<Option<SnapshotV2BlockBucketState>, ()> {
    match (config, state) {
        (Some(config), Some(state))
            if token_bucket_is_enabled(config)
                && state.config() == config
                && state.budget() <= config.size()
                && state.remaining_burst() <= config.one_time_burst().unwrap_or(0) =>
        {
            Ok(Some(SnapshotV2BlockBucketState {
                budget: state.budget(),
                remaining_burst: state.remaining_burst(),
                age_nanos: state.age_nanos(),
            }))
        }
        (None, None) => Ok(None),
        _ => Err(()),
    }
}

fn capture_mmio_common(
    state: &VirtioMmioTransportState,
    expected_features: u64,
) -> Result<SnapshotV2VirtioState, SnapshotV2DeviceGraphCaptureError> {
    if !state.requires_device_config_write_status() {
        return Err(SnapshotV2DeviceGraphCaptureError::InvalidMmioState);
    }
    let mut intents = Vec::new();
    intents
        .try_reserve_exact(2)
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::Allocation)?;
    if state
        .interrupt_status()
        .contains(DeviceInterruptKind::Queue)
    {
        intents.push(SnapshotV2InterruptIntent::Queue { queue_index: 0 });
    }
    if state
        .interrupt_status()
        .contains(DeviceInterruptKind::Config)
    {
        intents.push(SnapshotV2InterruptIntent::Configuration);
    }
    capture_common_state(
        *state.device_registers(),
        state.queues(),
        state.pending_notifications(),
        state.is_device_activated(),
        intents,
        expected_features,
    )
}

fn capture_pci_common(
    state: &VirtioPciTransportState,
    expected_features: u64,
) -> Result<SnapshotV2VirtioState, SnapshotV2DeviceGraphCaptureError> {
    if state.requires_device_config_write_status() {
        return Err(SnapshotV2DeviceGraphCaptureError::InvalidPciState);
    }
    let queue_count = state.queues().queue_count();
    if queue_count != 1 || state.interrupt_intents().len() > 2 {
        return Err(SnapshotV2DeviceGraphCaptureError::InvalidVirtioState);
    }
    let mut queues = Vec::new();
    queues
        .try_reserve_exact(queue_count)
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::Allocation)?;
    for index in 0..queue_count {
        let index = u32::try_from(index)
            .map_err(|_| SnapshotV2DeviceGraphCaptureError::InvalidVirtioState)?;
        let queue = state
            .queues()
            .queue(index)
            .map_err(|_| SnapshotV2DeviceGraphCaptureError::InvalidVirtioState)?;
        queues.push(*queue);
    }
    let pending_indices = state.queue_notifications().pending_queue_notifications();
    if pending_indices.len() > 1 {
        return Err(SnapshotV2DeviceGraphCaptureError::InvalidVirtioState);
    }
    let mut pending = Vec::new();
    pending
        .try_reserve_exact(pending_indices.len())
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::Allocation)?;
    for index in pending_indices {
        pending.push(
            u16::try_from(index)
                .map_err(|_| SnapshotV2DeviceGraphCaptureError::InvalidVirtioState)?,
        );
    }

    let mut intents = Vec::new();
    intents
        .try_reserve_exact(state.interrupt_intents().len())
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::Allocation)?;
    for intent in state.interrupt_intents() {
        intents.push(match *intent {
            VirtioInterruptIntent::Queue { queue_index } => {
                SnapshotV2InterruptIntent::Queue { queue_index }
            }
            VirtioInterruptIntent::Configuration => SnapshotV2InterruptIntent::Configuration,
        });
    }
    intents.sort_unstable();
    if intents
        .windows(2)
        .any(|window| matches!(window, [first, second] if first == second))
    {
        return Err(SnapshotV2DeviceGraphCaptureError::InvalidVirtioState);
    }
    capture_common_state_from_owned_queues(
        *state.device_registers(),
        queues,
        pending,
        state.is_device_activated(),
        intents,
        expected_features,
    )
}

fn capture_common_state(
    registers: VirtioMmioDeviceRegisters,
    queues: &[VirtioMmioQueueState],
    pending_notifications: &[bool],
    activated: bool,
    intents: Vec<SnapshotV2InterruptIntent>,
    expected_features: u64,
) -> Result<SnapshotV2VirtioState, SnapshotV2DeviceGraphCaptureError> {
    if queues.len() != 1 || pending_notifications.len() != 1 || intents.len() > 2 {
        return Err(SnapshotV2DeviceGraphCaptureError::InvalidVirtioState);
    }
    let mut owned_queues = Vec::new();
    owned_queues
        .try_reserve_exact(queues.len())
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::Allocation)?;
    for queue in queues {
        owned_queues.push(*queue);
    }
    let mut pending = Vec::new();
    pending
        .try_reserve_exact(pending_notifications.len())
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::Allocation)?;
    for (index, is_pending) in pending_notifications.iter().copied().enumerate() {
        if is_pending {
            pending.push(
                u16::try_from(index)
                    .map_err(|_| SnapshotV2DeviceGraphCaptureError::InvalidVirtioState)?,
            );
        }
    }
    capture_common_state_from_owned_queues(
        registers,
        owned_queues,
        pending,
        activated,
        intents,
        expected_features,
    )
}

fn capture_common_state_from_owned_queues(
    registers: VirtioMmioDeviceRegisters,
    queues: Vec<VirtioMmioQueueState>,
    pending_notifications: Vec<u16>,
    activated: bool,
    intents: Vec<SnapshotV2InterruptIntent>,
    expected_features: u64,
) -> Result<SnapshotV2VirtioState, SnapshotV2DeviceGraphCaptureError> {
    if queues.len() != 1 || pending_notifications.len() > 1 || intents.len() > 2 {
        return Err(SnapshotV2DeviceGraphCaptureError::InvalidVirtioState);
    }
    if registers.device_id() != VIRTIO_BLOCK_DEVICE_ID
        || registers.vendor_id() != VIRTIO_MMIO_VENDOR_ID
        || registers.device_features() != expected_features
    {
        return Err(SnapshotV2DeviceGraphCaptureError::InvalidVirtioState);
    }
    let mut artifact_queues = Vec::new();
    artifact_queues
        .try_reserve_exact(queues.len())
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::Allocation)?;
    for queue in queues {
        artifact_queues.push(SnapshotV2VirtioQueueState {
            max_size: queue.max_size(),
            size: queue.size(),
            ready: queue.ready(),
            descriptor_table: queue.descriptor_table(),
            driver_ring: queue.driver_ring(),
            device_ring: queue.device_ring(),
        });
    }
    let state = SnapshotV2VirtioState {
        available_features: registers.device_features(),
        driver_features: registers.driver_features(),
        config_generation: registers.config_generation(),
        status: registers.status(),
        activated,
        queues: artifact_queues,
        pending_notifications,
        interrupt_intents: intents,
    };
    validate_virtio_state(&state, expected_features)
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::InvalidVirtioState)?;
    Ok(state)
}

fn capture_mmio_transport(
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    state: &VirtioMmioTransportState,
) -> Result<SnapshotV2MmioDeviceState, SnapshotV2DeviceGraphCaptureError> {
    let registers = state.device_registers();
    let mmio = SnapshotV2MmioDeviceState {
        device_feature_select: registers.device_features_select(),
        driver_feature_select: registers.driver_features_select(),
        queue_select: state.queue_select(),
        region,
        interrupt_line,
    };
    validate_mmio_state(&mmio).map_err(|_| SnapshotV2DeviceGraphCaptureError::InvalidMmioState)?;
    Ok(mmio)
}

fn capture_pci_transport(
    origin: StorageDeviceOrigin,
    sbdf: PciSbdf,
    bar_range: GuestMemoryRange,
    state: &VirtioPciTransportState,
) -> Result<SnapshotV2PciDeviceState, SnapshotV2DeviceGraphCaptureError> {
    let guest_state = state
        .checked_configuration_guest_state(bar_range)
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::InvalidPciState)?;
    let writable_bytes = capture_pci_writable_bytes(state, &guest_state)?;
    let mut bar_probes = Vec::new();
    bar_probes
        .try_reserve_exact(guest_state.bar_probes().len())
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::Allocation)?;
    for probe in guest_state.bar_probes() {
        bar_probes.push(SnapshotV2PciBarProbeState {
            index: probe.index(),
            pending: probe.pending(),
        });
    }
    let msix = capture_msix_state(state.msix_state())?;
    let pci = SnapshotV2PciDeviceState {
        phase: state.phase(),
        origin,
        sbdf,
        bar_index: VIRTIO_PCI_CAPABILITY_BAR_INDEX,
        bar_address_space: PciBarAddressSpace::Memory64,
        bar_prefetchable: PciBarPrefetchable::No,
        bar_range,
        device_feature_select: state.device_feature_select(),
        driver_feature_select: state.driver_feature_select(),
        queue_select: state.queue_select(),
        pci_cfg_bar: state.pci_cfg_bar(),
        pci_cfg_offset: state.pci_cfg_offset(),
        pci_cfg_length: state.pci_cfg_length(),
        writable_bytes,
        bar_probes,
        msix,
    };
    validate_pci_state(&pci).map_err(|_| SnapshotV2DeviceGraphCaptureError::InvalidPciState)?;
    Ok(pci)
}

fn capture_pci_writable_bytes(
    state: &VirtioPciTransportState,
    guest_state: &PciType0GuestState,
) -> Result<Vec<SnapshotV2PciWritableByte>, SnapshotV2DeviceGraphCaptureError> {
    let pci_cfg_cap = state.pci_cfg_cap_offset();
    let msix_cap = state.msix_cap_offset();
    let selector_bar_offset = pci_cfg_cap
        .checked_add(4)
        .ok_or(SnapshotV2DeviceGraphCaptureError::InvalidPciState)?;
    let selector_offset_start = pci_cfg_cap
        .checked_add(8)
        .ok_or(SnapshotV2DeviceGraphCaptureError::InvalidPciState)?;
    let selector_length_start = pci_cfg_cap
        .checked_add(12)
        .ok_or(SnapshotV2DeviceGraphCaptureError::InvalidPciState)?;
    let data_start = pci_cfg_cap
        .checked_add(16)
        .ok_or(SnapshotV2DeviceGraphCaptureError::InvalidPciState)?;
    let data_end = data_start
        .checked_add(4)
        .ok_or(SnapshotV2DeviceGraphCaptureError::InvalidPciState)?;
    let msix_control_high = msix_cap
        .checked_add(3)
        .ok_or(SnapshotV2DeviceGraphCaptureError::InvalidPciState)?;
    let selector_offset = state.pci_cfg_offset().to_le_bytes();
    let selector_length = state.pci_cfg_length().to_le_bytes();
    let expected_msix_control = (u8::from(state.msix_state().enabled()) << 7)
        | (u8::from(state.msix_state().function_masked()) << 6);

    let mut generic = Vec::new();
    generic
        .try_reserve_exact(PCI_GENERIC_WRITABLE_BYTES.len())
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::Allocation)?;
    for writable in guest_state.writable_bytes() {
        let offset = writable.offset();
        let value = writable.value();
        let mask = writable.writable_mask();
        if offset == selector_bar_offset {
            if mask != u8::MAX || value != state.pci_cfg_bar() {
                return Err(SnapshotV2DeviceGraphCaptureError::InvalidPciState);
            }
            continue;
        }
        if (selector_offset_start..selector_length_start).contains(&offset) {
            let index = usize::from(offset - selector_offset_start);
            if mask != u8::MAX || selector_offset.get(index).copied() != Some(value) {
                return Err(SnapshotV2DeviceGraphCaptureError::InvalidPciState);
            }
            continue;
        }
        if (selector_length_start..data_start).contains(&offset) {
            let index = usize::from(offset - selector_length_start);
            if mask != u8::MAX || selector_length.get(index).copied() != Some(value) {
                return Err(SnapshotV2DeviceGraphCaptureError::InvalidPciState);
            }
            continue;
        }
        if (data_start..data_end).contains(&offset) {
            if mask != u8::MAX {
                return Err(SnapshotV2DeviceGraphCaptureError::InvalidPciState);
            }
            continue;
        }
        if offset == msix_control_high {
            if mask != 0xc0 || value != expected_msix_control {
                return Err(SnapshotV2DeviceGraphCaptureError::InvalidPciState);
            }
            continue;
        }
        let Some((_, expected_mask)) = PCI_GENERIC_WRITABLE_BYTES
            .iter()
            .copied()
            .find(|(expected_offset, _)| *expected_offset == offset)
        else {
            return Err(SnapshotV2DeviceGraphCaptureError::InvalidPciState);
        };
        if mask != expected_mask {
            return Err(SnapshotV2DeviceGraphCaptureError::InvalidPciState);
        }
        generic.push(SnapshotV2PciWritableByte { offset, value });
    }
    if generic
        .iter()
        .map(|byte| (byte.offset, u8::MAX))
        .ne(PCI_GENERIC_WRITABLE_BYTES)
    {
        return Err(SnapshotV2DeviceGraphCaptureError::InvalidPciState);
    }
    Ok(generic)
}

fn capture_msix_state(
    state: &VirtioPciMsixState,
) -> Result<SnapshotV2PciMsixState, SnapshotV2DeviceGraphCaptureError> {
    if state.entries().len() != 2
        || state.pending_words().len() != 1
        || state.queue_vectors().len() != 1
    {
        return Err(SnapshotV2DeviceGraphCaptureError::InvalidPciState);
    }
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(state.entries().len())
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::Allocation)?;
    for entry in state.entries() {
        entries.push(SnapshotV2PciMsixTableEntry {
            message_address_low: entry.message_address_low(),
            message_address_high: entry.message_address_high(),
            message_data: entry.message_data(),
            vector_control: entry.vector_control(),
        });
    }
    let mut pending_words = Vec::new();
    pending_words
        .try_reserve_exact(state.pending_words().len())
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::Allocation)?;
    pending_words.extend_from_slice(state.pending_words());
    let mut queue_vectors = Vec::new();
    queue_vectors
        .try_reserve_exact(state.queue_vectors().len())
        .map_err(|_| SnapshotV2DeviceGraphCaptureError::Allocation)?;
    queue_vectors.extend_from_slice(state.queue_vectors());
    Ok(SnapshotV2PciMsixState {
        entries,
        pending_words,
        enabled: state.enabled(),
        function_masked: state.function_masked(),
        config_vector: state.config_vector(),
        queue_vectors,
        pending_transition_observed: state.pending_transition_observed(),
    })
}

fn capture_validation_error(error: GraphValidationError) -> SnapshotV2DeviceGraphCaptureError {
    match error {
        GraphValidationError::Root | GraphValidationError::Configuration => {
            SnapshotV2DeviceGraphCaptureError::UnsupportedConfiguration
        }
        GraphValidationError::Block => SnapshotV2DeviceGraphCaptureError::InconsistentBlockState,
        GraphValidationError::Virtio => SnapshotV2DeviceGraphCaptureError::InvalidVirtioState,
        GraphValidationError::Mmio => SnapshotV2DeviceGraphCaptureError::InvalidMmioState,
        GraphValidationError::Pci => SnapshotV2DeviceGraphCaptureError::InvalidPciState,
    }
}

fn validate_graph(graph: &SnapshotV2DeviceGraph) -> Result<(), GraphValidationError> {
    if graph.root_key.kind != DEVICE_KIND_BLOCK
        || graph.root_key.instance != DEVICE_INSTANCE_ROOT
        || graph.record.key != graph.root_key
    {
        return Err(GraphValidationError::Root);
    }
    validate_root_config(&graph.record.config)?;
    validate_block_state(&graph.record.config, &graph.record.block)?;
    let expected_features = expected_block_features(graph.record.config.cache_type);
    validate_virtio_state(&graph.record.virtio, expected_features)?;
    if graph.record.block.active_queue.is_some() != graph.record.virtio.activated {
        return Err(GraphValidationError::Block);
    }
    let queue = graph
        .record
        .virtio
        .queues
        .first()
        .ok_or(GraphValidationError::Virtio)?;
    validate_block_queue_cursors(&graph.record.block, queue)?;
    let placement = match &graph.record.transport {
        SnapshotV2DeviceTransport::Mmio(state) => {
            validate_mmio_state(state)?;
            state.region.range()
        }
        SnapshotV2DeviceTransport::Pci(state) => {
            validate_pci_state(state)?;
            state.bar_range
        }
    };
    if queue_ranges(queue)?
        .is_some_and(|ranges| ranges.iter().any(|range| range.overlaps(placement)))
    {
        return Err(GraphValidationError::Virtio);
    }
    Ok(())
}

fn validate_root_config(config: &SnapshotV2RootBlockConfig) -> Result<(), GraphValidationError> {
    validate_nonempty_string(&config.drive_id, NATIVE_V2_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES)
        .map_err(|_| GraphValidationError::Configuration)?;
    if !config
        .drive_id
        .chars()
        .all(|character| character == '_' || character.is_alphanumeric())
    {
        return Err(GraphValidationError::Configuration);
    }
    if let Some(partuuid) = config.partuuid.as_deref() {
        validate_nonempty_string(partuuid, NATIVE_V2_DEVICE_GRAPH_MAX_PARTUUID_BYTES)
            .map_err(|_| GraphValidationError::Configuration)?;
    }
    validate_nonempty_string(&config.selector, NATIVE_V2_DEVICE_GRAPH_MAX_SELECTOR_BYTES)
        .map_err(|_| GraphValidationError::Configuration)?;
    validate_limiter_config(config.rate_limiter)
}

fn validate_block_state(
    config: &SnapshotV2RootBlockConfig,
    block: &SnapshotV2BlockState,
) -> Result<(), GraphValidationError> {
    if block.device_id.as_bytes().iter().all(|byte| *byte == 0) {
        return Err(GraphValidationError::Block);
    }
    validate_limiter_relationship(config.rate_limiter, block.limiter)?;
    if block.retry != StorageRetryState::None
        && (block.active_queue.is_none() || block.limiter.is_empty())
    {
        return Err(GraphValidationError::Block);
    }
    if matches!(block.retry, StorageRetryState::After { remaining_nanos: 0 }) {
        return Err(GraphValidationError::Block);
    }
    if block.retry != StorageRetryState::None
        && block
            .active_queue
            .is_some_and(|queue| queue.next_available() == queue.next_used())
    {
        return Err(GraphValidationError::Block);
    }
    Ok(())
}

fn validate_block_queue_cursors(
    block: &SnapshotV2BlockState,
    queue: &SnapshotV2VirtioQueueState,
) -> Result<(), GraphValidationError> {
    if block
        .active_queue
        .is_some_and(|state| state.next_available().wrapping_sub(state.next_used()) > queue.size)
    {
        Err(GraphValidationError::Block)
    } else {
        Ok(())
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
        state.bandwidth,
    )?;
    validate_bucket_relationship(config.and_then(DriveRateLimiterConfig::ops), state.ops)
}

fn validate_bucket_relationship(
    config: Option<DriveTokenBucketConfig>,
    state: Option<SnapshotV2BlockBucketState>,
) -> Result<(), GraphValidationError> {
    match (config, state) {
        (Some(config), Some(state))
            if token_bucket_is_enabled(config)
                && state.budget <= config.size()
                && state.remaining_burst <= config.one_time_burst().unwrap_or(0) =>
        {
            Ok(())
        }
        (None, None) => Ok(()),
        _ => Err(GraphValidationError::Block),
    }
}

fn validate_virtio_state(
    state: &SnapshotV2VirtioState,
    expected_features: u64,
) -> Result<(), GraphValidationError> {
    if state.available_features != expected_features
        || state.driver_features & !state.available_features != 0
        || state.queues.len() != 1
        || state.pending_notifications.len() > 1
        || state.interrupt_intents.len() > 2
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
    if !healthy_statuses.contains(&state.status)
        || state.status & (VIRTIO_DEVICE_STATUS_FAILED | VIRTIO_DEVICE_STATUS_DEVICE_NEEDS_RESET)
            != 0
    {
        return Err(GraphValidationError::Virtio);
    }
    if state.status & VIRTIO_DEVICE_STATUS_DRIVER == 0 && state.driver_features != 0 {
        return Err(GraphValidationError::Virtio);
    }
    if state.status & VIRTIO_DEVICE_STATUS_FEATURES_OK != 0
        && state.driver_features & VIRTIO_MMIO_VERSION_1_FEATURE == 0
    {
        return Err(GraphValidationError::Virtio);
    }
    if state.activated != (state.status == healthy_driver_ok) {
        return Err(GraphValidationError::Virtio);
    }
    let queue = state.queues.first().ok_or(GraphValidationError::Virtio)?;
    validate_queue(queue)?;
    if state.activated && !queue.ready {
        return Err(GraphValidationError::Virtio);
    }
    if state.status & VIRTIO_DEVICE_STATUS_FEATURES_OK == 0
        && (queue.size != 0
            || queue.ready
            || queue.descriptor_table.raw_value() != 0
            || queue.driver_ring.raw_value() != 0
            || queue.device_ring.raw_value() != 0)
    {
        return Err(GraphValidationError::Virtio);
    }
    if !state.activated && !state.pending_notifications.is_empty() {
        return Err(GraphValidationError::Virtio);
    }
    if state
        .pending_notifications
        .iter()
        .copied()
        .any(|index| index != 0)
    {
        return Err(GraphValidationError::Virtio);
    }
    if !state
        .interrupt_intents
        .windows(2)
        .all(|window| matches!(window, [first, second] if first < second))
        || state.interrupt_intents.iter().any(|intent| {
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
    if queue.max_size != VIRTIO_BLOCK_QUEUE_SIZE
        || (queue.size != 0 && (!queue.size.is_power_of_two() || queue.size > queue.max_size))
        || (queue.ready && queue.size == 0)
        || !queue
            .descriptor_table
            .raw_value()
            .is_multiple_of(crate::virtio_queue::VIRTQUEUE_DESCRIPTOR_ALIGNMENT)
        || !queue
            .driver_ring
            .raw_value()
            .is_multiple_of(crate::virtio_queue::VIRTQUEUE_AVAILABLE_RING_ALIGNMENT)
        || !queue
            .device_ring
            .raw_value()
            .is_multiple_of(crate::virtio_queue::VIRTQUEUE_USED_RING_ALIGNMENT)
    {
        return Err(GraphValidationError::Virtio);
    }
    if queue.size == 0 {
        return Ok(());
    }
    let ranges = queue_ranges(queue)?.ok_or(GraphValidationError::Virtio)?;
    if ranges[0].overlaps(ranges[1])
        || ranges[0].overlaps(ranges[2])
        || ranges[1].overlaps(ranges[2])
    {
        return Err(GraphValidationError::Virtio);
    }
    Ok(())
}

fn queue_ranges(
    queue: &SnapshotV2VirtioQueueState,
) -> Result<Option<[GuestMemoryRange; 3]>, GraphValidationError> {
    if queue.size == 0 {
        return Ok(None);
    }
    let descriptor_size = u64::from(queue.size)
        .checked_mul(16)
        .ok_or(GraphValidationError::Virtio)?;
    let available_size = u64::from(queue.size)
        .checked_mul(2)
        .and_then(|size| size.checked_add(6))
        .ok_or(GraphValidationError::Virtio)?;
    let used_size = u64::from(queue.size)
        .checked_mul(8)
        .and_then(|size| size.checked_add(6))
        .ok_or(GraphValidationError::Virtio)?;
    let descriptor = GuestMemoryRange::new(queue.descriptor_table, descriptor_size)
        .map_err(|_| GraphValidationError::Virtio)?;
    let available = GuestMemoryRange::new(queue.driver_ring, available_size)
        .map_err(|_| GraphValidationError::Virtio)?;
    let used = GuestMemoryRange::new(queue.device_ring, used_size)
        .map_err(|_| GraphValidationError::Virtio)?;
    Ok(Some([descriptor, available, used]))
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
) -> Result<VirtioBlockRateLimiterState, SnapshotV2RootRestorePlanError> {
    Ok(VirtioBlockRateLimiterState::new(
        persisted_bucket_state(
            config.and_then(DriveRateLimiterConfig::bandwidth),
            state.bandwidth,
        )?,
        persisted_bucket_state(config.and_then(DriveRateLimiterConfig::ops), state.ops)?,
    ))
}

fn persisted_bucket_state(
    config: Option<DriveTokenBucketConfig>,
    state: Option<SnapshotV2BlockBucketState>,
) -> Result<Option<VirtioBlockTokenBucketState>, SnapshotV2RootRestorePlanError> {
    match (config, state) {
        (Some(config), Some(state)) => Ok(Some(VirtioBlockTokenBucketState::new(
            config,
            state.budget,
            state.remaining_burst,
            state.age_nanos,
        ))),
        (None, None) => Ok(None),
        _ => Err(SnapshotV2RootRestorePlanError::InvalidGraph),
    }
}

fn validate_mmio_state(state: &SnapshotV2MmioDeviceState) -> Result<(), GraphValidationError> {
    if state.device_feature_select > 1
        || state.driver_feature_select > 1
        || state.queue_select != 0
        || state.region.id().raw_value() == 0
        || state.region.range().size() != VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        || state
            .region
            .range()
            .validate_alignment(VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
            .is_err()
        || state.interrupt_line.raw_value() < 32
    {
        Err(GraphValidationError::Mmio)
    } else {
        Ok(())
    }
}

fn validate_pci_state(state: &SnapshotV2PciDeviceState) -> Result<(), GraphValidationError> {
    if state.phase != VirtioPciEndpointPhase::Active
        || state.origin != StorageDeviceOrigin::Startup
        || state.sbdf.segment() != PCI_SEGMENT_ZERO
        || state.sbdf.bus() != PCI_BUS_ZERO
        || !(PCI_FIRST_ENDPOINT_DEVICE..=PCI_LAST_ENDPOINT_DEVICE).contains(&state.sbdf.device())
        || state.sbdf.function() != PCI_FUNCTION_ZERO
        || state.bar_index != VIRTIO_PCI_CAPABILITY_BAR_INDEX
        || state.bar_address_space != PciBarAddressSpace::Memory64
        || state.bar_prefetchable != PciBarPrefetchable::No
        || state.bar_range.size() != VIRTIO_PCI_CAPABILITY_BAR_SIZE
        || state
            .bar_range
            .validate_alignment(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .is_err()
        || state.bar_range.start().raw_value() < PCI_BAR64_START
        || state.bar_range.end_exclusive().raw_value()
            > PCI_BAR64_START.saturating_add(PCI_BAR64_SIZE)
        || state.writable_bytes.len() != PCI_GENERIC_WRITABLE_BYTES.len()
        || state.bar_probes.len() != 2
    {
        return Err(GraphValidationError::Pci);
    }
    for (actual, (expected_offset, _)) in
        state.writable_bytes.iter().zip(PCI_GENERIC_WRITABLE_BYTES)
    {
        if actual.offset != expected_offset {
            return Err(GraphValidationError::Pci);
        }
    }
    if state.bar_probes.iter().map(|probe| probe.index).ne([0, 1]) {
        return Err(GraphValidationError::Pci);
    }
    validate_msix_state(&state.msix)
}

fn validate_msix_state(state: &SnapshotV2PciMsixState) -> Result<(), GraphValidationError> {
    if state.entries.len() != 2
        || state.entries.len() > VIRTIO_PCI_MAX_MSIX_VECTORS
        || state.pending_words.len() != 1
        || state.queue_vectors.len() != 1
        || state
            .pending_words
            .first()
            .copied()
            .is_none_or(|pending| pending & !0b11 != 0)
        || !valid_msix_vector(state.config_vector, state.entries.len())
        || state
            .queue_vectors
            .iter()
            .copied()
            .any(|vector| !valid_msix_vector(vector, state.entries.len()))
        || state
            .entries
            .iter()
            .any(|entry| entry.vector_control & !1 != 0)
    {
        Err(GraphValidationError::Pci)
    } else {
        Ok(())
    }
}

fn valid_msix_vector(vector: u16, vector_count: usize) -> bool {
    vector == VIRTIO_PCI_NO_VECTOR || usize::from(vector) < vector_count
}

fn validate_nonempty_string(value: &str, max: usize) -> Result<(), ()> {
    if value.is_empty() || value.len() > max {
        Err(())
    } else {
        Ok(())
    }
}

fn validate_compatibility_version(version: SnapshotFormatVersion) -> Result<(), ()> {
    if version == NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION {
        Ok(())
    } else {
        Err(())
    }
}

fn expected_block_features(cache_type: DriveCacheType) -> u64 {
    crate::block::VirtioBlockConfigSpace::new(0, true, cache_type).available_features()
}

fn token_bucket_is_enabled(config: DriveTokenBucketConfig) -> bool {
    config.size() != 0
        && config
            .refill_time()
            .checked_mul(1_000_000)
            .is_some_and(|nanos| nanos != 0)
}

/// Encodes one validated native-v2 device-graph payload.
pub fn encode_snapshot_v2_device_graph(
    outer_version: SnapshotFormatVersion,
    graph: &SnapshotV2DeviceGraph,
) -> Result<Vec<u8>, SnapshotV2DeviceGraphEncodeError> {
    validate_compatibility_version(outer_version)
        .map_err(|_| SnapshotV2DeviceGraphEncodeError::UnsupportedVersion)?;
    validate_graph(graph).map_err(|_| SnapshotV2DeviceGraphEncodeError::InvalidGraph)?;

    let sections = [
        encode_config_section(&graph.record.config)?,
        encode_block_section(&graph.record.block)?,
        encode_common_section(&graph.record.virtio)?,
        encode_transport_section(&graph.record.transport)?,
    ];
    let mut section_offsets = [0_usize; DEVICE_GRAPH_SECTION_COUNT_USIZE];
    let mut cursor = DEVICE_GRAPH_PAYLOAD_OFFSET;
    for (index, section) in sections.iter().enumerate() {
        let slot = section_offsets
            .get_mut(index)
            .ok_or(SnapshotV2DeviceGraphEncodeError::InvalidGraph)?;
        *slot = cursor;
        cursor = cursor
            .checked_add(section.len())
            .ok_or(SnapshotV2DeviceGraphEncodeError::TooLarge)?;
    }
    if cursor > NATIVE_V2_DEVICE_GRAPH_MAX_BYTES {
        return Err(SnapshotV2DeviceGraphEncodeError::TooLarge);
    }

    let mut output = Vec::new();
    output
        .try_reserve_exact(cursor)
        .map_err(|_| SnapshotV2DeviceGraphEncodeError::Allocation)?;
    output.extend_from_slice(&DEVICE_GRAPH_MAGIC);
    write_u16(
        &mut output,
        u16::try_from(NATIVE_V2_DEVICE_GRAPH_HEADER_BYTES)
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::InvalidGraph)?,
    );
    write_u16(&mut output, DEVICE_GRAPH_PROFILE);
    write_u16(&mut output, transport_kind_tag(graph.transport_kind()));
    write_u16(&mut output, DEVICE_GRAPH_RECORD_COUNT);
    write_u16(&mut output, DEVICE_GRAPH_SECTION_COUNT);
    write_u16(&mut output, 0);
    write_u32(&mut output, DEVICE_GRAPH_FLAGS);
    write_u64(
        &mut output,
        u64::try_from(cursor).map_err(|_| SnapshotV2DeviceGraphEncodeError::TooLarge)?,
    );
    write_u32(&mut output, graph.root_key.kind);
    write_u32(&mut output, graph.root_key.instance);
    write_u64(
        &mut output,
        u64::try_from(DEVICE_GRAPH_RECORD_DIRECTORY_OFFSET)
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::InvalidGraph)?,
    );
    write_u64(
        &mut output,
        u64::try_from(DEVICE_GRAPH_SECTION_DIRECTORY_OFFSET)
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::InvalidGraph)?,
    );
    write_u64(
        &mut output,
        u64::try_from(DEVICE_GRAPH_PAYLOAD_OFFSET)
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::InvalidGraph)?,
    );

    write_u32(&mut output, graph.record.key.kind);
    write_u32(&mut output, graph.record.key.instance);
    write_u32(&mut output, 0);
    write_u32(&mut output, DEVICE_GRAPH_SECTION_COUNT_U32);
    output.extend_from_slice(&[0; 16]);

    for (index, ((kind, section), offset)) in [
        SECTION_KIND_CONFIG,
        SECTION_KIND_BLOCK,
        SECTION_KIND_COMMON,
        SECTION_KIND_TRANSPORT,
    ]
    .into_iter()
    .zip(sections.iter())
    .zip(section_offsets)
    .enumerate()
    {
        let record_index = u32::try_from(index / usize::from(DEVICE_GRAPH_SECTION_COUNT))
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::InvalidGraph)?;
        write_u32(&mut output, record_index);
        write_u16(&mut output, kind);
        write_u16(&mut output, 0);
        write_u64(&mut output, 0);
        write_u64(
            &mut output,
            u64::try_from(offset).map_err(|_| SnapshotV2DeviceGraphEncodeError::TooLarge)?,
        );
        write_u64(
            &mut output,
            u64::try_from(section.len()).map_err(|_| SnapshotV2DeviceGraphEncodeError::TooLarge)?,
        );
    }
    for section in &sections {
        output.extend_from_slice(section);
    }
    if output.len() != cursor {
        return Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph);
    }
    Ok(output)
}

fn encode_config_section(
    config: &SnapshotV2RootBlockConfig,
) -> Result<Vec<u8>, SnapshotV2DeviceGraphEncodeError> {
    let partuuid_len = config.partuuid.as_ref().map_or(0, String::len);
    let semantic_len = CONFIG_FIXED_BYTES
        .checked_add(config.drive_id.len())
        .and_then(|len| len.checked_add(partuuid_len))
        .and_then(|len| len.checked_add(config.selector.len()))
        .ok_or(SnapshotV2DeviceGraphEncodeError::TooLarge)?;
    let capacity = aligned_len(semantic_len).ok_or(SnapshotV2DeviceGraphEncodeError::TooLarge)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(capacity)
        .map_err(|_| SnapshotV2DeviceGraphEncodeError::Allocation)?;
    write_u8(&mut output, 1);
    write_u8(&mut output, ENGINE_SYNC);
    write_u8(
        &mut output,
        match config.cache_type {
            DriveCacheType::Unsafe => CACHE_UNSAFE,
            DriveCacheType::Writeback => CACHE_WRITEBACK,
        },
    );
    write_u8(&mut output, u8::from(config.partuuid.is_some()));
    let bandwidth = config
        .rate_limiter
        .and_then(DriveRateLimiterConfig::bandwidth);
    let ops = config.rate_limiter.and_then(DriveRateLimiterConfig::ops);
    write_u8(&mut output, u8::from(bandwidth.is_some()));
    write_u8(&mut output, u8::from(ops.is_some()));
    output.extend_from_slice(&[0; 2]);
    write_u16(
        &mut output,
        u16::try_from(config.drive_id.len())
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::TooLarge)?,
    );
    write_u16(
        &mut output,
        u16::try_from(partuuid_len).map_err(|_| SnapshotV2DeviceGraphEncodeError::TooLarge)?,
    );
    write_u16(
        &mut output,
        u16::try_from(config.selector.len())
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::TooLarge)?,
    );
    write_u16(&mut output, 0);
    encode_bucket_config(&mut output, bandwidth);
    encode_bucket_config(&mut output, ops);
    output.extend_from_slice(config.drive_id.as_bytes());
    if let Some(partuuid) = &config.partuuid {
        output.extend_from_slice(partuuid.as_bytes());
    }
    output.extend_from_slice(config.selector.as_bytes());
    pad_section(&mut output, capacity);
    Ok(output)
}

fn encode_bucket_config(output: &mut Vec<u8>, bucket: Option<DriveTokenBucketConfig>) {
    match bucket {
        Some(bucket) => {
            write_u64(output, bucket.size());
            write_u64(output, bucket.one_time_burst().unwrap_or(0));
            write_u64(output, bucket.refill_time());
            write_u8(output, u8::from(bucket.one_time_burst().is_some()));
            output.extend_from_slice(&[0; 7]);
        }
        None => output.extend_from_slice(&[0; 32]),
    }
}

fn encode_block_section(
    block: &SnapshotV2BlockState,
) -> Result<Vec<u8>, SnapshotV2DeviceGraphEncodeError> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(BLOCK_SECTION_BYTES)
        .map_err(|_| SnapshotV2DeviceGraphEncodeError::Allocation)?;
    write_u16(
        &mut output,
        u16::try_from(VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE)
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::InvalidGraph)?,
    );
    write_u8(&mut output, u8::from(block.active_queue.is_some()));
    write_u8(&mut output, u8::from(block.limiter.bandwidth.is_some()));
    write_u8(&mut output, u8::from(block.limiter.ops.is_some()));
    let (retry_tag, retry_nanos) = match block.retry {
        StorageRetryState::None => (RETRY_NONE, 0),
        StorageRetryState::Immediate => (RETRY_IMMEDIATE, 0),
        StorageRetryState::After { remaining_nanos } => (RETRY_AFTER, remaining_nanos),
    };
    write_u8(&mut output, retry_tag);
    output.extend_from_slice(&[0; 2]);
    write_u64(&mut output, block.capacity_sectors);
    output.extend_from_slice(block.device_id.as_bytes());
    output.extend_from_slice(&[0; 4]);
    let (next_available, next_used) = block
        .active_queue
        .map(|queue| (queue.next_available(), queue.next_used()))
        .unwrap_or((0, 0));
    write_u16(&mut output, next_available);
    write_u16(&mut output, next_used);
    write_u32(&mut output, 0);
    encode_bucket_state(&mut output, block.limiter.bandwidth);
    encode_bucket_state(&mut output, block.limiter.ops);
    write_u64(&mut output, retry_nanos);
    if output.len() != BLOCK_SECTION_BYTES {
        return Err(SnapshotV2DeviceGraphEncodeError::InvalidGraph);
    }
    Ok(output)
}

fn encode_bucket_state(output: &mut Vec<u8>, bucket: Option<SnapshotV2BlockBucketState>) {
    match bucket {
        Some(bucket) => {
            write_u64(output, bucket.budget);
            write_u64(output, bucket.remaining_burst);
            write_u64(output, bucket.age_nanos);
        }
        None => output.extend_from_slice(&[0; 24]),
    }
}

fn encode_common_section(
    state: &SnapshotV2VirtioState,
) -> Result<Vec<u8>, SnapshotV2DeviceGraphEncodeError> {
    let notification_bytes = state
        .pending_notifications
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or(SnapshotV2DeviceGraphEncodeError::TooLarge)?;
    let intent_bytes = state
        .interrupt_intents
        .len()
        .checked_mul(4)
        .ok_or(SnapshotV2DeviceGraphEncodeError::TooLarge)?;
    let semantic_len = COMMON_FIXED_BYTES
        .checked_add(COMMON_QUEUE_BYTES)
        .and_then(|len| len.checked_add(notification_bytes))
        .and_then(|len| len.checked_add(intent_bytes))
        .ok_or(SnapshotV2DeviceGraphEncodeError::TooLarge)?;
    let capacity = aligned_len(semantic_len).ok_or(SnapshotV2DeviceGraphEncodeError::TooLarge)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(capacity)
        .map_err(|_| SnapshotV2DeviceGraphEncodeError::Allocation)?;
    write_u64(&mut output, state.available_features);
    write_u64(&mut output, state.driver_features);
    write_u32(&mut output, state.config_generation);
    write_u32(&mut output, state.status);
    write_u8(&mut output, u8::from(state.activated));
    write_u8(&mut output, 0);
    write_u16(
        &mut output,
        u16::try_from(state.queues.len())
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::InvalidGraph)?,
    );
    write_u16(
        &mut output,
        u16::try_from(state.pending_notifications.len())
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::InvalidGraph)?,
    );
    write_u16(
        &mut output,
        u16::try_from(state.interrupt_intents.len())
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::InvalidGraph)?,
    );
    for queue in &state.queues {
        write_u16(&mut output, queue.max_size);
        write_u16(&mut output, queue.size);
        write_u8(&mut output, u8::from(queue.ready));
        output.extend_from_slice(&[0; 3]);
        write_u64(&mut output, queue.descriptor_table.raw_value());
        write_u64(&mut output, queue.driver_ring.raw_value());
        write_u64(&mut output, queue.device_ring.raw_value());
    }
    for notification in &state.pending_notifications {
        write_u16(&mut output, *notification);
    }
    for intent in &state.interrupt_intents {
        match intent {
            SnapshotV2InterruptIntent::Queue { queue_index } => {
                write_u8(&mut output, INTERRUPT_QUEUE);
                write_u8(&mut output, 0);
                write_u16(&mut output, *queue_index);
            }
            SnapshotV2InterruptIntent::Configuration => {
                write_u8(&mut output, INTERRUPT_CONFIGURATION);
                write_u8(&mut output, 0);
                write_u16(&mut output, 0);
            }
        }
    }
    pad_section(&mut output, capacity);
    Ok(output)
}

fn encode_transport_section(
    transport: &SnapshotV2DeviceTransport,
) -> Result<Vec<u8>, SnapshotV2DeviceGraphEncodeError> {
    match transport {
        SnapshotV2DeviceTransport::Mmio(state) => encode_mmio_section(state),
        SnapshotV2DeviceTransport::Pci(state) => encode_pci_section(state),
    }
}

fn encode_mmio_section(
    state: &SnapshotV2MmioDeviceState,
) -> Result<Vec<u8>, SnapshotV2DeviceGraphEncodeError> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(MMIO_SECTION_BYTES)
        .map_err(|_| SnapshotV2DeviceGraphEncodeError::Allocation)?;
    write_u32(&mut output, state.device_feature_select);
    write_u32(&mut output, state.driver_feature_select);
    write_u32(&mut output, state.queue_select);
    write_u32(&mut output, state.interrupt_line.raw_value());
    write_u64(&mut output, state.region.id().raw_value());
    write_u64(&mut output, state.region.range().start().raw_value());
    write_u64(&mut output, state.region.range().size());
    write_u64(&mut output, 0);
    Ok(output)
}

fn encode_pci_section(
    state: &SnapshotV2PciDeviceState,
) -> Result<Vec<u8>, SnapshotV2DeviceGraphEncodeError> {
    let writable_bytes = state
        .writable_bytes
        .len()
        .checked_mul(PCI_WRITABLE_ENTRY_BYTES)
        .ok_or(SnapshotV2DeviceGraphEncodeError::TooLarge)?;
    let probe_bytes = state
        .bar_probes
        .len()
        .checked_mul(PCI_BAR_PROBE_ENTRY_BYTES)
        .ok_or(SnapshotV2DeviceGraphEncodeError::TooLarge)?;
    let entry_bytes = state
        .msix
        .entries
        .len()
        .checked_mul(PCI_MSIX_ENTRY_BYTES)
        .ok_or(SnapshotV2DeviceGraphEncodeError::TooLarge)?;
    let pending_bytes = state
        .msix
        .pending_words
        .len()
        .checked_mul(PCI_PENDING_WORD_BYTES)
        .ok_or(SnapshotV2DeviceGraphEncodeError::TooLarge)?;
    let vector_bytes = state
        .msix
        .queue_vectors
        .len()
        .checked_mul(PCI_QUEUE_VECTOR_BYTES)
        .ok_or(SnapshotV2DeviceGraphEncodeError::TooLarge)?;
    let semantic_len = PCI_FIXED_BYTES
        .checked_add(writable_bytes)
        .and_then(|len| len.checked_add(probe_bytes))
        .and_then(|len| len.checked_add(entry_bytes))
        .and_then(|len| len.checked_add(pending_bytes))
        .and_then(|len| len.checked_add(vector_bytes))
        .ok_or(SnapshotV2DeviceGraphEncodeError::TooLarge)?;
    let capacity = aligned_len(semantic_len).ok_or(SnapshotV2DeviceGraphEncodeError::TooLarge)?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(capacity)
        .map_err(|_| SnapshotV2DeviceGraphEncodeError::Allocation)?;
    write_u8(&mut output, PCI_PHASE_ACTIVE);
    write_u8(&mut output, PCI_ORIGIN_STARTUP);
    write_u8(&mut output, state.bar_index);
    write_u8(&mut output, PCI_BAR_MEMORY64);
    write_u8(&mut output, PCI_BAR_NOT_PREFETCHABLE);
    write_u8(&mut output, state.pci_cfg_bar);
    write_u8(&mut output, state.sbdf.function());
    write_u8(&mut output, 0);
    write_u16(&mut output, state.sbdf.segment());
    write_u8(&mut output, state.sbdf.bus());
    write_u8(&mut output, state.sbdf.device());
    write_u32(&mut output, 0);
    write_u64(&mut output, state.bar_range.start().raw_value());
    write_u64(&mut output, state.bar_range.size());
    write_u32(&mut output, state.device_feature_select);
    write_u32(&mut output, state.driver_feature_select);
    write_u16(&mut output, state.queue_select);
    write_u16(
        &mut output,
        u16::try_from(state.writable_bytes.len())
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::InvalidGraph)?,
    );
    write_u16(
        &mut output,
        u16::try_from(state.bar_probes.len())
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::InvalidGraph)?,
    );
    write_u16(
        &mut output,
        u16::try_from(state.msix.entries.len())
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::InvalidGraph)?,
    );
    write_u16(
        &mut output,
        u16::try_from(state.msix.pending_words.len())
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::InvalidGraph)?,
    );
    write_u16(
        &mut output,
        u16::try_from(state.msix.queue_vectors.len())
            .map_err(|_| SnapshotV2DeviceGraphEncodeError::InvalidGraph)?,
    );
    write_u32(&mut output, 0);
    write_u32(&mut output, state.pci_cfg_offset);
    write_u32(&mut output, state.pci_cfg_length);
    write_u8(&mut output, u8::from(state.msix.enabled));
    write_u8(&mut output, u8::from(state.msix.function_masked));
    write_u8(
        &mut output,
        u8::from(state.msix.pending_transition_observed),
    );
    write_u8(&mut output, 0);
    write_u16(&mut output, state.msix.config_vector);
    write_u16(&mut output, 0);
    for writable in &state.writable_bytes {
        write_u16(&mut output, writable.offset);
        write_u8(&mut output, writable.value);
        write_u8(&mut output, 0);
    }
    for probe in &state.bar_probes {
        write_u8(&mut output, probe.index);
        write_u8(&mut output, u8::from(probe.pending));
        write_u16(&mut output, 0);
    }
    for entry in &state.msix.entries {
        write_u32(&mut output, entry.message_address_low);
        write_u32(&mut output, entry.message_address_high);
        write_u32(&mut output, entry.message_data);
        write_u32(&mut output, entry.vector_control);
    }
    for pending in &state.msix.pending_words {
        write_u64(&mut output, *pending);
    }
    for vector in &state.msix.queue_vectors {
        write_u16(&mut output, *vector);
    }
    pad_section(&mut output, capacity);
    Ok(output)
}

/// Decodes and validates one native-v2 device-graph payload.
pub fn decode_snapshot_v2_device_graph(
    outer_version: SnapshotFormatVersion,
    bytes: &[u8],
) -> Result<SnapshotV2DeviceGraph, SnapshotV2DeviceGraphDecodeError> {
    validate_compatibility_version(outer_version)
        .map_err(|_| SnapshotV2DeviceGraphDecodeError::UnsupportedVersion)?;
    if bytes.len() < NATIVE_V2_DEVICE_GRAPH_HEADER_BYTES {
        return Err(SnapshotV2DeviceGraphDecodeError::TooSmall);
    }
    if bytes.len() > NATIVE_V2_DEVICE_GRAPH_MAX_BYTES {
        return Err(SnapshotV2DeviceGraphDecodeError::TooLarge);
    }
    if read_array_at::<8>(bytes, HEADER_MAGIC_OFFSET)? != DEVICE_GRAPH_MAGIC {
        return Err(SnapshotV2DeviceGraphDecodeError::InvalidMagic);
    }
    let header_bytes = usize::from(read_u16_at(bytes, HEADER_BYTES_OFFSET)?);
    let profile = read_u16_at(bytes, HEADER_PROFILE_OFFSET)?;
    let transport = decode_transport_kind(read_u16_at(bytes, HEADER_TRANSPORT_OFFSET)?)?;
    let record_count = read_u16_at(bytes, HEADER_RECORD_COUNT_OFFSET)?;
    let section_count = read_u16_at(bytes, HEADER_SECTION_COUNT_OFFSET)?;
    if header_bytes != NATIVE_V2_DEVICE_GRAPH_HEADER_BYTES
        || profile != DEVICE_GRAPH_PROFILE
        || record_count != DEVICE_GRAPH_RECORD_COUNT
        || section_count != DEVICE_GRAPH_SECTION_COUNT
        || read_u32_at(bytes, HEADER_FLAGS_OFFSET)? != DEVICE_GRAPH_FLAGS
    {
        return Err(SnapshotV2DeviceGraphDecodeError::UnsupportedProfile);
    }
    if read_u16_at(bytes, HEADER_RESERVED_OFFSET)? != 0 {
        return Err(SnapshotV2DeviceGraphDecodeError::NonzeroReserved);
    }
    let total_length = read_usize_u64_at(bytes, HEADER_TOTAL_LENGTH_OFFSET)?;
    if total_length != bytes.len()
        || read_usize_u64_at(bytes, HEADER_RECORD_DIRECTORY_OFFSET_OFFSET)?
            != DEVICE_GRAPH_RECORD_DIRECTORY_OFFSET
        || read_usize_u64_at(bytes, HEADER_SECTION_DIRECTORY_OFFSET_OFFSET)?
            != DEVICE_GRAPH_SECTION_DIRECTORY_OFFSET
        || read_usize_u64_at(bytes, HEADER_PAYLOAD_OFFSET_OFFSET)? != DEVICE_GRAPH_PAYLOAD_OFFSET
    {
        return Err(SnapshotV2DeviceGraphDecodeError::InvalidStructure);
    }
    let root_key = SnapshotV2DeviceKey {
        kind: read_u32_at(bytes, HEADER_ROOT_KIND_OFFSET)?,
        instance: read_u32_at(bytes, HEADER_ROOT_INSTANCE_OFFSET)?,
    };

    let record = bytes
        .get(
            DEVICE_GRAPH_RECORD_DIRECTORY_OFFSET
                ..DEVICE_GRAPH_RECORD_DIRECTORY_OFFSET + NATIVE_V2_DEVICE_GRAPH_RECORD_ENTRY_BYTES,
        )
        .ok_or(SnapshotV2DeviceGraphDecodeError::Truncated)?;
    let record_key = SnapshotV2DeviceKey {
        kind: read_u32_at(record, RECORD_KIND_OFFSET)?,
        instance: read_u32_at(record, RECORD_INSTANCE_OFFSET)?,
    };
    if read_u32_at(record, RECORD_FIRST_SECTION_OFFSET)? != 0
        || read_u32_at(record, RECORD_SECTION_COUNT_OFFSET)? != DEVICE_GRAPH_SECTION_COUNT_U32
    {
        return Err(SnapshotV2DeviceGraphDecodeError::InvalidStructure);
    }
    if record
        .get(RECORD_RESERVED_OFFSET..)
        .is_none_or(|reserved| reserved.iter().any(|byte| *byte != 0))
    {
        return Err(SnapshotV2DeviceGraphDecodeError::NonzeroReserved);
    }

    let mut sections = [SectionBounds {
        offset: 0,
        length: 0,
    }; DEVICE_GRAPH_SECTION_COUNT_USIZE];
    let mut expected_offset = DEVICE_GRAPH_PAYLOAD_OFFSET;
    for (index, expected_kind) in [
        SECTION_KIND_CONFIG,
        SECTION_KIND_BLOCK,
        SECTION_KIND_COMMON,
        SECTION_KIND_TRANSPORT,
    ]
    .into_iter()
    .enumerate()
    {
        let entry_offset = DEVICE_GRAPH_SECTION_DIRECTORY_OFFSET
            .checked_add(
                index
                    .checked_mul(NATIVE_V2_DEVICE_GRAPH_SECTION_ENTRY_BYTES)
                    .ok_or(SnapshotV2DeviceGraphDecodeError::InvalidStructure)?,
            )
            .ok_or(SnapshotV2DeviceGraphDecodeError::InvalidStructure)?;
        let entry = bytes
            .get(
                entry_offset
                    ..entry_offset
                        .checked_add(NATIVE_V2_DEVICE_GRAPH_SECTION_ENTRY_BYTES)
                        .ok_or(SnapshotV2DeviceGraphDecodeError::InvalidStructure)?,
            )
            .ok_or(SnapshotV2DeviceGraphDecodeError::Truncated)?;
        if read_u32_at(entry, SECTION_RECORD_INDEX_OFFSET)? != 0
            || read_u16_at(entry, SECTION_KIND_OFFSET)? != expected_kind
            || read_u16_at(entry, SECTION_FLAGS_OFFSET)? != 0
        {
            return Err(SnapshotV2DeviceGraphDecodeError::InvalidStructure);
        }
        if read_u64_at(entry, SECTION_RESERVED_OFFSET)? != 0 {
            return Err(SnapshotV2DeviceGraphDecodeError::NonzeroReserved);
        }
        let offset = read_usize_u64_at(entry, SECTION_PAYLOAD_OFFSET)?;
        let length = read_usize_u64_at(entry, SECTION_LENGTH_OFFSET)?;
        let end = offset
            .checked_add(length)
            .ok_or(SnapshotV2DeviceGraphDecodeError::InvalidStructure)?;
        if offset != expected_offset
            || length == 0
            || !offset.is_multiple_of(DEVICE_GRAPH_ALIGNMENT)
            || !length.is_multiple_of(DEVICE_GRAPH_ALIGNMENT)
            || end > bytes.len()
        {
            return Err(SnapshotV2DeviceGraphDecodeError::InvalidStructure);
        }
        let slot = sections
            .get_mut(index)
            .ok_or(SnapshotV2DeviceGraphDecodeError::InvalidStructure)?;
        *slot = SectionBounds { offset, length };
        expected_offset = end;
    }
    if expected_offset != bytes.len() {
        return Err(SnapshotV2DeviceGraphDecodeError::InvalidStructure);
    }

    let config = decode_config_section(section_bytes(bytes, sections[0])?)?;
    let block = decode_block_section(section_bytes(bytes, sections[1])?)?;
    let virtio = decode_common_section(section_bytes(bytes, sections[2])?)?;
    let transport = match transport {
        SnapshotV2DeviceTransportKind::Mmio => SnapshotV2DeviceTransport::Mmio(
            decode_mmio_section(section_bytes(bytes, sections[3])?)?,
        ),
        SnapshotV2DeviceTransportKind::Pci => {
            SnapshotV2DeviceTransport::Pci(decode_pci_section(section_bytes(bytes, sections[3])?)?)
        }
    };
    let graph = SnapshotV2DeviceGraph {
        root_key,
        record: SnapshotV2DeviceRecord {
            key: record_key,
            config,
            block,
            virtio,
            transport,
        },
    };
    validate_graph(&graph).map_err(|_| SnapshotV2DeviceGraphDecodeError::InvalidGraph)?;
    Ok(graph)
}

#[derive(Clone, Copy)]
struct SectionBounds {
    offset: usize,
    length: usize,
}

fn section_bytes(
    bytes: &[u8],
    section: SectionBounds,
) -> Result<&[u8], SnapshotV2DeviceGraphDecodeError> {
    let end = section
        .offset
        .checked_add(section.length)
        .ok_or(SnapshotV2DeviceGraphDecodeError::InvalidStructure)?;
    bytes
        .get(section.offset..end)
        .ok_or(SnapshotV2DeviceGraphDecodeError::Truncated)
}

fn decode_config_section(
    bytes: &[u8],
) -> Result<SnapshotV2RootBlockConfig, SnapshotV2DeviceGraphDecodeError> {
    if bytes.len() < CONFIG_FIXED_BYTES {
        return Err(SnapshotV2DeviceGraphDecodeError::Truncated);
    }
    let mut reader = DeviceGraphReader::new(bytes);
    if !reader.read_bool()? || reader.read_u8()? != ENGINE_SYNC {
        return Err(SnapshotV2DeviceGraphDecodeError::InvalidValue);
    }
    let cache_type = match reader.read_u8()? {
        CACHE_UNSAFE => DriveCacheType::Unsafe,
        CACHE_WRITEBACK => DriveCacheType::Writeback,
        _ => return Err(SnapshotV2DeviceGraphDecodeError::InvalidValue),
    };
    let partuuid_present = reader.read_bool()?;
    let bandwidth_present = reader.read_bool()?;
    let ops_present = reader.read_bool()?;
    reader.read_zeroes(2)?;
    let drive_id_len = usize::from(reader.read_u16()?);
    let partuuid_len = usize::from(reader.read_u16()?);
    let selector_len = usize::from(reader.read_u16()?);
    reader.read_zeroes(2)?;
    let bandwidth = decode_bucket_config(&mut reader, bandwidth_present)?;
    let ops = decode_bucket_config(&mut reader, ops_present)?;
    if drive_id_len == 0
        || drive_id_len > NATIVE_V2_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES
        || selector_len == 0
        || selector_len > NATIVE_V2_DEVICE_GRAPH_MAX_SELECTOR_BYTES
        || partuuid_present != (partuuid_len != 0)
        || partuuid_len > NATIVE_V2_DEVICE_GRAPH_MAX_PARTUUID_BYTES
    {
        return Err(SnapshotV2DeviceGraphDecodeError::InvalidString);
    }
    let drive_id = reader.read_string(drive_id_len)?;
    let partuuid = if partuuid_present {
        Some(reader.read_string(partuuid_len)?)
    } else {
        None
    };
    let selector = reader.read_string(selector_len)?;
    reader.finish_padded()?;
    let rate_limiter = if bandwidth.is_some() || ops.is_some() {
        Some(DriveRateLimiterConfig::new(bandwidth, ops))
    } else {
        None
    };
    Ok(SnapshotV2RootBlockConfig {
        drive_id,
        partuuid,
        cache_type,
        rate_limiter,
        selector,
    })
}

fn decode_bucket_config(
    reader: &mut DeviceGraphReader<'_>,
    present: bool,
) -> Result<Option<DriveTokenBucketConfig>, SnapshotV2DeviceGraphDecodeError> {
    let size = reader.read_u64()?;
    let burst = reader.read_u64()?;
    let refill_time = reader.read_u64()?;
    let burst_present = reader.read_bool()?;
    reader.read_zeroes(7)?;
    if !present {
        if size != 0 || burst != 0 || refill_time != 0 || burst_present {
            return Err(SnapshotV2DeviceGraphDecodeError::InvalidValue);
        }
        return Ok(None);
    }
    let config = DriveTokenBucketConfig::new(
        size,
        if burst_present { Some(burst) } else { None },
        refill_time,
    );
    if (!burst_present && burst != 0) || !token_bucket_is_enabled(config) {
        return Err(SnapshotV2DeviceGraphDecodeError::InvalidValue);
    }
    Ok(Some(config))
}

fn decode_block_section(
    bytes: &[u8],
) -> Result<SnapshotV2BlockState, SnapshotV2DeviceGraphDecodeError> {
    if bytes.len() != BLOCK_SECTION_BYTES {
        return Err(SnapshotV2DeviceGraphDecodeError::InvalidStructure);
    }
    let mut reader = DeviceGraphReader::new(bytes);
    if usize::from(reader.read_u16()?) != VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE {
        return Err(SnapshotV2DeviceGraphDecodeError::InvalidValue);
    }
    let active_present = reader.read_bool()?;
    let bandwidth_present = reader.read_bool()?;
    let ops_present = reader.read_bool()?;
    let retry_tag = reader.read_u8()?;
    reader.read_zeroes(2)?;
    let capacity_sectors = reader.read_u64()?;
    let device_id =
        VirtioBlockDeviceId::new(reader.read_array::<{ VIRTIO_BLOCK_ID_BYTES as usize }>()?);
    reader.read_zeroes(4)?;
    let next_available = reader.read_u16()?;
    let next_used = reader.read_u16()?;
    reader.read_zeroes(4)?;
    let bandwidth = decode_bucket_state(&mut reader, bandwidth_present)?;
    let ops = decode_bucket_state(&mut reader, ops_present)?;
    let retry_nanos = reader.read_u64()?;
    if !reader.is_finished() {
        return Err(SnapshotV2DeviceGraphDecodeError::InvalidStructure);
    }
    let active_queue = if active_present {
        Some(crate::block::VirtioBlockQueueState::new(
            next_available,
            next_used,
        ))
    } else {
        if next_available != 0 || next_used != 0 {
            return Err(SnapshotV2DeviceGraphDecodeError::InvalidValue);
        }
        None
    };
    let retry = match retry_tag {
        RETRY_NONE if retry_nanos == 0 => StorageRetryState::None,
        RETRY_IMMEDIATE if retry_nanos == 0 => StorageRetryState::Immediate,
        RETRY_AFTER if retry_nanos != 0 => StorageRetryState::After {
            remaining_nanos: retry_nanos,
        },
        _ => return Err(SnapshotV2DeviceGraphDecodeError::InvalidValue),
    };
    Ok(SnapshotV2BlockState {
        capacity_sectors,
        device_id,
        active_queue,
        limiter: SnapshotV2BlockLimiterState { bandwidth, ops },
        retry,
    })
}

fn decode_bucket_state(
    reader: &mut DeviceGraphReader<'_>,
    present: bool,
) -> Result<Option<SnapshotV2BlockBucketState>, SnapshotV2DeviceGraphDecodeError> {
    let budget = reader.read_u64()?;
    let remaining_burst = reader.read_u64()?;
    let age_nanos = reader.read_u64()?;
    if present {
        Ok(Some(SnapshotV2BlockBucketState {
            budget,
            remaining_burst,
            age_nanos,
        }))
    } else if budget == 0 && remaining_burst == 0 && age_nanos == 0 {
        Ok(None)
    } else {
        Err(SnapshotV2DeviceGraphDecodeError::InvalidValue)
    }
}

fn decode_common_section(
    bytes: &[u8],
) -> Result<SnapshotV2VirtioState, SnapshotV2DeviceGraphDecodeError> {
    if bytes.len() < COMMON_FIXED_BYTES + COMMON_QUEUE_BYTES {
        return Err(SnapshotV2DeviceGraphDecodeError::Truncated);
    }
    let mut reader = DeviceGraphReader::new(bytes);
    let available_features = reader.read_u64()?;
    let driver_features = reader.read_u64()?;
    let config_generation = reader.read_u32()?;
    let status = reader.read_u32()?;
    let activated = reader.read_bool()?;
    reader.read_zeroes(1)?;
    let queue_count = usize::from(reader.read_u16()?);
    let notification_count = usize::from(reader.read_u16()?);
    let intent_count = usize::from(reader.read_u16()?);
    if queue_count != 1 || notification_count > 1 || intent_count > 2 {
        return Err(SnapshotV2DeviceGraphDecodeError::InvalidValue);
    }
    let mut queues = Vec::new();
    queues
        .try_reserve_exact(queue_count)
        .map_err(|_| SnapshotV2DeviceGraphDecodeError::Allocation)?;
    for _ in 0..queue_count {
        let max_size = reader.read_u16()?;
        let size = reader.read_u16()?;
        let ready = reader.read_bool()?;
        reader.read_zeroes(3)?;
        queues.push(SnapshotV2VirtioQueueState {
            max_size,
            size,
            ready,
            descriptor_table: GuestAddress::new(reader.read_u64()?),
            driver_ring: GuestAddress::new(reader.read_u64()?),
            device_ring: GuestAddress::new(reader.read_u64()?),
        });
    }
    let mut pending_notifications = Vec::new();
    pending_notifications
        .try_reserve_exact(notification_count)
        .map_err(|_| SnapshotV2DeviceGraphDecodeError::Allocation)?;
    for _ in 0..notification_count {
        pending_notifications.push(reader.read_u16()?);
    }
    let mut interrupt_intents = Vec::new();
    interrupt_intents
        .try_reserve_exact(intent_count)
        .map_err(|_| SnapshotV2DeviceGraphDecodeError::Allocation)?;
    for _ in 0..intent_count {
        let tag = reader.read_u8()?;
        reader.read_zeroes(1)?;
        let queue_index = reader.read_u16()?;
        interrupt_intents.push(match tag {
            INTERRUPT_QUEUE => SnapshotV2InterruptIntent::Queue { queue_index },
            INTERRUPT_CONFIGURATION if queue_index == 0 => SnapshotV2InterruptIntent::Configuration,
            _ => return Err(SnapshotV2DeviceGraphDecodeError::InvalidValue),
        });
    }
    reader.finish_padded()?;
    Ok(SnapshotV2VirtioState {
        available_features,
        driver_features,
        config_generation,
        status,
        activated,
        queues,
        pending_notifications,
        interrupt_intents,
    })
}

fn decode_mmio_section(
    bytes: &[u8],
) -> Result<SnapshotV2MmioDeviceState, SnapshotV2DeviceGraphDecodeError> {
    if bytes.len() != MMIO_SECTION_BYTES {
        return Err(SnapshotV2DeviceGraphDecodeError::InvalidStructure);
    }
    let mut reader = DeviceGraphReader::new(bytes);
    let device_feature_select = reader.read_u32()?;
    let driver_feature_select = reader.read_u32()?;
    let queue_select = reader.read_u32()?;
    let interrupt_line = GuestInterruptLine::new(reader.read_u32()?)
        .map_err(|_| SnapshotV2DeviceGraphDecodeError::InvalidValue)?;
    let region_id = crate::mmio::MmioRegionId::new(reader.read_u64()?);
    let start = GuestAddress::new(reader.read_u64()?);
    let size = reader.read_u64()?;
    reader.read_zeroes(8)?;
    let region = MmioRegion::new(region_id, start, size)
        .map_err(|_| SnapshotV2DeviceGraphDecodeError::InvalidValue)?;
    Ok(SnapshotV2MmioDeviceState {
        device_feature_select,
        driver_feature_select,
        queue_select,
        region,
        interrupt_line,
    })
}

fn decode_pci_section(
    bytes: &[u8],
) -> Result<SnapshotV2PciDeviceState, SnapshotV2DeviceGraphDecodeError> {
    if bytes.len() < PCI_FIXED_BYTES {
        return Err(SnapshotV2DeviceGraphDecodeError::Truncated);
    }
    let mut reader = DeviceGraphReader::new(bytes);
    let phase = match reader.read_u8()? {
        PCI_PHASE_ACTIVE => VirtioPciEndpointPhase::Active,
        _ => return Err(SnapshotV2DeviceGraphDecodeError::InvalidValue),
    };
    let origin = match reader.read_u8()? {
        PCI_ORIGIN_STARTUP => StorageDeviceOrigin::Startup,
        _ => return Err(SnapshotV2DeviceGraphDecodeError::InvalidValue),
    };
    let bar_index = reader.read_u8()?;
    let bar_address_space = match reader.read_u8()? {
        PCI_BAR_MEMORY64 => PciBarAddressSpace::Memory64,
        _ => return Err(SnapshotV2DeviceGraphDecodeError::InvalidValue),
    };
    let bar_prefetchable = match reader.read_u8()? {
        PCI_BAR_NOT_PREFETCHABLE => PciBarPrefetchable::No,
        _ => return Err(SnapshotV2DeviceGraphDecodeError::InvalidValue),
    };
    let pci_cfg_bar = reader.read_u8()?;
    let function = reader.read_u8()?;
    reader.read_zeroes(1)?;
    let segment = reader.read_u16()?;
    let bus = reader.read_u8()?;
    let device = reader.read_u8()?;
    reader.read_zeroes(4)?;
    let bar_start = GuestAddress::new(reader.read_u64()?);
    let bar_size = reader.read_u64()?;
    let device_feature_select = reader.read_u32()?;
    let driver_feature_select = reader.read_u32()?;
    let queue_select = reader.read_u16()?;
    let writable_count = usize::from(reader.read_u16()?);
    let probe_count = usize::from(reader.read_u16()?);
    let msix_entry_count = usize::from(reader.read_u16()?);
    let pending_word_count = usize::from(reader.read_u16()?);
    let queue_vector_count = usize::from(reader.read_u16()?);
    reader.read_zeroes(4)?;
    let pci_cfg_offset = reader.read_u32()?;
    let pci_cfg_length = reader.read_u32()?;
    let enabled = reader.read_bool()?;
    let function_masked = reader.read_bool()?;
    let pending_transition_observed = reader.read_bool()?;
    reader.read_zeroes(1)?;
    let config_vector = reader.read_u16()?;
    reader.read_zeroes(2)?;
    if writable_count != PCI_GENERIC_WRITABLE_BYTES.len()
        || probe_count != 2
        || msix_entry_count != 2
        || pending_word_count != 1
        || queue_vector_count != 1
    {
        return Err(SnapshotV2DeviceGraphDecodeError::InvalidValue);
    }

    let mut writable_bytes = Vec::new();
    writable_bytes
        .try_reserve_exact(writable_count)
        .map_err(|_| SnapshotV2DeviceGraphDecodeError::Allocation)?;
    for _ in 0..writable_count {
        let offset = reader.read_u16()?;
        let value = reader.read_u8()?;
        reader.read_zeroes(1)?;
        writable_bytes.push(SnapshotV2PciWritableByte { offset, value });
    }
    let mut bar_probes = Vec::new();
    bar_probes
        .try_reserve_exact(probe_count)
        .map_err(|_| SnapshotV2DeviceGraphDecodeError::Allocation)?;
    for _ in 0..probe_count {
        let index = reader.read_u8()?;
        let pending = reader.read_bool()?;
        reader.read_zeroes(2)?;
        bar_probes.push(SnapshotV2PciBarProbeState { index, pending });
    }
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(msix_entry_count)
        .map_err(|_| SnapshotV2DeviceGraphDecodeError::Allocation)?;
    for _ in 0..msix_entry_count {
        entries.push(SnapshotV2PciMsixTableEntry {
            message_address_low: reader.read_u32()?,
            message_address_high: reader.read_u32()?,
            message_data: reader.read_u32()?,
            vector_control: reader.read_u32()?,
        });
    }
    let mut pending_words = Vec::new();
    pending_words
        .try_reserve_exact(pending_word_count)
        .map_err(|_| SnapshotV2DeviceGraphDecodeError::Allocation)?;
    for _ in 0..pending_word_count {
        pending_words.push(reader.read_u64()?);
    }
    let mut queue_vectors = Vec::new();
    queue_vectors
        .try_reserve_exact(queue_vector_count)
        .map_err(|_| SnapshotV2DeviceGraphDecodeError::Allocation)?;
    for _ in 0..queue_vector_count {
        queue_vectors.push(reader.read_u16()?);
    }
    reader.finish_padded()?;
    let sbdf = PciSbdf::new(segment, bus, device, function)
        .map_err(|_| SnapshotV2DeviceGraphDecodeError::InvalidValue)?;
    let bar_range = GuestMemoryRange::new(bar_start, bar_size)
        .map_err(|_| SnapshotV2DeviceGraphDecodeError::InvalidValue)?;
    Ok(SnapshotV2PciDeviceState {
        phase,
        origin,
        sbdf,
        bar_index,
        bar_address_space,
        bar_prefetchable,
        bar_range,
        device_feature_select,
        driver_feature_select,
        queue_select,
        pci_cfg_bar,
        pci_cfg_offset,
        pci_cfg_length,
        writable_bytes,
        bar_probes,
        msix: SnapshotV2PciMsixState {
            entries,
            pending_words,
            enabled,
            function_masked,
            config_vector,
            queue_vectors,
            pending_transition_observed,
        },
    })
}

struct DeviceGraphReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> DeviceGraphReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    const fn is_finished(&self) -> bool {
        self.position == self.bytes.len()
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    fn read_u8(&mut self) -> Result<u8, SnapshotV2DeviceGraphDecodeError> {
        Ok(self.read_array::<1>()?[0])
    }

    fn read_u16(&mut self) -> Result<u16, SnapshotV2DeviceGraphDecodeError> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    fn read_u32(&mut self) -> Result<u32, SnapshotV2DeviceGraphDecodeError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    fn read_u64(&mut self) -> Result<u64, SnapshotV2DeviceGraphDecodeError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    fn read_bool(&mut self) -> Result<bool, SnapshotV2DeviceGraphDecodeError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotV2DeviceGraphDecodeError::InvalidValue),
        }
    }

    fn read_zeroes(&mut self, len: usize) -> Result<(), SnapshotV2DeviceGraphDecodeError> {
        if self.read_bytes(len)?.iter().any(|byte| *byte != 0) {
            Err(SnapshotV2DeviceGraphDecodeError::NonzeroReserved)
        } else {
            Ok(())
        }
    }

    fn read_string(&mut self, len: usize) -> Result<String, SnapshotV2DeviceGraphDecodeError> {
        let value = std::str::from_utf8(self.read_bytes(len)?)
            .map_err(|_| SnapshotV2DeviceGraphDecodeError::InvalidString)?;
        let mut owned = String::new();
        owned
            .try_reserve_exact(value.len())
            .map_err(|_| SnapshotV2DeviceGraphDecodeError::Allocation)?;
        owned.push_str(value);
        Ok(owned)
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], SnapshotV2DeviceGraphDecodeError> {
        self.read_bytes(N)?
            .try_into()
            .map_err(|_| SnapshotV2DeviceGraphDecodeError::Truncated)
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], SnapshotV2DeviceGraphDecodeError> {
        let end = self
            .position
            .checked_add(len)
            .ok_or(SnapshotV2DeviceGraphDecodeError::Truncated)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(SnapshotV2DeviceGraphDecodeError::Truncated)?;
        self.position = end;
        Ok(value)
    }

    fn finish_padded(&mut self) -> Result<(), SnapshotV2DeviceGraphDecodeError> {
        let remaining = self.remaining();
        if remaining >= DEVICE_GRAPH_ALIGNMENT {
            return Err(SnapshotV2DeviceGraphDecodeError::InvalidStructure);
        }
        self.read_zeroes(remaining)
    }
}

fn transport_kind_tag(kind: SnapshotV2DeviceTransportKind) -> u16 {
    match kind {
        SnapshotV2DeviceTransportKind::Mmio => TRANSPORT_KIND_MMIO,
        SnapshotV2DeviceTransportKind::Pci => TRANSPORT_KIND_PCI,
    }
}

fn decode_transport_kind(
    value: u16,
) -> Result<SnapshotV2DeviceTransportKind, SnapshotV2DeviceGraphDecodeError> {
    match value {
        TRANSPORT_KIND_MMIO => Ok(SnapshotV2DeviceTransportKind::Mmio),
        TRANSPORT_KIND_PCI => Ok(SnapshotV2DeviceTransportKind::Pci),
        _ => Err(SnapshotV2DeviceGraphDecodeError::UnsupportedProfile),
    }
}

fn aligned_len(len: usize) -> Option<usize> {
    len.checked_add(DEVICE_GRAPH_ALIGNMENT - 1)
        .map(|value| value & !(DEVICE_GRAPH_ALIGNMENT - 1))
}

fn pad_section(output: &mut Vec<u8>, capacity: usize) {
    output.resize(capacity, 0);
}

fn write_u8(output: &mut Vec<u8>, value: u8) {
    output.push(value);
}

fn write_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn read_array_at<const N: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; N], SnapshotV2DeviceGraphDecodeError> {
    let end = offset
        .checked_add(N)
        .ok_or(SnapshotV2DeviceGraphDecodeError::Truncated)?;
    bytes
        .get(offset..end)
        .ok_or(SnapshotV2DeviceGraphDecodeError::Truncated)?
        .try_into()
        .map_err(|_| SnapshotV2DeviceGraphDecodeError::Truncated)
}

fn read_u16_at(bytes: &[u8], offset: usize) -> Result<u16, SnapshotV2DeviceGraphDecodeError> {
    Ok(u16::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_u32_at(bytes: &[u8], offset: usize) -> Result<u32, SnapshotV2DeviceGraphDecodeError> {
    Ok(u32::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_u64_at(bytes: &[u8], offset: usize) -> Result<u64, SnapshotV2DeviceGraphDecodeError> {
    Ok(u64::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_usize_u64_at(
    bytes: &[u8],
    offset: usize,
) -> Result<usize, SnapshotV2DeviceGraphDecodeError> {
    usize::try_from(read_u64_at(bytes, offset)?)
        .map_err(|_| SnapshotV2DeviceGraphDecodeError::InvalidStructure)
}

#[cfg(test)]
mod tests;
