//! Exact native-v2 2.11 portable network and MMDS state.
//!
//! The component retains only reconstructible guest/device configuration.
//! Packet-I/O owners, callbacks, cached packets, metrics, host clocks, and
//! live MMDS data or protocol sessions are deliberately outside this model.

use std::fmt;
use std::net::Ipv4Addr;

use crate::interrupt::GuestInterruptLine;
use crate::memory::GuestMemoryRange;
use crate::mmds::{
    DEFAULT_MMDS_IPV4_ADDRESS, DEFAULT_MMDS_MAC_ADDRESS, EthernetMacAddress, MMDS_GUEST_TCP_PORT,
    MmdsVersion,
};
use crate::mmio::MmioRegion;
use crate::network::{
    GuestMacAddress, MAX_NETWORK_INTERFACE_COUNT, NetworkDeviceProfile, NetworkInterfaceConfig,
    VIRTIO_NET_DEVICE_ID, VIRTIO_NET_MAX_MTU, VIRTIO_NET_MIN_MTU, VIRTIO_NET_QUEUE_COUNT,
    VIRTIO_NET_QUEUE_SIZE, VirtioNetworkConfigSpace, VirtioNetworkDeviceCaptureState,
    VirtioNetworkFeatureCapabilities, VirtioNetworkMmioCaptureState, VirtioNetworkPciCaptureState,
    VirtioNetworkRateLimiterCaptureState, VirtioNetworkRetryCaptureState,
    VirtioNetworkTokenBucketCaptureState,
};
use crate::pci::{
    PCI_BAR64_SIZE, PCI_BAR64_START, PCI_BUS_ZERO, PCI_FIRST_ENDPOINT_DEVICE, PCI_FUNCTION_ZERO,
    PCI_LAST_ENDPOINT_DEVICE, PCI_SEGMENT_ZERO, PciBarAddressSpace, PciBarPrefetchable, PciSbdf,
};
use crate::snapshot_device_v2::{
    SnapshotV2DeviceGraphCaptureError, SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind,
    SnapshotV2InterruptIntent, SnapshotV2MmioDeviceState, SnapshotV2PciDeviceState,
    SnapshotV2VirtioQueueState, SnapshotV2VirtioState,
    capture_mmio_common_for_device_with_queue_count_and_config_status_gate,
    capture_mmio_transport_parts, capture_pci_common_for_device_with_queue_count,
    capture_pci_transport_parts_with_queue_count,
};
use crate::snapshot_device_v2_5::queue_ranges;
use crate::snapshot_format::SnapshotFormatVersion;
use crate::storage_capture::StorageDeviceOrigin;
use crate::virtio::{
    VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DEVICE_NEEDS_RESET,
    VIRTIO_DEVICE_STATUS_DRIVER, VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FAILED,
    VIRTIO_DEVICE_STATUS_FEATURES_OK, VIRTIO_DEVICE_STATUS_INIT, VirtioInterruptIntent,
};
use crate::virtio_mmio::{VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VIRTIO_MMIO_VERSION_1_FEATURE};
use crate::virtio_pci::{
    VIRTIO_PCI_CAPABILITY_BAR_INDEX, VIRTIO_PCI_CAPABILITY_BAR_SIZE, VIRTIO_PCI_MAX_MSIX_VECTORS,
    VIRTIO_PCI_NO_VECTOR, VirtioPciEndpointPhase,
};

mod codec;

#[cfg(test)]
mod tests;

macro_rules! redacted_debug {
    ($type:ty, $name:literal) => {
        impl fmt::Debug for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct($name)
                    .field("state", &"<redacted>")
                    .finish()
            }
        }
    };
}

/// Exact compatibility context of the optional singleton network component.
pub const NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION: SnapshotFormatVersion =
    SnapshotFormatVersion::new(2, 11, 0);

/// Maximum UTF-8 byte length of one stable interface identifier.
pub const NATIVE_V2_NETWORK_MAX_INTERFACE_ID_BYTES: usize = 255;

/// Maximum UTF-8 byte length of one inert captured host selector.
pub const NATIVE_V2_NETWORK_MAX_CAPTURED_SELECTOR_BYTES: usize = 4096;

/// Maximum number of ordered network records.
pub const NATIVE_V2_NETWORK_MAX_INTERFACES: usize = MAX_NETWORK_INTERFACE_COUNT;

/// Fixed outer aggregate header size.
pub const NATIVE_V2_NETWORK_STATE_HEADER_BYTES: usize = 64;

/// Fixed outer interface-directory entry size.
pub const NATIVE_V2_NETWORK_INTERFACE_DIRECTORY_ENTRY_BYTES: usize = 32;

/// Fixed independently framed interface-record header size.
pub const NATIVE_V2_NETWORK_INTERFACE_RECORD_HEADER_BYTES: usize = 64;

/// Fixed interface-record section-directory entry size.
pub const NATIVE_V2_NETWORK_INTERFACE_SECTION_ENTRY_BYTES: usize = 32;

/// Number of exact sections in one interface record.
pub const NATIVE_V2_NETWORK_INTERFACE_SECTION_COUNT: usize = 5;

/// Fixed network-local continuation section size.
pub const NATIVE_V2_NETWORK_LOCAL_STATE_BYTES: usize = 64;

/// Fixed encoded size of all four network limiter bucket slots.
pub const NATIVE_V2_NETWORK_LIMITER_STATE_BYTES: usize = 224;

/// Maximum complete common virtio section for two queues.
pub const NATIVE_V2_NETWORK_COMMON_STATE_MAX_BYTES: usize = 112;

/// Maximum exact network PCI transport section.
pub const NATIVE_V2_NETWORK_PCI_STATE_BYTES: usize = 160;

/// Maximum exact MMDS section for sixteen selected interfaces.
pub const NATIVE_V2_NETWORK_MMDS_STATE_MAX_BYTES: usize =
    32 + NATIVE_V2_NETWORK_MAX_INTERFACES * 16;

const IDENTITY_PREFIX_BYTES: usize = 32;
const MAX_IDENTITY_BYTES: usize = align_up_const(
    IDENTITY_PREFIX_BYTES
        + NATIVE_V2_NETWORK_MAX_INTERFACE_ID_BYTES
        + NATIVE_V2_NETWORK_MAX_CAPTURED_SELECTOR_BYTES,
    8,
);

