//! Exact native-v2 2.12 portable vsock state.
//!
//! The component retains only reconstructible guest/device continuation.
//! Socket owners, listeners, connections, guest memory, metrics, callbacks,
//! and other host authority are deliberately outside this model.

use std::fmt;

use crate::interrupt::GuestInterruptLine;
use crate::memory::GuestMemoryRange;
use crate::mmio::MmioRegion;
use crate::pci::{
    PCI_BAR64_SIZE, PCI_BAR64_START, PCI_BUS_ZERO, PCI_FIRST_ENDPOINT_DEVICE, PCI_FUNCTION_ZERO,
    PCI_LAST_ENDPOINT_DEVICE, PCI_SEGMENT_ZERO, PciBarAddressSpace, PciBarPrefetchable, PciSbdf,
};
use crate::snapshot_device_v2::{
    SnapshotV2DeviceGraphCaptureError, SnapshotV2DeviceKey, SnapshotV2DeviceTransport,
    SnapshotV2InterruptIntent, SnapshotV2MmioDeviceState, SnapshotV2PciDeviceState,
    SnapshotV2PciMsixState, SnapshotV2VirtioQueueState, SnapshotV2VirtioState,
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
    VIRTIO_DEVICE_STATUS_FEATURES_OK, VIRTIO_DEVICE_STATUS_INIT,
};
use crate::virtio_mmio::{VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VIRTIO_MMIO_VERSION_1_FEATURE};
use crate::virtio_pci::{
    VIRTIO_PCI_CAPABILITY_BAR_INDEX, VIRTIO_PCI_CAPABILITY_BAR_SIZE, VIRTIO_PCI_MAX_MSIX_VECTORS,
    VIRTIO_PCI_NO_VECTOR, VirtioPciEndpointPhase,
};
use crate::vsock::VIRTIO_VSOCK_DEVICE_ID;
use crate::vsock::{
    MIN_GUEST_CID, VIRTIO_RING_FEATURE_EVENT_IDX, VIRTIO_VSOCK_QUEUE_COUNT,
    VIRTIO_VSOCK_QUEUE_SIZE, VirtioVsockActiveQueuesCaptureState, VirtioVsockConfigSpace,
    VirtioVsockDeviceCaptureState, VirtioVsockMmioCaptureState, VirtioVsockPciCaptureState,
    VsockBackendSelector, VsockHostLocalPortCursor,
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

/// Exact compatibility context of the optional singleton vsock component.
pub const NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION: SnapshotFormatVersion =
    SnapshotFormatVersion::new(2, 12, 0);

/// Maximum UTF-8 byte length of the inert backend selector.
///
/// This freezes the supported macOS `sockaddr_un.sun_path` capacity while
/// reserving one byte for the terminating NUL.
pub const NATIVE_V2_VSOCK_MAX_SELECTOR_BYTES: usize = 103;

/// Fixed component header size.
pub const NATIVE_V2_VSOCK_STATE_HEADER_BYTES: usize = 64;

/// Fixed encoded size of one section-directory entry.
pub const NATIVE_V2_VSOCK_STATE_SECTION_ENTRY_BYTES: usize = 32;

/// Number of canonical sections in the component.
pub const NATIVE_V2_VSOCK_STATE_SECTION_COUNT: usize = 3;

/// Fixed prefix of the local identity and continuation section.
pub const NATIVE_V2_VSOCK_LOCAL_PREFIX_BYTES: usize = 48;

/// Maximum complete common virtio section for three queues.
pub const NATIVE_V2_VSOCK_COMMON_STATE_MAX_BYTES: usize = 152;

/// Exact MMIO transport section size.
pub const NATIVE_V2_VSOCK_MMIO_STATE_BYTES: usize = 48;

/// Exact PCI transport section size.
pub const NATIVE_V2_VSOCK_PCI_STATE_BYTES: usize = 176;

const MAX_LOCAL_BYTES: usize = align_up_const(
    NATIVE_V2_VSOCK_LOCAL_PREFIX_BYTES + NATIVE_V2_VSOCK_MAX_SELECTOR_BYTES,
    8,
);

/// Exact maximum shape derivable from all bounded component fields.
pub const NATIVE_V2_VSOCK_STATE_WORST_CASE_BYTES: usize = NATIVE_V2_VSOCK_STATE_HEADER_BYTES
    + NATIVE_V2_VSOCK_STATE_SECTION_COUNT * NATIVE_V2_VSOCK_STATE_SECTION_ENTRY_BYTES
    + MAX_LOCAL_BYTES
    + NATIVE_V2_VSOCK_COMMON_STATE_MAX_BYTES
    + NATIVE_V2_VSOCK_PCI_STATE_BYTES;

/// Defensive maximum component size.
pub const NATIVE_V2_VSOCK_STATE_MAX_BYTES: usize = 64 * 1024;

const _: () = assert!(VIRTIO_VSOCK_QUEUE_COUNT == 3);
const _: () = assert!(MAX_LOCAL_BYTES == 152);
const _: () = assert!(NATIVE_V2_VSOCK_STATE_WORST_CASE_BYTES == 640);
const _: () = assert!(NATIVE_V2_VSOCK_STATE_WORST_CASE_BYTES <= NATIVE_V2_VSOCK_STATE_MAX_BYTES);
const _: () = assert!(
    NATIVE_V2_VSOCK_STATE_MAX_BYTES <= crate::snapshot_format_v2::NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES
);

const fn align_up_const(value: usize, alignment: usize) -> usize {
    let remainder = value % alignment;
    if remainder == 0 {
        value
    } else {
        value + alignment - remainder
    }
}

/// One active vsock queue's host-independent ring cursors.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2VsockQueueState {
    next_available: u16,
    next_used: u16,
    event_idx_enabled: bool,
}

