//! Host-operation-free exact-2.11 network destination preparation.

use std::fmt;
use std::time::{Duration, Instant};

use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestMemory, GuestMemoryRange};
use crate::message_interrupt::GuestMessageInterruptRegistry;
use crate::metrics::SharedNetworkInterfaceMetrics;
use crate::mmds::{MmdsConfig, MmdsConfigInput};
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::network::{
    NetworkDeviceProfile, NetworkInterfaceConfig, NetworkMmioDeviceRegistration,
    NetworkRateLimiterConfig, NetworkTokenBucketConfig, VIRTIO_NET_DEVICE_ID,
    VIRTIO_NET_QUEUE_SIZES, VirtioNetworkConfigSpace, VirtioNetworkDevice,
    VirtioNetworkMmioHandler, VirtioNetworkRateLimiter, VirtioNetworkRateLimiterCaptureState,
    VirtioNetworkRxQueue, VirtioNetworkTokenBucketCaptureState, VirtioNetworkTxQueue,
    validate_network_queue_pair_ranges,
};
use crate::snapshot::SnapshotNetworkOverride;
use crate::snapshot_device_v2::{
    SnapshotV2DeviceKey, SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind,
    range_is_wholly_contained, restore_mmio_transport_state_for_device_with_config_status_gate,
};
use crate::snapshot_device_v2_5::queue_ranges;
use crate::snapshot_network_v2_11::{
    NATIVE_V2_NETWORK_MAX_CAPTURED_SELECTOR_BYTES, NATIVE_V2_NETWORK_MAX_INTERFACE_ID_BYTES,
    NATIVE_V2_NETWORK_MAX_INTERFACES, SnapshotV2MmdsInterfaceState, SnapshotV2MmdsState,
    SnapshotV2NetworkInterfaceState, SnapshotV2NetworkInterfaceStateParts,
    SnapshotV2NetworkLimiterState, SnapshotV2NetworkRetryState, SnapshotV2NetworkState,
    SnapshotV2NetworkTokenBucketState,
};
use crate::snapshot_restore::{
    SnapshotRestorePublicId, SnapshotRestorePublicIdError, SnapshotRestoreResourceClass,
    SnapshotRestoreResourceKey,
};
use crate::storage_capture::StorageDeviceOrigin;
use crate::virtio::{VirtioDeviceType, VirtioInterruptIntent};
use crate::virtio_mmio::VirtioMmioQueueState;
use crate::virtio_pci::{
    PreparedVirtioPciEndpoint, VirtioPciEndpointError, VirtioPciIdentity, VirtioPciTransportState,
};

const REDACTED: &str = "<redacted>";

struct RestoredSnapshotV2NetworkDevice {
    queue_ranges: [Option<[GuestMemoryRange; 3]>; 2],
    retry_deadline: Option<Instant>,
    config_space: VirtioNetworkConfigSpace,
    device: VirtioNetworkDevice,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RestoreSnapshotV2NetworkDeviceError {
    Profile,
    Queue,
    QueueMemory,
    Limiter,
    Retry,
    Device,
}

/// Stable cancellation checkpoints before an owner-free topology is published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2NetworkRestorePreparationStage {
    /// Before retaining any caller value.
    Start,
    /// Before validating and retaining one caller override.
    Override,
    /// Before projecting one destination controller entry.
    Controller,
    /// Before projecting global MMDS configuration.
    Mmds,
    /// After complete validation and before returning the immutable result.
    Completion,
}

/// One exact destination entry paired with its portable continuation.
#[derive(Clone, PartialEq, Eq)]
pub struct PreparedSnapshotV2NetworkRestoreInterface {
    source_index: u16,
    resource_key: SnapshotRestoreResourceKey,
    controller: NetworkInterfaceConfig,
    portable: SnapshotV2NetworkInterfaceState,
    mmds_stack: Option<SnapshotV2MmdsInterfaceState>,
}

impl PreparedSnapshotV2NetworkRestoreInterface {
    /// Returns the saved configuration-order index.
    pub const fn source_index(&self) -> u16 {
        self.source_index
    }

    /// Returns the exact packet-I/O resource identity.
    pub const fn resource_key(&self) -> &SnapshotRestoreResourceKey {
        &self.resource_key
    }

    /// Returns destination controller configuration with the explicit selector.
    pub const fn controller(&self) -> &NetworkInterfaceConfig {
        &self.controller
    }

    /// Returns the unchanged portable device continuation.
    pub const fn portable(&self) -> &SnapshotV2NetworkInterfaceState {
        &self.portable
    }

    /// Returns the selected fresh-MMDS stack seed, when configured.
    pub const fn mmds_stack(&self) -> Option<SnapshotV2MmdsInterfaceState> {
        self.mmds_stack
    }

    /// Consumes one checked interface into a complete, still-unpublished MMIO
    /// register handler against destination memory and fresh metrics.
    #[doc(hidden)]
    pub fn into_mmio_handler(
        self,
        destination_memory: &GuestMemory,
        realized_profile: NetworkDeviceProfile,
        interface_metrics: SharedNetworkInterfaceMetrics,
        aggregate_metrics: SharedNetworkInterfaceMetrics,
        now: Instant,
    ) -> Result<PreparedSnapshotV2NetworkMmioHandler, SnapshotV2NetworkMmioHandlerError> {
        let Self {
            source_index,
            resource_key,
            controller,
            portable,
            mmds_stack,
        } = self;
        validate_snapshot_v2_network_destination_profile(&controller, &portable, realized_profile)
            .map_err(SnapshotV2NetworkMmioHandlerError::from_device)?;
        if !matches!(portable.transport(), SnapshotV2DeviceTransport::Mmio(_)) {
            return Err(SnapshotV2NetworkMmioHandlerError::WrongTransport);
        }
        let RestoredSnapshotV2NetworkDevice {
            queue_ranges,
            retry_deadline,
            config_space,
            device,
        } = restore_snapshot_v2_network_device(
            &controller,
            &portable,
            destination_memory,
            realized_profile,
            interface_metrics,
            aggregate_metrics,
            now,
        )
        .map_err(SnapshotV2NetworkMmioHandlerError::from_device)?;
        let activation_is_active = device.is_activated();

        let SnapshotV2NetworkInterfaceStateParts {
            iface_id,
            captured_selector: _,
            requested_guest_mac,
            requested_mtu,
            profile,
            backend,
            local,
            virtio,
            rx_limiter,
            tx_limiter,
            transport,
        } = portable.into_parts();
        let SnapshotV2DeviceTransport::Mmio(mmio) = &transport else {
            return Err(SnapshotV2NetworkMmioHandlerError::WrongTransport);
        };
        let region = mmio.region();
        let interrupt_line = mmio.interrupt_line();
        let retained = restore_mmio_transport_state_for_device_with_config_status_gate(
            VIRTIO_NET_DEVICE_ID,
            &virtio,
            mmio,
            true,
        )
        .map_err(|_| SnapshotV2NetworkMmioHandlerError::RetainedTransport)?;
        let registers = *retained.device_registers();
        let mut handler =
            VirtioNetworkMmioHandler::with_vendor_id_and_config_generation_and_device_config_and_activation(
                registers.device_id(),
                registers.vendor_id(),
                registers.device_features(),
                registers.config_generation(),
                &VIRTIO_NET_QUEUE_SIZES,
                config_space,
                device,
            )
            .map_err(|_| SnapshotV2NetworkMmioHandlerError::Handler)?;
        handler
            .restore_transport_state(&retained, activation_is_active)
            .map_err(|_| SnapshotV2NetworkMmioHandlerError::Transport)?;
        let mut interrupt_intents = Vec::new();
        interrupt_intents
            .try_reserve_exact(virtio.interrupt_intents().len())
            .map_err(|_| SnapshotV2NetworkMmioHandlerError::Allocation)?;
        interrupt_intents.extend(
            virtio
                .interrupt_intents()
                .iter()
                .map(|intent| match intent {
                    crate::snapshot_device_v2::SnapshotV2InterruptIntent::Queue { queue_index } => {
                        VirtioInterruptIntent::Queue {
                            queue_index: *queue_index,
                        }
                    }
                    crate::snapshot_device_v2::SnapshotV2InterruptIntent::Configuration => {
                        VirtioInterruptIntent::Configuration
                    }
                }),
        );
        handler
            .restore_network_interrupt_intents(&interrupt_intents)
            .map_err(|_| SnapshotV2NetworkMmioHandlerError::Allocation)?;

        let mut captured_selector = String::new();
        captured_selector
            .try_reserve_exact(controller.host_dev_name().len())
            .map_err(|_| SnapshotV2NetworkMmioHandlerError::Allocation)?;
        captured_selector.push_str(controller.host_dev_name());
        let expected_state =
            SnapshotV2NetworkInterfaceState::try_from_parts(SnapshotV2NetworkInterfaceStateParts {
                iface_id,
                captured_selector,
                requested_guest_mac,
                requested_mtu,
                profile,
                backend,
                local,
                virtio,
                rx_limiter,
                tx_limiter,
                transport,
            })
            .map_err(|_| SnapshotV2NetworkMmioHandlerError::ExpectedState)?;
        let (captured, validation) = handler
            .capture_network_state_at(&controller, realized_profile, destination_memory, None, now)
            .map_err(|_| SnapshotV2NetworkMmioHandlerError::Capture)?;
        if validation.source_rx_retry().is_some() {
            return Err(SnapshotV2NetworkMmioHandlerError::Capture);
        }
        let normalized = SnapshotV2NetworkInterfaceState::try_from_mmio_capture(
            &controller,
            backend,
            region,
            interrupt_line,
            &captured,
        )
        .map_err(|_| SnapshotV2NetworkMmioHandlerError::Normalize)?;
        if normalized != expected_state {
            return Err(SnapshotV2NetworkMmioHandlerError::StateMismatch);
        }
        let registration = NetworkMmioDeviceRegistration::from_restored(
            usize::from(source_index),
            &controller,
            region,
        )
        .map_err(|_| SnapshotV2NetworkMmioHandlerError::Allocation)?;

        Ok(PreparedSnapshotV2NetworkMmioHandler {
            source_index,
            resource_key,
            controller,
            expected_state,
            mmds_stack,
            queue_ranges,
            retry: normalized.local().tx_retry(),
            retry_deadline,
            region,
            interrupt_line,
            registration,
            handler,
        })
    }