/// Maximum independently framed interface record.
pub const NATIVE_V2_NETWORK_INTERFACE_RECORD_MAX_BYTES: usize =
    NATIVE_V2_NETWORK_INTERFACE_RECORD_HEADER_BYTES
        + NATIVE_V2_NETWORK_INTERFACE_SECTION_COUNT
            * NATIVE_V2_NETWORK_INTERFACE_SECTION_ENTRY_BYTES
        + MAX_IDENTITY_BYTES
        + NATIVE_V2_NETWORK_LOCAL_STATE_BYTES
        + NATIVE_V2_NETWORK_COMMON_STATE_MAX_BYTES
        + NATIVE_V2_NETWORK_LIMITER_STATE_BYTES
        + NATIVE_V2_NETWORK_PCI_STATE_BYTES;

/// Exact maximum semantic payload derivable from all bounded fields.
pub const NATIVE_V2_NETWORK_STATE_WORST_CASE_BYTES: usize = NATIVE_V2_NETWORK_STATE_HEADER_BYTES
    + NATIVE_V2_NETWORK_MAX_INTERFACES * NATIVE_V2_NETWORK_INTERFACE_DIRECTORY_ENTRY_BYTES
    + NATIVE_V2_NETWORK_MAX_INTERFACES * NATIVE_V2_NETWORK_INTERFACE_RECORD_MAX_BYTES
    + NATIVE_V2_NETWORK_MMDS_STATE_MAX_BYTES;

/// Maximum complete exact-2.11 network component size.
pub const NATIVE_V2_NETWORK_STATE_MAX_BYTES: usize = 512 * 1024;

const _: () = assert!(NATIVE_V2_NETWORK_MAX_INTERFACES == 16);
const _: () = assert!(MAX_IDENTITY_BYTES == 4384);
const _: () = assert!(NATIVE_V2_NETWORK_INTERFACE_RECORD_MAX_BYTES == 5168);
const _: () = assert!(NATIVE_V2_NETWORK_MMDS_STATE_MAX_BYTES == 288);
const _: () = assert!(NATIVE_V2_NETWORK_STATE_WORST_CASE_BYTES == 83_552);
const _: () =
    assert!(NATIVE_V2_NETWORK_STATE_WORST_CASE_BYTES <= NATIVE_V2_NETWORK_STATE_MAX_BYTES);
const _: () = assert!(
    NATIVE_V2_NETWORK_STATE_MAX_BYTES
        <= crate::snapshot_format_v2::NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES
);

const fn align_up_const(value: usize, alignment: usize) -> usize {
    let remainder = value % alignment;
    if remainder == 0 {
        value
    } else {
        value + alignment - remainder
    }
}

/// Packet-I/O class proven by the source configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2NetworkBackendClass {
    /// Fresh packet I/O needs only a local MMDS detour.
    MmdsOnly,
    /// Fresh packet I/O needs a destination network provider.
    Vmnet,
}

/// One active virtqueue's host-independent cursors.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2NetworkQueueState {
    next_available: u16,
    next_used: u16,
}

impl SnapshotV2NetworkQueueState {
    /// Creates detached queue cursors.
    pub const fn new(next_available: u16, next_used: u16) -> Self {
        Self {
            next_available,
            next_used,
        }
    }

    /// Returns the next available-ring cursor.
    pub const fn next_available(self) -> u16 {
        self.next_available
    }

    /// Returns the next used-ring cursor.
    pub const fn next_used(self) -> u16 {
        self.next_used
    }
}

redacted_debug!(SnapshotV2NetworkQueueState, "SnapshotV2NetworkQueueState");

/// Host-time-free retry disposition for reconstructible guest TX work.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2NetworkRetryState {
    /// No retry is pending.
    None,
    /// Retry as soon as the destination scheduler starts.
    Immediate,
    /// Retry after a relative duration.
    After { remaining_nanos: u64 },
}

impl SnapshotV2NetworkRetryState {
    /// Returns whether a retry is pending.
    pub const fn has_retry(self) -> bool {
        !matches!(self, Self::None)
    }

    /// Returns a delayed retry duration.
    pub const fn remaining_nanos(self) -> Option<u64> {
        match self {
            Self::After { remaining_nanos } => Some(remaining_nanos),
            Self::None | Self::Immediate => None,
        }
    }
}

impl fmt::Debug for SnapshotV2NetworkRetryState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::None => "SnapshotV2NetworkRetryState::None",
            Self::Immediate => "SnapshotV2NetworkRetryState::Immediate",
            Self::After { .. } => "SnapshotV2NetworkRetryState::After(<redacted>)",
        })
    }
}

/// One enabled token bucket and its detached time-relative state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2NetworkTokenBucketState {
    size: u64,
    configured_burst: Option<u64>,
    refill_time_millis: u64,
    budget: u64,
    remaining_burst: u64,
    age_nanos: u64,
}

impl SnapshotV2NetworkTokenBucketState {
    /// Creates one bucket value. Complete state validation checks its bounds.
    pub const fn new(
        size: u64,
        configured_burst: Option<u64>,
        refill_time_millis: u64,
        budget: u64,
        remaining_burst: u64,
        age_nanos: u64,
    ) -> Self {
        Self {
            size,
            configured_burst,
            refill_time_millis,
            budget,
            remaining_burst,
            age_nanos,
        }
    }

    pub const fn size(self) -> u64 {
        self.size
    }

    pub const fn configured_burst(self) -> Option<u64> {
        self.configured_burst
    }

    pub const fn refill_time_millis(self) -> u64 {
        self.refill_time_millis
    }

    pub const fn budget(self) -> u64 {
        self.budget
    }

    pub const fn remaining_burst(self) -> u64 {
        self.remaining_burst
    }

    pub const fn age_nanos(self) -> u64 {
        self.age_nanos
    }
}

redacted_debug!(
    SnapshotV2NetworkTokenBucketState,
    "SnapshotV2NetworkTokenBucketState"
);

/// Detached bandwidth and operations buckets for one direction.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2NetworkLimiterState {
    bandwidth: Option<SnapshotV2NetworkTokenBucketState>,
    ops: Option<SnapshotV2NetworkTokenBucketState>,
}

impl SnapshotV2NetworkLimiterState {
    pub const fn new(
        bandwidth: Option<SnapshotV2NetworkTokenBucketState>,
        ops: Option<SnapshotV2NetworkTokenBucketState>,
    ) -> Self {
        Self { bandwidth, ops }
    }

    pub const fn bandwidth(self) -> Option<SnapshotV2NetworkTokenBucketState> {
        self.bandwidth
    }

    pub const fn ops(self) -> Option<SnapshotV2NetworkTokenBucketState> {
        self.ops
    }

    pub const fn is_configured(self) -> bool {
        self.bandwidth.is_some() || self.ops.is_some()
    }
}

redacted_debug!(
    SnapshotV2NetworkLimiterState,
    "SnapshotV2NetworkLimiterState"
);