impl SnapshotV2VsockQueueState {
    /// Creates one detached queue continuation value.
    pub const fn new(next_available: u16, next_used: u16, event_idx_enabled: bool) -> Self {
        Self {
            next_available,
            next_used,
            event_idx_enabled,
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

    /// Returns whether EVENT_IDX behavior is active for this queue.
    pub const fn event_idx_enabled(self) -> bool {
        self.event_idx_enabled
    }
}

redacted_debug!(SnapshotV2VsockQueueState, "SnapshotV2VsockQueueState");

/// All-or-none continuation of the active RX, TX, and event queues.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2VsockActiveQueuesState {
    rx: SnapshotV2VsockQueueState,
    tx: SnapshotV2VsockQueueState,
    event: SnapshotV2VsockQueueState,
}

impl SnapshotV2VsockActiveQueuesState {
    /// Creates the three canonical active queue continuations.
    pub const fn new(
        rx: SnapshotV2VsockQueueState,
        tx: SnapshotV2VsockQueueState,
        event: SnapshotV2VsockQueueState,
    ) -> Self {
        Self { rx, tx, event }
    }

    /// Returns RX queue continuation.
    pub const fn rx(self) -> SnapshotV2VsockQueueState {
        self.rx
    }

    /// Returns TX queue continuation.
    pub const fn tx(self) -> SnapshotV2VsockQueueState {
        self.tx
    }

    /// Returns event queue continuation.
    pub const fn event(self) -> SnapshotV2VsockQueueState {
        self.event
    }

