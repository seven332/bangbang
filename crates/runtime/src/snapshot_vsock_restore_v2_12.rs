//! Owner-free exact native-v2 2.12 vsock restore preparation.
//!
//! This module resolves logical destination intent, validates the canonical
//! restore resource identity, and reconstructs capture-compatible MMIO or PCI
//! state against destination memory. It deliberately creates no socket,
//! descriptor, grant, broker session, runtime device owner, metric, interrupt
//! route, VM, vCPU, or platform authority.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestMemory, GuestMemoryRange};
use crate::message_interrupt::GuestMessageInterruptRegistry;
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::pci::PciSbdf;
use crate::snapshot::{
    SnapshotVsockOverride, SnapshotVsockSelectorError, SnapshotVsockSelectors,
    resolve_snapshot_vsock_selectors,
};
use crate::snapshot_device_v2::{
    SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind, SnapshotV2RootTransportRestoreError,
    restore_mmio_transport_state_for_device_with_config_status_gate,
};
use crate::snapshot_restore::{
    NATIVE_V2_VSOCK_RESTORE_PUBLIC_ID, SnapshotRestorePublicId, SnapshotRestorePublicIdError,
    SnapshotRestoreResourceClass, SnapshotRestoreResourceKey,
    validate_native_v2_vsock_resource_key,
};
use crate::snapshot_vsock_v2_12::{
    SnapshotV2VsockQueueState, SnapshotV2VsockState, SnapshotV2VsockStateCaptureError,
};
use crate::storage_capture::StorageDeviceOrigin;
use crate::virtio::{VirtioDeviceType, VirtioDeviceTypeError};
use crate::virtio_pci::{
    PreparedVirtioPciEndpoint, VirtioPciEndpointError, VirtioPciIdentity, VirtioPciTransportState,
};
use crate::vsock::{
    VIRTIO_VSOCK_DEVICE_ID, VIRTIO_VSOCK_QUEUE_SIZES, VirtioVsockActiveQueuesCaptureState,
    VirtioVsockConfigSpace, VirtioVsockDevice, VirtioVsockDeviceCaptureError,
    VirtioVsockDeviceCaptureState, VirtioVsockMmioCaptureState, VirtioVsockPciCaptureState,
    VirtioVsockQueueCaptureState, VirtioVsockReconstructionError,
    VirtioVsockReconstructionResource, VsockConfig, VsockConfigInput,
};

const REDACTED: &str = "<redacted>";

/// Stable checkpoints after all captured/destination selectors have resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2VsockRestorePreparationStage {
    /// Before projecting the canonical resource request and destination config.
    Resource,
    /// Before reconstructing checked capture-compatible device state.
    Device,
    /// Before normalizing reconstructed state back to the portable component.
    Normalize,
    /// After complete validation and before publishing the immutable topology.
    Completion,
}

/// Host-operation-free request for one exact-2.12 vsock endpoint.
#[derive(PartialEq, Eq)]
pub struct SnapshotV2VsockRestoreResourceRequest {
    resource_key: SnapshotRestoreResourceKey,
    selectors: SnapshotVsockSelectors,
    config: VsockConfig,
    overridden: bool,
}

impl SnapshotV2VsockRestoreResourceRequest {
    /// Returns the canonical singleton endpoint resource identity.
    pub const fn resource_key(&self) -> &SnapshotRestoreResourceKey {
        &self.resource_key
    }

    /// Returns the captured and selected destination logical selectors.
    pub const fn selectors(&self) -> &SnapshotVsockSelectors {
        &self.selectors
    }

    /// Returns destination configuration without proving access to its path.
    pub const fn config(&self) -> &VsockConfig {
        &self.config
    }

    /// Returns whether the API explicitly supplied the destination selector.
    pub const fn is_overridden(&self) -> bool {
        self.overridden
    }

    /// Consumes the request into still owner-free logical values.
    pub fn into_parts(
        self,
    ) -> (
        SnapshotRestoreResourceKey,
        SnapshotVsockSelectors,
        VsockConfig,
        bool,
    ) {
        (
            self.resource_key,
            self.selectors,
            self.config,
            self.overridden,
        )
    }
}

impl fmt::Debug for SnapshotV2VsockRestoreResourceRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2VsockRestoreResourceRequest")
            .field("overridden", &self.overridden)
            .field("state", &REDACTED)
            .finish()
    }
}