    /// Consumes one checked interface into a complete retained PCI endpoint
    /// against destination memory and fresh metrics.
    ///
    /// The caller supplies the dispatcher region fixed by the platform plan and
    /// a fresh destination message registry. The result still owns no message
    /// resources, BAR/function/dispatcher lease, packet-I/O provider, callback,
    /// scheduler, session, or VM authority.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn into_pci_endpoint(
        self,
        destination_memory: &GuestMemory,
        realized_profile: NetworkDeviceProfile,
        interface_metrics: SharedNetworkInterfaceMetrics,
        aggregate_metrics: SharedNetworkInterfaceMetrics,
        region_id: MmioRegionId,
        messages: GuestMessageInterruptRegistry,
        now: Instant,
    ) -> Result<PreparedSnapshotV2NetworkPciEndpoint, SnapshotV2NetworkPciEndpointError> {
        let restored = restore_snapshot_v2_network_device(
            &self.controller,
            &self.portable,
            destination_memory,
            realized_profile,
            interface_metrics,
            aggregate_metrics,
            now,
        )
        .map_err(SnapshotV2NetworkPciEndpointError::from_device)?;
        let Self {
            source_index,
            resource_key,
            controller,
            portable,
            mmds_stack,
        } = self;
        let SnapshotV2NetworkInterfaceStateParts {
            iface_id,
            captured_selector: _,
            requested_guest_mac,
            requested_mtu,
            profile,
            backend,
            local,
            virtio,
            rx_limiter,
            tx_limiter,
            transport,
        } = portable.into_parts();
        let SnapshotV2DeviceTransport::Pci(pci) = &transport else {
            return Err(SnapshotV2NetworkPciEndpointError::WrongTransport);
        };
        let origin = pci.origin();
        let sbdf = pci.sbdf();
        let bar_range = pci.bar_range();
        let mut captured_selector = String::new();
        captured_selector
            .try_reserve_exact(controller.host_dev_name().len())
            .map_err(|_| SnapshotV2NetworkPciEndpointError::Allocation)?;
        captured_selector.push_str(controller.host_dev_name());
        let expected_state =
            SnapshotV2NetworkInterfaceState::try_from_parts(SnapshotV2NetworkInterfaceStateParts {
                iface_id,
                captured_selector,
                requested_guest_mac,
                requested_mtu,
                profile,
                backend,
                local,
                virtio,
                rx_limiter,
                tx_limiter,
                transport,
            })
            .map_err(|_| SnapshotV2NetworkPciEndpointError::ExpectedState)?;
        let device_type = VirtioDeviceType::new(VIRTIO_NET_DEVICE_ID)
            .map_err(|_| SnapshotV2NetworkPciEndpointError::DeviceType)?;
        let identity =
            VirtioPciIdentity::new(device_type, expected_state.virtio().available_features())
                .with_config_generation(expected_state.virtio().config_generation());
        let SnapshotV2DeviceTransport::Pci(pci) = expected_state.transport() else {
            return Err(SnapshotV2NetworkPciEndpointError::WrongTransport);
        };
        let retained = VirtioPciTransportState::from_snapshot_v2_parts(
            identity,
            expected_state.virtio(),
            pci,
            false,
        )
        .map_err(|_| SnapshotV2NetworkPciEndpointError::RetainedTransport)?;
        let activation_is_active = restored.device.is_activated();
        let endpoint = PreparedVirtioPciEndpoint::new(
            identity,
            &VIRTIO_NET_QUEUE_SIZES,
            restored.config_space,
            restored.device,
            activation_is_active,
            false,
            &retained,
            sbdf,
            bar_range,
            region_id,
            messages,
        )
        .map_err(SnapshotV2NetworkPciEndpointError::Endpoint)?;
        let (captured, validation) = endpoint
            .endpoint()
            .capture_network_state_at(&controller, realized_profile, destination_memory, None, now)
            .map_err(|_| SnapshotV2NetworkPciEndpointError::Capture)?;
        if validation.source_rx_retry().is_some() {
            return Err(SnapshotV2NetworkPciEndpointError::Capture);
        }
        let normalized = SnapshotV2NetworkInterfaceState::try_from_pci_capture(
            &controller,
            backend,
            origin,
            sbdf,
            bar_range,
            &captured,
        )
        .map_err(|_| SnapshotV2NetworkPciEndpointError::Normalize)?;
        if normalized != expected_state {
            return Err(SnapshotV2NetworkPciEndpointError::StateMismatch);
        }

        Ok(PreparedSnapshotV2NetworkPciEndpoint {
            source_index,
            resource_key,
            controller,
            expected_state,
            mmds_stack,
            queue_ranges: restored.queue_ranges,
            retry: normalized.local().tx_retry(),
            retry_deadline: restored.retry_deadline,
            origin,
            endpoint,
        })
    }
}

impl fmt::Debug for PreparedSnapshotV2NetworkRestoreInterface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2NetworkRestoreInterface")
            .field("source_index", &self.source_index)
            .field("state", &REDACTED)
            .finish()
    }
}

/// One checked, complete, and still-unpublished MMIO network handler.
///
/// Packet I/O, readiness callbacks, dispatcher leases, interrupt routes,
/// scheduler publication, MMDS data, and VM authority remain outside this
/// value.
#[doc(hidden)]
pub struct PreparedSnapshotV2NetworkMmioHandler {
    source_index: u16,
    resource_key: SnapshotRestoreResourceKey,
    controller: NetworkInterfaceConfig,
    expected_state: SnapshotV2NetworkInterfaceState,
    mmds_stack: Option<SnapshotV2MmdsInterfaceState>,
    queue_ranges: [Option<[GuestMemoryRange; 3]>; 2],
    retry: SnapshotV2NetworkRetryState,
    retry_deadline: Option<Instant>,
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    registration: NetworkMmioDeviceRegistration,
    handler: VirtioNetworkMmioHandler,
}

impl PreparedSnapshotV2NetworkMmioHandler {
    pub const fn source_index(&self) -> u16 {
        self.source_index
    }

    pub const fn resource_key(&self) -> &SnapshotRestoreResourceKey {
        &self.resource_key
    }

    pub const fn controller(&self) -> &NetworkInterfaceConfig {
        &self.controller
    }

    pub const fn expected_state(&self) -> &SnapshotV2NetworkInterfaceState {
        &self.expected_state
    }

    pub const fn mmds_stack(&self) -> Option<SnapshotV2MmdsInterfaceState> {
        self.mmds_stack
    }

    pub const fn queue_ranges(&self) -> &[Option<[GuestMemoryRange; 3]>; 2] {
        &self.queue_ranges
    }

    pub const fn retry(&self) -> SnapshotV2NetworkRetryState {
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

    pub const fn registration(&self) -> &NetworkMmioDeviceRegistration {
        &self.registration
    }

    pub const fn handler(&self) -> &VirtioNetworkMmioHandler {
        &self.handler
    }

    pub fn into_parts(self) -> PreparedSnapshotV2NetworkMmioHandlerParts {
        (
            self.source_index,
            self.resource_key,
            self.controller,
            self.expected_state,
            self.mmds_stack,
            self.queue_ranges,
            self.retry,
            self.retry_deadline,
            self.region,
            self.interrupt_line,
            self.registration,
            self.handler,
        )
    }
}

/// Owned parts of one unpublished exact-2.11 MMIO network handler.
#[doc(hidden)]
pub type PreparedSnapshotV2NetworkMmioHandlerParts = (
    u16,
    SnapshotRestoreResourceKey,
    NetworkInterfaceConfig,
    SnapshotV2NetworkInterfaceState,
    Option<SnapshotV2MmdsInterfaceState>,
    [Option<[GuestMemoryRange; 3]>; 2],
    SnapshotV2NetworkRetryState,
    Option<Instant>,
    MmioRegion,
    GuestInterruptLine,
    NetworkMmioDeviceRegistration,
    VirtioNetworkMmioHandler,
);

impl fmt::Debug for PreparedSnapshotV2NetworkMmioHandler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2NetworkMmioHandler")
            .field("source_index", &self.source_index)
            .field("state", &REDACTED)
            .finish()
    }
}

/// Redacted failure while materializing one checked network MMIO handler.
#[derive(Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum SnapshotV2NetworkMmioHandlerError {
    Profile,
    WrongTransport,
    Queue,
    QueueMemory,
    Limiter,
    Retry,
    Device,
    RetainedTransport,
    Handler,
    Transport,
    ExpectedState,
    Capture,
    Normalize,
    StateMismatch,
    Allocation,
}

impl SnapshotV2NetworkMmioHandlerError {
    const fn from_device(source: RestoreSnapshotV2NetworkDeviceError) -> Self {
        match source {
            RestoreSnapshotV2NetworkDeviceError::Profile => Self::Profile,
            RestoreSnapshotV2NetworkDeviceError::Queue => Self::Queue,
            RestoreSnapshotV2NetworkDeviceError::QueueMemory => Self::QueueMemory,
            RestoreSnapshotV2NetworkDeviceError::Limiter => Self::Limiter,
            RestoreSnapshotV2NetworkDeviceError::Retry => Self::Retry,
            RestoreSnapshotV2NetworkDeviceError::Device => Self::Device,
        }
    }
}

impl fmt::Debug for SnapshotV2NetworkMmioHandlerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2NetworkMmioHandlerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Profile => "native-v2 network destination profile is invalid",
            Self::WrongTransport => "native-v2 network restore interface is not MMIO",
            Self::Queue => "native-v2 network queue reconstruction failed",
            Self::QueueMemory => "native-v2 network queue memory is invalid",
            Self::Limiter => "native-v2 network limiter reconstruction failed",
            Self::Retry => "native-v2 network retry reconstruction failed",
            Self::Device => "native-v2 network device reconstruction failed",
            Self::RetainedTransport => "native-v2 network retained MMIO state is invalid",
            Self::Handler => "native-v2 network MMIO handler construction failed",
            Self::Transport => "native-v2 network MMIO handler state is invalid",
            Self::ExpectedState => "native-v2 network normalized state is invalid",
            Self::Capture => "native-v2 network immediate capture failed",
            Self::Normalize => "native-v2 network immediate normalization failed",
            Self::StateMismatch => "native-v2 network immediate state does not match",
            Self::Allocation => "native-v2 network MMIO preparation allocation failed",
        })
    }
}

impl std::error::Error for SnapshotV2NetworkMmioHandlerError {}

/// One checked exact-2.11 network endpoint awaiting destination PCI
/// publication.
#[doc(hidden)]
pub struct PreparedSnapshotV2NetworkPciEndpoint {
    source_index: u16,
    resource_key: SnapshotRestoreResourceKey,
    controller: NetworkInterfaceConfig,
    expected_state: SnapshotV2NetworkInterfaceState,
    mmds_stack: Option<SnapshotV2MmdsInterfaceState>,
    queue_ranges: [Option<[GuestMemoryRange; 3]>; 2],
    retry: SnapshotV2NetworkRetryState,
    retry_deadline: Option<Instant>,
    origin: StorageDeviceOrigin,
    endpoint: PreparedVirtioPciEndpoint<VirtioNetworkConfigSpace, VirtioNetworkDevice>,
}

/// Consumed checked network continuation and retained PCI endpoint.
#[doc(hidden)]
pub type PreparedSnapshotV2NetworkPciEndpointParts = (
    u16,
    SnapshotRestoreResourceKey,
    NetworkInterfaceConfig,
    SnapshotV2NetworkInterfaceState,
    Option<SnapshotV2MmdsInterfaceState>,
    [Option<[GuestMemoryRange; 3]>; 2],
    SnapshotV2NetworkRetryState,
    Option<Instant>,
    StorageDeviceOrigin,
    PreparedVirtioPciEndpoint<VirtioNetworkConfigSpace, VirtioNetworkDevice>,
);