    const fn as_array(self) -> [SnapshotV2VsockQueueState; VIRTIO_VSOCK_QUEUE_COUNT] {
        [self.rx, self.tx, self.event]
    }
}

redacted_debug!(
    SnapshotV2VsockActiveQueuesState,
    "SnapshotV2VsockActiveQueuesState"
);

/// Owned fields used to construct one portable exact-2.12 vsock value.
#[doc(hidden)]
pub struct SnapshotV2VsockStateParts {
    pub guest_cid: u64,
    pub backend_selector: VsockBackendSelector,
    pub host_local_port_cursor: VsockHostLocalPortCursor,
    pub active_queues: Option<SnapshotV2VsockActiveQueuesState>,
    pub virtio: SnapshotV2VirtioState,
    pub transport: SnapshotV2DeviceTransport,
}

redacted_debug!(SnapshotV2VsockStateParts, "SnapshotV2VsockStateParts");

/// Complete resource-free exact-2.12 vsock component value.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2VsockState {
    guest_cid: u64,
    backend_selector: VsockBackendSelector,
    host_local_port_cursor: VsockHostLocalPortCursor,
    active_queues: Option<SnapshotV2VsockActiveQueuesState>,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
}

impl SnapshotV2VsockState {
    /// Converts one checked MMIO source capture without retaining source
    /// ownership, reset validation, or normalized source-only work.
    pub fn try_from_mmio_capture(
        region: MmioRegion,
        interrupt_line: GuestInterruptLine,
        captured: VirtioVsockMmioCaptureState,
    ) -> Result<Self, SnapshotV2VsockStateCaptureError> {
        let (device, transport) = captured.into_parts();
        let virtio = capture_mmio_common_for_device_with_queue_count_and_config_status_gate(
            &transport,
            VIRTIO_VSOCK_DEVICE_ID,
            device.available_features(),
            VIRTIO_VSOCK_QUEUE_COUNT,
            true,
        )
        .map_err(capture_common_error)?;
        let transport = SnapshotV2DeviceTransport::Mmio(capture_mmio_transport_parts(
            region,
            interrupt_line,
            &transport,
        ));
        capture_vsock_state(device, virtio, transport)
    }

    /// Converts one checked PCI source capture without retaining source
    /// ownership, reset validation, or normalized source-only work.
    pub fn try_from_pci_capture(
        origin: StorageDeviceOrigin,
        sbdf: PciSbdf,
        bar_range: GuestMemoryRange,
        captured: VirtioVsockPciCaptureState,
    ) -> Result<Self, SnapshotV2VsockStateCaptureError> {
        let (device, transport) = captured.into_parts();
        let virtio = capture_pci_common_for_device_with_queue_count(
            &transport,
            VIRTIO_VSOCK_DEVICE_ID,
            device.available_features(),
            VIRTIO_VSOCK_QUEUE_COUNT,
        )
        .map_err(capture_common_error)?;
        let transport = capture_pci_transport_parts_with_queue_count(
            origin,
            sbdf,
            bar_range,
            &transport,
            VIRTIO_VSOCK_QUEUE_COUNT,
        )
        .map(SnapshotV2DeviceTransport::Pci)
        .map_err(capture_common_error)?;
        capture_vsock_state(device, virtio, transport)
    }

    /// Validates and retains one complete detached state value.
    pub fn try_from_parts(
        parts: SnapshotV2VsockStateParts,
    ) -> Result<Self, SnapshotV2VsockStateBuildError> {
        let state = Self {
            guest_cid: parts.guest_cid,
            backend_selector: parts.backend_selector,
            host_local_port_cursor: parts.host_local_port_cursor,
            active_queues: parts.active_queues,
            virtio: parts.virtio,
            transport: parts.transport,
        };
        validate_vsock_state(&state)?;
        Ok(state)
    }

    /// Returns the guest CID.
    pub const fn guest_cid(&self) -> u64 {
        self.guest_cid
    }

    /// Returns the inert logical backend selector.
    pub const fn backend_selector(&self) -> &VsockBackendSelector {
        &self.backend_selector
    }

    /// Returns the detached host-local allocation cursor.
    pub const fn host_local_port_cursor(&self) -> VsockHostLocalPortCursor {
        self.host_local_port_cursor
    }

    /// Returns all active queue cursors, or none for an inactive device.
    pub const fn active_queues(&self) -> Option<SnapshotV2VsockActiveQueuesState> {
        self.active_queues
    }