/// Checked owner-free MMIO vsock continuation and exact placement.
#[derive(Clone, PartialEq, Eq)]
pub struct PreparedSnapshotV2VsockMmioState {
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    capture: VirtioVsockMmioCaptureState,
}

impl PreparedSnapshotV2VsockMmioState {
    /// Returns the exact retained MMIO region.
    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    /// Returns the exact retained guest interrupt line.
    pub const fn interrupt_line(&self) -> GuestInterruptLine {
        self.interrupt_line
    }

    /// Returns checked capture-compatible device and transport state.
    pub const fn capture(&self) -> &VirtioVsockMmioCaptureState {
        &self.capture
    }

    /// Consumes the value into exact placement and checked capture state.
    pub fn into_parts(self) -> (MmioRegion, GuestInterruptLine, VirtioVsockMmioCaptureState) {
        (self.region, self.interrupt_line, self.capture)
    }
}

impl fmt::Debug for PreparedSnapshotV2VsockMmioState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2VsockMmioState")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Checked owner-free PCI vsock continuation and exact placement.
#[derive(Clone, PartialEq, Eq)]
pub struct PreparedSnapshotV2VsockPciState {
    origin: StorageDeviceOrigin,
    sbdf: PciSbdf,
    bar_range: GuestMemoryRange,
    capture: VirtioVsockPciCaptureState,
}

impl PreparedSnapshotV2VsockPciState {
    /// Returns the captured source-family placement origin.
    pub const fn origin(&self) -> StorageDeviceOrigin {
        self.origin
    }

    /// Returns the exact retained PCI function.
    pub const fn sbdf(&self) -> PciSbdf {
        self.sbdf
    }

    /// Returns the exact retained PCI BAR range.
    pub const fn bar_range(&self) -> GuestMemoryRange {
        self.bar_range
    }

    /// Returns checked capture-compatible device and transport state.
    pub const fn capture(&self) -> &VirtioVsockPciCaptureState {
        &self.capture
    }

    /// Consumes the value into exact placement and checked capture state.
    pub fn into_parts(
        self,
    ) -> (
        StorageDeviceOrigin,
        PciSbdf,
        GuestMemoryRange,
        VirtioVsockPciCaptureState,
    ) {
        (self.origin, self.sbdf, self.bar_range, self.capture)
    }

    /// Consumes checked PCI state into one complete retained endpoint.
    ///
    /// The caller supplies the destination endpoint resource, fixed dispatcher
    /// region, and fresh message registry. Function, BAR, dispatcher,
    /// interrupt-resource, run-loop, session, and VM ownership remain outside
    /// this value.
    #[doc(hidden)]
    pub fn into_pci_endpoint(
        self,
        config: &VsockConfig,
        destination_memory: &GuestMemory,
        resource: &mut VirtioVsockReconstructionResource,
        region_id: MmioRegionId,
        messages: GuestMessageInterruptRegistry,
    ) -> Result<PreparedSnapshotV2VsockPciEndpoint, SnapshotV2VsockPciEndpointError> {
        if resource.captured_selector() != self.capture.device().backend_selector()
            || resource.destination_selector().path() != config.uds_path()
            || self.capture.device().guest_cid() != u64::from(config.guest_cid())
        {
            return Err(SnapshotV2VsockPciEndpointError::ResourceIdentity);
        }
        let expected = PreparedSnapshotV2VsockRestoreState::Pci(self.clone())
            .into_destination_normalized_state(config)
            .map_err(|_| SnapshotV2VsockPciEndpointError::ExpectedState)?;
        let Self {
            origin,
            sbdf,
            bar_range,
            capture,
        } = self;
        let retained = capture.transport().clone();
        let prepared = capture
            .reconstruct_snapshot_device(destination_memory, resource)
            .map_err(SnapshotV2VsockPciEndpointError::Device)?;
        let activation_is_active = prepared.device().is_activated();
        let (guest_cid, uds_path, config_space, device) = prepared.into_parts();
        let device_type = VirtioDeviceType::new(VIRTIO_VSOCK_DEVICE_ID)
            .map_err(SnapshotV2VsockPciEndpointError::DeviceType)?;
        let identity = VirtioPciIdentity::new(device_type, config_space.available_features())
            .with_config_generation(retained.device_registers().config_generation());
        let endpoint = PreparedVirtioPciEndpoint::new(
            identity,
            &VIRTIO_VSOCK_QUEUE_SIZES,
            config_space,
            device,
            activation_is_active,
            false,
            &retained,
            sbdf,
            bar_range,
            region_id,
            messages,
        )
        .map_err(SnapshotV2VsockPciEndpointError::Endpoint)?;
        let recaptured = endpoint
            .endpoint()
            .transport_state()
            .map_err(SnapshotV2VsockPciEndpointError::Endpoint)?;
        if recaptured != retained {
            return Err(SnapshotV2VsockPciEndpointError::StateMismatch);
        }

        Ok(PreparedSnapshotV2VsockPciEndpoint {
            guest_cid,
            uds_path,
            expected,
            origin,
            endpoint,
        })
    }
}