impl PreparedSnapshotV2NetworkPciEndpoint {
    pub const fn source_index(&self) -> u16 {
        self.source_index
    }

    pub const fn resource_key(&self) -> &SnapshotRestoreResourceKey {
        &self.resource_key
    }

    pub const fn controller(&self) -> &NetworkInterfaceConfig {
        &self.controller
    }

    pub const fn expected_state(&self) -> &SnapshotV2NetworkInterfaceState {
        &self.expected_state
    }

    pub const fn mmds_stack(&self) -> Option<SnapshotV2MmdsInterfaceState> {
        self.mmds_stack
    }

    pub const fn queue_ranges(&self) -> &[Option<[GuestMemoryRange; 3]>; 2] {
        &self.queue_ranges
    }

    pub const fn retry(&self) -> SnapshotV2NetworkRetryState {
        self.retry
    }

    pub const fn retry_deadline(&self) -> Option<Instant> {
        self.retry_deadline
    }

    pub const fn origin(&self) -> StorageDeviceOrigin {
        self.origin
    }

    pub const fn endpoint(
        &self,
    ) -> &PreparedVirtioPciEndpoint<VirtioNetworkConfigSpace, VirtioNetworkDevice> {
        &self.endpoint
    }

    pub fn into_parts(self) -> PreparedSnapshotV2NetworkPciEndpointParts {
        (
            self.source_index,
            self.resource_key,
            self.controller,
            self.expected_state,
            self.mmds_stack,
            self.queue_ranges,
            self.retry,
            self.retry_deadline,
            self.origin,
            self.endpoint,
        )
    }
}

impl fmt::Debug for PreparedSnapshotV2NetworkPciEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2NetworkPciEndpoint")
            .field("source_index", &self.source_index)
            .field("state", &REDACTED)
            .finish()
    }
}

/// Redacted failure while materializing one checked network PCI endpoint.
#[doc(hidden)]
pub enum SnapshotV2NetworkPciEndpointError {
    Profile,
    WrongTransport,
    Queue,
    QueueMemory,
    Limiter,
    Retry,
    Device,
    DeviceType,
    RetainedTransport,
    Endpoint(VirtioPciEndpointError),
    ExpectedState,
    Capture,
    Normalize,
    StateMismatch,
    Allocation,
}

impl SnapshotV2NetworkPciEndpointError {
    const fn from_device(source: RestoreSnapshotV2NetworkDeviceError) -> Self {
        match source {
            RestoreSnapshotV2NetworkDeviceError::Profile => Self::Profile,
            RestoreSnapshotV2NetworkDeviceError::Queue => Self::Queue,
            RestoreSnapshotV2NetworkDeviceError::QueueMemory => Self::QueueMemory,
            RestoreSnapshotV2NetworkDeviceError::Limiter => Self::Limiter,
            RestoreSnapshotV2NetworkDeviceError::Retry => Self::Retry,
            RestoreSnapshotV2NetworkDeviceError::Device => Self::Device,
        }
    }
}

impl fmt::Debug for SnapshotV2NetworkPciEndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2NetworkPciEndpointError")
            .field(
                "kind",
                &match self {
                    Self::Profile => "Profile",
                    Self::WrongTransport => "WrongTransport",
                    Self::Queue => "Queue",
                    Self::QueueMemory => "QueueMemory",
                    Self::Limiter => "Limiter",
                    Self::Retry => "Retry",
                    Self::Device => "Device",
                    Self::DeviceType => "DeviceType",
                    Self::RetainedTransport => "RetainedTransport",
                    Self::Endpoint(_) => "Endpoint",
                    Self::ExpectedState => "ExpectedState",
                    Self::Capture => "Capture",
                    Self::Normalize => "Normalize",
                    Self::StateMismatch => "StateMismatch",
                    Self::Allocation => "Allocation",
                },
            )
            .field("state", &REDACTED)
            .finish()
    }
}

impl fmt::Display for SnapshotV2NetworkPciEndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Profile => "native-v2 network destination profile is invalid",
            Self::WrongTransport => "native-v2 network restore interface is not PCI",
            Self::Queue => "native-v2 network queue reconstruction failed",
            Self::QueueMemory => "native-v2 network queue memory is invalid",
            Self::Limiter => "native-v2 network limiter reconstruction failed",
            Self::Retry => "native-v2 network retry reconstruction failed",
            Self::Device => "native-v2 network device reconstruction failed",
            Self::DeviceType => "native-v2 network PCI device type is invalid",
            Self::RetainedTransport => "native-v2 network retained PCI state is invalid",
            Self::Endpoint(_) => "native-v2 network PCI endpoint construction failed",
            Self::ExpectedState => "native-v2 network normalized state is invalid",
            Self::Capture => "native-v2 network immediate PCI capture failed",
            Self::Normalize => "native-v2 network immediate PCI normalization failed",
            Self::StateMismatch => "native-v2 network immediate PCI state does not match",
            Self::Allocation => "native-v2 network PCI preparation allocation failed",
        })
    }
}

impl std::error::Error for SnapshotV2NetworkPciEndpointError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Endpoint(source) => Some(source),
            Self::Profile
            | Self::WrongTransport
            | Self::Queue
            | Self::QueueMemory
            | Self::Limiter
            | Self::Retry
            | Self::Device
            | Self::DeviceType
            | Self::RetainedTransport
            | Self::ExpectedState
            | Self::Capture
            | Self::Normalize
            | Self::StateMismatch
            | Self::Allocation => None,
        }
    }
}

/// Immutable exact-2.11 network/controller/MMDS destination topology.
///
/// The value owns no descriptor, provider, packet owner, callback, metric,
/// datastore, token, session, device, platform slot, or VM authority.
#[derive(Clone, PartialEq, Eq)]
pub struct PreparedSnapshotV2NetworkRestoreTopology {
    transport_kind: SnapshotV2DeviceTransportKind,
    interfaces: Vec<PreparedSnapshotV2NetworkRestoreInterface>,
    mmds_state: Option<SnapshotV2MmdsState>,
    mmds_controller: Option<MmdsConfig>,
}

impl PreparedSnapshotV2NetworkRestoreTopology {
    /// Constructs the internal network-free form for one retained exact-2.11
    /// destination. Portable kind-12 state itself remains non-empty.
    #[doc(hidden)]
    pub fn empty(transport_kind: SnapshotV2DeviceTransportKind) -> Self {
        Self {
            transport_kind,
            interfaces: Vec::new(),
            mmds_state: None,
            mmds_controller: None,
        }
    }

    /// Resolves one complete explicit override vector.
    pub fn prepare(
        state: SnapshotV2NetworkState,
        overrides: &[SnapshotNetworkOverride],
    ) -> Result<Self, SnapshotV2NetworkRestorePreparationError> {
        prepare_network_restore_topology(state, overrides, |_| false, AllocationPolicy::System)
    }

    /// Resolves with stable cancellation checkpoints.
    pub fn prepare_with_cancel<C>(
        state: SnapshotV2NetworkState,
        overrides: &[SnapshotNetworkOverride],
        is_cancelled: C,
    ) -> Result<Self, SnapshotV2NetworkRestorePreparationError>
    where
        C: FnMut(SnapshotV2NetworkRestorePreparationStage) -> bool,
    {
        prepare_network_restore_topology(state, overrides, is_cancelled, AllocationPolicy::System)
    }

    /// Returns interfaces in immutable saved configuration order.
    pub fn interfaces(&self) -> &[PreparedSnapshotV2NetworkRestoreInterface] {
        &self.interfaces
    }

    /// Returns the exact aggregate device transport, including for the
    /// network-free internal form.
    pub const fn transport_kind(&self) -> SnapshotV2DeviceTransportKind {
        self.transport_kind
    }

    /// Returns the unchanged portable MMDS continuation.
    pub const fn mmds_state(&self) -> Option<&SnapshotV2MmdsState> {
        self.mmds_state.as_ref()
    }

    /// Returns destination MMDS controller configuration without live state.
    pub const fn mmds_controller(&self) -> Option<&MmdsConfig> {
        self.mmds_controller.as_ref()
    }

    /// Consumes the topology into still owner-free prepared parts.
    pub fn into_parts(self) -> PreparedSnapshotV2NetworkRestoreTopologyParts {
        (self.interfaces, self.mmds_state, self.mmds_controller)
    }
}

/// Owned parts of one prepared exact-2.11 network destination topology.
pub type PreparedSnapshotV2NetworkRestoreTopologyParts = (
    Vec<PreparedSnapshotV2NetworkRestoreInterface>,
    Option<SnapshotV2MmdsState>,
    Option<MmdsConfig>,
);

impl fmt::Debug for PreparedSnapshotV2NetworkRestoreTopology {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2NetworkRestoreTopology")
            .field("interface_count", &self.interfaces.len())
            .field("mmds", &self.mmds_state.as_ref().map(|_| "<configured>"))
            .field("state", &REDACTED)
            .finish()
    }
}

/// Pure exact-2.11 destination-resolution failure.
pub enum SnapshotV2NetworkRestorePreparationError {
    /// More caller entries were supplied than the network ceiling.
    TooManyOverrides,
    /// A caller interface identifier is malformed or overlong.
    InvalidInterfaceId,
    /// A caller destination selector is empty, overlong, or contains controls.
    InvalidDestinationSelector,
    /// A caller interface does not exist in the saved network vector.
    UnknownInterface,
    /// More than one caller entry targets the same saved interface.
    DuplicateInterface,
    /// At least one saved interface has no explicit destination.
    MissingInterface,
    /// A destination controller projection contradicted validated portable state.
    Controller,
    /// A destination MMDS controller projection contradicted validated state.
    Mmds,
    /// A stable network resource public ID could not be retained.
    ResourceId {
        /// Public-ID validation or allocation failure.
        source: SnapshotRestorePublicIdError,
    },
    /// Bounded topology storage could not be allocated.
    Allocation,
    /// Preparation was cancelled at a stable owner-free stage.
    Cancelled {
        /// The checkpoint that observed cancellation.
        stage: SnapshotV2NetworkRestorePreparationStage,
    },
}

impl SnapshotV2NetworkRestorePreparationError {
    /// Returns whether preparation stopped at an explicit cancellation checkpoint.
    pub const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }
}

impl fmt::Debug for SnapshotV2NetworkRestorePreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooManyOverrides => "SnapshotV2NetworkRestorePreparationError::TooManyOverrides",
            Self::InvalidInterfaceId => {
                "SnapshotV2NetworkRestorePreparationError::InvalidInterfaceId"
            }
            Self::InvalidDestinationSelector => {
                "SnapshotV2NetworkRestorePreparationError::InvalidDestinationSelector"
            }
            Self::UnknownInterface => "SnapshotV2NetworkRestorePreparationError::UnknownInterface",
            Self::DuplicateInterface => {
                "SnapshotV2NetworkRestorePreparationError::DuplicateInterface"
            }
            Self::MissingInterface => "SnapshotV2NetworkRestorePreparationError::MissingInterface",
            Self::Controller => "SnapshotV2NetworkRestorePreparationError::Controller",
            Self::Mmds => "SnapshotV2NetworkRestorePreparationError::Mmds",
            Self::ResourceId { .. } => "SnapshotV2NetworkRestorePreparationError::ResourceId",
            Self::Allocation => "SnapshotV2NetworkRestorePreparationError::Allocation",
            Self::Cancelled { .. } => "SnapshotV2NetworkRestorePreparationError::Cancelled",
        })
    }
}