    /// Returns common transport-neutral virtio continuation.
    pub const fn virtio(&self) -> &SnapshotV2VirtioState {
        &self.virtio
    }

    /// Returns retained MMIO or PCI transport continuation.
    pub const fn transport(&self) -> &SnapshotV2DeviceTransport {
        &self.transport
    }

    /// Returns the exact component compatibility version.
    pub const fn compatibility_version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION
    }

    /// Returns the stable singleton artifact-resource key.
    pub const fn device_key(&self) -> SnapshotV2DeviceKey {
        SnapshotV2DeviceKey::vsock()
    }

    /// Consumes the value into detached fields.
    pub fn into_parts(self) -> SnapshotV2VsockStateParts {
        SnapshotV2VsockStateParts {
            guest_cid: self.guest_cid,
            backend_selector: self.backend_selector,
            host_local_port_cursor: self.host_local_port_cursor,
            active_queues: self.active_queues,
            virtio: self.virtio,
            transport: self.transport,
        }
    }

    /// Encodes this component in an exact outer compatibility context.
    pub fn encode(
        &self,
        outer_version: SnapshotFormatVersion,
    ) -> Result<Vec<u8>, SnapshotV2VsockStateEncodeError> {
        codec::encode(outer_version, self)
    }

    /// Decodes and fully validates one exact component payload.
    pub fn decode(
        outer_version: SnapshotFormatVersion,
        bytes: &[u8],
    ) -> Result<Self, SnapshotV2VsockStateDecodeError> {
        codec::decode(outer_version, bytes)
    }
}

redacted_debug!(SnapshotV2VsockState, "SnapshotV2VsockState");

/// Invalid relationship in a detached exact-2.12 vsock value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2VsockStateBuildError {
    /// Guest CID is outside the supported portable domain.
    GuestCid,
    /// The inert backend selector is invalid.
    BackendSelector,
    /// The detached host-local allocation cursor is invalid.
    HostLocalPortCursor,
    /// Common virtio features, status, activation, or intents are invalid.
    Virtio,
    /// Queue registers, cursors, or ring geometry are invalid.
    Queue,
    /// MMIO or PCI continuation is invalid.
    Transport,
    /// Queue rings overlap another queue or transport placement.
    Placement,
}

impl fmt::Display for SnapshotV2VsockStateBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::GuestCid => "native-v2 vsock guest CID is invalid",
            Self::BackendSelector => "native-v2 vsock backend selector is invalid",
            Self::HostLocalPortCursor => "native-v2 vsock host-local port cursor is invalid",
            Self::Virtio => "native-v2 vsock common virtio state is invalid",
            Self::Queue => "native-v2 vsock queue state is invalid",
            Self::Transport => "native-v2 vsock transport state is invalid",
            Self::Placement => "native-v2 vsock placement state is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2VsockStateBuildError {}

/// Failure while converting one checked live vsock capture into exact-2.12
/// portable state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2VsockStateCaptureError {
    /// A bounded common-state collection could not be allocated.
    Allocation,
    /// Repeated live device and transport state disagree.
    Device,
    /// Common virtio or transport capture failed.
    Common {
        /// Redacted common capture category.
        source: SnapshotV2DeviceGraphCaptureError,
    },
    /// Complete converted state failed its final semantic gate.
    Build {
        /// Redacted exact-2.12 relationship category.
        source: SnapshotV2VsockStateBuildError,
    },
}

impl fmt::Debug for SnapshotV2VsockStateCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2VsockStateCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Allocation => "native-v2 captured vsock state allocation failed",
            Self::Device => "native-v2 captured vsock device state is inconsistent",
            Self::Common { .. } => "native-v2 captured vsock transport state is invalid",
            Self::Build { .. } => "native-v2 captured vsock state is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2VsockStateCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Common { source } => Some(source),
            Self::Build { source } => Some(source),
            Self::Allocation | Self::Device => None,
        }
    }
}