/// Network-local continuation outside common virtio transport registers.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2NetworkLocalState {
    active_rx_queue: Option<SnapshotV2NetworkQueueState>,
    active_tx_queue: Option<SnapshotV2NetworkQueueState>,
    tx_retry: SnapshotV2NetworkRetryState,
}

impl SnapshotV2NetworkLocalState {
    pub const fn new(
        active_rx_queue: Option<SnapshotV2NetworkQueueState>,
        active_tx_queue: Option<SnapshotV2NetworkQueueState>,
        tx_retry: SnapshotV2NetworkRetryState,
    ) -> Self {
        Self {
            active_rx_queue,
            active_tx_queue,
            tx_retry,
        }
    }

    pub const fn active_rx_queue(&self) -> Option<SnapshotV2NetworkQueueState> {
        self.active_rx_queue
    }

    pub const fn active_tx_queue(&self) -> Option<SnapshotV2NetworkQueueState> {
        self.active_tx_queue
    }

    pub const fn tx_retry(&self) -> SnapshotV2NetworkRetryState {
        self.tx_retry
    }
}

redacted_debug!(SnapshotV2NetworkLocalState, "SnapshotV2NetworkLocalState");

/// Public construction parts for one portable network record.
#[doc(hidden)]
pub struct SnapshotV2NetworkInterfaceStateParts {
    pub iface_id: String,
    pub captured_selector: String,
    pub requested_guest_mac: Option<GuestMacAddress>,
    pub requested_mtu: Option<u16>,
    pub profile: NetworkDeviceProfile,
    pub backend: SnapshotV2NetworkBackendClass,
    pub local: SnapshotV2NetworkLocalState,
    pub virtio: SnapshotV2VirtioState,
    pub rx_limiter: SnapshotV2NetworkLimiterState,
    pub tx_limiter: SnapshotV2NetworkLimiterState,
    pub transport: SnapshotV2DeviceTransport,
}

redacted_debug!(
    SnapshotV2NetworkInterfaceStateParts,
    "SnapshotV2NetworkInterfaceStateParts"
);

/// One ordered portable network-interface record.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2NetworkInterfaceState {
    iface_id: String,
    captured_selector: String,
    requested_guest_mac: Option<GuestMacAddress>,
    requested_mtu: Option<u16>,
    profile: NetworkDeviceProfile,
    backend: SnapshotV2NetworkBackendClass,
    local: SnapshotV2NetworkLocalState,
    virtio: SnapshotV2VirtioState,
    rx_limiter: SnapshotV2NetworkLimiterState,
    tx_limiter: SnapshotV2NetworkLimiterState,
    transport: SnapshotV2DeviceTransport,
}

impl SnapshotV2NetworkInterfaceState {
    /// Converts one checked MMIO live capture without retaining source
    /// ownership or normalized source-only work.
    pub fn try_from_mmio_capture(
        config: &NetworkInterfaceConfig,
        backend: SnapshotV2NetworkBackendClass,
        region: MmioRegion,
        interrupt_line: GuestInterruptLine,
        captured: &VirtioNetworkMmioCaptureState,
    ) -> Result<Self, SnapshotV2NetworkStateCaptureError> {
        let device = captured.device();
        let mut virtio = capture_mmio_common_for_device_with_queue_count_and_config_status_gate(
            captured.transport(),
            VIRTIO_NET_DEVICE_ID,
            device.available_features(),
            VIRTIO_NET_QUEUE_COUNT,
            true,
        )
        .map_err(capture_common_error)?;
        let coarse_queue = virtio
            .interrupt_intents()
            .iter()
            .any(|intent| matches!(intent, SnapshotV2InterruptIntent::Queue { .. }));
        let coarse_configuration = virtio
            .interrupt_intents()
            .contains(&SnapshotV2InterruptIntent::Configuration);
        let mut interrupt_intents = Vec::new();
        interrupt_intents
            .try_reserve_exact(captured.interrupt_intents().len())
            .map_err(|_| SnapshotV2NetworkStateCaptureError::Allocation)?;
        interrupt_intents.extend(
            captured
                .interrupt_intents()
                .iter()
                .map(|intent| match intent {
                    VirtioInterruptIntent::Queue { queue_index } => {
                        SnapshotV2InterruptIntent::Queue {
                            queue_index: *queue_index,
                        }
                    }
                    VirtioInterruptIntent::Configuration => {
                        SnapshotV2InterruptIntent::Configuration
                    }
                }),
        );
        interrupt_intents.sort_unstable();
        let exact_queue = interrupt_intents
            .iter()
            .any(|intent| matches!(intent, SnapshotV2InterruptIntent::Queue { .. }));
        let exact_configuration =
            interrupt_intents.contains(&SnapshotV2InterruptIntent::Configuration);
        if coarse_queue != exact_queue
            || coarse_configuration != exact_configuration
            || interrupt_intents
                .windows(2)
                .any(|window| matches!(window, [left, right] if left == right))
        {
            return Err(SnapshotV2NetworkStateCaptureError::Device);
        }
        virtio.replace_interrupt_intents(interrupt_intents);
        let transport = SnapshotV2DeviceTransport::Mmio(capture_mmio_transport_parts(
            region,
            interrupt_line,
            captured.transport(),
        ));
        capture_network_interface(config, backend, device, virtio, transport)
    }

    /// Converts one checked PCI live capture without retaining source
    /// ownership or normalized source-only work.
    pub fn try_from_pci_capture(
        config: &NetworkInterfaceConfig,
        backend: SnapshotV2NetworkBackendClass,
        origin: StorageDeviceOrigin,
        sbdf: PciSbdf,
        bar_range: GuestMemoryRange,
        captured: &VirtioNetworkPciCaptureState,
    ) -> Result<Self, SnapshotV2NetworkStateCaptureError> {
        let device = captured.device();
        let virtio = capture_pci_common_for_device_with_queue_count(
            captured.transport(),
            VIRTIO_NET_DEVICE_ID,
            device.available_features(),
            VIRTIO_NET_QUEUE_COUNT,
        )
        .map_err(capture_common_error)?;
        let transport = capture_pci_transport_parts_with_queue_count(
            origin,
            sbdf,
            bar_range,
            captured.transport(),
            VIRTIO_NET_QUEUE_COUNT,
        )
        .map(SnapshotV2DeviceTransport::Pci)
        .map_err(capture_common_error)?;
        capture_network_interface(config, backend, device, virtio, transport)
    }

    /// Creates one record and validates all record-local relationships.
    pub fn try_from_parts(
        parts: SnapshotV2NetworkInterfaceStateParts,
    ) -> Result<Self, SnapshotV2NetworkStateBuildError> {
        let state = Self::from_parts_unchecked(parts);
        validate_interface(&state)?;
        Ok(state)
    }