impl fmt::Debug for PreparedSnapshotV2VsockPciState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2VsockPciState")
            .field("state", &REDACTED)
            .finish()
    }
}

/// One checked exact-2.12 vsock endpoint awaiting PCI publication.
#[doc(hidden)]
pub struct PreparedSnapshotV2VsockPciEndpoint {
    guest_cid: u32,
    uds_path: PathBuf,
    expected: SnapshotV2VsockState,
    origin: StorageDeviceOrigin,
    endpoint: PreparedVirtioPciEndpoint<VirtioVsockConfigSpace, VirtioVsockDevice>,
}

/// Consumed exact-2.12 vsock continuation and retained PCI endpoint.
#[doc(hidden)]
pub type PreparedSnapshotV2VsockPciEndpointParts = (
    u32,
    PathBuf,
    SnapshotV2VsockState,
    StorageDeviceOrigin,
    PreparedVirtioPciEndpoint<VirtioVsockConfigSpace, VirtioVsockDevice>,
);

impl PreparedSnapshotV2VsockPciEndpoint {
    pub const fn guest_cid(&self) -> u32 {
        self.guest_cid
    }

    pub fn uds_path(&self) -> &Path {
        &self.uds_path
    }

    pub const fn expected_state(&self) -> &SnapshotV2VsockState {
        &self.expected
    }

    pub const fn origin(&self) -> StorageDeviceOrigin {
        self.origin
    }

    pub const fn endpoint(
        &self,
    ) -> &PreparedVirtioPciEndpoint<VirtioVsockConfigSpace, VirtioVsockDevice> {
        &self.endpoint
    }

    pub fn into_parts(self) -> PreparedSnapshotV2VsockPciEndpointParts {
        (
            self.guest_cid,
            self.uds_path,
            self.expected,
            self.origin,
            self.endpoint,
        )
    }
}

impl fmt::Debug for PreparedSnapshotV2VsockPciEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2VsockPciEndpoint")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Redacted failure while materializing one checked vsock PCI endpoint.
#[doc(hidden)]
pub enum SnapshotV2VsockPciEndpointError {
    ResourceIdentity,
    ExpectedState,
    Device(VirtioVsockReconstructionError),
    DeviceType(VirtioDeviceTypeError),
    Endpoint(VirtioPciEndpointError),
    StateMismatch,
}

impl fmt::Debug for SnapshotV2VsockPciEndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2VsockPciEndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ResourceIdentity => "native-v2 vsock PCI resource identity is invalid",
            Self::ExpectedState => "native-v2 vsock PCI destination state is invalid",
            Self::Device(_) => "native-v2 vsock PCI device reconstruction failed",
            Self::DeviceType(_) => "native-v2 vsock PCI device type is invalid",
            Self::Endpoint(_) => "native-v2 vsock PCI endpoint construction failed",
            Self::StateMismatch => "native-v2 vsock PCI endpoint state does not match",
        })
    }
}

impl std::error::Error for SnapshotV2VsockPciEndpointError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Device(source) => Some(source),
            Self::DeviceType(source) => Some(source),
            Self::Endpoint(source) => Some(source),
            Self::ResourceIdentity | Self::ExpectedState | Self::StateMismatch => None,
        }
    }
}

/// Checked capture-compatible exact-2.12 vsock state.
#[derive(Clone, PartialEq, Eq)]
pub enum PreparedSnapshotV2VsockRestoreState {
    /// Modern virtio-mmio continuation.
    Mmio(PreparedSnapshotV2VsockMmioState),
    /// Modern non-transitional virtio-pci continuation.
    Pci(PreparedSnapshotV2VsockPciState),
}