/// Exact-2.12 vsock component encoding failure.
#[derive(Debug)]
pub enum SnapshotV2VsockStateEncodeError {
    /// The requested outer compatibility context is not exact 2.12.
    UnsupportedVersion,
    /// The detached state is semantically invalid.
    InvalidState {
        /// Redacted relationship category.
        source: SnapshotV2VsockStateBuildError,
    },
    /// Encoded length arithmetic overflowed.
    LengthOverflow,
    /// The component exceeds the exact supported shape.
    TooLarge,
    /// Output allocation failed.
    Allocation,
}

impl fmt::Display for SnapshotV2VsockStateEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => "native-v2 vsock state version is unsupported",
            Self::InvalidState { .. } => "native-v2 vsock state is invalid",
            Self::LengthOverflow => "native-v2 vsock state length arithmetic overflowed",
            Self::TooLarge => "native-v2 vsock state exceeds its exact size limit",
            Self::Allocation => "native-v2 vsock state output allocation failed",
        })
    }
}

impl std::error::Error for SnapshotV2VsockStateEncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidState { source } => Some(source),
            Self::UnsupportedVersion | Self::LengthOverflow | Self::TooLarge | Self::Allocation => {
                None
            }
        }
    }
}

/// Exact-2.12 vsock component decoding failure.
#[derive(Debug)]
pub enum SnapshotV2VsockStateDecodeError {
    /// The supplied compatibility context is not exact 2.12.
    UnsupportedVersion,
    /// The component exceeds the defensive or exact shape limit.
    TooLarge,
    /// Required bytes are absent.
    Truncated,
    /// Component magic is wrong.
    InvalidMagic,
    /// Header size or profile is wrong.
    InvalidProfile,
    /// Transport tag is unknown.
    InvalidTransport,
    /// Header, directory, lengths, or padding are noncanonical.
    InvalidStructure,
    /// A field has an unknown or noncanonical value.
    InvalidValue,
    /// Selector bytes are not valid UTF-8.
    InvalidUtf8,
    /// Reserved bytes or canonical padding are nonzero.
    NonzeroReserved,
    /// A bounded retained collection could not be allocated.
    Allocation,
    /// Structurally valid fields violate the complete semantic validator.
    InvalidState {
        /// Redacted relationship category.
        source: SnapshotV2VsockStateBuildError,
    },
}

impl fmt::Display for SnapshotV2VsockStateDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => "native-v2 vsock state version is unsupported",
            Self::TooLarge => "native-v2 vsock state exceeds its exact size limit",
            Self::Truncated => "native-v2 vsock state is truncated",
            Self::InvalidMagic => "native-v2 vsock state magic is invalid",
            Self::InvalidProfile => "native-v2 vsock state profile is invalid",
            Self::InvalidTransport => "native-v2 vsock state transport is invalid",
            Self::InvalidStructure => "native-v2 vsock state structure is invalid",
            Self::InvalidValue => "native-v2 vsock state field is invalid",
            Self::InvalidUtf8 => "native-v2 vsock backend selector is invalid UTF-8",
            Self::NonzeroReserved => "native-v2 vsock reserved bytes are nonzero",
            Self::Allocation => "native-v2 vsock state allocation failed",
            Self::InvalidState { .. } => "decoded native-v2 vsock state is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2VsockStateDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidState { source } => Some(source),
            Self::UnsupportedVersion
            | Self::TooLarge
            | Self::Truncated
            | Self::InvalidMagic
            | Self::InvalidProfile
            | Self::InvalidTransport
            | Self::InvalidStructure
            | Self::InvalidValue
            | Self::InvalidUtf8
            | Self::NonzeroReserved
            | Self::Allocation => None,
        }
    }
}