    pub(crate) fn from_parts_unchecked(parts: SnapshotV2NetworkInterfaceStateParts) -> Self {
        Self {
            iface_id: parts.iface_id,
            captured_selector: parts.captured_selector,
            requested_guest_mac: parts.requested_guest_mac,
            requested_mtu: parts.requested_mtu,
            profile: parts.profile,
            backend: parts.backend,
            local: parts.local,
            virtio: parts.virtio,
            rx_limiter: parts.rx_limiter,
            tx_limiter: parts.tx_limiter,
            transport: parts.transport,
        }
    }

    pub fn iface_id(&self) -> &str {
        &self.iface_id
    }

    pub fn captured_selector(&self) -> &str {
        &self.captured_selector
    }

    pub const fn requested_guest_mac(&self) -> Option<GuestMacAddress> {
        self.requested_guest_mac
    }

    pub const fn requested_mtu(&self) -> Option<u16> {
        self.requested_mtu
    }

    pub const fn profile(&self) -> NetworkDeviceProfile {
        self.profile
    }

    pub const fn backend(&self) -> SnapshotV2NetworkBackendClass {
        self.backend
    }

    pub const fn local(&self) -> &SnapshotV2NetworkLocalState {
        &self.local
    }

    pub const fn virtio(&self) -> &SnapshotV2VirtioState {
        &self.virtio
    }

    pub const fn rx_limiter(&self) -> SnapshotV2NetworkLimiterState {
        self.rx_limiter
    }

    pub const fn tx_limiter(&self) -> SnapshotV2NetworkLimiterState {
        self.tx_limiter
    }

    pub const fn transport(&self) -> &SnapshotV2DeviceTransport {
        &self.transport
    }

    /// Consumes this record into its still-detached portable fields.
    pub fn into_parts(self) -> SnapshotV2NetworkInterfaceStateParts {
        SnapshotV2NetworkInterfaceStateParts {
            iface_id: self.iface_id,
            captured_selector: self.captured_selector,
            requested_guest_mac: self.requested_guest_mac,
            requested_mtu: self.requested_mtu,
            profile: self.profile,
            backend: self.backend,
            local: self.local,
            virtio: self.virtio,
            rx_limiter: self.rx_limiter,
            tx_limiter: self.tx_limiter,
            transport: self.transport,
        }
    }
}

redacted_debug!(
    SnapshotV2NetworkInterfaceState,
    "SnapshotV2NetworkInterfaceState"
);

/// One selected interface and its reconstructible fresh MMDS stack identity.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2MmdsInterfaceState {
    interface_index: u16,
    local_mac_address: EthernetMacAddress,
    ipv4_address: Ipv4Addr,
    tcp_port: u16,
}

impl SnapshotV2MmdsInterfaceState {
    pub const fn new(
        interface_index: u16,
        local_mac_address: EthernetMacAddress,
        ipv4_address: Ipv4Addr,
        tcp_port: u16,
    ) -> Self {
        Self {
            interface_index,
            local_mac_address,
            ipv4_address,
            tcp_port,
        }
    }

    pub const fn interface_index(self) -> u16 {
        self.interface_index
    }

    pub const fn local_mac_address(self) -> EthernetMacAddress {
        self.local_mac_address
    }

    pub const fn ipv4_address(self) -> Ipv4Addr {
        self.ipv4_address
    }

    pub const fn tcp_port(self) -> u16 {
        self.tcp_port
    }
}

redacted_debug!(SnapshotV2MmdsInterfaceState, "SnapshotV2MmdsInterfaceState");

/// Portable MMDS configuration without metadata or live protocol state.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2MmdsState {
    version: MmdsVersion,
    ipv4_address: Option<Ipv4Addr>,
    imds_compat: bool,
    interfaces: Vec<SnapshotV2MmdsInterfaceState>,
}

impl SnapshotV2MmdsState {
    pub fn new(
        version: MmdsVersion,
        ipv4_address: Option<Ipv4Addr>,
        imds_compat: bool,
        interfaces: Vec<SnapshotV2MmdsInterfaceState>,
    ) -> Self {
        Self {
            version,
            ipv4_address,
            imds_compat,
            interfaces,
        }
    }

    pub const fn version(&self) -> MmdsVersion {
        self.version
    }

    pub const fn ipv4_address(&self) -> Option<Ipv4Addr> {
        self.ipv4_address
    }

    pub fn effective_ipv4_address(&self) -> Ipv4Addr {
        self.ipv4_address.unwrap_or(DEFAULT_MMDS_IPV4_ADDRESS)
    }

    pub const fn imds_compat(&self) -> bool {
        self.imds_compat
    }

    pub fn interfaces(&self) -> &[SnapshotV2MmdsInterfaceState] {
        &self.interfaces
    }
}

redacted_debug!(SnapshotV2MmdsState, "SnapshotV2MmdsState");

/// Complete bounded exact-2.11 network component value.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2NetworkState {
    interfaces: Vec<SnapshotV2NetworkInterfaceState>,
    mmds: Option<SnapshotV2MmdsState>,
}

/// Owned parts of one validated exact-2.11 network aggregate.
pub type SnapshotV2NetworkStateParts = (
    Vec<SnapshotV2NetworkInterfaceState>,
    Option<SnapshotV2MmdsState>,
);

impl SnapshotV2NetworkState {
    /// Validates and retains one complete aggregate.
    pub fn try_new(
        interfaces: Vec<SnapshotV2NetworkInterfaceState>,
        mmds: Option<SnapshotV2MmdsState>,
    ) -> Result<Self, SnapshotV2NetworkStateBuildError> {
        let state = Self { interfaces, mmds };
        validate_network_state(&state)?;
        Ok(state)
    }

    pub fn interfaces(&self) -> &[SnapshotV2NetworkInterfaceState] {
        &self.interfaces
    }

    pub const fn mmds(&self) -> Option<&SnapshotV2MmdsState> {
        self.mmds.as_ref()
    }

    /// Consumes the aggregate without changing interface or MMDS order.
    pub fn into_parts(self) -> SnapshotV2NetworkStateParts {
        (self.interfaces, self.mmds)
    }

    pub const fn compatibility_version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION
    }

    /// Encodes this value in an exact outer compatibility context.
    pub fn encode(
        &self,
        outer_version: SnapshotFormatVersion,
    ) -> Result<Vec<u8>, SnapshotV2NetworkStateEncodeError> {
        codec::encode(outer_version, self)
    }

    /// Decodes and validates one exact component payload.
    pub fn decode(
        outer_version: SnapshotFormatVersion,
        bytes: &[u8],
    ) -> Result<Self, SnapshotV2NetworkStateDecodeError> {
        codec::decode(outer_version, bytes)
    }
}