impl PreparedSnapshotV2VsockRestoreState {
    /// Returns the retained destination transport kind.
    pub const fn transport_kind(&self) -> SnapshotV2DeviceTransportKind {
        match self {
            Self::Mmio(_) => SnapshotV2DeviceTransportKind::Mmio,
            Self::Pci(_) => SnapshotV2DeviceTransportKind::Pci,
        }
    }

    /// Consumes checked state and normalizes it to the portable component.
    pub fn into_normalized_state(
        self,
    ) -> Result<SnapshotV2VsockState, SnapshotV2VsockStateCaptureError> {
        match self {
            Self::Mmio(prepared) => {
                let (region, interrupt_line, capture) = prepared.into_parts();
                SnapshotV2VsockState::try_from_mmio_capture(region, interrupt_line, capture)
            }
            Self::Pci(prepared) => {
                let (origin, sbdf, bar_range, capture) = prepared.into_parts();
                SnapshotV2VsockState::try_from_pci_capture(origin, sbdf, bar_range, capture)
            }
        }
    }

    /// Consumes checked source state and projects the exact state expected
    /// from a live destination using the already-validated destination
    /// configuration.
    pub fn into_destination_normalized_state(
        self,
        config: &VsockConfig,
    ) -> Result<SnapshotV2VsockState, SnapshotV2VsockRestorePreparationError> {
        let normalized = self
            .into_normalized_state()
            .map_err(SnapshotV2VsockRestorePreparationError::Normalize)?;
        if normalized.guest_cid() != u64::from(config.guest_cid()) {
            return Err(SnapshotV2VsockRestorePreparationError::Config);
        }
        let mut parts = normalized.into_parts();
        parts.backend_selector =
            crate::vsock::VsockBackendSelector::try_from_path(config.uds_path())
                .map_err(|_| SnapshotV2VsockRestorePreparationError::Config)?;
        SnapshotV2VsockState::try_from_parts(parts)
            .map_err(|_| SnapshotV2VsockRestorePreparationError::StateMismatch)
    }
}

impl fmt::Debug for PreparedSnapshotV2VsockRestoreState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2VsockRestoreState")
            .field("transport", &self.transport_kind())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Immutable owner-free exact-2.12 vsock destination topology.
#[derive(PartialEq, Eq)]
pub struct PreparedSnapshotV2VsockRestoreTopology {
    request: SnapshotV2VsockRestoreResourceRequest,
    state: PreparedSnapshotV2VsockRestoreState,
}

impl PreparedSnapshotV2VsockRestoreTopology {
    /// Resolves and validates one present portable vsock component.
    pub fn prepare(
        state: SnapshotV2VsockState,
        requested_override: Option<&SnapshotVsockOverride>,
        expected_transport: SnapshotV2DeviceTransportKind,
        destination_memory: &GuestMemory,
    ) -> Result<Self, SnapshotV2VsockRestorePreparationError> {
        Self::prepare_with_cancel(
            state,
            requested_override,
            expected_transport,
            destination_memory,
            |_| false,
        )
    }

    /// Resolves and validates one optional component before any host operation.
    pub fn prepare_optional(
        state: Option<SnapshotV2VsockState>,
        requested_override: Option<&SnapshotVsockOverride>,
        expected_transport: SnapshotV2DeviceTransportKind,
        destination_memory: &GuestMemory,
    ) -> Result<Option<Self>, SnapshotV2VsockRestorePreparationError> {
        Self::prepare_optional_with_cancel(
            state,
            requested_override,
            expected_transport,
            destination_memory,
            |_| false,
        )
    }

    /// Resolves and validates one present component with stable cancellation.
    pub fn prepare_with_cancel<C>(
        state: SnapshotV2VsockState,
        requested_override: Option<&SnapshotVsockOverride>,
        expected_transport: SnapshotV2DeviceTransportKind,
        destination_memory: &GuestMemory,
        is_cancelled: C,
    ) -> Result<Self, SnapshotV2VsockRestorePreparationError>
    where
        C: FnMut(SnapshotV2VsockRestorePreparationStage) -> bool,
    {
        Self::prepare_optional_with_cancel(
            Some(state),
            requested_override,
            expected_transport,
            destination_memory,
            is_cancelled,
        )?
        .ok_or(SnapshotV2VsockRestorePreparationError::ResourceIdentity)
    }