impl fmt::Display for SnapshotV2NetworkRestorePreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooManyOverrides => "network snapshot override count exceeds its maximum",
            Self::InvalidInterfaceId => "network snapshot override interface ID is invalid",
            Self::InvalidDestinationSelector => {
                "network snapshot override destination selector is invalid"
            }
            Self::UnknownInterface => "network snapshot override targets an unknown interface",
            Self::DuplicateInterface => "network snapshot override interface is duplicated",
            Self::MissingInterface => "network snapshot override set is incomplete",
            Self::Controller => "network snapshot destination controller projection is invalid",
            Self::Mmds => "network snapshot destination MMDS projection is invalid",
            Self::ResourceId { .. } => "network snapshot resource identity is invalid",
            Self::Allocation => "network snapshot destination allocation failed",
            Self::Cancelled { .. } => "network snapshot destination preparation was cancelled",
        })
    }
}

impl std::error::Error for SnapshotV2NetworkRestorePreparationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ResourceId { source } => Some(source),
            Self::TooManyOverrides
            | Self::InvalidInterfaceId
            | Self::InvalidDestinationSelector
            | Self::UnknownInterface
            | Self::DuplicateInterface
            | Self::MissingInterface
            | Self::Controller
            | Self::Mmds
            | Self::Allocation
            | Self::Cancelled { .. } => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AllocationFailure {
    OverrideSlots,
    DestinationSelector,
    ControllerConfigs,
    InterfaceId,
    ResourceKeys,
    MmdsInterfaceIds,
    MmdsInterfaceId,
    PreparedInterfaces,
}

#[derive(Clone, Copy)]
enum AllocationPolicy {
    System,
    #[cfg(test)]
    Fail(AllocationFailure),
}

impl AllocationPolicy {
    fn fails(self, point: AllocationFailure) -> bool {
        #[cfg(test)]
        {
            matches!(self, Self::Fail(failure) if failure == point)
        }
        #[cfg(not(test))]
        {
            let _ = (self, point);
            false
        }
    }

    fn reserve<T>(
        self,
        values: &mut Vec<T>,
        count: usize,
        point: AllocationFailure,
    ) -> Result<(), SnapshotV2NetworkRestorePreparationError> {
        if self.fails(point) {
            return Err(SnapshotV2NetworkRestorePreparationError::Allocation);
        }
        values
            .try_reserve_exact(count)
            .map_err(|_| SnapshotV2NetworkRestorePreparationError::Allocation)
    }

    fn copy_string(
        self,
        value: &str,
        point: AllocationFailure,
    ) -> Result<String, SnapshotV2NetworkRestorePreparationError> {
        if self.fails(point) {
            return Err(SnapshotV2NetworkRestorePreparationError::Allocation);
        }
        let mut copy = String::new();
        copy.try_reserve_exact(value.len())
            .map_err(|_| SnapshotV2NetworkRestorePreparationError::Allocation)?;
        copy.push_str(value);
        Ok(copy)
    }
}

fn prepare_network_restore_topology<C>(
    state: SnapshotV2NetworkState,
    overrides: &[SnapshotNetworkOverride],
    mut is_cancelled: C,
    allocation: AllocationPolicy,
) -> Result<PreparedSnapshotV2NetworkRestoreTopology, SnapshotV2NetworkRestorePreparationError>
where
    C: FnMut(SnapshotV2NetworkRestorePreparationStage) -> bool,
{
    check_cancelled(
        &mut is_cancelled,
        SnapshotV2NetworkRestorePreparationStage::Start,
    )?;
    if overrides.len() > NATIVE_V2_NETWORK_MAX_INTERFACES {
        return Err(SnapshotV2NetworkRestorePreparationError::TooManyOverrides);
    }

    let interfaces = state.interfaces();
    let transport_kind = interfaces
        .first()
        .map(|interface| interface.transport().kind())
        .ok_or(SnapshotV2NetworkRestorePreparationError::Controller)?;
    let mut destinations = Vec::new();
    allocation.reserve(
        &mut destinations,
        interfaces.len(),
        AllocationFailure::OverrideSlots,
    )?;
    destinations.resize_with(interfaces.len(), || None);

    for requested in overrides {
        check_cancelled(
            &mut is_cancelled,
            SnapshotV2NetworkRestorePreparationStage::Override,
        )?;
        validate_requested_interface_id(requested.iface_id())?;
        validate_destination_selector(requested.host_dev_name())?;
        let index = interfaces
            .iter()
            .position(|interface| interface.iface_id() == requested.iface_id())
            .ok_or(SnapshotV2NetworkRestorePreparationError::UnknownInterface)?;
        let slot = destinations
            .get_mut(index)
            .ok_or(SnapshotV2NetworkRestorePreparationError::UnknownInterface)?;
        if slot.is_some() {
            return Err(SnapshotV2NetworkRestorePreparationError::DuplicateInterface);
        }
        *slot = Some(allocation.copy_string(
            requested.host_dev_name(),
            AllocationFailure::DestinationSelector,
        )?);
    }
    if destinations.iter().any(Option::is_none) {
        return Err(SnapshotV2NetworkRestorePreparationError::MissingInterface);
    }

    let mut controllers = Vec::new();
    allocation.reserve(
        &mut controllers,
        interfaces.len(),
        AllocationFailure::ControllerConfigs,
    )?;
    let mut resource_keys = Vec::new();
    allocation.reserve(
        &mut resource_keys,
        interfaces.len(),
        AllocationFailure::ResourceKeys,
    )?;
    for (index, (interface, destination)) in interfaces.iter().zip(destinations).enumerate() {
        check_cancelled(
            &mut is_cancelled,
            SnapshotV2NetworkRestorePreparationStage::Controller,
        )?;
        let iface_id =
            allocation.copy_string(interface.iface_id(), AllocationFailure::InterfaceId)?;
        let controller = NetworkInterfaceConfig::try_from_snapshot_projection(
            iface_id,
            destination.ok_or(SnapshotV2NetworkRestorePreparationError::MissingInterface)?,
            interface.requested_guest_mac(),
            interface.requested_mtu(),
            limiter_config(interface.rx_limiter()),
            limiter_config(interface.tx_limiter()),
        )
        .map_err(|_| SnapshotV2NetworkRestorePreparationError::Controller)?;
        let public_id = SnapshotRestorePublicId::try_from(interface.iface_id())
            .map_err(|source| SnapshotV2NetworkRestorePreparationError::ResourceId { source })?;
        let instance = u32::try_from(index)
            .map_err(|_| SnapshotV2NetworkRestorePreparationError::Controller)?;
        resource_keys.push(SnapshotRestoreResourceKey::new(
            SnapshotV2DeviceKey::network(instance),
            public_id,
            SnapshotRestoreResourceClass::NetworkPacketIo,
        ));
        controllers.push(controller);
    }

    check_cancelled(
        &mut is_cancelled,
        SnapshotV2NetworkRestorePreparationStage::Mmds,
    )?;
    let mmds_controller = state
        .mmds()
        .map(|mmds| {
            let mut selected = Vec::new();
            allocation.reserve(
                &mut selected,
                mmds.interfaces().len(),
                AllocationFailure::MmdsInterfaceIds,
            )?;
            for selected_interface in mmds.interfaces() {
                let interface = interfaces
                    .get(usize::from(selected_interface.interface_index()))
                    .ok_or(SnapshotV2NetworkRestorePreparationError::Mmds)?;
                selected.push(
                    allocation
                        .copy_string(interface.iface_id(), AllocationFailure::MmdsInterfaceId)?,
                );
            }
            let mut input = MmdsConfigInput::new(selected)
                .with_version(mmds.version())
                .with_imds_compat(mmds.imds_compat());
            if let Some(address) = mmds.ipv4_address() {
                input = input.with_ipv4_address(address);
            }
            input
                .validate(&controllers)
                .map_err(|_| SnapshotV2NetworkRestorePreparationError::Mmds)
        })
        .transpose()?;

    let (portable_interfaces, mmds_state) = state.into_parts();
    let mut prepared_interfaces = Vec::new();
    allocation.reserve(
        &mut prepared_interfaces,
        portable_interfaces.len(),
        AllocationFailure::PreparedInterfaces,
    )?;
    for (index, ((portable, controller), resource_key)) in portable_interfaces
        .into_iter()
        .zip(controllers)
        .zip(resource_keys)
        .enumerate()
    {
        let source_index = u16::try_from(index)
            .map_err(|_| SnapshotV2NetworkRestorePreparationError::Controller)?;
        let mmds_stack = mmds_state.as_ref().and_then(|mmds| {
            mmds.interfaces()
                .iter()
                .copied()
                .find(|entry| entry.interface_index() == source_index)
        });
        prepared_interfaces.push(PreparedSnapshotV2NetworkRestoreInterface {
            source_index,
            resource_key,
            controller,
            portable,
            mmds_stack,
        });
    }

    check_cancelled(
        &mut is_cancelled,
        SnapshotV2NetworkRestorePreparationStage::Completion,
    )?;
    Ok(PreparedSnapshotV2NetworkRestoreTopology {
        transport_kind,
        interfaces: prepared_interfaces,
        mmds_state,
        mmds_controller,
    })
}

fn check_cancelled<C>(
    is_cancelled: &mut C,
    stage: SnapshotV2NetworkRestorePreparationStage,
) -> Result<(), SnapshotV2NetworkRestorePreparationError>
where
    C: FnMut(SnapshotV2NetworkRestorePreparationStage) -> bool,
{
    if is_cancelled(stage) {
        Err(SnapshotV2NetworkRestorePreparationError::Cancelled { stage })
    } else {
        Ok(())
    }
}

fn validate_requested_interface_id(
    iface_id: &str,
) -> Result<(), SnapshotV2NetworkRestorePreparationError> {
    if iface_id.is_empty()
        || iface_id.len() > NATIVE_V2_NETWORK_MAX_INTERFACE_ID_BYTES
        || !iface_id
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
    {
        Err(SnapshotV2NetworkRestorePreparationError::InvalidInterfaceId)
    } else {
        Ok(())
    }
}

fn validate_destination_selector(
    selector: &str,
) -> Result<(), SnapshotV2NetworkRestorePreparationError> {
    if selector.is_empty()
        || selector.len() > NATIVE_V2_NETWORK_MAX_CAPTURED_SELECTOR_BYTES
        || selector.chars().any(char::is_control)
    {
        Err(SnapshotV2NetworkRestorePreparationError::InvalidDestinationSelector)
    } else {
        Ok(())
    }
}

