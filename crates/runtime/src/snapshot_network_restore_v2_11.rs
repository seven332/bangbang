//! Host-operation-free exact-2.11 network destination preparation.

use std::fmt;

use crate::mmds::{MmdsConfig, MmdsConfigInput};
use crate::network::{NetworkInterfaceConfig, NetworkRateLimiterConfig, NetworkTokenBucketConfig};
use crate::snapshot::SnapshotNetworkOverride;
use crate::snapshot_device_v2::SnapshotV2DeviceKey;
use crate::snapshot_network_v2_11::{
    NATIVE_V2_NETWORK_MAX_CAPTURED_SELECTOR_BYTES, NATIVE_V2_NETWORK_MAX_INTERFACE_ID_BYTES,
    NATIVE_V2_NETWORK_MAX_INTERFACES, SnapshotV2MmdsInterfaceState, SnapshotV2MmdsState,
    SnapshotV2NetworkInterfaceState, SnapshotV2NetworkLimiterState, SnapshotV2NetworkState,
    SnapshotV2NetworkTokenBucketState,
};
use crate::snapshot_restore::{
    SnapshotRestorePublicId, SnapshotRestorePublicIdError, SnapshotRestoreResourceClass,
    SnapshotRestoreResourceKey,
};

const REDACTED: &str = "<redacted>";

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
#[derive(PartialEq, Eq)]
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

/// Immutable exact-2.11 network/controller/MMDS destination topology.
///
/// The value owns no descriptor, provider, packet owner, callback, metric,
/// datastore, token, session, device, platform slot, or VM authority.
#[derive(PartialEq, Eq)]
pub struct PreparedSnapshotV2NetworkRestoreTopology {
    interfaces: Vec<PreparedSnapshotV2NetworkRestoreInterface>,
    mmds_state: Option<SnapshotV2MmdsState>,
    mmds_controller: Option<MmdsConfig>,
}

impl PreparedSnapshotV2NetworkRestoreTopology {
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

fn limiter_config(limiter: SnapshotV2NetworkLimiterState) -> Option<NetworkRateLimiterConfig> {
    let configured = NetworkRateLimiterConfig::new(
        limiter.bandwidth().map(token_bucket_config),
        limiter.ops().map(token_bucket_config),
    );
    configured.is_configured().then_some(configured)
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
    use super::*;
    use crate::interrupt::GuestInterruptLine;
    use crate::memory::GuestAddress;
    use crate::mmio::{MmioRegion, MmioRegionId};
    use crate::network::{GuestMacAddress, NetworkDeviceProfile};
    use crate::snapshot_device_v2::{SnapshotV2DeviceTransport, SnapshotV2MmioDeviceState};
    use crate::snapshot_format::SnapshotFormatVersion;
    use crate::snapshot_network_v2_11::{
        SnapshotV2NetworkBackendClass, SnapshotV2NetworkInterfaceStateParts,
    };
    use crate::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE;

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