    /// Resolves selectors before invoking any cancellation callback.
    pub fn prepare_optional_with_cancel<C>(
        state: Option<SnapshotV2VsockState>,
        requested_override: Option<&SnapshotVsockOverride>,
        expected_transport: SnapshotV2DeviceTransportKind,
        destination_memory: &GuestMemory,
        is_cancelled: C,
    ) -> Result<Option<Self>, SnapshotV2VsockRestorePreparationError>
    where
        C: FnMut(SnapshotV2VsockRestorePreparationStage) -> bool,
    {
        prepare_optional_vsock_restore_topology(
            state,
            requested_override,
            expected_transport,
            destination_memory,
            is_cancelled,
            AllocationPolicy::System,
            None,
        )
    }

    /// Returns the canonical logical endpoint request.
    pub const fn request(&self) -> &SnapshotV2VsockRestoreResourceRequest {
        &self.request
    }

    /// Returns checked owner-free MMIO or PCI state.
    pub const fn state(&self) -> &PreparedSnapshotV2VsockRestoreState {
        &self.state
    }

    /// Returns the exact retained transport kind.
    pub const fn transport_kind(&self) -> SnapshotV2DeviceTransportKind {
        self.state.transport_kind()
    }

    /// Consumes the topology into request and checked state.
    pub fn into_parts(
        self,
    ) -> (
        SnapshotV2VsockRestoreResourceRequest,
        PreparedSnapshotV2VsockRestoreState,
    ) {
        (self.request, self.state)
    }

    /// Consumes the topology and normalizes checked state back to portable form.
    pub fn into_normalized_state(
        self,
    ) -> Result<SnapshotV2VsockState, SnapshotV2VsockStateCaptureError> {
        self.state.into_normalized_state()
    }
}

impl fmt::Debug for PreparedSnapshotV2VsockRestoreTopology {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2VsockRestoreTopology")
            .field("transport", &self.transport_kind())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Failure while preparing owner-free exact-2.12 vsock restore state.
pub enum SnapshotV2VsockRestorePreparationError {
    /// Captured and requested selectors could not be resolved.
    Selector(SnapshotVsockSelectorError),
    /// Destination `VsockConfig` could not be projected.
    Config,
    /// The stable private resource identifier could not be retained.
    ResourceId(SnapshotRestorePublicIdError),
    /// Resource key, class, or private public identity is inconsistent.
    ResourceIdentity,
    /// Portable and selected exact-product transports disagree.
    DestinationTransport,
    /// Encoding-independent vsock device state is invalid.
    Device(VirtioVsockDeviceCaptureError),
    /// Owner-free MMIO transport reconstruction failed.
    Mmio(SnapshotV2RootTransportRestoreError),
    /// The fixed modern PCI device type could not be represented.
    PciDeviceType(VirtioDeviceTypeError),
    /// Owner-free PCI transport reconstruction failed.
    Pci(VirtioPciEndpointError),
    /// Checked state could not normalize to portable exact-2.12 state.
    Normalize(SnapshotV2VsockStateCaptureError),
    /// Normalized state differs from the consumed portable component.
    StateMismatch,
    /// A bounded preparation allocation failed.
    Allocation,
    /// Preparation was cancelled at a stable post-selection checkpoint.
    Cancelled {
        /// The checkpoint that observed cancellation.
        stage: SnapshotV2VsockRestorePreparationStage,
    },
}

impl SnapshotV2VsockRestorePreparationError {
    /// Returns whether preparation stopped at an explicit cancellation checkpoint.
    pub const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }
}

impl fmt::Debug for SnapshotV2VsockRestorePreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2VsockRestorePreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Selector(_) => "native-v2 vsock restore selector is invalid",
            Self::Config => "native-v2 vsock restore configuration is invalid",
            Self::ResourceId(_) | Self::ResourceIdentity => {
                "native-v2 vsock restore resource identity is invalid"
            }
            Self::DestinationTransport => {
                "native-v2 vsock restore destination transport is inconsistent"
            }
            Self::Device(_) => "native-v2 vsock restore device state is invalid",
            Self::Mmio(_) => "native-v2 vsock MMIO restore state is invalid",
            Self::PciDeviceType(_) | Self::Pci(_) => "native-v2 vsock PCI restore state is invalid",
            Self::Normalize(_) | Self::StateMismatch => {
                "native-v2 vsock restored state does not normalize"
            }
            Self::Allocation => "native-v2 vsock restore preparation allocation failed",
            Self::Cancelled { .. } => "native-v2 vsock restore preparation was cancelled",
        })
    }
}