fn capture_vsock_state(
    device: VirtioVsockDeviceCaptureState,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
) -> Result<SnapshotV2VsockState, SnapshotV2VsockStateCaptureError> {
    let (
        guest_cid,
        available_features,
        negotiated_features,
        active_queues,
        backend_selector,
        host_local_port_cursor,
    ) = device.into_parts();
    if available_features != virtio.available_features()
        || negotiated_features != virtio.driver_features()
        || active_queues.is_some() != virtio.is_activated()
    {
        return Err(SnapshotV2VsockStateCaptureError::Device);
    }
    SnapshotV2VsockState::try_from_parts(SnapshotV2VsockStateParts {
        guest_cid,
        backend_selector,
        host_local_port_cursor,
        active_queues: active_queues.map(capture_active_queues),
        virtio,
        transport,
    })
    .map_err(|source| SnapshotV2VsockStateCaptureError::Build { source })
}

fn capture_active_queues(
    queues: VirtioVsockActiveQueuesCaptureState,
) -> SnapshotV2VsockActiveQueuesState {
    let capture = |queue: crate::vsock::VirtioVsockQueueCaptureState| {
        SnapshotV2VsockQueueState::new(
            queue.next_available(),
            queue.next_used(),
            queue.event_idx_enabled(),
        )
    };
    SnapshotV2VsockActiveQueuesState::new(
        capture(queues.rx()),
        capture(queues.tx()),
        capture(queues.event()),
    )
}

fn capture_common_error(
    source: SnapshotV2DeviceGraphCaptureError,
) -> SnapshotV2VsockStateCaptureError {
    if source == SnapshotV2DeviceGraphCaptureError::Allocation {
        SnapshotV2VsockStateCaptureError::Allocation
    } else {
        SnapshotV2VsockStateCaptureError::Common { source }
    }
}

pub(crate) fn validate_vsock_state(
    state: &SnapshotV2VsockState,
) -> Result<(), SnapshotV2VsockStateBuildError> {
    let guest_cid =
        u32::try_from(state.guest_cid).map_err(|_| SnapshotV2VsockStateBuildError::GuestCid)?;
    if guest_cid < MIN_GUEST_CID {
        return Err(SnapshotV2VsockStateBuildError::GuestCid);
    }
    let selector = state
        .backend_selector
        .path()
        .to_str()
        .ok_or(SnapshotV2VsockStateBuildError::BackendSelector)?;
    if selector.is_empty()
        || selector.len() > NATIVE_V2_VSOCK_MAX_SELECTOR_BYTES
        || selector.chars().any(char::is_control)
        || state.backend_selector.validate().is_err()
    {
        return Err(SnapshotV2VsockStateBuildError::BackendSelector);
    }
    if VsockHostLocalPortCursor::try_from_last_used(state.host_local_port_cursor.last_used())
        .is_err()
    {
        return Err(SnapshotV2VsockStateBuildError::HostLocalPortCursor);
    }

    validate_virtio(state)?;
    validate_transport(&state.transport)?;
    validate_placement(state)
}