fn validate_snapshot_v2_network_destination_profile(
    controller: &NetworkInterfaceConfig,
    portable: &SnapshotV2NetworkInterfaceState,
    realized_profile: NetworkDeviceProfile,
) -> Result<(), RestoreSnapshotV2NetworkDeviceError> {
    if realized_profile != portable.profile()
        || controller
            .guest_mac()
            .is_some_and(|mac| realized_profile.guest_mac() != Some(mac))
        || controller
            .mtu()
            .is_some_and(|mtu| realized_profile.mtu() != Some(mtu))
        || !realized_profile
            .feature_capabilities()
            .is_dependency_complete()
    {
        Err(RestoreSnapshotV2NetworkDeviceError::Profile)
    } else {
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn restore_snapshot_v2_network_device(
    controller: &NetworkInterfaceConfig,
    portable: &SnapshotV2NetworkInterfaceState,
    destination_memory: &GuestMemory,
    realized_profile: NetworkDeviceProfile,
    interface_metrics: SharedNetworkInterfaceMetrics,
    aggregate_metrics: SharedNetworkInterfaceMetrics,
    now: Instant,
) -> Result<RestoredSnapshotV2NetworkDevice, RestoreSnapshotV2NetworkDeviceError> {
    validate_snapshot_v2_network_destination_profile(controller, portable, realized_profile)?;

    let virtio = portable.virtio();
    let local = portable.local();
    let rx_transport = virtio
        .queues()
        .first()
        .copied()
        .ok_or(RestoreSnapshotV2NetworkDeviceError::Queue)?;
    let tx_transport = virtio
        .queues()
        .get(1)
        .copied()
        .ok_or(RestoreSnapshotV2NetworkDeviceError::Queue)?;
    if virtio.queues().len() != VIRTIO_NET_QUEUE_SIZES.len() {
        return Err(RestoreSnapshotV2NetworkDeviceError::Queue);
    }
    let queue_ranges = [
        queue_ranges(&rx_transport).map_err(|_| RestoreSnapshotV2NetworkDeviceError::Queue)?,
        queue_ranges(&tx_transport).map_err(|_| RestoreSnapshotV2NetworkDeviceError::Queue)?,
    ];
    if queue_ranges
        .iter()
        .flatten()
        .flatten()
        .copied()
        .any(|range| !range_is_wholly_contained(destination_memory, range))
    {
        return Err(RestoreSnapshotV2NetworkDeviceError::QueueMemory);
    }

    let negotiated_features = virtio.driver_features();
    let rx_queue = local
        .active_rx_queue()
        .map(|cursor| {
            VirtioNetworkRxQueue::from_snapshot_state(
                restore_queue_state(rx_transport),
                cursor.next_available(),
                cursor.next_used(),
                negotiated_features,
            )
            .map_err(|_| RestoreSnapshotV2NetworkDeviceError::Queue)
        })
        .transpose()?;
    let tx_queue = local
        .active_tx_queue()
        .map(|cursor| {
            VirtioNetworkTxQueue::from_snapshot_state(
                restore_queue_state(tx_transport),
                cursor.next_available(),
                cursor.next_used(),
                negotiated_features,
            )
            .map_err(|_| RestoreSnapshotV2NetworkDeviceError::Queue)
        })
        .transpose()?;
    if rx_queue.is_some() != virtio.is_activated() || tx_queue.is_some() != virtio.is_activated() {
        return Err(RestoreSnapshotV2NetworkDeviceError::Queue);
    }
    if let Some(queue) = rx_queue.as_ref() {
        queue
            .validate_snapshot_state(destination_memory)
            .map_err(|_| RestoreSnapshotV2NetworkDeviceError::Queue)?;
    }
    let has_tx_retry = local.tx_retry().has_retry();
    if let Some(queue) = tx_queue.as_ref() {
        queue
            .validate_snapshot_state(destination_memory, has_tx_retry)
            .map_err(|_| RestoreSnapshotV2NetworkDeviceError::Queue)?;
    }
    if let (Some(rx), Some(tx)) = (rx_queue.as_ref(), tx_queue.as_ref()) {
        validate_network_queue_pair_ranges(rx, tx)
            .map_err(|_| RestoreSnapshotV2NetworkDeviceError::Queue)?;
    }

    let rx_rate_limiter = VirtioNetworkRateLimiter::from_persisted_state_at(
        controller.rx_rate_limiter(),
        restore_limiter_state(portable.rx_limiter()),
        now,
    )
    .map_err(|_| RestoreSnapshotV2NetworkDeviceError::Limiter)?;
    let tx_rate_limiter = VirtioNetworkRateLimiter::from_persisted_state_at(
        controller.tx_rate_limiter(),
        restore_limiter_state(portable.tx_limiter()),
        now,
    )
    .map_err(|_| RestoreSnapshotV2NetworkDeviceError::Limiter)?;
    let retry_deadline = match local.tx_retry() {
        SnapshotV2NetworkRetryState::None => None,
        SnapshotV2NetworkRetryState::Immediate => Some(now),
        SnapshotV2NetworkRetryState::After { remaining_nanos } => Some(
            now.checked_add(Duration::from_nanos(remaining_nanos))
                .ok_or(RestoreSnapshotV2NetworkDeviceError::Retry)?,
        ),
    };
    let mut device = VirtioNetworkDevice::from_snapshot_parts(
        rx_queue,
        tx_queue,
        rx_rate_limiter,
        tx_rate_limiter,
        has_tx_retry,
    )
    .map_err(|_| RestoreSnapshotV2NetworkDeviceError::Device)?;
    let mut config_space = VirtioNetworkConfigSpace::with_feature_capabilities(
        realized_profile.guest_mac(),
        realized_profile.mtu(),
        realized_profile.feature_capabilities(),
    );
    if config_space.available_features() != virtio.available_features() {
        return Err(RestoreSnapshotV2NetworkDeviceError::Profile);
    }
    config_space
        .attach_metrics_with_aggregate(interface_metrics.clone(), aggregate_metrics.clone());
    device.attach_metrics_with_aggregate(interface_metrics, aggregate_metrics);

    Ok(RestoredSnapshotV2NetworkDevice {
        queue_ranges,
        retry_deadline,
        config_space,
        device,
    })
}

fn limiter_config(limiter: SnapshotV2NetworkLimiterState) -> Option<NetworkRateLimiterConfig> {
    let configured = NetworkRateLimiterConfig::new(
        limiter.bandwidth().map(token_bucket_config),
        limiter.ops().map(token_bucket_config),
    );
    configured.is_configured().then_some(configured)
}

fn restore_limiter_state(
    limiter: SnapshotV2NetworkLimiterState,
) -> VirtioNetworkRateLimiterCaptureState {
    VirtioNetworkRateLimiterCaptureState::new(
        limiter.bandwidth().map(restore_token_bucket_state),
        limiter.ops().map(restore_token_bucket_state),
    )
}

fn restore_token_bucket_state(
    bucket: SnapshotV2NetworkTokenBucketState,
) -> VirtioNetworkTokenBucketCaptureState {
    VirtioNetworkTokenBucketCaptureState::new(
        token_bucket_config(bucket),
        bucket.budget(),
        bucket.remaining_burst(),
        bucket.age_nanos(),
    )
}

fn restore_queue_state(
    queue: crate::snapshot_device_v2::SnapshotV2VirtioQueueState,
) -> VirtioMmioQueueState {
    VirtioMmioQueueState::from_parts(
        queue.max_size(),
        queue.size(),
        queue.ready(),
        queue.descriptor_table(),
        queue.driver_ring(),
        queue.device_ring(),
    )
}

fn token_bucket_config(bucket: SnapshotV2NetworkTokenBucketState) -> NetworkTokenBucketConfig {
    NetworkTokenBucketConfig::new(
        bucket.size(),
        bucket.configured_burst(),
        bucket.refill_time_millis(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::interrupt::GuestInterruptLine;
    use crate::memory::{GuestAddress, GuestMemoryLayout};
    use crate::message_interrupt::{
        GuestMessage, GuestMessageInterrupt, GuestMessageInterruptSignalError,
    };
    use crate::mmio::{MmioRegion, MmioRegionId};
    use crate::network::{
        GuestMacAddress, NetworkDeviceProfile, VIRTIO_NET_QUEUE_SIZE, VirtioNetworkConfigSpace,
    };
    use crate::snapshot_device_v2::{
        SnapshotV2DeviceTransport, SnapshotV2InterruptIntent, SnapshotV2MmioDeviceState,
        SnapshotV2VirtioQueueState, SnapshotV2VirtioState, SnapshotV2VirtioStateParts,
    };
    use crate::snapshot_format::SnapshotFormatVersion;
    use crate::snapshot_network_v2_11::{
        SnapshotV2NetworkBackendClass, SnapshotV2NetworkInterfaceStateParts,
        SnapshotV2NetworkLocalState, SnapshotV2NetworkQueueState,
    };
    use crate::virtio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
    };
    use crate::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE;
    use crate::virtio_queue::VIRTQUEUE_DESC_F_NEXT;

    const TEST_MEMORY_SIZE: u64 = 0x20_0000;
    const RX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x10_0000);
    const RX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x10_2000);
    const RX_USED_RING: GuestAddress = GuestAddress::new(0x10_4000);
    const TX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x11_0000);
    const TX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x11_2000);
    const TX_USED_RING: GuestAddress = GuestAddress::new(0x11_4000);
    const TX_HEADER: GuestAddress = GuestAddress::new(0x18_0000);
    const TX_PAYLOAD: GuestAddress = GuestAddress::new(0x18_0100);
    const AVAILABLE_INDEX_OFFSET: u64 = 2;
    const AVAILABLE_RING_OFFSET: u64 = 4;
    const USED_INDEX_OFFSET: u64 = 2;

    #[derive(Debug)]
    struct TestMessageRoute(GuestMessage);

    impl GuestMessageInterrupt for TestMessageRoute {
        fn matches(&self, message: GuestMessage) -> bool {
            self.0 == message
        }

        fn signal(&self, message: GuestMessage) -> Result<(), GuestMessageInterruptSignalError> {
            if self.matches(message) {
                Ok(())
            } else {
                Err(GuestMessageInterruptSignalError::new(
                    "test route rejected an unknown message",
                    false,
                ))
            }
        }
    }

    fn network_message_registry(
        interface: &SnapshotV2NetworkInterfaceState,
    ) -> GuestMessageInterruptRegistry {
        let SnapshotV2DeviceTransport::Pci(pci) = interface.transport() else {
            panic!("test interface should use PCI");
        };
        let routes: Vec<Arc<dyn GuestMessageInterrupt>> = pci
            .msix()
            .entries()
            .iter()
            .map(|entry| {
                let address = (u64::from(entry.message_address_high()) << 32)
                    | u64::from(entry.message_address_low());
                Arc::new(TestMessageRoute(GuestMessage::new(
                    address,
                    entry.message_data(),
                ))) as Arc<dyn GuestMessageInterrupt>
            })
            .collect();
        GuestMessageInterruptRegistry::new(routes)
            .expect("network message registry should validate")
    }

    fn fixture_bytes(fixture: &str) -> Vec<u8> {
        let compact = fixture.split_ascii_whitespace().collect::<String>();
        compact
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                u8::from_str_radix(
                    std::str::from_utf8(pair).expect("fixture should be ASCII"),
                    16,
                )
                .expect("fixture should contain hexadecimal bytes")
            })
            .collect()
    }

    fn fixture(path: &str) -> SnapshotV2NetworkState {
        let fixture = match path {
            "inactive" => include_str!("snapshot_network_v2_11/fixtures/inactive-mmio.hex"),
            "active" => include_str!("snapshot_network_v2_11/fixtures/active-pci-mmds.hex"),
            _ => panic!("unknown fixture"),
        };
        SnapshotV2NetworkState::decode(
            SnapshotFormatVersion::new(2, 11, 0),
            &fixture_bytes(fixture),
        )
        .expect("network fixture should decode")
    }

    fn exact_overrides(state: &SnapshotV2NetworkState) -> Vec<SnapshotNetworkOverride> {
        state
            .interfaces()
            .iter()
            .map(|interface| SnapshotNetworkOverride::new(interface.iface_id(), "vmnet:shared"))
            .collect()
    }

    fn inactive_state_with_interface_count(count: usize) -> SnapshotV2NetworkState {
        let fixture = fixture("inactive");
        let source = &fixture.interfaces()[0];
        let SnapshotV2DeviceTransport::Mmio(mmio) = source.transport() else {
            panic!("inactive fixture should use MMIO");
        };
        let interfaces = (0..count)
            .map(|index| {
                let index_u64 = u64::try_from(index).expect("test index should fit");
                let region = MmioRegion::new(
                    MmioRegionId::new(
                        mmio.region()
                            .id()
                            .raw_value()
                            .checked_add(index_u64)
                            .expect("test region ID should fit"),
                    ),
                    GuestAddress::new(
                        mmio.region()
                            .range()
                            .start()
                            .raw_value()
                            .checked_add(index_u64 * VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
                            .expect("test MMIO placement should fit"),
                    ),
                    VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
                )
                .expect("test MMIO region should validate");
                let guest_mac = GuestMacAddress::from_bytes([
                    0x02,
                    0,
                    0,
                    0,
                    0x60,
                    u8::try_from(index).expect("test MAC index should fit"),
                ]);
                SnapshotV2NetworkInterfaceState::try_from_parts(
                    SnapshotV2NetworkInterfaceStateParts {
                        iface_id: format!("eth{index}"),
                        captured_selector: format!("captured{index}"),
                        requested_guest_mac: Some(guest_mac),
                        requested_mtu: source.requested_mtu(),
                        profile: NetworkDeviceProfile::new(Some(guest_mac), source.requested_mtu()),
                        backend: SnapshotV2NetworkBackendClass::Vmnet,
                        local: source.local().clone(),
                        virtio: source.virtio().clone(),
                        rx_limiter: source.rx_limiter(),
                        tx_limiter: source.tx_limiter(),
                        transport: SnapshotV2DeviceTransport::Mmio(
                            SnapshotV2MmioDeviceState::from_parts(
                                mmio.device_feature_select(),
                                mmio.driver_feature_select(),
                                mmio.queue_select(),
                                region,
                                GuestInterruptLine::new(
                                    mmio.interrupt_line()
                                        .raw_value()
                                        .checked_add(u32::try_from(index).unwrap())
                                        .expect("test interrupt should fit"),
                                )
                                .expect("test interrupt should validate"),
                            ),
                        ),
                    },
                )
                .expect("expanded test interface should validate")
            })
            .collect::<Vec<_>>();

        SnapshotV2NetworkState::try_new(interfaces, None)
            .expect("expanded network state should validate")
    }

    fn active_mmio_state_with_tx_retry() -> SnapshotV2NetworkState {
        let guest_mac = GuestMacAddress::from_bytes([0x02, 0, 0, 0, 0x70, 0]);
        let profile = NetworkDeviceProfile::new(Some(guest_mac), Some(1500));
        let available_features = VirtioNetworkConfigSpace::with_feature_capabilities(
            profile.guest_mac(),
            profile.mtu(),
            profile.feature_capabilities(),
        )
        .available_features();
        let queue = |descriptor_table, driver_ring, device_ring| {
            SnapshotV2VirtioQueueState::from_parts(
                VIRTIO_NET_QUEUE_SIZE,
                VIRTIO_NET_QUEUE_SIZE,
                true,
                descriptor_table,
                driver_ring,
                device_ring,
            )
        };
        let virtio = SnapshotV2VirtioState::from_parts(SnapshotV2VirtioStateParts {
            available_features,
            driver_features: available_features,
            config_generation: 7,
            status: VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                | VIRTIO_DEVICE_STATUS_DRIVER
                | VIRTIO_DEVICE_STATUS_FEATURES_OK
                | VIRTIO_DEVICE_STATUS_DRIVER_OK,
            activated: true,
            queues: vec![
                queue(RX_DESCRIPTOR_TABLE, RX_AVAILABLE_RING, RX_USED_RING),
                queue(TX_DESCRIPTOR_TABLE, TX_AVAILABLE_RING, TX_USED_RING),
            ],
            pending_notifications: vec![0, 1],
            interrupt_intents: vec![
                SnapshotV2InterruptIntent::Queue { queue_index: 0 },
                SnapshotV2InterruptIntent::Configuration,
            ],
        });
        let tx_limiter = SnapshotV2NetworkLimiterState::new(
            None,
            Some(SnapshotV2NetworkTokenBucketState::new(
                1, None, 100, 0, 0, 0,
            )),
        );
        let interface =
            SnapshotV2NetworkInterfaceState::try_from_parts(SnapshotV2NetworkInterfaceStateParts {
                iface_id: "eth0".to_owned(),
                captured_selector: "captured:source".to_owned(),
                requested_guest_mac: Some(guest_mac),
                requested_mtu: Some(1500),
                profile,
                backend: SnapshotV2NetworkBackendClass::Vmnet,
                local: SnapshotV2NetworkLocalState::new(
                    Some(SnapshotV2NetworkQueueState::new(7, 7)),
                    Some(SnapshotV2NetworkQueueState::new(9, 9)),
                    SnapshotV2NetworkRetryState::After {
                        remaining_nanos: 100_000_000,
                    },
                ),
                virtio,
                rx_limiter: SnapshotV2NetworkLimiterState::new(None, None),
                tx_limiter,
                transport: SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
                    1,
                    0,
                    1,
                    MmioRegion::new(
                        MmioRegionId::new(1),
                        GuestAddress::new(0xd000_0000),
                        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
                    )
                    .expect("network MMIO region should validate"),
                    GuestInterruptLine::new(32).expect("network SPI should validate"),
                )),
            })
            .expect("active retry interface should validate");
        SnapshotV2NetworkState::try_new(vec![interface], None)
            .expect("active retry network state should validate")
    }

    fn active_pci_state_with_immediate_retry() -> SnapshotV2NetworkState {
        let fixture = fixture("active");
        let source = &fixture.interfaces()[0];
        let interface =
            SnapshotV2NetworkInterfaceState::try_from_parts(SnapshotV2NetworkInterfaceStateParts {
                iface_id: source.iface_id().to_owned(),
                captured_selector: source.captured_selector().to_owned(),
                requested_guest_mac: source.requested_guest_mac(),
                requested_mtu: source.requested_mtu(),
                profile: source.profile(),
                backend: source.backend(),
                local: SnapshotV2NetworkLocalState::new(
                    source.local().active_rx_queue(),
                    source.local().active_tx_queue(),
                    SnapshotV2NetworkRetryState::Immediate,
                ),
                virtio: source.virtio().clone(),
                rx_limiter: source.rx_limiter(),
                tx_limiter: source.tx_limiter(),
                transport: source.transport().clone(),
            })
            .expect("active PCI immediate-retry state should validate");
        SnapshotV2NetworkState::try_new(vec![interface], fixture.mmds().cloned())
            .expect("active PCI immediate-retry aggregate should validate")
    }

    fn restore_memory_with_pending_tx() -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), TEST_MEMORY_SIZE)
                .expect("restore memory range should validate"),
        ])
        .expect("restore memory layout should validate");
        let mut memory = GuestMemory::allocate(&layout).expect("restore memory should allocate");

        write_u16(&mut memory, RX_AVAILABLE_RING, AVAILABLE_INDEX_OFFSET, 7);
        write_u16(&mut memory, RX_USED_RING, USED_INDEX_OFFSET, 7);
        write_u16(&mut memory, TX_AVAILABLE_RING, AVAILABLE_INDEX_OFFSET, 10);
        write_u16(
            &mut memory,
            TX_AVAILABLE_RING,
            AVAILABLE_RING_OFFSET + u64::from(9 % VIRTIO_NET_QUEUE_SIZE) * 2,
            0,
        );
        write_u16(&mut memory, TX_USED_RING, USED_INDEX_OFFSET, 9);

        write_u64(&mut memory, TX_DESCRIPTOR_TABLE, 0, TX_HEADER.raw_value());
        write_u32(&mut memory, TX_DESCRIPTOR_TABLE, 8, 12);
        write_u16(&mut memory, TX_DESCRIPTOR_TABLE, 12, VIRTQUEUE_DESC_F_NEXT);
        write_u16(&mut memory, TX_DESCRIPTOR_TABLE, 14, 1);
        let payload_descriptor = TX_DESCRIPTOR_TABLE
            .checked_add(16)
            .expect("payload descriptor address should fit");
        write_u64(&mut memory, payload_descriptor, 0, TX_PAYLOAD.raw_value());
        write_u32(&mut memory, payload_descriptor, 8, 64);
        write_u16(&mut memory, payload_descriptor, 12, 0);
        write_u16(&mut memory, payload_descriptor, 14, 0);
        memory
    }

    fn write_u16(memory: &mut GuestMemory, base: GuestAddress, offset: u64, value: u16) {
        memory
            .write_slice(
                &value.to_le_bytes(),
                base.checked_add(offset)
                    .expect("test write address should fit"),
            )
            .expect("u16 should write to guest memory");
    }

    fn write_u32(memory: &mut GuestMemory, base: GuestAddress, offset: u64, value: u32) {
        memory
            .write_slice(
                &value.to_le_bytes(),
                base.checked_add(offset)
                    .expect("test write address should fit"),
            )
            .expect("u32 should write to guest memory");
    }

    fn write_u64(memory: &mut GuestMemory, base: GuestAddress, offset: u64, value: u64) {
        memory
            .write_slice(
                &value.to_le_bytes(),
                base.checked_add(offset)
                    .expect("test write address should fit"),
            )
            .expect("u64 should write to guest memory");
    }

    #[test]
    fn complete_overrides_prepare_redacted_controller_and_resource_topology() {
        let state = fixture("active");
        let original = state.clone();
        let overrides = exact_overrides(&state);
        let prepared = PreparedSnapshotV2NetworkRestoreTopology::prepare(state, &overrides)
            .expect("complete overrides should prepare");

        assert_eq!(prepared.interfaces().len(), original.interfaces().len());
        for (index, (entry, portable)) in prepared
            .interfaces()
            .iter()
            .zip(original.interfaces())
            .enumerate()
        {
            assert_eq!(entry.source_index(), u16::try_from(index).unwrap());
            assert_eq!(entry.portable(), portable);
            assert_eq!(entry.controller().iface_id(), portable.iface_id());
            assert_eq!(entry.controller().host_dev_name(), "vmnet:shared");
            assert_eq!(
                entry.resource_key().resource_class(),
                SnapshotRestoreResourceClass::NetworkPacketIo
            );
            assert_eq!(
                entry.resource_key().device_key().kind(),
                SnapshotV2DeviceKey::network(0).kind()
            );
            assert_eq!(
                entry.resource_key().device_key().instance(),
                u32::try_from(index).unwrap()
            );
            assert_eq!(
                entry.resource_key().public_id().as_str(),
                portable.iface_id()
            );
        }
        assert_eq!(prepared.mmds_state(), original.mmds());
        assert_eq!(
            prepared
                .mmds_controller()
                .expect("active fixture has MMDS")
                .network_interfaces(),
            &["eth0".to_string()]
        );
        let debug = format!("{prepared:?}");
        assert!(debug.contains(REDACTED));
        assert!(!debug.contains("vmnet:shared"));
        assert!(!debug.contains("eth0"));
    }

    #[test]
    fn caller_order_and_same_string_destination_are_explicit_but_canonical() {
        let state = fixture("active");
        let mut overrides = exact_overrides(&state);
        for (requested, interface) in overrides.iter_mut().zip(state.interfaces()) {
            *requested =
                SnapshotNetworkOverride::new(interface.iface_id(), interface.captured_selector());
        }
        overrides.reverse();
        let prepared = PreparedSnapshotV2NetworkRestoreTopology::prepare(state.clone(), &overrides)
            .expect("same-string explicit destinations should prepare");
        assert!(
            prepared
                .interfaces()
                .iter()
                .zip(state.interfaces())
                .all(|(entry, source)| {
                    entry.controller().iface_id() == source.iface_id()
                        && entry.controller().host_dev_name() == source.captured_selector()
                })
        );
        assert_eq!(overrides, {
            let mut copy = exact_overrides(&state);
            for (requested, interface) in copy.iter_mut().zip(state.interfaces()) {
                *requested = SnapshotNetworkOverride::new(
                    interface.iface_id(),
                    interface.captured_selector(),
                );
            }
            copy.reverse();
            copy
        });
    }

    #[test]
    fn one_and_sixteen_interface_permutations_remain_canonical_and_complete() {
        for count in [1, NATIVE_V2_NETWORK_MAX_INTERFACES] {
            let state = inactive_state_with_interface_count(count);
            let original = state.clone();
            let mut overrides = state
                .interfaces()
                .iter()
                .map(|interface| {
                    SnapshotNetworkOverride::new(
                        interface.iface_id(),
                        interface.captured_selector(),
                    )
                })
                .collect::<Vec<_>>();
            overrides.reverse();
            let caller_copy = overrides.clone();

            let prepared =
                PreparedSnapshotV2NetworkRestoreTopology::prepare(state.clone(), &overrides)
                    .expect("complete reversed boundary set should prepare");
            let retry = PreparedSnapshotV2NetworkRestoreTopology::prepare(state, &overrides)
                .expect("unchanged boundary set should prepare repeatedly");
            assert_eq!(prepared, retry);
            assert_eq!(overrides, caller_copy);
            assert_eq!(prepared.interfaces().len(), count);
            for (index, (entry, source)) in prepared
                .interfaces()
                .iter()
                .zip(original.interfaces())
                .enumerate()
            {
                assert_eq!(entry.source_index(), u16::try_from(index).unwrap());
                assert_eq!(entry.portable(), source);
                assert_eq!(
                    entry.controller().host_dev_name(),
                    source.captured_selector()
                );
                assert_eq!(
                    entry.resource_key().device_key().instance(),
                    u32::try_from(index).unwrap()
                );
                assert_eq!(entry.resource_key().public_id().as_str(), source.iface_id());
            }
        }
    }

    #[test]
    fn malformed_unknown_duplicate_missing_and_oversized_sets_fail() {
        let state = fixture("active");
        let iface = state.interfaces()[0].iface_id();
        for (overrides, expected) in [
            (
                vec![],
                SnapshotV2NetworkRestorePreparationError::MissingInterface,
            ),
            (
                vec![SnapshotNetworkOverride::new("missing", "vmnet:shared")],
                SnapshotV2NetworkRestorePreparationError::UnknownInterface,
            ),
            (
                vec![
                    SnapshotNetworkOverride::new(iface, "vmnet:shared"),
                    SnapshotNetworkOverride::new(iface, "vmnet:host"),
                ],
                SnapshotV2NetworkRestorePreparationError::DuplicateInterface,
            ),
            (
                vec![SnapshotNetworkOverride::new("", "vmnet:shared")],
                SnapshotV2NetworkRestorePreparationError::InvalidInterfaceId,
            ),
            (
                vec![SnapshotNetworkOverride::new(iface, "bad\nselector")],
                SnapshotV2NetworkRestorePreparationError::InvalidDestinationSelector,
            ),
        ] {
            let caller_copy = overrides.clone();
            let error =
                PreparedSnapshotV2NetworkRestoreTopology::prepare(state.clone(), &overrides)
                    .expect_err("invalid override set should fail");
            assert_eq!(overrides, caller_copy);
            assert_eq!(format!("{error:?}"), format!("{expected:?}"));
            let diagnostic = format!("{error:?} {error}");
            assert!(!diagnostic.contains(iface));
            assert!(!diagnostic.contains("bad"));
            assert!(!diagnostic.contains("vmnet:"));
        }

        let too_many = (0..=NATIVE_V2_NETWORK_MAX_INTERFACES)
            .map(|index| SnapshotNetworkOverride::new(format!("eth{index}"), "vmnet:shared"))
            .collect::<Vec<_>>();
        assert!(matches!(
            PreparedSnapshotV2NetworkRestoreTopology::prepare(state, &too_many),
            Err(SnapshotV2NetworkRestorePreparationError::TooManyOverrides)
        ));
    }

    #[test]
    fn overlong_and_type_invalid_override_values_fail_before_projection() {
        let state = fixture("inactive");
        let iface_id = state.interfaces()[0].iface_id();
        for (overrides, expected) in [
            (
                vec![SnapshotNetworkOverride::new(
                    "a".repeat(NATIVE_V2_NETWORK_MAX_INTERFACE_ID_BYTES + 1),
                    "vmnet:shared",
                )],
                SnapshotV2NetworkRestorePreparationError::InvalidInterfaceId,
            ),
            (
                vec![SnapshotNetworkOverride::new("eth-invalid", "vmnet:shared")],
                SnapshotV2NetworkRestorePreparationError::InvalidInterfaceId,
            ),
            (
                vec![SnapshotNetworkOverride::new(
                    iface_id,
                    "a".repeat(NATIVE_V2_NETWORK_MAX_CAPTURED_SELECTOR_BYTES + 1),
                )],
                SnapshotV2NetworkRestorePreparationError::InvalidDestinationSelector,
            ),
            (
                vec![SnapshotNetworkOverride::new(iface_id, "")],
                SnapshotV2NetworkRestorePreparationError::InvalidDestinationSelector,
            ),
        ] {
            let caller_copy = overrides.clone();
            let error =
                PreparedSnapshotV2NetworkRestoreTopology::prepare(state.clone(), &overrides)
                    .expect_err("invalid bounded override should fail");
            assert_eq!(overrides, caller_copy);
            assert_eq!(format!("{error:?}"), format!("{expected:?}"));
        }
    }

    #[test]
    fn every_stable_cancellation_checkpoint_prevents_publication() {
        for target in [
            SnapshotV2NetworkRestorePreparationStage::Start,
            SnapshotV2NetworkRestorePreparationStage::Override,
            SnapshotV2NetworkRestorePreparationStage::Controller,
            SnapshotV2NetworkRestorePreparationStage::Mmds,
            SnapshotV2NetworkRestorePreparationStage::Completion,
        ] {
            let state = fixture("active");
            let source = state.clone();
            let overrides = exact_overrides(&state);
            let error = PreparedSnapshotV2NetworkRestoreTopology::prepare_with_cancel(
                state,
                &overrides,
                |stage| stage == target,
            )
            .expect_err("targeted cancellation should fail");
            assert!(error.is_cancelled());
            assert!(matches!(
                error,
                SnapshotV2NetworkRestorePreparationError::Cancelled { stage } if stage == target
            ));
            assert_eq!(source, fixture("active"));
        }
    }

    #[test]
    fn every_injected_allocation_failure_is_redacted() {
        for point in [
            AllocationFailure::OverrideSlots,
            AllocationFailure::DestinationSelector,
            AllocationFailure::ControllerConfigs,
            AllocationFailure::InterfaceId,
            AllocationFailure::ResourceKeys,
            AllocationFailure::MmdsInterfaceIds,
            AllocationFailure::MmdsInterfaceId,
            AllocationFailure::PreparedInterfaces,
        ] {
            let state = fixture("active");
            let overrides = exact_overrides(&state);
            let error = prepare_network_restore_topology(
                state,
                &overrides,
                |_| false,
                AllocationPolicy::Fail(point),
            )
            .expect_err("injected allocation failure should fail");
            assert!(matches!(
                error,
                SnapshotV2NetworkRestorePreparationError::Allocation
            ));
            assert!(!format!("{error:?} {error}").contains("vmnet:shared"));
        }
    }

    #[test]
    fn inactive_mmio_handler_rebinds_only_selector_and_recaptures_exactly() {
        let state = fixture("inactive");
        let topology = PreparedSnapshotV2NetworkRestoreTopology::prepare(
            state.clone(),
            &exact_overrides(&state),
        )
        .expect("inactive topology should prepare");
        let (mut interfaces, _, _) = topology.into_parts();
        let interface = interfaces.pop().expect("one interface should be prepared");
        let realized_profile = interface.portable().profile();
        let memory = restore_memory_with_pending_tx();
        let prepared = interface
            .into_mmio_handler(
                &memory,
                realized_profile,
                SharedNetworkInterfaceMetrics::default(),
                SharedNetworkInterfaceMetrics::default(),
                Instant::now(),
            )
            .expect("inactive MMIO handler should materialize");

        assert_eq!(prepared.source_index(), 0);
        assert_eq!(prepared.controller().host_dev_name(), "vmnet:shared");
        assert_eq!(
            prepared.expected_state().captured_selector(),
            "vmnet:shared"
        );
        assert_eq!(
            prepared.expected_state().requested_guest_mac(),
            state.interfaces()[0].requested_guest_mac()
        );
        assert_eq!(prepared.queue_ranges(), &[None, None]);
        assert_eq!(prepared.retry(), SnapshotV2NetworkRetryState::None);
        assert_eq!(prepared.retry_deadline(), None);
        assert!(!prepared.handler().is_device_activated());
        assert_eq!(prepared.registration().index(), 0);
        assert_eq!(prepared.registration().region_id(), prepared.region().id());
        let debug = format!("{prepared:?}");
        assert!(debug.contains(REDACTED));
        assert!(!debug.contains("vmnet:shared"));
    }

    #[test]
    fn active_mmio_handler_restores_queues_limiter_retry_and_common_state() {
        let state = active_mmio_state_with_tx_retry();
        let topology = PreparedSnapshotV2NetworkRestoreTopology::prepare(
            state.clone(),
            &exact_overrides(&state),
        )
        .expect("active topology should prepare");
        let (mut interfaces, _, _) = topology.into_parts();
        let interface = interfaces.pop().expect("one interface should be prepared");
        let realized_profile = interface.portable().profile();
        let memory = restore_memory_with_pending_tx();
        let now = Instant::now();
        let prepared = interface
            .into_mmio_handler(
                &memory,
                realized_profile,
                SharedNetworkInterfaceMetrics::default(),
                SharedNetworkInterfaceMetrics::default(),
                now,
            )
            .expect("active MMIO handler should materialize");

        assert!(prepared.handler().is_device_activated());
        assert!(
            prepared
                .handler()
                .activation_handler()
                .has_pending_rate_limited_tx_queue()
        );
        assert_eq!(
            prepared.retry(),
            SnapshotV2NetworkRetryState::After {
                remaining_nanos: 100_000_000
            }
        );
        assert_eq!(
            prepared.retry_deadline(),
            now.checked_add(Duration::from_millis(100))
        );
        assert!(prepared.queue_ranges().iter().all(Option::is_some));
        assert_eq!(
            prepared.expected_state().virtio(),
            state.interfaces()[0].virtio()
        );
        assert_eq!(
            prepared.expected_state().tx_limiter(),
            state.interfaces()[0].tx_limiter()
        );
        assert_eq!(
            prepared.expected_state().captured_selector(),
            "vmnet:shared"
        );
    }

    #[test]
    fn active_pci_endpoint_rebinds_selector_and_recaptures_exactly() {
        let state = active_pci_state_with_immediate_retry();
        let topology = PreparedSnapshotV2NetworkRestoreTopology::prepare(
            state.clone(),
            &exact_overrides(&state),
        )
        .expect("active PCI topology should prepare");
        let (mut interfaces, _, _) = topology.into_parts();
        let interface = interfaces.pop().expect("one interface should be prepared");
        let profile = interface.portable().profile();
        let messages = network_message_registry(interface.portable());
        let now = Instant::now();
        let prepared = interface
            .into_pci_endpoint(
                &restore_memory_with_pending_tx(),
                profile,
                SharedNetworkInterfaceMetrics::default(),
                SharedNetworkInterfaceMetrics::default(),
                MmioRegionId::new(44),
                messages,
                now,
            )
            .expect("active PCI endpoint should materialize");

        let SnapshotV2DeviceTransport::Pci(expected_pci) = state.interfaces()[0].transport() else {
            panic!("active fixture should use PCI");
        };
        assert_eq!(prepared.source_index(), 0);
        assert_eq!(prepared.controller().host_dev_name(), "vmnet:shared");
        assert_eq!(
            prepared.expected_state().captured_selector(),
            "vmnet:shared"
        );
        assert_eq!(prepared.origin(), expected_pci.origin());
        assert_eq!(prepared.endpoint().sbdf(), expected_pci.sbdf());
        assert_eq!(prepared.endpoint().bar_range(), expected_pci.bar_range());
        assert_eq!(prepared.endpoint().region_id(), MmioRegionId::new(44));
        assert_eq!(
            prepared.expected_state().virtio(),
            state.interfaces()[0].virtio()
        );
        assert!(prepared.queue_ranges().iter().all(Option::is_some));
        let debug = format!("{prepared:?}");
        assert!(debug.contains(REDACTED));
        assert!(!debug.contains("vmnet:shared"));
    }

    #[test]
    fn pci_endpoint_rejects_mmio_transport_and_unmapped_queue_memory_redacted() {
        let state = fixture("inactive");
        let topology = PreparedSnapshotV2NetworkRestoreTopology::prepare(
            state.clone(),
            &exact_overrides(&state),
        )
        .expect("inactive MMIO topology should prepare");
        let (mut interfaces, _, _) = topology.into_parts();
        let interface = interfaces.pop().expect("one interface should be prepared");
        let profile = interface.portable().profile();
        let dummy_messages = GuestMessageInterruptRegistry::new(vec![Arc::new(TestMessageRoute(
            GuestMessage::new(0x0800_0040, 64),
        ))
            as Arc<dyn GuestMessageInterrupt>])
        .expect("dummy registry should validate");
        let error = interface
            .into_pci_endpoint(
                &restore_memory_with_pending_tx(),
                profile,
                SharedNetworkInterfaceMetrics::default(),
                SharedNetworkInterfaceMetrics::default(),
                MmioRegionId::new(44),
                dummy_messages,
                Instant::now(),
            )
            .expect_err("MMIO continuation must not materialize as PCI");
        assert!(matches!(
            error,
            SnapshotV2NetworkPciEndpointError::WrongTransport
        ));

        let state = active_pci_state_with_immediate_retry();
        let topology = PreparedSnapshotV2NetworkRestoreTopology::prepare(
            state.clone(),
            &exact_overrides(&state),
        )
        .expect("active PCI topology should prepare");
        let (mut interfaces, _, _) = topology.into_parts();
        let interface = interfaces.pop().expect("one interface should be prepared");
        let profile = interface.portable().profile();
        let messages = network_message_registry(interface.portable());
        let tiny_layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), 0x4000)
                .expect("tiny range should validate"),
        ])
        .expect("tiny layout should validate");
        let tiny_memory = GuestMemory::allocate(&tiny_layout).expect("tiny memory should allocate");
        let error = interface
            .into_pci_endpoint(
                &tiny_memory,
                profile,
                SharedNetworkInterfaceMetrics::default(),
                SharedNetworkInterfaceMetrics::default(),
                MmioRegionId::new(44),
                messages,
                Instant::now(),
            )
            .expect_err("unmapped PCI queue memory should fail");
        assert!(matches!(
            error,
            SnapshotV2NetworkPciEndpointError::QueueMemory
        ));
        let diagnostic = format!("{error:?} {error}");
        assert!(diagnostic.contains(REDACTED));
        assert!(!diagnostic.contains("eth0"));
        assert!(!diagnostic.contains("vmnet:"));
    }

    #[test]
    fn sixteen_inactive_mmio_handlers_preserve_saved_order_and_exact_capacity() {
        let state = inactive_state_with_interface_count(NATIVE_V2_NETWORK_MAX_INTERFACES);
        let topology = PreparedSnapshotV2NetworkRestoreTopology::prepare(
            state.clone(),
            &exact_overrides(&state),
        )
        .expect("sixteen-interface topology should prepare");
        let (interfaces, _, _) = topology.into_parts();
        let memory = restore_memory_with_pending_tx();
        let aggregate = SharedNetworkInterfaceMetrics::default();

        let prepared = interfaces
            .into_iter()
            .enumerate()
            .map(|(index, interface)| {
                let profile = interface.portable().profile();
                let prepared = interface
                    .into_mmio_handler(
                        &memory,
                        profile,
                        SharedNetworkInterfaceMetrics::default(),
                        aggregate.clone(),
                        Instant::now(),
                    )
                    .expect("saved-order interface should materialize");
                assert_eq!(usize::from(prepared.source_index()), index);
                assert_eq!(prepared.registration().index(), index);
                prepared
            })
            .collect::<Vec<_>>();

        assert_eq!(prepared.len(), NATIVE_V2_NETWORK_MAX_INTERFACES);
        assert_eq!(
            prepared
                .iter()
                .map(|entry| entry.region().id())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            NATIVE_V2_NETWORK_MAX_INTERFACES
        );
    }

    #[test]
    fn handler_materialization_rejects_profile_and_queue_memory_redacted() {
        let state = active_mmio_state_with_tx_retry();
        let topology = PreparedSnapshotV2NetworkRestoreTopology::prepare(
            state.clone(),
            &exact_overrides(&state),
        )
        .expect("active topology should prepare");
        let (mut interfaces, _, _) = topology.into_parts();
        let interface = interfaces.pop().expect("one interface should be prepared");
        let error = interface
            .into_mmio_handler(
                &restore_memory_with_pending_tx(),
                NetworkDeviceProfile::new(None, None),
                SharedNetworkInterfaceMetrics::default(),
                SharedNetworkInterfaceMetrics::default(),
                Instant::now(),
            )
            .expect_err("mismatched profile should fail");
        assert!(matches!(error, SnapshotV2NetworkMmioHandlerError::Profile));
        assert!(!format!("{error:?} {error}").contains("eth0"));

        let topology = PreparedSnapshotV2NetworkRestoreTopology::prepare(
            state.clone(),
            &exact_overrides(&state),
        )
        .expect("active topology should prepare again");
        let (mut interfaces, _, _) = topology.into_parts();
        let interface = interfaces.pop().expect("one interface should be prepared");
        let profile = interface.portable().profile();
        let tiny_layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), 0x4000)
                .expect("tiny range should validate"),
        ])
        .expect("tiny layout should validate");
        let tiny_memory = GuestMemory::allocate(&tiny_layout).expect("tiny memory should allocate");
        let error = interface
            .into_mmio_handler(
                &tiny_memory,
                profile,
                SharedNetworkInterfaceMetrics::default(),
                SharedNetworkInterfaceMetrics::default(),
                Instant::now(),
            )
            .expect_err("unmapped queue memory should fail");
        assert!(matches!(
            error,
            SnapshotV2NetworkMmioHandlerError::QueueMemory
        ));
        assert!(!format!("{error:?} {error}").contains("1048576"));
    }

    #[test]
    fn controller_projection_preserves_requested_not_realized_configuration() {
        let state = fixture("active");
        let expected = state.interfaces()[0].clone();
        let prepared = PreparedSnapshotV2NetworkRestoreTopology::prepare(
            state.clone(),
            &exact_overrides(&state),
        )
        .expect("fixture should prepare");
        let controller = prepared.interfaces()[0].controller();
        assert_eq!(controller.guest_mac(), expected.requested_guest_mac());
        assert_eq!(controller.mtu(), expected.requested_mtu());
        assert_eq!(
            controller
                .rx_rate_limiter()
                .and_then(NetworkRateLimiterConfig::bandwidth),
            expected.rx_limiter().bandwidth().map(token_bucket_config)
        );
        assert_eq!(
            controller
                .rx_rate_limiter()
                .and_then(NetworkRateLimiterConfig::ops),
            expected.rx_limiter().ops().map(token_bucket_config)
        );
        assert_eq!(
            controller
                .tx_rate_limiter()
                .and_then(NetworkRateLimiterConfig::bandwidth),
            expected.tx_limiter().bandwidth().map(token_bucket_config)
        );
        assert_eq!(
            controller
                .tx_rate_limiter()
                .and_then(NetworkRateLimiterConfig::ops),
            expected.tx_limiter().ops().map(token_bucket_config)
        );
        assert_eq!(prepared.interfaces()[0].portable(), &expected);
    }
}