impl std::error::Error for SnapshotV2VsockRestorePreparationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Selector(source) => Some(source),
            Self::ResourceId(source) => Some(source),
            Self::Device(source) => Some(source),
            Self::Mmio(source) => Some(source),
            Self::PciDeviceType(source) => Some(source),
            Self::Pci(source) => Some(source),
            Self::Normalize(source) => Some(source),
            Self::Config
            | Self::ResourceIdentity
            | Self::DestinationTransport
            | Self::StateMismatch
            | Self::Allocation
            | Self::Cancelled { .. } => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AllocationFailure {
    ResourceId,
    DestinationConfig,
    DeviceState,
    TransportState,
    Normalization,
}

#[derive(Clone, Copy)]
enum AllocationPolicy {
    System,
    #[cfg(test)]
    Fail(AllocationFailure),
}

impl AllocationPolicy {
    fn check(self, point: AllocationFailure) -> Result<(), SnapshotV2VsockRestorePreparationError> {
        #[cfg(test)]
        if matches!(self, Self::Fail(failure) if failure == point) {
            return Err(SnapshotV2VsockRestorePreparationError::Allocation);
        }
        #[cfg(not(test))]
        let _ = (self, point);
        Ok(())
    }

    fn copy_string(
        self,
        value: &str,
        point: AllocationFailure,
    ) -> Result<String, SnapshotV2VsockRestorePreparationError> {
        self.check(point)?;
        let mut copy = String::new();
        copy.try_reserve_exact(value.len())
            .map_err(|_| SnapshotV2VsockRestorePreparationError::Allocation)?;
        copy.push_str(value);
        Ok(copy)
    }
}

fn prepare_optional_vsock_restore_topology<C>(
    state: Option<SnapshotV2VsockState>,
    requested_override: Option<&SnapshotVsockOverride>,
    expected_transport: SnapshotV2DeviceTransportKind,
    destination_memory: &GuestMemory,
    mut is_cancelled: C,
    allocation: AllocationPolicy,
    injected_resource_key: Option<SnapshotRestoreResourceKey>,
) -> Result<Option<PreparedSnapshotV2VsockRestoreTopology>, SnapshotV2VsockRestorePreparationError>
where
    C: FnMut(SnapshotV2VsockRestorePreparationStage) -> bool,
{
    let selectors = resolve_snapshot_vsock_selectors(
        state.as_ref().map(SnapshotV2VsockState::backend_selector),
        requested_override,
    )
    .map_err(SnapshotV2VsockRestorePreparationError::Selector)?;
    let Some(state) = state else {
        return Ok(None);
    };
    let selectors = selectors.ok_or(SnapshotV2VsockRestorePreparationError::ResourceIdentity)?;

    check_cancelled(
        &mut is_cancelled,
        SnapshotV2VsockRestorePreparationStage::Resource,
    )?;
    if state.transport().kind() != expected_transport {
        return Err(SnapshotV2VsockRestorePreparationError::DestinationTransport);
    }
    let resource_key = match injected_resource_key {
        Some(resource_key) => resource_key,
        None => {
            allocation.check(AllocationFailure::ResourceId)?;
            let public_id = SnapshotRestorePublicId::try_from(NATIVE_V2_VSOCK_RESTORE_PUBLIC_ID)
                .map_err(SnapshotV2VsockRestorePreparationError::ResourceId)?;
            SnapshotRestoreResourceKey::new(
                state.device_key(),
                public_id,
                SnapshotRestoreResourceClass::VsockEndpoint,
            )
        }
    };
    validate_native_v2_vsock_resource_key(&resource_key)
        .map_err(|_| SnapshotV2VsockRestorePreparationError::ResourceIdentity)?;
    let guest_cid = u32::try_from(state.guest_cid())
        .map_err(|_| SnapshotV2VsockRestorePreparationError::Config)?;
    let destination = selectors
        .destination()
        .path()
        .to_str()
        .ok_or(SnapshotV2VsockRestorePreparationError::Config)?;
    let destination = allocation.copy_string(destination, AllocationFailure::DestinationConfig)?;
    let config = VsockConfigInput::new(guest_cid, destination)
        .validate()
        .map_err(|_| SnapshotV2VsockRestorePreparationError::Config)?;
    let request = SnapshotV2VsockRestoreResourceRequest {
        resource_key,
        selectors,
        config,
        overridden: requested_override.is_some(),
    };

    check_cancelled(
        &mut is_cancelled,
        SnapshotV2VsockRestorePreparationStage::Device,
    )?;
    let checked = restore_checked_vsock_state(&state, destination_memory, allocation)?;

    check_cancelled(
        &mut is_cancelled,
        SnapshotV2VsockRestorePreparationStage::Normalize,
    )?;
    allocation.check(AllocationFailure::Normalization)?;
    let normalized = checked
        .clone()
        .into_normalized_state()
        .map_err(SnapshotV2VsockRestorePreparationError::Normalize)?;
    if normalized != state {
        return Err(SnapshotV2VsockRestorePreparationError::StateMismatch);
    }

    check_cancelled(
        &mut is_cancelled,
        SnapshotV2VsockRestorePreparationStage::Completion,
    )?;
    Ok(Some(PreparedSnapshotV2VsockRestoreTopology {
        request,
        state: checked,
    }))
}