redacted_debug!(SnapshotV2NetworkState, "SnapshotV2NetworkState");

/// Typed relationship failure for an exact network aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2NetworkStateBuildError {
    InterfaceCount,
    InterfaceIdentity,
    InterfaceProfile,
    DuplicateInterface,
    DuplicateMac,
    Virtio,
    Queue,
    Limiter,
    Retry,
    Transport,
    DuplicatePlacement,
    Mmds,
}

impl fmt::Display for SnapshotV2NetworkStateBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InterfaceCount => "native-v2 network interface count is invalid",
            Self::InterfaceIdentity => "native-v2 network interface identity is invalid",
            Self::InterfaceProfile => "native-v2 network guest profile is invalid",
            Self::DuplicateInterface => "native-v2 network interface identity is duplicated",
            Self::DuplicateMac => "native-v2 network guest MAC identity is duplicated",
            Self::Virtio => "native-v2 network common virtio state is invalid",
            Self::Queue => "native-v2 network queue state is invalid",
            Self::Limiter => "native-v2 network limiter state is invalid",
            Self::Retry => "native-v2 network retry state is invalid",
            Self::Transport => "native-v2 network transport state is invalid",
            Self::DuplicatePlacement => "native-v2 network placement is duplicated or overlapping",
            Self::Mmds => "native-v2 MMDS configuration is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2NetworkStateBuildError {}

/// Failure while converting one trusted live network capture into exact-2.11
/// state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2NetworkStateCaptureError {
    /// A bounded string or common-state collection could not be allocated.
    Allocation,
    /// Repeated device, profile, queue, and common state disagree.
    Device,
    /// Source-owned cached packet or retry work was normalized away.
    NormalizedWork,
    /// Common virtio or transport capture failed.
    Common {
        /// Redacted common capture category.
        source: SnapshotV2DeviceGraphCaptureError,
    },
    /// Complete converted state failed its final semantic gate.
    Build {
        /// Redacted exact-2.11 build category.
        source: SnapshotV2NetworkStateBuildError,
    },
}

impl fmt::Debug for SnapshotV2NetworkStateCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2NetworkStateCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Allocation => "native-v2 captured network state allocation failed",
            Self::Device => "native-v2 captured network device state is inconsistent",
            Self::NormalizedWork => {
                "native-v2 captured network state contains normalized source work"
            }
            Self::Common { .. } => "native-v2 captured network transport state is invalid",
            Self::Build { .. } => "native-v2 captured network state is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2NetworkStateCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Common { source } => Some(source),
            Self::Build { source } => Some(source),
            Self::Allocation | Self::Device | Self::NormalizedWork => None,
        }
    }
}

/// Failure while encoding exact-2.11 network state.
#[derive(Debug)]
pub enum SnapshotV2NetworkStateEncodeError {
    UnsupportedVersion,
    InvalidState(SnapshotV2NetworkStateBuildError),
    LengthOverflow,
    TooLarge,
    Allocation,
}

impl fmt::Display for SnapshotV2NetworkStateEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion => {
                formatter.write_str("native-v2 network state version is unsupported")
            }
            Self::InvalidState(source) => write!(formatter, "invalid network state: {source}"),
            Self::LengthOverflow => {
                formatter.write_str("native-v2 network state length overflowed")
            }
            Self::TooLarge => formatter.write_str("native-v2 network state exceeds its fixed cap"),
            Self::Allocation => formatter.write_str("native-v2 network state allocation failed"),
        }
    }
}

impl std::error::Error for SnapshotV2NetworkStateEncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidState(source) => Some(source),
            Self::UnsupportedVersion | Self::LengthOverflow | Self::TooLarge | Self::Allocation => {
                None
            }
        }
    }
}

/// Failure while decoding exact-2.11 network state.
#[derive(Debug)]
pub enum SnapshotV2NetworkStateDecodeError {
    UnsupportedVersion,
    TooLarge,
    Truncated,
    InvalidHeader,
    InvalidLayout,
    InvalidField,
    InvalidUtf8,
    Allocation,
    InvalidState(SnapshotV2NetworkStateBuildError),
}

impl fmt::Display for SnapshotV2NetworkStateDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion => {
                formatter.write_str("native-v2 network state version is unsupported")
            }
            Self::TooLarge => formatter.write_str("native-v2 network state exceeds its fixed cap"),
            Self::Truncated => formatter.write_str("native-v2 network state is truncated"),
            Self::InvalidHeader => formatter.write_str("native-v2 network state header is invalid"),
            Self::InvalidLayout => formatter.write_str("native-v2 network state layout is invalid"),
            Self::InvalidField => formatter.write_str("native-v2 network state field is invalid"),
            Self::InvalidUtf8 => {
                formatter.write_str("native-v2 network state string is invalid UTF-8")
            }
            Self::Allocation => formatter.write_str("native-v2 network state allocation failed"),
            Self::InvalidState(source) => {
                write!(formatter, "decoded network state is invalid: {source}")
            }
        }
    }
}

impl std::error::Error for SnapshotV2NetworkStateDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidState(source) => Some(source),
            Self::UnsupportedVersion
            | Self::TooLarge
            | Self::Truncated
            | Self::InvalidHeader
            | Self::InvalidLayout
            | Self::InvalidField
            | Self::InvalidUtf8
            | Self::Allocation => None,
        }
    }
}

fn capture_network_interface(
    config: &NetworkInterfaceConfig,
    backend: SnapshotV2NetworkBackendClass,
    device: &VirtioNetworkDeviceCaptureState,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
) -> Result<SnapshotV2NetworkInterfaceState, SnapshotV2NetworkStateCaptureError> {
    if device.source_rx_cache_normalized() || device.source_rx_retry_normalized() {
        return Err(SnapshotV2NetworkStateCaptureError::NormalizedWork);
    }
    if device.available_features() != virtio.available_features()
        || device.negotiated_features() != virtio.driver_features()
        || device.active_rx_queue().is_some() != virtio.is_activated()
        || device.active_tx_queue().is_some() != virtio.is_activated()
    {
        return Err(SnapshotV2NetworkStateCaptureError::Device);
    }

    let iface_id = copy_capture_string(config.iface_id())?;
    let captured_selector = copy_capture_string(config.host_dev_name())?;
    let local = SnapshotV2NetworkLocalState::new(
        device.active_rx_queue().map(|queue| {
            SnapshotV2NetworkQueueState::new(queue.next_available(), queue.next_used())
        }),
        device.active_tx_queue().map(|queue| {
            SnapshotV2NetworkQueueState::new(queue.next_available(), queue.next_used())
        }),
        capture_retry(device.tx_retry()),
    );
    let rx_limiter = capture_limiter(device.rx_rate_limiter());
    let tx_limiter = capture_limiter(device.tx_rate_limiter());
    SnapshotV2NetworkInterfaceState::try_from_parts(SnapshotV2NetworkInterfaceStateParts {
        iface_id,
        captured_selector,
        requested_guest_mac: config.guest_mac(),
        requested_mtu: config.mtu(),
        profile: device.profile(),
        backend,
        local,
        virtio,
        rx_limiter,
        tx_limiter,
        transport,
    })
    .map_err(|source| SnapshotV2NetworkStateCaptureError::Build { source })
}