fn validate_virtio(state: &SnapshotV2VsockState) -> Result<(), SnapshotV2VsockStateBuildError> {
    let common = &state.virtio;
    let expected_features = VirtioVsockConfigSpace::new(state.guest_cid).available_features();
    if common.available_features() != expected_features
        || common.driver_features() & !common.available_features() != 0
        || common.config_generation() != 0
        || common.queues().len() != VIRTIO_VSOCK_QUEUE_COUNT
        || common.pending_notifications().len() > VIRTIO_VSOCK_QUEUE_COUNT
        || common.interrupt_intents().len() > VIRTIO_VSOCK_QUEUE_COUNT + 1
    {
        return Err(SnapshotV2VsockStateBuildError::Virtio);
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
    if !healthy_statuses.contains(&common.status())
        || common.status() & (VIRTIO_DEVICE_STATUS_FAILED | VIRTIO_DEVICE_STATUS_DEVICE_NEEDS_RESET)
            != 0
        || common.status() & VIRTIO_DEVICE_STATUS_DRIVER == 0 && common.driver_features() != 0
        || common.status() & VIRTIO_DEVICE_STATUS_FEATURES_OK != 0
            && common.driver_features() & VIRTIO_MMIO_VERSION_1_FEATURE == 0
        || common.is_activated() != (common.status() == healthy_driver_ok)
        || common.is_activated() != state.active_queues.is_some()
    {
        return Err(SnapshotV2VsockStateBuildError::Virtio);
    }

    for queue in common.queues() {
        validate_queue(queue, common.status(), common.is_activated())?;
    }
    validate_cross_queue_ranges(common.queues())?;

    if !common.is_activated()
        && (!common.pending_notifications().is_empty()
            || common
                .interrupt_intents()
                .iter()
                .any(|intent| matches!(intent, SnapshotV2InterruptIntent::Queue { .. })))
    {
        return Err(SnapshotV2VsockStateBuildError::Virtio);
    }
    if !common
        .pending_notifications()
        .windows(2)
        .all(|window| matches!(window, [first, second] if first < second))
        || common
            .pending_notifications()
            .iter()
            .any(|index| usize::from(*index) >= VIRTIO_VSOCK_QUEUE_COUNT)
        || !common
            .interrupt_intents()
            .windows(2)
            .all(|window| matches!(window, [first, second] if first < second))
        || common.interrupt_intents().iter().any(|intent| {
            matches!(
                intent,
                SnapshotV2InterruptIntent::Queue { queue_index }
                    if usize::from(*queue_index) >= VIRTIO_VSOCK_QUEUE_COUNT
            )
        })
    {
        return Err(SnapshotV2VsockStateBuildError::Virtio);
    }

    if let Some(active) = state.active_queues {
        let expected_event_idx =
            common.driver_features() & (1_u64 << VIRTIO_RING_FEATURE_EVENT_IDX) != 0;
        for cursor in active.as_array() {
            if cursor.next_available != cursor.next_used
                || cursor.event_idx_enabled != expected_event_idx
            {
                return Err(SnapshotV2VsockStateBuildError::Queue);
            }
        }
    }
    Ok(())
}

fn validate_queue(
    queue: &SnapshotV2VirtioQueueState,
    status: u32,
    activated: bool,
) -> Result<(), SnapshotV2VsockStateBuildError> {
    if queue.max_size() != VIRTIO_VSOCK_QUEUE_SIZE
        || queue.size() > queue.max_size()
        || queue.size() != 0 && !queue.size().is_power_of_two()
        || queue.ready() && queue.size() == 0
        || activated && (!queue.ready() || queue.size() == 0)
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
        || status & VIRTIO_DEVICE_STATUS_FEATURES_OK == 0
            && (queue.size() != 0
                || queue.ready()
                || queue.descriptor_table().raw_value() != 0
                || queue.driver_ring().raw_value() != 0
                || queue.device_ring().raw_value() != 0)
    {
        return Err(SnapshotV2VsockStateBuildError::Queue);
    }
    if queue_ranges(queue)
        .map_err(|_| SnapshotV2VsockStateBuildError::Queue)?
        .is_some_and(|ranges| {
            ranges[0].overlaps(ranges[1])
                || ranges[0].overlaps(ranges[2])
                || ranges[1].overlaps(ranges[2])
        })
    {
        return Err(SnapshotV2VsockStateBuildError::Queue);
    }
    Ok(())
}

fn validate_cross_queue_ranges(
    queues: &[SnapshotV2VirtioQueueState],
) -> Result<(), SnapshotV2VsockStateBuildError> {
    for (index, queue) in queues.iter().enumerate() {
        let Some(current) =
            queue_ranges(queue).map_err(|_| SnapshotV2VsockStateBuildError::Queue)?
        else {
            continue;
        };
        for previous in queues
            .get(..index)
            .ok_or(SnapshotV2VsockStateBuildError::Queue)?
        {
            let Some(previous) =
                queue_ranges(previous).map_err(|_| SnapshotV2VsockStateBuildError::Queue)?
            else {
                continue;
            };
            if current
                .iter()
                .any(|current| previous.iter().any(|previous| current.overlaps(*previous)))
            {
                return Err(SnapshotV2VsockStateBuildError::Queue);
            }
        }
    }
    Ok(())
}

fn validate_transport(
    transport: &SnapshotV2DeviceTransport,
) -> Result<(), SnapshotV2VsockStateBuildError> {
    match transport {
        SnapshotV2DeviceTransport::Mmio(mmio) => validate_mmio(mmio),
        SnapshotV2DeviceTransport::Pci(pci) => validate_pci(pci),
    }
}

fn validate_mmio(state: &SnapshotV2MmioDeviceState) -> Result<(), SnapshotV2VsockStateBuildError> {
    if state.device_feature_select() > 1
        || state.driver_feature_select() > 1
        || usize::try_from(state.queue_select())
            .ok()
            .is_none_or(|queue| queue >= VIRTIO_VSOCK_QUEUE_COUNT)
        || state.region().id().raw_value() == 0
        || state.region().range().size() != VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        || state
            .region()
            .range()
            .validate_alignment(VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
            .is_err()
        || state.interrupt_line().raw_value() < 32
    {
        Err(SnapshotV2VsockStateBuildError::Transport)
    } else {
        Ok(())
    }
}

fn validate_pci(state: &SnapshotV2PciDeviceState) -> Result<(), SnapshotV2VsockStateBuildError> {
    const WRITABLE_OFFSETS: [u16; 4] = [0x04, 0x05, 0x0c, 0x3c];
    let aperture_end = PCI_BAR64_START
        .checked_add(PCI_BAR64_SIZE)
        .ok_or(SnapshotV2VsockStateBuildError::Transport)?;
    if state.phase() != VirtioPciEndpointPhase::Active
        || state.origin() != StorageDeviceOrigin::Startup
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
        || usize::from(state.queue_select()) >= VIRTIO_VSOCK_QUEUE_COUNT
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
        return Err(SnapshotV2VsockStateBuildError::Transport);
    }
    validate_msix(state.msix())
}

fn validate_msix(state: &SnapshotV2PciMsixState) -> Result<(), SnapshotV2VsockStateBuildError> {
    let entry_count = VIRTIO_VSOCK_QUEUE_COUNT + 1;
    let pending_mask = (1_u64 << entry_count) - 1;
    if state.entries().len() != entry_count
        || entry_count > VIRTIO_PCI_MAX_MSIX_VECTORS
        || state.pending_words().len() != 1
        || state.queue_vectors().len() != VIRTIO_VSOCK_QUEUE_COUNT
        || state
            .pending_words()
            .first()
            .copied()
            .is_none_or(|pending| pending & !pending_mask != 0)
        || !valid_msix_vector(state.config_vector(), entry_count)
        || state
            .queue_vectors()
            .iter()
            .copied()
            .any(|vector| !valid_msix_vector(vector, entry_count))
        || state
            .entries()
            .iter()
            .any(|entry| entry.vector_control() & !1 != 0)
    {
        Err(SnapshotV2VsockStateBuildError::Transport)
    } else {
        Ok(())
    }
}

fn valid_msix_vector(vector: u16, count: usize) -> bool {
    vector == VIRTIO_PCI_NO_VECTOR || usize::from(vector) < count
}

fn validate_placement(state: &SnapshotV2VsockState) -> Result<(), SnapshotV2VsockStateBuildError> {
    let placement: GuestMemoryRange = match &state.transport {
        SnapshotV2DeviceTransport::Mmio(mmio) => mmio.region().range(),
        SnapshotV2DeviceTransport::Pci(pci) => pci.bar_range(),
    };
    for queue in state.virtio.queues() {
        if queue_ranges(queue)
            .map_err(|_| SnapshotV2VsockStateBuildError::Queue)?
            .is_some_and(|ranges| ranges.into_iter().any(|range| range.overlaps(placement)))
        {
            return Err(SnapshotV2VsockStateBuildError::Placement);
        }
    }
    Ok(())
}