fn restore_checked_vsock_state(
    state: &SnapshotV2VsockState,
    destination_memory: &GuestMemory,
    allocation: AllocationPolicy,
) -> Result<PreparedSnapshotV2VsockRestoreState, SnapshotV2VsockRestorePreparationError> {
    allocation.check(AllocationFailure::DeviceState)?;
    let active_queues = state.active_queues().map(|active| {
        VirtioVsockActiveQueuesCaptureState::new(
            capture_queue(active.rx()),
            capture_queue(active.tx()),
            capture_queue(active.event()),
        )
    });
    let device = VirtioVsockDeviceCaptureState::try_from_parts(
        state.guest_cid(),
        state.virtio().available_features(),
        state.virtio().driver_features(),
        active_queues,
        state.backend_selector().clone(),
        state.host_local_port_cursor(),
    )
    .map_err(SnapshotV2VsockRestorePreparationError::Device)?;

    allocation.check(AllocationFailure::TransportState)?;
    match state.transport() {
        SnapshotV2DeviceTransport::Mmio(mmio) => {
            let retained = restore_mmio_transport_state_for_device_with_config_status_gate(
                VIRTIO_VSOCK_DEVICE_ID,
                state.virtio(),
                mmio,
                true,
            )
            .map_err(SnapshotV2VsockRestorePreparationError::Mmio)?;
            let capture =
                VirtioVsockMmioCaptureState::try_from_parts(device, retained, destination_memory)
                    .map_err(SnapshotV2VsockRestorePreparationError::Device)?;
            Ok(PreparedSnapshotV2VsockRestoreState::Mmio(
                PreparedSnapshotV2VsockMmioState {
                    region: mmio.region(),
                    interrupt_line: mmio.interrupt_line(),
                    capture,
                },
            ))
        }
        SnapshotV2DeviceTransport::Pci(pci) => {
            let device_type = VirtioDeviceType::new(VIRTIO_VSOCK_DEVICE_ID)
                .map_err(SnapshotV2VsockRestorePreparationError::PciDeviceType)?;
            let identity = VirtioPciIdentity::new(device_type, state.virtio().available_features())
                .with_config_generation(state.virtio().config_generation());
            let retained = VirtioPciTransportState::from_snapshot_v2_parts(
                identity,
                state.virtio(),
                pci,
                false,
            )
            .map_err(SnapshotV2VsockRestorePreparationError::Pci)?;
            let capture =
                VirtioVsockPciCaptureState::try_from_parts(device, retained, destination_memory)
                    .map_err(SnapshotV2VsockRestorePreparationError::Device)?;
            Ok(PreparedSnapshotV2VsockRestoreState::Pci(
                PreparedSnapshotV2VsockPciState {
                    origin: pci.origin(),
                    sbdf: pci.sbdf(),
                    bar_range: pci.bar_range(),
                    capture,
                },
            ))
        }
    }
}

fn capture_queue(state: SnapshotV2VsockQueueState) -> VirtioVsockQueueCaptureState {
    VirtioVsockQueueCaptureState::new(
        state.next_available(),
        state.next_used(),
        state.event_idx_enabled(),
    )
}

fn check_cancelled<C>(
    is_cancelled: &mut C,
    stage: SnapshotV2VsockRestorePreparationStage,
) -> Result<(), SnapshotV2VsockRestorePreparationError>
where
    C: FnMut(SnapshotV2VsockRestorePreparationStage) -> bool,
{
    if is_cancelled(stage) {
        Err(SnapshotV2VsockRestorePreparationError::Cancelled { stage })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