fn capture_limiter(limiter: VirtioNetworkRateLimiterCaptureState) -> SnapshotV2NetworkLimiterState {
    SnapshotV2NetworkLimiterState::new(
        limiter.bandwidth().map(capture_token_bucket),
        limiter.ops().map(capture_token_bucket),
    )
}

fn capture_token_bucket(
    bucket: VirtioNetworkTokenBucketCaptureState,
) -> SnapshotV2NetworkTokenBucketState {
    let config = bucket.config();
    SnapshotV2NetworkTokenBucketState::new(
        config.size(),
        config.one_time_burst(),
        config.refill_time(),
        bucket.budget(),
        bucket.one_time_burst(),
        bucket.age_nanos(),
    )
}

const fn capture_retry(retry: VirtioNetworkRetryCaptureState) -> SnapshotV2NetworkRetryState {
    match retry {
        VirtioNetworkRetryCaptureState::None => SnapshotV2NetworkRetryState::None,
        VirtioNetworkRetryCaptureState::Immediate => SnapshotV2NetworkRetryState::Immediate,
        VirtioNetworkRetryCaptureState::After { remaining_nanos } => {
            SnapshotV2NetworkRetryState::After { remaining_nanos }
        }
    }
}

fn copy_capture_string(value: &str) -> Result<String, SnapshotV2NetworkStateCaptureError> {
    let mut copy = String::new();
    copy.try_reserve_exact(value.len())
        .map_err(|_| SnapshotV2NetworkStateCaptureError::Allocation)?;
    copy.push_str(value);
    Ok(copy)
}

fn capture_common_error(
    source: SnapshotV2DeviceGraphCaptureError,
) -> SnapshotV2NetworkStateCaptureError {
    if source == SnapshotV2DeviceGraphCaptureError::Allocation {
        SnapshotV2NetworkStateCaptureError::Allocation
    } else {
        SnapshotV2NetworkStateCaptureError::Common { source }
    }
}

fn validate_network_state(
    state: &SnapshotV2NetworkState,
) -> Result<(), SnapshotV2NetworkStateBuildError> {
    if !(1..=NATIVE_V2_NETWORK_MAX_INTERFACES).contains(&state.interfaces.len()) {
        return Err(SnapshotV2NetworkStateBuildError::InterfaceCount);
    }

    let transport_kind = state
        .interfaces
        .first()
        .map(|interface| interface.transport.kind())
        .ok_or(SnapshotV2NetworkStateBuildError::InterfaceCount)?;

    for (interface_index, interface) in state.interfaces.iter().enumerate() {
        validate_interface(interface)?;
        if interface.transport.kind() != transport_kind {
            return Err(SnapshotV2NetworkStateBuildError::Transport);
        }

        let placement = match &interface.transport {
            SnapshotV2DeviceTransport::Mmio(mmio) => mmio.region().range(),
            SnapshotV2DeviceTransport::Pci(pci) => pci.bar_range(),
        };
        for previous in state.interfaces.iter().take(interface_index) {
            if interface.iface_id == previous.iface_id {
                return Err(SnapshotV2NetworkStateBuildError::DuplicateInterface);
            }
            if interface
                .requested_guest_mac
                .is_some_and(|mac| previous.requested_guest_mac == Some(mac))
                || interface
                    .profile
                    .guest_mac()
                    .is_some_and(|mac| previous.profile.guest_mac() == Some(mac))
            {
                return Err(SnapshotV2NetworkStateBuildError::DuplicateMac);
            }
            let previous_placement = match (&interface.transport, &previous.transport) {
                (
                    SnapshotV2DeviceTransport::Mmio(current),
                    SnapshotV2DeviceTransport::Mmio(previous),
                ) => {
                    if current.region().id() == previous.region().id()
                        || current.interrupt_line() == previous.interrupt_line()
                    {
                        return Err(SnapshotV2NetworkStateBuildError::DuplicatePlacement);
                    }
                    previous.region().range()
                }
                (
                    SnapshotV2DeviceTransport::Pci(current),
                    SnapshotV2DeviceTransport::Pci(previous),
                ) => {
                    if current.sbdf() == previous.sbdf() {
                        return Err(SnapshotV2NetworkStateBuildError::DuplicatePlacement);
                    }
                    previous.bar_range()
                }
                _ => return Err(SnapshotV2NetworkStateBuildError::Transport),
            };
            if placement.overlaps(previous_placement) {
                return Err(SnapshotV2NetworkStateBuildError::DuplicatePlacement);
            }
            for previous_queue in previous.virtio.queues() {
                if let Some(previous_ranges) = queue_ranges(previous_queue)
                    .map_err(|_| SnapshotV2NetworkStateBuildError::Queue)?
                    && previous_ranges
                        .iter()
                        .any(|range| range.overlaps(placement))
                {
                    return Err(SnapshotV2NetworkStateBuildError::Queue);
                }
            }
        }

        for (queue_index, queue) in interface.virtio.queues().iter().enumerate() {
            if let Some(ranges) =
                queue_ranges(queue).map_err(|_| SnapshotV2NetworkStateBuildError::Queue)?
            {
                if ranges.iter().any(|range| range.overlaps(placement)) {
                    return Err(SnapshotV2NetworkStateBuildError::Queue);
                }
                for previous_queue in interface.virtio.queues().iter().take(queue_index) {
                    if let Some(previous_ranges) = queue_ranges(previous_queue)
                        .map_err(|_| SnapshotV2NetworkStateBuildError::Queue)?
                        && ranges.iter().any(|range| {
                            previous_ranges
                                .iter()
                                .any(|previous| range.overlaps(*previous))
                        })
                    {
                        return Err(SnapshotV2NetworkStateBuildError::Queue);
                    }
                }
                for previous_interface in state.interfaces.iter().take(interface_index) {
                    let previous_placement = match &previous_interface.transport {
                        SnapshotV2DeviceTransport::Mmio(mmio) => mmio.region().range(),
                        SnapshotV2DeviceTransport::Pci(pci) => pci.bar_range(),
                    };
                    if ranges
                        .iter()
                        .any(|range| range.overlaps(previous_placement))
                    {
                        return Err(SnapshotV2NetworkStateBuildError::Queue);
                    }
                    for previous_queue in previous_interface.virtio.queues() {
                        if let Some(previous_ranges) = queue_ranges(previous_queue)
                            .map_err(|_| SnapshotV2NetworkStateBuildError::Queue)?
                            && ranges.iter().any(|range| {
                                previous_ranges
                                    .iter()
                                    .any(|previous| range.overlaps(*previous))
                            })
                        {
                            return Err(SnapshotV2NetworkStateBuildError::Queue);
                        }
                    }
                }
            }
        }
    }

    validate_mmds_relationship(state)
}

fn validate_interface(
    interface: &SnapshotV2NetworkInterfaceState,
) -> Result<(), SnapshotV2NetworkStateBuildError> {
    if interface.iface_id.is_empty()
        || interface.iface_id.len() > NATIVE_V2_NETWORK_MAX_INTERFACE_ID_BYTES
        || !interface
            .iface_id
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
        || interface.captured_selector.is_empty()
        || interface.captured_selector.len() > NATIVE_V2_NETWORK_MAX_CAPTURED_SELECTOR_BYTES
        || interface.captured_selector.chars().any(char::is_control)
    {
        return Err(SnapshotV2NetworkStateBuildError::InterfaceIdentity);
    }

    let profile = interface.profile;
    if interface
        .requested_guest_mac
        .is_some_and(|requested| profile.guest_mac() != Some(requested))
        || interface
            .requested_mtu
            .is_some_and(|requested| profile.mtu() != Some(requested))
        || interface
            .requested_mtu
            .is_some_and(|mtu| !(VIRTIO_NET_MIN_MTU..=VIRTIO_NET_MAX_MTU).contains(&mtu))
        || profile
            .mtu()
            .is_some_and(|mtu| !(VIRTIO_NET_MIN_MTU..=VIRTIO_NET_MAX_MTU).contains(&mtu))
        || !profile.feature_capabilities().is_dependency_complete()
        || matches!(interface.backend, SnapshotV2NetworkBackendClass::Vmnet)
            && profile.guest_mac().is_none()
        || matches!(interface.backend, SnapshotV2NetworkBackendClass::MmdsOnly)
            && (profile.guest_mac() != interface.requested_guest_mac
                || profile.mtu() != interface.requested_mtu
                || profile.packet_envelope()
                    != crate::network_packet::VirtioNetworkPacketEnvelope::RawEthernet)
    {
        return Err(SnapshotV2NetworkStateBuildError::InterfaceProfile);
    }

    validate_virtio(interface)?;
    validate_limiter(interface.rx_limiter)?;
    validate_limiter(interface.tx_limiter)?;
    if matches!(
        interface.local.tx_retry,
        SnapshotV2NetworkRetryState::After { remaining_nanos: 0 }
    ) || interface.local.tx_retry.has_retry()
        && (interface.local.active_tx_queue.is_none() || !interface.tx_limiter.is_configured())
    {
        return Err(SnapshotV2NetworkStateBuildError::Retry);
    }
    validate_transport(&interface.transport)
}

fn validate_virtio(
    interface: &SnapshotV2NetworkInterfaceState,
) -> Result<(), SnapshotV2NetworkStateBuildError> {
    let state = &interface.virtio;
    let expected_features = VirtioNetworkConfigSpace::with_feature_capabilities(
        interface.profile.guest_mac(),
        interface.profile.mtu(),
        interface.profile.feature_capabilities(),
    )
    .available_features();
    if state.available_features() != expected_features
        || state.driver_features() & !state.available_features() != 0
        || state.queues().len() != VIRTIO_NET_QUEUE_COUNT
        || state.pending_notifications().len() > VIRTIO_NET_QUEUE_COUNT
        || state.interrupt_intents().len() > VIRTIO_NET_QUEUE_COUNT + 1
    {
        return Err(SnapshotV2NetworkStateBuildError::Virtio);
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
        || state.status() & VIRTIO_DEVICE_STATUS_DRIVER == 0 && state.driver_features() != 0
        || state.status() & VIRTIO_DEVICE_STATUS_FEATURES_OK != 0
            && state.driver_features() & VIRTIO_MMIO_VERSION_1_FEATURE == 0
        || state.is_activated() != (state.status() == healthy_driver_ok)
    {
        return Err(SnapshotV2NetworkStateBuildError::Virtio);
    }

    for queue in state.queues() {
        validate_queue(queue)?;
        if state.status() & VIRTIO_DEVICE_STATUS_FEATURES_OK == 0
            && (queue.size() != 0
                || queue.ready()
                || queue.descriptor_table().raw_value() != 0
                || queue.driver_ring().raw_value() != 0
                || queue.device_ring().raw_value() != 0)
        {
            return Err(SnapshotV2NetworkStateBuildError::Queue);
        }
    }
    if state.is_activated()
        && (!state.queues().iter().all(|queue| queue.ready())
            || interface.local.active_rx_queue.is_none()
            || interface.local.active_tx_queue.is_none())
        || !state.is_activated()
            && (interface.local.active_rx_queue.is_some()
                || interface.local.active_tx_queue.is_some()
                || !state.pending_notifications().is_empty())
        || interface
            .local
            .active_rx_queue
            .is_some_and(|queue| queue.next_available != queue.next_used)
        || interface
            .local
            .active_tx_queue
            .is_some_and(|queue| queue.next_available != queue.next_used)
    {
        return Err(SnapshotV2NetworkStateBuildError::Queue);
    }

    if !state
        .pending_notifications()
        .windows(2)
        .all(|window| matches!(window, [left, right] if left < right))
        || state
            .pending_notifications()
            .iter()
            .any(|index| usize::from(*index) >= VIRTIO_NET_QUEUE_COUNT)
        || !state
            .interrupt_intents()
            .windows(2)
            .all(|window| matches!(window, [left, right] if left < right))
        || state.interrupt_intents().iter().any(|intent| {
            matches!(
                intent,
                SnapshotV2InterruptIntent::Queue { queue_index }
                    if usize::from(*queue_index) >= VIRTIO_NET_QUEUE_COUNT
            )
        })
    {
        return Err(SnapshotV2NetworkStateBuildError::Virtio);
    }
    Ok(())
}

fn validate_queue(
    queue: &SnapshotV2VirtioQueueState,
) -> Result<(), SnapshotV2NetworkStateBuildError> {
    if queue.max_size() != VIRTIO_NET_QUEUE_SIZE
        || queue.size() > queue.max_size()
        || queue.size() != 0 && !queue.size().is_power_of_two()
        || queue.ready() && queue.size() == 0
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
        || queue.size() == 0
            && (queue.descriptor_table().raw_value() != 0
                || queue.driver_ring().raw_value() != 0
                || queue.device_ring().raw_value() != 0)
    {
        return Err(SnapshotV2NetworkStateBuildError::Queue);
    }
    if let Some(ranges) =
        queue_ranges(queue).map_err(|_| SnapshotV2NetworkStateBuildError::Queue)?
        && (ranges[0].overlaps(ranges[1])
            || ranges[0].overlaps(ranges[2])
            || ranges[1].overlaps(ranges[2]))
    {
        return Err(SnapshotV2NetworkStateBuildError::Queue);
    }
    Ok(())
}

fn validate_limiter(
    limiter: SnapshotV2NetworkLimiterState,
) -> Result<(), SnapshotV2NetworkStateBuildError> {
    for bucket in [limiter.bandwidth, limiter.ops].into_iter().flatten() {
        if bucket.size == 0
            || bucket
                .refill_time_millis
                .checked_mul(1_000_000)
                .is_none_or(|nanos| nanos == 0)
            || bucket.budget > bucket.size
            || bucket.remaining_burst > bucket.configured_burst.unwrap_or(0)
        {
            return Err(SnapshotV2NetworkStateBuildError::Limiter);
        }
    }
    Ok(())
}

fn validate_transport(
    transport: &SnapshotV2DeviceTransport,
) -> Result<(), SnapshotV2NetworkStateBuildError> {
    match transport {
        SnapshotV2DeviceTransport::Mmio(mmio) => validate_mmio(mmio),
        SnapshotV2DeviceTransport::Pci(pci) => validate_pci(pci),
    }
}

fn validate_mmio(
    state: &SnapshotV2MmioDeviceState,
) -> Result<(), SnapshotV2NetworkStateBuildError> {
    if state.device_feature_select() > 1
        || state.driver_feature_select() > 1
        || usize::try_from(state.queue_select())
            .ok()
            .is_none_or(|queue| queue >= VIRTIO_NET_QUEUE_COUNT)
        || state.region().id().raw_value() == 0
        || state.region().range().size() != VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        || state
            .region()
            .range()
            .validate_alignment(VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
            .is_err()
        || state.interrupt_line().raw_value() < 32
    {
        Err(SnapshotV2NetworkStateBuildError::Transport)
    } else {
        Ok(())
    }
}

fn validate_pci(state: &SnapshotV2PciDeviceState) -> Result<(), SnapshotV2NetworkStateBuildError> {
    const WRITABLE_OFFSETS: [u16; 4] = [0x04, 0x05, 0x0c, 0x3c];
    let aperture_end = PCI_BAR64_START
        .checked_add(PCI_BAR64_SIZE)
        .ok_or(SnapshotV2NetworkStateBuildError::Transport)?;
    if state.phase() != VirtioPciEndpointPhase::Active
        || !matches!(
            state.origin(),
            StorageDeviceOrigin::Startup | StorageDeviceOrigin::Runtime
        )
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
        || usize::from(state.queue_select()) >= VIRTIO_NET_QUEUE_COUNT
        || state.writable_bytes().len() != WRITABLE_OFFSETS.len()
        || state
            .writable_bytes()
            .iter()
            .map(|byte| byte.offset())
            .ne(WRITABLE_OFFSETS)
        || state.bar_probes().len() != 2
        || state
            .bar_probes()
            .iter()
            .map(|probe| probe.index())
            .ne([0, 1])
    {
        return Err(SnapshotV2NetworkStateBuildError::Transport);
    }
    let msix = state.msix();
    if msix.entries().len() != 3
        || msix.entries().len() > VIRTIO_PCI_MAX_MSIX_VECTORS
        || msix.pending_words().len() != 1
        || msix.queue_vectors().len() != VIRTIO_NET_QUEUE_COUNT
        || msix
            .pending_words()
            .first()
            .copied()
            .is_none_or(|pending| pending & !0b111 != 0)
        || !valid_msix_vector(msix.config_vector(), msix.entries().len())
        || msix
            .queue_vectors()
            .iter()
            .copied()
            .any(|vector| !valid_msix_vector(vector, msix.entries().len()))
        || msix
            .entries()
            .iter()
            .any(|entry| entry.vector_control() & !1 != 0)
    {
        return Err(SnapshotV2NetworkStateBuildError::Transport);
    }
    Ok(())
}

fn valid_msix_vector(vector: u16, count: usize) -> bool {
    vector == VIRTIO_PCI_NO_VECTOR || usize::from(vector) < count
}

fn validate_mmds_relationship(
    state: &SnapshotV2NetworkState,
) -> Result<(), SnapshotV2NetworkStateBuildError> {
    let Some(mmds) = &state.mmds else {
        return if state
            .interfaces
            .iter()
            .all(|interface| interface.backend == SnapshotV2NetworkBackendClass::Vmnet)
        {
            Ok(())
        } else {
            Err(SnapshotV2NetworkStateBuildError::Mmds)
        };
    };
    if mmds.interfaces.is_empty()
        || mmds.interfaces.len() > state.interfaces.len()
        || mmds
            .ipv4_address
            .is_some_and(|address| !matches!(address.octets(), [169, 254, 1..=254, _]))
        || !mmds
            .interfaces
            .windows(2)
            .all(|window| {
                matches!(window, [left, right] if left.interface_index < right.interface_index)
            })
    {
        return Err(SnapshotV2NetworkStateBuildError::Mmds);
    }
    let effective_address = mmds.effective_ipv4_address();
    if mmds.interfaces.iter().any(|interface| {
        usize::from(interface.interface_index) >= state.interfaces.len()
            || interface.local_mac_address != DEFAULT_MMDS_MAC_ADDRESS
            || interface.ipv4_address != effective_address
            || interface.tcp_port != MMDS_GUEST_TCP_PORT
    }) {
        return Err(SnapshotV2NetworkStateBuildError::Mmds);
    }
    let expected_backend = if mmds.interfaces.len() == state.interfaces.len() {
        SnapshotV2NetworkBackendClass::MmdsOnly
    } else {
        SnapshotV2NetworkBackendClass::Vmnet
    };
    if state
        .interfaces
        .iter()
        .any(|interface| interface.backend != expected_backend)
    {
        return Err(SnapshotV2NetworkStateBuildError::Mmds);
    }
    Ok(())
}
