//! Host-operation-free snapshot restore resource manifests and bindings.

use std::collections::TryReserveError;
use std::fmt;

use crate::network::MAX_NETWORK_INTERFACE_COUNT;
use crate::snapshot_device_v2::{
    NATIVE_V2_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES, NATIVE_V2_DEVICE_GRAPH_MAX_RECORDS,
    SnapshotV2DeviceGraph, SnapshotV2DeviceKey,
};
use crate::snapshot_device_v2_6::SnapshotV2StorageDeviceGraph;
use crate::snapshot_network_v2_11::SnapshotV2NetworkState;
use crate::snapshot_serial_v2_7::SnapshotV2SerialState;

/// Maximum number of logical resources in one snapshot restore transaction.
///
/// Every snapshot graph/profile that produces manifest entries must keep its
/// own maximum checked against this value. The process and backend crates
/// separately check their contained-authority and PCI topology ceilings.
pub const MAX_SNAPSHOT_RESTORE_RESOURCES: usize = 64;

/// Maximum UTF-8 byte length of one snapshot restore public identifier.
pub const MAX_SNAPSHOT_RESTORE_PUBLIC_ID_BYTES: usize = NATIVE_V2_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES;

/// Stable public identity of the singleton configured serial restore sink.
pub const NATIVE_V2_SERIAL_RESTORE_PUBLIC_ID: &str = "serial0";

/// Stable snapshot-resource identity of the singleton vsock endpoint.
///
/// This internal restore identity is not the deprecated Firecracker API
/// `vsock_id` field.
pub const NATIVE_V2_VSOCK_RESTORE_PUBLIC_ID: &str = "vsock0";

const REDACTED: &str = "<redacted>";

const _: () =
    assert!(MAX_SNAPSHOT_RESTORE_RESOURCES >= NATIVE_V2_DEVICE_GRAPH_MAX_RECORDS as usize);
const _: () = assert!(MAX_SNAPSHOT_RESTORE_RESOURCES >= MAX_NETWORK_INTERFACE_COUNT);

/// Stable public identifier used to correlate one restored resource.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SnapshotRestorePublicId {
    value: String,
}

impl SnapshotRestorePublicId {
    /// Returns the validated public identifier.
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// Fallibly copies this already validated public identifier.
    pub fn try_clone(&self) -> Result<Self, TryReserveError> {
        let mut value = String::new();
        value.try_reserve_exact(self.value.len())?;
        value.push_str(&self.value);
        Ok(Self { value })
    }
}

impl TryFrom<String> for SnapshotRestorePublicId {
    type Error = SnapshotRestorePublicIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate_public_id(&value)?;
        Ok(Self { value })
    }
}

impl TryFrom<&str> for SnapshotRestorePublicId {
    type Error = SnapshotRestorePublicIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        validate_public_id(value)?;
        let mut owned = String::new();
        owned
            .try_reserve_exact(value.len())
            .map_err(|source| SnapshotRestorePublicIdError::AllocationFailed { source })?;
        owned.push_str(value);
        Ok(Self { value: owned })
    }
}

impl fmt::Debug for SnapshotRestorePublicId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotRestorePublicId")
            .field("value", &REDACTED)
            .finish()
    }
}

/// Failure while validating or retaining one restore public identifier.
pub enum SnapshotRestorePublicIdError {
    /// The identifier is empty.
    Empty,
    /// The identifier exceeds the fixed byte maximum.
    TooLong,
    /// Identifier storage could not be allocated.
    AllocationFailed {
        /// Underlying allocation failure.
        source: TryReserveError,
    },
}

impl fmt::Debug for SnapshotRestorePublicIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "SnapshotRestorePublicIdError::Empty",
            Self::TooLong => "SnapshotRestorePublicIdError::TooLong",
            Self::AllocationFailed { .. } => "SnapshotRestorePublicIdError::AllocationFailed",
        })
    }
}

impl fmt::Display for SnapshotRestorePublicIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "snapshot restore public identifier is empty",
            Self::TooLong => "snapshot restore public identifier exceeds its maximum length",
            Self::AllocationFailed { .. } => {
                "failed to allocate snapshot restore public identifier"
            }
        })
    }
}

impl std::error::Error for SnapshotRestorePublicIdError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocationFailed { source } => Some(source),
            Self::Empty | Self::TooLong => None,
        }
    }
}

fn validate_public_id(value: &str) -> Result<(), SnapshotRestorePublicIdError> {
    if value.is_empty() {
        return Err(SnapshotRestorePublicIdError::Empty);
    }
    if value.len() > MAX_SNAPSHOT_RESTORE_PUBLIC_ID_BYTES {
        return Err(SnapshotRestorePublicIdError::TooLong);
    }
    Ok(())
}

fn checked_native_v2_restore_resource_count(
    storage_count: usize,
    serial_count: usize,
    network_count: usize,
    vsock_count: usize,
) -> Result<usize, SnapshotRestoreManifestError> {
    let resource_count = storage_count
        .checked_add(serial_count)
        .and_then(|count| count.checked_add(network_count))
        .and_then(|count| count.checked_add(vsock_count))
        .ok_or(SnapshotRestoreManifestError::TooManyResources)?;
    if resource_count > MAX_SNAPSHOT_RESTORE_RESOURCES {
        return Err(SnapshotRestoreManifestError::TooManyResources);
    }
    Ok(resource_count)
}

fn collect_native_v2_network_resources(
    graph: Option<&SnapshotV2StorageDeviceGraph>,
    serial: &SnapshotV2SerialState,
    network: Option<&SnapshotV2NetworkState>,
    additional_resource_count: usize,
    additional_override_count: usize,
) -> Result<
    (
        Vec<SnapshotRestoreResourceKey>,
        Vec<SnapshotRestoreResourceKey>,
    ),
    SnapshotRestoreManifestError,
> {
    let storage_count = graph.map_or(0, SnapshotV2StorageDeviceGraph::record_count);
    let serial_count = usize::from(serial.endpoint_intent().configured_selector().is_some());
    let network_count = network.map_or(0, |state| state.interfaces().len());
    let resource_count = checked_native_v2_restore_resource_count(
        storage_count,
        serial_count,
        network_count,
        additional_resource_count,
    )?;
    let override_count = network_count
        .checked_add(additional_override_count)
        .ok_or(SnapshotRestoreManifestError::TooManyOverrides)?;
    if override_count > MAX_SNAPSHOT_RESTORE_RESOURCES {
        return Err(SnapshotRestoreManifestError::TooManyOverrides);
    }

    let mut resources = Vec::new();
    resources
        .try_reserve_exact(resource_count)
        .map_err(|source| SnapshotRestoreManifestError::AllocationFailed { source })?;
    if let Some(graph) = graph {
        for record in graph.block_records() {
            let public_id = SnapshotRestorePublicId::try_from(record.config().drive_id())
                .map_err(|source| SnapshotRestoreManifestError::PublicId { source })?;
            resources.push(SnapshotRestoreResourceKey::new(
                record.key(),
                public_id,
                SnapshotRestoreResourceClass::BlockBacking,
            ));
        }
        for record in graph.pmem_records() {
            let public_id = SnapshotRestorePublicId::try_from(record.config().pmem_id())
                .map_err(|source| SnapshotRestoreManifestError::PublicId { source })?;
            resources.push(SnapshotRestoreResourceKey::new(
                record.key(),
                public_id,
                SnapshotRestoreResourceClass::PmemBacking,
            ));
        }
    }
    if serial_count == 1 {
        let public_id = SnapshotRestorePublicId::try_from(NATIVE_V2_SERIAL_RESTORE_PUBLIC_ID)
            .map_err(|source| SnapshotRestoreManifestError::PublicId { source })?;
        resources.push(SnapshotRestoreResourceKey::new(
            SnapshotV2DeviceKey::serial(),
            public_id,
            SnapshotRestoreResourceClass::SerialSink,
        ));
    }

    let mut overrides = Vec::new();
    overrides
        .try_reserve_exact(override_count)
        .map_err(|source| SnapshotRestoreManifestError::AllocationFailed { source })?;
    if let Some(network) = network {
        for (index, interface) in network.interfaces().iter().enumerate() {
            let instance =
                u32::try_from(index).map_err(|_| SnapshotRestoreManifestError::TooManyResources)?;
            let public_id = SnapshotRestorePublicId::try_from(interface.iface_id())
                .map_err(|source| SnapshotRestoreManifestError::PublicId { source })?;
            let key = SnapshotRestoreResourceKey::new(
                SnapshotV2DeviceKey::network(instance),
                public_id,
                SnapshotRestoreResourceClass::NetworkPacketIo,
            );
            overrides.push(key.clone());
            resources.push(key);
        }
    }
    Ok((resources, overrides))
}

pub(crate) fn validate_native_v2_vsock_resource_key(
    key: &SnapshotRestoreResourceKey,
) -> Result<(), SnapshotRestoreManifestError> {
    if key.device_key() != SnapshotV2DeviceKey::vsock() {
        return Err(SnapshotRestoreManifestError::InvalidVsockDeviceKey);
    }
    if key.resource_class() != SnapshotRestoreResourceClass::VsockEndpoint {
        return Err(SnapshotRestoreManifestError::InvalidVsockResourceClass);
    }
    if key.public_id().as_str() != NATIVE_V2_VSOCK_RESTORE_PUBLIC_ID {
        return Err(SnapshotRestoreManifestError::InvalidVsockPublicId);
    }
    Ok(())
}

/// Closed logical class of one snapshot restore resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SnapshotRestoreResourceClass {
    /// File backing for a restored virtio-block device.
    BlockBacking,
    /// File backing for a restored virtio-pmem device.
    PmemBacking,
    /// Destination output for the restored singleton serial device.
    SerialSink,
    /// Fresh packet-I/O owner for one restored network interface.
    NetworkPacketIo,
    /// Destination endpoint for a restored virtio-vsock device.
    VsockEndpoint,
}

impl SnapshotRestoreResourceClass {
    const fn accepts_override(self) -> bool {
        matches!(self, Self::VsockEndpoint | Self::NetworkPacketIo)
    }
}

/// Exact logical key of one snapshot restore resource.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SnapshotRestoreResourceKey {
    resource_class: SnapshotRestoreResourceClass,
    device_key: SnapshotV2DeviceKey,
    public_id: SnapshotRestorePublicId,
}

impl SnapshotRestoreResourceKey {
    /// Creates a key from an identity produced by validated device state.
    ///
    /// [`SnapshotV2DeviceKey`] has no public raw-parts constructor, so this
    /// operation cannot manufacture a graph record identity.
    pub const fn new(
        device_key: SnapshotV2DeviceKey,
        public_id: SnapshotRestorePublicId,
        resource_class: SnapshotRestoreResourceClass,
    ) -> Self {
        Self {
            resource_class,
            device_key,
            public_id,
        }
    }

    /// Returns the resource class.
    pub const fn resource_class(&self) -> SnapshotRestoreResourceClass {
        self.resource_class
    }

    /// Returns the validated device-graph identity.
    pub const fn device_key(&self) -> SnapshotV2DeviceKey {
        self.device_key
    }

    /// Returns the validated public identifier.
    pub const fn public_id(&self) -> &SnapshotRestorePublicId {
        &self.public_id
    }

    /// Fallibly copies this exact resource key.
    pub fn try_clone(&self) -> Result<Self, TryReserveError> {
        Ok(Self {
            resource_class: self.resource_class,
            device_key: self.device_key,
            public_id: self.public_id.try_clone()?,
        })
    }

    fn has_same_identity(&self, other: &Self) -> bool {
        self.device_key == other.device_key && self.public_id == other.public_id
    }
}

impl PartialOrd for SnapshotRestoreResourceKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SnapshotRestoreResourceKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.resource_class
            .cmp(&other.resource_class)
            .then_with(|| self.device_key.cmp(&other.device_key))
            .then_with(|| self.public_id.cmp(&other.public_id))
    }
}

impl fmt::Debug for SnapshotRestoreResourceKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotRestoreResourceKey")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Immutable canonical logical resource set for one snapshot restore.
#[derive(PartialEq, Eq)]
pub struct SnapshotRestoreManifest {
    resources: Vec<SnapshotRestoreResourceKey>,
    override_indices: Vec<usize>,
}

impl SnapshotRestoreManifest {
    /// Builds and validates one canonical logical manifest.
    ///
    /// `overrides` contains value-free exact resource keys with an explicit
    /// destination binding. A binding may deliberately name the same logical
    /// selector as captured state. The final manifest retains only canonical
    /// resource indices, never another public-ID copy.
    pub fn try_new(
        resources: Vec<SnapshotRestoreResourceKey>,
        overrides: Vec<SnapshotRestoreResourceKey>,
    ) -> Result<Self, SnapshotRestoreManifestError> {
        Self::try_new_with_reserve(resources, overrides, Vec::try_reserve_exact)
    }

    /// Derives the complete resource set for the current validated native-v2
    /// device graph.
    ///
    /// The current profile contains exactly one root block record. Future
    /// multi-record profiles must add their own complete traversal producer
    /// and checked cardinality relationship.
    pub fn try_from_native_v2_device_graph(
        graph: &SnapshotV2DeviceGraph,
        overrides: Vec<SnapshotRestoreResourceKey>,
    ) -> Result<Self, SnapshotRestoreManifestError> {
        let public_id = SnapshotRestorePublicId::try_from(graph.record().config().drive_id())
            .map_err(|source| SnapshotRestoreManifestError::PublicId { source })?;
        let key = SnapshotRestoreResourceKey::new(
            graph.root_key(),
            public_id,
            SnapshotRestoreResourceClass::BlockBacking,
        );
        let mut resources = Vec::new();
        resources
            .try_reserve_exact(NATIVE_V2_DEVICE_GRAPH_MAX_RECORDS as usize)
            .map_err(|source| SnapshotRestoreManifestError::AllocationFailed { source })?;
        resources.push(key);
        Self::try_new(resources, overrides)
    }

    /// Derives the complete resource set for one validated native-v2 2.6
    /// storage graph.
    ///
    /// Resources retain canonical graph order: block backings first, then pmem
    /// backings. [`Self::try_new`] independently canonicalizes and validates
    /// the exact class/key/public-ID identities.
    pub fn try_from_native_v2_storage_device_graph(
        graph: &SnapshotV2StorageDeviceGraph,
        overrides: Vec<SnapshotRestoreResourceKey>,
    ) -> Result<Self, SnapshotRestoreManifestError> {
        let mut resources = Vec::new();
        resources
            .try_reserve_exact(graph.record_count())
            .map_err(|source| SnapshotRestoreManifestError::AllocationFailed { source })?;
        for record in graph.block_records() {
            let public_id = SnapshotRestorePublicId::try_from(record.config().drive_id())
                .map_err(|source| SnapshotRestoreManifestError::PublicId { source })?;
            resources.push(SnapshotRestoreResourceKey::new(
                record.key(),
                public_id,
                SnapshotRestoreResourceClass::BlockBacking,
            ));
        }
        for record in graph.pmem_records() {
            let public_id = SnapshotRestorePublicId::try_from(record.config().pmem_id())
                .map_err(|source| SnapshotRestoreManifestError::PublicId { source })?;
            resources.push(SnapshotRestoreResourceKey::new(
                record.key(),
                public_id,
                SnapshotRestoreResourceClass::PmemBacking,
            ));
        }
        Self::try_new(resources, overrides)
    }

    /// Derives the complete exact-2.7 storage and configured-serial resource
    /// set.
    ///
    /// Default process stdio is destination-local and contributes no logical
    /// resource. A configured output contributes the stable singleton serial
    /// sink after every canonical block and pmem backing.
    pub fn try_from_native_v2_serial_state(
        graph: Option<&SnapshotV2StorageDeviceGraph>,
        serial: &SnapshotV2SerialState,
    ) -> Result<Self, SnapshotRestoreManifestError> {
        let storage_count = graph.map_or(0, SnapshotV2StorageDeviceGraph::record_count);
        let serial_count = usize::from(serial.endpoint_intent().configured_selector().is_some());
        let resource_count = storage_count
            .checked_add(serial_count)
            .ok_or(SnapshotRestoreManifestError::TooManyResources)?;
        if resource_count > MAX_SNAPSHOT_RESTORE_RESOURCES {
            return Err(SnapshotRestoreManifestError::TooManyResources);
        }

        let mut resources = Vec::new();
        resources
            .try_reserve_exact(resource_count)
            .map_err(|source| SnapshotRestoreManifestError::AllocationFailed { source })?;
        if let Some(graph) = graph {
            for record in graph.block_records() {
                let public_id = SnapshotRestorePublicId::try_from(record.config().drive_id())
                    .map_err(|source| SnapshotRestoreManifestError::PublicId { source })?;
                resources.push(SnapshotRestoreResourceKey::new(
                    record.key(),
                    public_id,
                    SnapshotRestoreResourceClass::BlockBacking,
                ));
            }
            for record in graph.pmem_records() {
                let public_id = SnapshotRestorePublicId::try_from(record.config().pmem_id())
                    .map_err(|source| SnapshotRestoreManifestError::PublicId { source })?;
                resources.push(SnapshotRestoreResourceKey::new(
                    record.key(),
                    public_id,
                    SnapshotRestoreResourceClass::PmemBacking,
                ));
            }
        }
        if serial_count == 1 {
            let public_id = SnapshotRestorePublicId::try_from(NATIVE_V2_SERIAL_RESTORE_PUBLIC_ID)
                .map_err(|source| SnapshotRestoreManifestError::PublicId { source })?;
            resources.push(SnapshotRestoreResourceKey::new(
                SnapshotV2DeviceKey::serial(),
                public_id,
                SnapshotRestoreResourceClass::SerialSink,
            ));
        }
        Self::try_new(resources, Vec::new())
    }

    /// Derives the complete exact-2.11 storage, configured-serial, and network
    /// resource set.
    ///
    /// Every network interface has one explicit destination binding even when
    /// the caller deliberately selects the captured logical name. Network keys
    /// use stable kind 4 and configuration-order instances. The unchanged
    /// 64-entry limit applies to the aggregate rather than to each component's
    /// independent maximum.
    pub fn try_from_native_v2_network_state(
        graph: Option<&SnapshotV2StorageDeviceGraph>,
        serial: &SnapshotV2SerialState,
        network: Option<&SnapshotV2NetworkState>,
    ) -> Result<Self, SnapshotRestoreManifestError> {
        let (resources, overrides) =
            collect_native_v2_network_resources(graph, serial, network, 0, 0)?;
        Self::try_new(resources, overrides)
    }

    /// Derives the complete exact-2.12 storage, configured-serial, network, and
    /// optional vsock resource set.
    ///
    /// The vsock request must use the validated singleton device key, endpoint
    /// class, and private stable restore identity. It contributes an override
    /// only when the caller explicitly selected a destination backend.
    pub fn try_from_native_v2_vsock_state(
        graph: Option<&SnapshotV2StorageDeviceGraph>,
        serial: &SnapshotV2SerialState,
        network: Option<&SnapshotV2NetworkState>,
        vsock: Option<(&SnapshotRestoreResourceKey, bool)>,
    ) -> Result<Self, SnapshotRestoreManifestError> {
        let vsock_count = usize::from(vsock.is_some());
        let vsock_override_count =
            usize::from(vsock.is_some_and(|(_, is_overridden)| is_overridden));
        let (mut resources, mut overrides) = collect_native_v2_network_resources(
            graph,
            serial,
            network,
            vsock_count,
            vsock_override_count,
        )?;
        if let Some((key, is_overridden)) = vsock {
            validate_native_v2_vsock_resource_key(key)?;
            let retained = key
                .try_clone()
                .map_err(|source| SnapshotRestoreManifestError::AllocationFailed { source })?;
            if is_overridden {
                overrides.push(
                    retained.try_clone().map_err(|source| {
                        SnapshotRestoreManifestError::AllocationFailed { source }
                    })?,
                );
            }
            resources.push(retained);
        }
        Self::try_new(resources, overrides)
    }

    fn try_new_with_reserve(
        mut resources: Vec<SnapshotRestoreResourceKey>,
        overrides: Vec<SnapshotRestoreResourceKey>,
        reserve: impl FnOnce(&mut Vec<usize>, usize) -> Result<(), TryReserveError>,
    ) -> Result<Self, SnapshotRestoreManifestError> {
        if resources.len() > MAX_SNAPSHOT_RESTORE_RESOURCES {
            return Err(SnapshotRestoreManifestError::TooManyResources);
        }
        if overrides.len() > MAX_SNAPSHOT_RESTORE_RESOURCES {
            return Err(SnapshotRestoreManifestError::TooManyOverrides);
        }

        for (index, resource) in resources.iter().enumerate() {
            for other in resources.iter().skip(index.saturating_add(1)) {
                if resource.has_same_identity(other) {
                    return Err(if resource.resource_class == other.resource_class {
                        SnapshotRestoreManifestError::DuplicateResource
                    } else {
                        SnapshotRestoreManifestError::ResourceClassConflict
                    });
                }
            }
        }

        resources.sort_unstable();
        let mut override_indices = Vec::new();
        reserve(&mut override_indices, overrides.len())
            .map_err(|source| SnapshotRestoreManifestError::AllocationFailed { source })?;
        for requested in overrides {
            let Some((index, resource)) = resources
                .iter()
                .enumerate()
                .find(|(_, resource)| resource.has_same_identity(&requested))
            else {
                return Err(SnapshotRestoreManifestError::UnknownOverride);
            };
            if resource.resource_class != requested.resource_class {
                return Err(SnapshotRestoreManifestError::OverrideClassMismatch);
            }
            if !resource.resource_class.accepts_override() {
                return Err(SnapshotRestoreManifestError::UnsupportedOverrideClass);
            }
            if override_indices.contains(&index) {
                return Err(SnapshotRestoreManifestError::DuplicateOverride);
            }
            override_indices.push(index);
        }
        override_indices.sort_unstable();

        Ok(Self {
            resources,
            override_indices,
        })
    }

    /// Returns the number of required resources.
    pub fn len(&self) -> usize {
        self.resources.len()
    }

    /// Returns whether no resource is required.
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }

    /// Returns resources in canonical class/key order.
    pub fn resources(&self) -> &[SnapshotRestoreResourceKey] {
        &self.resources
    }

    /// Iterates normalized override members in canonical resource order.
    pub fn overrides(&self) -> impl DoubleEndedIterator<Item = &SnapshotRestoreResourceKey> + '_ {
        self.override_indices
            .iter()
            .filter_map(|index| self.resources.get(*index))
    }

    /// Returns whether the exact resource has a normalized override.
    pub fn is_overridden(&self, key: &SnapshotRestoreResourceKey) -> bool {
        self.resources
            .binary_search(key)
            .ok()
            .is_some_and(|index| self.override_indices.binary_search(&index).is_ok())
    }

    /// Consumes the manifest into an empty exact binding collection.
    pub fn try_into_bindings<T>(
        self,
    ) -> Result<SnapshotRestoreBindings<T>, SnapshotRestoreBindingAllocationError> {
        SnapshotRestoreBindings::try_from_manifest_with_reserve(self, Vec::try_reserve_exact)
    }
}

impl fmt::Debug for SnapshotRestoreManifest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotRestoreManifest")
            .field("resource_count", &self.resources.len())
            .field("override_count", &self.override_indices.len())
            .field("values", &REDACTED)
            .finish()
    }
}

/// Failure while constructing one logical restore manifest.
pub enum SnapshotRestoreManifestError {
    /// The resource count exceeds the fixed manifest maximum.
    TooManyResources,
    /// The override count exceeds the fixed manifest maximum.
    TooManyOverrides,
    /// The exact same resource occurs more than once.
    DuplicateResource,
    /// One device/public identity occurs under multiple resource classes.
    ResourceClassConflict,
    /// An override does not target any manifest identity.
    UnknownOverride,
    /// An override identity exists under a different resource class.
    OverrideClassMismatch,
    /// The target resource class does not support overrides.
    UnsupportedOverrideClass,
    /// The same exact override occurs more than once.
    DuplicateOverride,
    /// The exact-2.12 vsock request uses the wrong device-graph key.
    InvalidVsockDeviceKey,
    /// The exact-2.12 vsock request uses the wrong resource class.
    InvalidVsockResourceClass,
    /// The exact-2.12 vsock request uses the wrong private restore identity.
    InvalidVsockPublicId,
    /// A graph-derived public identifier could not be retained.
    PublicId {
        /// Public-ID validation or allocation failure.
        source: SnapshotRestorePublicIdError,
    },
    /// Manifest storage could not be allocated.
    AllocationFailed {
        /// Underlying allocation failure.
        source: TryReserveError,
    },
}

impl fmt::Debug for SnapshotRestoreManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooManyResources => "SnapshotRestoreManifestError::TooManyResources",
            Self::TooManyOverrides => "SnapshotRestoreManifestError::TooManyOverrides",
            Self::DuplicateResource => "SnapshotRestoreManifestError::DuplicateResource",
            Self::ResourceClassConflict => "SnapshotRestoreManifestError::ResourceClassConflict",
            Self::UnknownOverride => "SnapshotRestoreManifestError::UnknownOverride",
            Self::OverrideClassMismatch => "SnapshotRestoreManifestError::OverrideClassMismatch",
            Self::UnsupportedOverrideClass => {
                "SnapshotRestoreManifestError::UnsupportedOverrideClass"
            }
            Self::DuplicateOverride => "SnapshotRestoreManifestError::DuplicateOverride",
            Self::InvalidVsockDeviceKey => "SnapshotRestoreManifestError::InvalidVsockDeviceKey",
            Self::InvalidVsockResourceClass => {
                "SnapshotRestoreManifestError::InvalidVsockResourceClass"
            }
            Self::InvalidVsockPublicId => "SnapshotRestoreManifestError::InvalidVsockPublicId",
            Self::PublicId { .. } => "SnapshotRestoreManifestError::PublicId",
            Self::AllocationFailed { .. } => "SnapshotRestoreManifestError::AllocationFailed",
        })
    }
}

impl fmt::Display for SnapshotRestoreManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooManyResources => "snapshot restore manifest has too many resources",
            Self::TooManyOverrides => "snapshot restore manifest has too many overrides",
            Self::DuplicateResource => "snapshot restore manifest contains a duplicate resource",
            Self::ResourceClassConflict => "snapshot restore manifest changes a resource class",
            Self::UnknownOverride => "snapshot restore override targets an unknown resource",
            Self::OverrideClassMismatch => {
                "snapshot restore override targets the wrong resource class"
            }
            Self::UnsupportedOverrideClass => {
                "snapshot restore resource class does not support an override"
            }
            Self::DuplicateOverride => "snapshot restore manifest contains a duplicate override",
            Self::InvalidVsockDeviceKey => {
                "snapshot restore vsock resource uses the wrong device identity"
            }
            Self::InvalidVsockResourceClass => {
                "snapshot restore vsock resource uses the wrong class"
            }
            Self::InvalidVsockPublicId => {
                "snapshot restore vsock resource uses the wrong private identity"
            }
            Self::PublicId { .. } => "snapshot restore manifest has an invalid public identifier",
            Self::AllocationFailed { .. } => "failed to allocate snapshot restore manifest storage",
        })
    }
}

impl std::error::Error for SnapshotRestoreManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PublicId { source } => Some(source),
            Self::AllocationFailed { source } => Some(source),
            Self::TooManyResources
            | Self::TooManyOverrides
            | Self::DuplicateResource
            | Self::ResourceClassConflict
            | Self::UnknownOverride
            | Self::OverrideClassMismatch
            | Self::UnsupportedOverrideClass
            | Self::DuplicateOverride
            | Self::InvalidVsockDeviceKey
            | Self::InvalidVsockResourceClass
            | Self::InvalidVsockPublicId => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceLookupError {
    Unknown,
    WrongClass,
}

fn resource_index(
    resources: &[SnapshotRestoreResourceKey],
    key: &SnapshotRestoreResourceKey,
) -> Result<usize, ResourceLookupError> {
    match resources.binary_search(key) {
        Ok(index) => Ok(index),
        Err(_) if resources.iter().any(|entry| entry.has_same_identity(key)) => {
            Err(ResourceLookupError::WrongClass)
        }
        Err(_) => Err(ResourceLookupError::Unknown),
    }
}

/// Allocation failure before any restore owner can be bound.
pub struct SnapshotRestoreBindingAllocationError {
    source: TryReserveError,
}

impl fmt::Debug for SnapshotRestoreBindingAllocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SnapshotRestoreBindingAllocationError")
    }
}

impl fmt::Display for SnapshotRestoreBindingAllocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("failed to allocate snapshot restore binding storage")
    }
}

impl std::error::Error for SnapshotRestoreBindingAllocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Reason one prepared value was rejected by an incomplete binding set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotRestoreBindingRejectionReason {
    /// The value does not belong to the manifest.
    ExtraBinding,
    /// The device/public identity belongs to another resource class.
    WrongClass,
    /// The exact resource already has a bound value.
    DuplicateBinding,
}

impl fmt::Display for SnapshotRestoreBindingRejectionReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ExtraBinding => "snapshot restore binding is extra",
            Self::WrongClass => "snapshot restore binding has the wrong resource class",
            Self::DuplicateBinding => "snapshot restore resource is already bound",
        })
    }
}

/// Rejected binding that retains the untouched prepared value.
pub struct SnapshotRestoreBindingRejection<T> {
    reason: SnapshotRestoreBindingRejectionReason,
    value: T,
}

impl<T> SnapshotRestoreBindingRejection<T> {
    /// Returns the stable rejection category.
    pub const fn reason(&self) -> SnapshotRestoreBindingRejectionReason {
        self.reason
    }

    /// Returns ownership of the rejected value.
    pub fn into_value(self) -> T {
        self.value
    }
}

impl<T> fmt::Debug for SnapshotRestoreBindingRejection<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotRestoreBindingRejection")
            .field("reason", &self.reason)
            .field("value", &REDACTED)
            .finish()
    }
}

impl<T> fmt::Display for SnapshotRestoreBindingRejection<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.reason.fmt(formatter)
    }
}

impl<T> std::error::Error for SnapshotRestoreBindingRejection<T> {}

/// Incomplete exact bindings for one canonical restore manifest.
pub struct SnapshotRestoreBindings<T> {
    manifest: SnapshotRestoreManifest,
    values: Vec<Option<T>>,
    bound_count: usize,
}

impl<T> SnapshotRestoreBindings<T> {
    fn try_from_manifest_with_reserve(
        manifest: SnapshotRestoreManifest,
        reserve: impl FnOnce(&mut Vec<Option<T>>, usize) -> Result<(), TryReserveError>,
    ) -> Result<Self, SnapshotRestoreBindingAllocationError> {
        let mut values = Vec::new();
        reserve(&mut values, manifest.len())
            .map_err(|source| SnapshotRestoreBindingAllocationError { source })?;
        values.resize_with(manifest.len(), || None);
        Ok(Self {
            manifest,
            values,
            bound_count: 0,
        })
    }

    /// Returns the canonical logical manifest.
    pub const fn manifest(&self) -> &SnapshotRestoreManifest {
        &self.manifest
    }

    /// Returns how many required values remain unbound.
    pub fn missing_count(&self) -> usize {
        self.manifest.len().saturating_sub(self.bound_count)
    }

    /// Binds one prepared value to its exact manifest key.
    ///
    /// A rejected value is returned intact in the error.
    pub fn bind(
        &mut self,
        key: &SnapshotRestoreResourceKey,
        value: T,
    ) -> Result<(), SnapshotRestoreBindingRejection<T>> {
        let index = match resource_index(self.manifest.resources(), key) {
            Ok(index) => index,
            Err(ResourceLookupError::Unknown) => {
                return Err(SnapshotRestoreBindingRejection {
                    reason: SnapshotRestoreBindingRejectionReason::ExtraBinding,
                    value,
                });
            }
            Err(ResourceLookupError::WrongClass) => {
                return Err(SnapshotRestoreBindingRejection {
                    reason: SnapshotRestoreBindingRejectionReason::WrongClass,
                    value,
                });
            }
        };
        let Some(slot) = self.values.get_mut(index) else {
            return Err(SnapshotRestoreBindingRejection {
                reason: SnapshotRestoreBindingRejectionReason::ExtraBinding,
                value,
            });
        };
        if slot.is_some() {
            return Err(SnapshotRestoreBindingRejection {
                reason: SnapshotRestoreBindingRejectionReason::DuplicateBinding,
                value,
            });
        }
        *slot = Some(value);
        self.bound_count = self.bound_count.saturating_add(1);
        Ok(())
    }

    /// Requires every manifest resource to have one exact bound value.
    pub fn complete(
        self,
    ) -> Result<PreparedSnapshotRestoreBindings<T>, SnapshotRestoreIncompleteBindings<T>> {
        let missing_count = self.missing_count();
        if missing_count != 0 {
            return Err(SnapshotRestoreIncompleteBindings {
                missing_count,
                bindings: self,
            });
        }
        let Self {
            manifest,
            values,
            bound_count,
        } = self;
        Ok(PreparedSnapshotRestoreBindings {
            manifest,
            values,
            remaining_count: bound_count,
        })
    }

    /// Consumes the collection and iterates all retained values canonically.
    ///
    /// Reversing this iterator gives deterministic reverse-order abort.
    pub fn into_values(self) -> impl DoubleEndedIterator<Item = T> {
        self.values.into_iter().flatten()
    }
}

impl<T> fmt::Debug for SnapshotRestoreBindings<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotRestoreBindings")
            .field("resource_count", &self.manifest.len())
            .field("bound_count", &self.bound_count)
            .field("values", &REDACTED)
            .finish()
    }
}

/// Incomplete binding transition that retains all already-bound values.
pub struct SnapshotRestoreIncompleteBindings<T> {
    missing_count: usize,
    bindings: SnapshotRestoreBindings<T>,
}

impl<T> SnapshotRestoreIncompleteBindings<T> {
    /// Returns how many required resources remain unbound.
    pub const fn missing_count(&self) -> usize {
        self.missing_count
    }

    /// Returns the incomplete collection for explicit abort or further work.
    pub fn into_bindings(self) -> SnapshotRestoreBindings<T> {
        self.bindings
    }
}

impl<T> fmt::Debug for SnapshotRestoreIncompleteBindings<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotRestoreIncompleteBindings")
            .field("missing_count", &self.missing_count)
            .field("values", &REDACTED)
            .finish()
    }
}

impl<T> fmt::Display for SnapshotRestoreIncompleteBindings<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("snapshot restore binding set is incomplete")
    }
}

impl<T> std::error::Error for SnapshotRestoreIncompleteBindings<T> {}

/// Failure while taking one value from a complete binding set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotRestoreTakeError {
    /// The requested identity is not part of the manifest.
    UnknownResource,
    /// The identity exists under another resource class.
    WrongClass,
    /// The exact value was already taken.
    AlreadyTaken,
}

impl fmt::Display for SnapshotRestoreTakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnknownResource => "snapshot restore resource is unknown",
            Self::WrongClass => "snapshot restore resource has the wrong class",
            Self::AlreadyTaken => "snapshot restore resource was already taken",
        })
    }
}

impl std::error::Error for SnapshotRestoreTakeError {}

/// Complete exact bindings ready for one-time consumer takes.
pub struct PreparedSnapshotRestoreBindings<T> {
    manifest: SnapshotRestoreManifest,
    values: Vec<Option<T>>,
    remaining_count: usize,
}

impl<T> PreparedSnapshotRestoreBindings<T> {
    /// Returns the canonical logical manifest.
    pub const fn manifest(&self) -> &SnapshotRestoreManifest {
        &self.manifest
    }

    /// Returns how many prepared values remain unconsumed.
    pub const fn remaining_count(&self) -> usize {
        self.remaining_count
    }

    /// Takes the exact prepared value once.
    pub fn take(
        &mut self,
        key: &SnapshotRestoreResourceKey,
    ) -> Result<T, SnapshotRestoreTakeError> {
        let index = match resource_index(self.manifest.resources(), key) {
            Ok(index) => index,
            Err(ResourceLookupError::Unknown) => {
                return Err(SnapshotRestoreTakeError::UnknownResource);
            }
            Err(ResourceLookupError::WrongClass) => {
                return Err(SnapshotRestoreTakeError::WrongClass);
            }
        };
        let value = self
            .values
            .get_mut(index)
            .and_then(Option::take)
            .ok_or(SnapshotRestoreTakeError::AlreadyTaken)?;
        self.remaining_count = self.remaining_count.saturating_sub(1);
        Ok(value)
    }

    /// Requires every prepared value to have been taken.
    pub fn finish(self) -> Result<(), SnapshotRestoreUnconsumedBindings<T>> {
        if self.remaining_count == 0 {
            Ok(())
        } else {
            Err(SnapshotRestoreUnconsumedBindings {
                unconsumed_count: self.remaining_count,
                bindings: self,
            })
        }
    }

    /// Consumes the collection and iterates retained values canonically.
    ///
    /// Reversing this iterator gives deterministic reverse-order abort.
    pub fn into_values(self) -> impl DoubleEndedIterator<Item = T> {
        self.values.into_iter().flatten()
    }
}

impl<T> fmt::Debug for PreparedSnapshotRestoreBindings<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotRestoreBindings")
            .field("resource_count", &self.manifest.len())
            .field("remaining_count", &self.remaining_count)
            .field("values", &REDACTED)
            .finish()
    }
}

/// Failed finish that retains every unconsumed prepared value.
pub struct SnapshotRestoreUnconsumedBindings<T> {
    unconsumed_count: usize,
    bindings: PreparedSnapshotRestoreBindings<T>,
}

impl<T> SnapshotRestoreUnconsumedBindings<T> {
    /// Returns how many prepared values remain unconsumed.
    pub const fn unconsumed_count(&self) -> usize {
        self.unconsumed_count
    }

    /// Returns the prepared collection for explicit abort or further work.
    pub fn into_bindings(self) -> PreparedSnapshotRestoreBindings<T> {
        self.bindings
    }
}

impl<T> fmt::Debug for SnapshotRestoreUnconsumedBindings<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotRestoreUnconsumedBindings")
            .field("unconsumed_count", &self.unconsumed_count)
            .field("values", &REDACTED)
            .finish()
    }
}

impl<T> fmt::Display for SnapshotRestoreUnconsumedBindings<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("snapshot restore binding set has unconsumed values")
    }
}

impl<T> std::error::Error for SnapshotRestoreUnconsumedBindings<T> {}

#[cfg(test)]
mod tests {
    use std::collections::TryReserveError;
    use std::fmt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::snapshot_device_v2::snapshot_v2_device_key_for_test;
    use crate::snapshot_device_v2_6::NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION;
    use crate::snapshot_network_v2_11::NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION;
    use crate::snapshot_serial_v2_7::NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION;

    const DEFAULT_SERIAL_HEX: &str = include_str!("snapshot_serial_v2_7/fixtures/default.hex");
    const CONFIGURED_SERIAL_HEX: &str =
        include_str!("snapshot_serial_v2_7/fixtures/configured.hex");
    const MIXED_STORAGE_HEX: &str =
        include_str!("snapshot_device_v2_6/fixtures/mixed-pmem-root-pci.hex");
    const NETWORK_HEX: &str = include_str!("snapshot_network_v2_11/fixtures/inactive-mmio.hex");

    fn public_id(value: impl Into<String>) -> SnapshotRestorePublicId {
        SnapshotRestorePublicId::try_from(value.into())
            .expect("test public identifier should validate")
    }

    fn key(
        kind: u32,
        instance: u32,
        id: impl Into<String>,
        resource_class: SnapshotRestoreResourceClass,
    ) -> SnapshotRestoreResourceKey {
        SnapshotRestoreResourceKey::new(
            snapshot_v2_device_key_for_test(kind, instance),
            public_id(id),
            resource_class,
        )
    }

    fn allocation_error() -> TryReserveError {
        Vec::<u8>::new()
            .try_reserve_exact(usize::MAX)
            .expect_err("impossible allocation should fail")
    }

    fn decode_hex(value: &str) -> Vec<u8> {
        let value = value.split_whitespace().collect::<String>();
        assert!(value.len().is_multiple_of(2));
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                u8::from_str_radix(
                    std::str::from_utf8(pair).expect("hex pair should be UTF-8"),
                    16,
                )
                .expect("fixture should be hexadecimal")
            })
            .collect()
    }

    fn serial_state(configured: bool) -> SnapshotV2SerialState {
        SnapshotV2SerialState::decode(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            &decode_hex(if configured {
                CONFIGURED_SERIAL_HEX
            } else {
                DEFAULT_SERIAL_HEX
            }),
        )
        .expect("serial fixture should decode")
    }

    fn storage_graph() -> SnapshotV2StorageDeviceGraph {
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &decode_hex(MIXED_STORAGE_HEX),
        )
        .expect("storage fixture should decode")
    }

    fn network_state() -> SnapshotV2NetworkState {
        SnapshotV2NetworkState::decode(
            NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION,
            &decode_hex(NETWORK_HEX),
        )
        .expect("network fixture should decode")
    }

    fn maximum_keys() -> Vec<SnapshotRestoreResourceKey> {
        (0..MAX_SNAPSHOT_RESTORE_RESOURCES)
            .map(|index| {
                key(
                    u32::try_from(index.saturating_add(1))
                        .expect("bounded test kind should fit u32"),
                    0,
                    format!("device{index}"),
                    SnapshotRestoreResourceClass::BlockBacking,
                )
            })
            .collect()
    }

    fn maximum_storage_keys() -> Vec<SnapshotRestoreResourceKey> {
        (0..MAX_SNAPSHOT_RESTORE_RESOURCES)
            .map(|index| {
                key(
                    if index.is_multiple_of(2) { 1 } else { 4 },
                    u32::try_from(index / 2).expect("bounded storage instance should fit"),
                    format!("storage{index}"),
                    if index.is_multiple_of(2) {
                        SnapshotRestoreResourceClass::BlockBacking
                    } else {
                        SnapshotRestoreResourceClass::PmemBacking
                    },
                )
            })
            .collect()
    }

    #[test]
    fn public_ids_use_nonempty_utf8_byte_bounds_and_redacted_debug() {
        assert!(matches!(
            SnapshotRestorePublicId::try_from(String::new()),
            Err(SnapshotRestorePublicIdError::Empty)
        ));
        let maximum = "a".repeat(MAX_SNAPSHOT_RESTORE_PUBLIC_ID_BYTES);
        let id = SnapshotRestorePublicId::try_from(maximum.clone())
            .expect("maximum public identifier should validate");
        assert_eq!(id.as_str(), maximum);
        assert!(!format!("{id:?}").contains(&maximum));

        let maximum_unicode = format!("{}a", "é".repeat(127));
        assert_eq!(maximum_unicode.len(), MAX_SNAPSHOT_RESTORE_PUBLIC_ID_BYTES);
        assert!(SnapshotRestorePublicId::try_from(maximum_unicode).is_ok());
        assert!(matches!(
            SnapshotRestorePublicId::try_from("é".repeat(128)),
            Err(SnapshotRestorePublicIdError::TooLong)
        ));
        assert!(matches!(
            SnapshotRestorePublicId::try_from(
                "a".repeat(MAX_SNAPSHOT_RESTORE_PUBLIC_ID_BYTES.saturating_add(1))
            ),
            Err(SnapshotRestorePublicIdError::TooLong)
        ));
    }

    #[test]
    fn manifest_accepts_empty_maximum_and_rejects_one_over() {
        let empty = SnapshotRestoreManifest::try_new(Vec::new(), Vec::new())
            .expect("empty manifest should validate");
        assert!(empty.is_empty());

        let maximum = SnapshotRestoreManifest::try_new(maximum_keys(), Vec::new())
            .expect("maximum manifest should validate");
        assert_eq!(maximum.len(), MAX_SNAPSHOT_RESTORE_RESOURCES);

        let maximum_storage = SnapshotRestoreManifest::try_new(maximum_storage_keys(), Vec::new())
            .expect("maximum mixed storage manifest should validate");
        assert_eq!(maximum_storage.len(), MAX_SNAPSHOT_RESTORE_RESOURCES);
        assert_eq!(
            maximum_storage
                .resources()
                .iter()
                .filter(|key| {
                    key.resource_class() == SnapshotRestoreResourceClass::BlockBacking
                })
                .count(),
            MAX_SNAPSHOT_RESTORE_RESOURCES / 2
        );
        assert_eq!(
            maximum_storage
                .resources()
                .iter()
                .filter(|key| { key.resource_class() == SnapshotRestoreResourceClass::PmemBacking })
                .count(),
            MAX_SNAPSHOT_RESTORE_RESOURCES / 2
        );

        let mut one_over = maximum_keys();
        one_over.push(key(
            1000,
            0,
            "one-over",
            SnapshotRestoreResourceClass::BlockBacking,
        ));
        assert!(matches!(
            SnapshotRestoreManifest::try_new(one_over, Vec::new()),
            Err(SnapshotRestoreManifestError::TooManyResources)
        ));
    }

    #[test]
    fn manifest_canonicalizes_order_and_rejects_identity_conflicts() {
        let block_later = key(2, 0, "z", SnapshotRestoreResourceClass::BlockBacking);
        let block_earlier = key(1, 1, "b", SnapshotRestoreResourceClass::BlockBacking);
        let pmem = key(4, 0, "p", SnapshotRestoreResourceClass::PmemBacking);
        let vsock = key(0, 0, "a", SnapshotRestoreResourceClass::VsockEndpoint);
        let manifest = SnapshotRestoreManifest::try_new(
            vec![
                vsock.clone(),
                pmem.clone(),
                block_later.clone(),
                block_earlier.clone(),
            ],
            Vec::new(),
        )
        .expect("reordered manifest should validate");
        let observed = manifest
            .resources()
            .iter()
            .map(|resource| {
                (
                    resource.resource_class(),
                    resource.device_key().kind(),
                    resource.device_key().instance(),
                    resource.public_id().as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            [
                (SnapshotRestoreResourceClass::BlockBacking, 1, 1, "b"),
                (SnapshotRestoreResourceClass::BlockBacking, 2, 0, "z"),
                (SnapshotRestoreResourceClass::PmemBacking, 4, 0, "p"),
                (SnapshotRestoreResourceClass::VsockEndpoint, 0, 0, "a"),
            ]
        );

        assert!(matches!(
            SnapshotRestoreManifest::try_new(
                vec![block_earlier.clone(), block_earlier.clone()],
                Vec::new()
            ),
            Err(SnapshotRestoreManifestError::DuplicateResource)
        ));
        let wrong_class = SnapshotRestoreResourceKey::new(
            block_later.device_key(),
            block_later.public_id().clone(),
            SnapshotRestoreResourceClass::PmemBacking,
        );
        assert!(matches!(
            SnapshotRestoreManifest::try_new(vec![block_later, wrong_class], Vec::new()),
            Err(SnapshotRestoreManifestError::ResourceClassConflict)
        ));
    }

    #[test]
    fn exact_2_7_manifest_adds_only_the_stable_configured_serial_sink() {
        let default =
            SnapshotRestoreManifest::try_from_native_v2_serial_state(None, &serial_state(false))
                .expect("default serial manifest should validate");
        assert!(default.is_empty());

        let configured =
            SnapshotRestoreManifest::try_from_native_v2_serial_state(None, &serial_state(true))
                .expect("configured serial manifest should validate");
        assert_eq!(configured.len(), 1);
        let serial = &configured.resources()[0];
        assert_eq!(
            serial.resource_class(),
            SnapshotRestoreResourceClass::SerialSink
        );
        assert_eq!(serial.device_key().kind(), 3);
        assert_eq!(serial.device_key().instance(), 0);
        assert_eq!(
            serial.public_id().as_str(),
            NATIVE_V2_SERIAL_RESTORE_PUBLIC_ID
        );
        assert!(matches!(
            SnapshotRestoreManifest::try_new(vec![serial.clone()], vec![serial.clone()]),
            Err(SnapshotRestoreManifestError::UnsupportedOverrideClass)
        ));
    }

    #[test]
    fn exact_2_7_manifest_keeps_storage_canonical_and_serial_last() {
        let graph = storage_graph();
        let storage = SnapshotRestoreManifest::try_from_native_v2_serial_state(
            Some(&graph),
            &serial_state(false),
        )
        .expect("storage default-serial manifest should validate");
        assert_eq!(storage.len(), graph.record_count());
        assert!(storage.resources().iter().all(|resource| {
            matches!(
                resource.resource_class(),
                SnapshotRestoreResourceClass::BlockBacking
                    | SnapshotRestoreResourceClass::PmemBacking
            )
        }));

        let configured = SnapshotRestoreManifest::try_from_native_v2_serial_state(
            Some(&graph),
            &serial_state(true),
        )
        .expect("storage configured-serial manifest should validate");
        assert_eq!(configured.len(), graph.record_count() + 1);
        assert_eq!(
            &configured.resources()[..graph.record_count()],
            storage.resources()
        );
        assert_eq!(
            configured
                .resources()
                .last()
                .expect("serial sink should be last")
                .resource_class(),
            SnapshotRestoreResourceClass::SerialSink
        );
    }

    #[test]
    fn exact_2_11_manifest_closes_storage_serial_and_network_resources() {
        let graph = storage_graph();
        let serial = serial_state(true);
        let network = network_state();
        let manifest = SnapshotRestoreManifest::try_from_native_v2_network_state(
            Some(&graph),
            &serial,
            Some(&network),
        )
        .expect("mixed exact-2.11 manifest should validate");

        assert_eq!(
            manifest.len(),
            graph.record_count() + 1 + network.interfaces().len()
        );
        assert_eq!(
            manifest
                .resources()
                .iter()
                .filter(|resource| {
                    matches!(
                        resource.resource_class(),
                        SnapshotRestoreResourceClass::BlockBacking
                            | SnapshotRestoreResourceClass::PmemBacking
                    )
                })
                .count(),
            graph.record_count()
        );
        let serial_resource = manifest
            .resources()
            .iter()
            .find(|resource| resource.resource_class() == SnapshotRestoreResourceClass::SerialSink)
            .expect("configured serial resource should exist");
        assert_eq!(
            serial_resource.public_id().as_str(),
            NATIVE_V2_SERIAL_RESTORE_PUBLIC_ID
        );

        let network_resources = manifest
            .resources()
            .iter()
            .filter(|resource| {
                resource.resource_class() == SnapshotRestoreResourceClass::NetworkPacketIo
            })
            .collect::<Vec<_>>();
        assert_eq!(network_resources.len(), network.interfaces().len());
        assert_eq!(manifest.overrides().collect::<Vec<_>>(), network_resources);
        for (index, (resource, interface)) in network_resources
            .iter()
            .zip(network.interfaces())
            .enumerate()
        {
            assert_eq!(resource.device_key().kind(), 4);
            assert_eq!(
                resource.device_key().instance(),
                u32::try_from(index).unwrap()
            );
            assert_eq!(resource.public_id().as_str(), interface.iface_id());
            assert!(manifest.is_overridden(resource));
        }

        let without_network =
            SnapshotRestoreManifest::try_from_native_v2_network_state(Some(&graph), &serial, None)
                .expect("network-free exact-2.11 manifest should validate");
        let exact_2_7 =
            SnapshotRestoreManifest::try_from_native_v2_serial_state(Some(&graph), &serial)
                .expect("equivalent exact-2.7 manifest should validate");
        assert_eq!(without_network.resources(), exact_2_7.resources());
        assert_eq!(without_network.overrides().count(), 0);
    }

    #[test]
    fn exact_2_11_manifest_applies_one_aggregate_resource_ceiling() {
        assert_eq!(
            checked_native_v2_restore_resource_count(48, 0, 16, 0)
                .expect("storage plus network at the aggregate ceiling should fit"),
            MAX_SNAPSHOT_RESTORE_RESOURCES
        );
        assert_eq!(
            checked_native_v2_restore_resource_count(47, 1, 16, 0)
                .expect("storage, serial, and network at the aggregate ceiling should fit"),
            MAX_SNAPSHOT_RESTORE_RESOURCES
        );
        assert!(matches!(
            checked_native_v2_restore_resource_count(48, 1, 16, 0),
            Err(SnapshotRestoreManifestError::TooManyResources)
        ));
        assert!(matches!(
            checked_native_v2_restore_resource_count(usize::MAX, 1, 1, 0),
            Err(SnapshotRestoreManifestError::TooManyResources)
        ));
    }

    #[test]
    fn exact_2_12_manifest_adds_one_canonical_optional_vsock_request() {
        let graph = storage_graph();
        let serial = serial_state(true);
        let network = network_state();
        let vsock = key(
            5,
            0,
            NATIVE_V2_VSOCK_RESTORE_PUBLIC_ID,
            SnapshotRestoreResourceClass::VsockEndpoint,
        );
        let without_override = SnapshotRestoreManifest::try_from_native_v2_vsock_state(
            Some(&graph),
            &serial,
            Some(&network),
            Some((&vsock, false)),
        )
        .expect("captured-selector vsock manifest should validate");
        assert_eq!(
            without_override.len(),
            graph.record_count() + 1 + network.interfaces().len() + 1
        );
        assert!(!without_override.is_overridden(&vsock));
        assert_eq!(
            without_override
                .resources()
                .iter()
                .filter(|key| key.resource_class() == SnapshotRestoreResourceClass::VsockEndpoint)
                .count(),
            1
        );
        let classes = without_override
            .resources()
            .iter()
            .map(SnapshotRestoreResourceKey::resource_class)
            .collect::<Vec<_>>();
        let serial_index = classes
            .iter()
            .position(|class| *class == SnapshotRestoreResourceClass::SerialSink)
            .expect("configured serial should be present");
        let network_index = classes
            .iter()
            .position(|class| *class == SnapshotRestoreResourceClass::NetworkPacketIo)
            .expect("network should be present");
        let vsock_index = classes
            .iter()
            .position(|class| *class == SnapshotRestoreResourceClass::VsockEndpoint)
            .expect("vsock should be present");
        assert!(
            serial_index < network_index && network_index < vsock_index,
            "exact-2.12 resources must retain storage/serial/network/vsock order"
        );
        assert_eq!(
            classes.last(),
            Some(&SnapshotRestoreResourceClass::VsockEndpoint)
        );

        let overridden = SnapshotRestoreManifest::try_from_native_v2_vsock_state(
            Some(&graph),
            &serial,
            Some(&network),
            Some((&vsock, true)),
        )
        .expect("overridden vsock manifest should validate");
        assert!(overridden.is_overridden(&vsock));
        assert_eq!(
            overridden.overrides().count(),
            network.interfaces().len() + 1
        );

        let without_vsock = SnapshotRestoreManifest::try_from_native_v2_vsock_state(
            Some(&graph),
            &serial,
            Some(&network),
            None,
        )
        .expect("vsock-free exact-2.12 manifest should validate");
        let exact_2_11 = SnapshotRestoreManifest::try_from_native_v2_network_state(
            Some(&graph),
            &serial,
            Some(&network),
        )
        .expect("equivalent exact-2.11 manifest should validate");
        assert_eq!(without_vsock.resources(), exact_2_11.resources());
        assert_eq!(
            without_vsock.overrides().collect::<Vec<_>>(),
            exact_2_11.overrides().collect::<Vec<_>>()
        );
    }

    #[test]
    fn exact_2_12_manifest_rejects_wrong_vsock_identity_and_bounds_product() {
        let serial = serial_state(false);
        let wrong_device = key(
            4,
            0,
            NATIVE_V2_VSOCK_RESTORE_PUBLIC_ID,
            SnapshotRestoreResourceClass::VsockEndpoint,
        );
        assert!(matches!(
            SnapshotRestoreManifest::try_from_native_v2_vsock_state(
                None,
                &serial,
                None,
                Some((&wrong_device, false)),
            ),
            Err(SnapshotRestoreManifestError::InvalidVsockDeviceKey)
        ));

        let wrong_class = key(
            5,
            0,
            NATIVE_V2_VSOCK_RESTORE_PUBLIC_ID,
            SnapshotRestoreResourceClass::NetworkPacketIo,
        );
        assert!(matches!(
            SnapshotRestoreManifest::try_from_native_v2_vsock_state(
                None,
                &serial,
                None,
                Some((&wrong_class, false)),
            ),
            Err(SnapshotRestoreManifestError::InvalidVsockResourceClass)
        ));

        let wrong_public_id = key(
            5,
            0,
            "private-vsock",
            SnapshotRestoreResourceClass::VsockEndpoint,
        );
        assert!(matches!(
            SnapshotRestoreManifest::try_from_native_v2_vsock_state(
                None,
                &serial,
                None,
                Some((&wrong_public_id, false)),
            ),
            Err(SnapshotRestoreManifestError::InvalidVsockPublicId)
        ));

        assert_eq!(
            checked_native_v2_restore_resource_count(46, 1, 16, 1)
                .expect("complete exact-2.12 product at the aggregate ceiling should fit"),
            MAX_SNAPSHOT_RESTORE_RESOURCES
        );
        assert!(matches!(
            checked_native_v2_restore_resource_count(47, 1, 16, 1),
            Err(SnapshotRestoreManifestError::TooManyResources)
        ));
        assert!(matches!(
            checked_native_v2_restore_resource_count(usize::MAX, 1, 1, 1),
            Err(SnapshotRestoreManifestError::TooManyResources)
        ));
    }

    #[test]
    fn network_override_membership_accepts_explicit_same_destination_identity() {
        let network = key(
            4,
            0,
            "network-secret",
            SnapshotRestoreResourceClass::NetworkPacketIo,
        );
        let manifest =
            SnapshotRestoreManifest::try_new(vec![network.clone()], vec![network.clone()])
                .expect("explicit network binding should validate even when identity is unchanged");

        assert!(manifest.is_overridden(&network));
        assert_eq!(manifest.overrides().collect::<Vec<_>>(), [&network]);
        let debug = format!("{manifest:?} {network:?}");
        assert!(!debug.contains("network-secret"));
    }

    #[test]
    fn network_bindings_reject_alias_class_extra_duplicate_missing_reuse_and_unconsumed() {
        let network = key(4, 0, "eth0", SnapshotRestoreResourceClass::NetworkPacketIo);
        let manifest =
            SnapshotRestoreManifest::try_new(vec![network.clone()], vec![network.clone()])
                .expect("network manifest should validate");
        let mut bindings = manifest
            .try_into_bindings()
            .expect("network binding slots should allocate");

        let alias = key(4, 1, "eth0", SnapshotRestoreResourceClass::NetworkPacketIo);
        let alias_rejection = bindings
            .bind(&alias, "alias")
            .expect_err("same public ID under another device key must be extra");
        assert_eq!(
            alias_rejection.reason(),
            SnapshotRestoreBindingRejectionReason::ExtraBinding
        );
        assert_eq!(alias_rejection.into_value(), "alias");

        let wrong_class = SnapshotRestoreResourceKey::new(
            network.device_key(),
            network.public_id().clone(),
            SnapshotRestoreResourceClass::VsockEndpoint,
        );
        let class_rejection = bindings
            .bind(&wrong_class, "class")
            .expect_err("network resource class swap must fail");
        assert_eq!(
            class_rejection.reason(),
            SnapshotRestoreBindingRejectionReason::WrongClass
        );
        assert_eq!(class_rejection.into_value(), "class");

        let incomplete = bindings
            .complete()
            .expect_err("missing network value must fail completion");
        assert_eq!(incomplete.missing_count(), 1);
        let mut bindings = incomplete.into_bindings();
        bindings
            .bind(&network, "owner")
            .expect("exact network owner should bind");
        let duplicate = bindings
            .bind(&network, "duplicate")
            .expect_err("duplicate network owner must fail");
        assert_eq!(
            duplicate.reason(),
            SnapshotRestoreBindingRejectionReason::DuplicateBinding
        );
        assert_eq!(duplicate.into_value(), "duplicate");

        let prepared = bindings
            .complete()
            .expect("complete network owner set should prepare");
        let unconsumed = prepared
            .finish()
            .expect_err("unconsumed network owner must fail");
        assert_eq!(unconsumed.unconsumed_count(), 1);
        let mut prepared = unconsumed.into_bindings();
        assert_eq!(prepared.take(&network), Ok("owner"));
        assert_eq!(
            prepared.take(&network),
            Err(SnapshotRestoreTakeError::AlreadyTaken)
        );
        prepared
            .finish()
            .expect("consumed network owner set should finish");
    }

    #[test]
    fn overrides_are_exact_canonical_and_fail_closed() {
        let block = key(
            1,
            0,
            "block-secret",
            SnapshotRestoreResourceClass::BlockBacking,
        );
        let vsock = key(
            2,
            0,
            "vsock-secret",
            SnapshotRestoreResourceClass::VsockEndpoint,
        );
        let manifest = SnapshotRestoreManifest::try_new(
            vec![vsock.clone(), block.clone()],
            vec![vsock.clone()],
        )
        .expect("exact vsock override should validate");
        assert!(manifest.is_overridden(&vsock));
        assert!(!manifest.is_overridden(&block));
        assert_eq!(
            manifest
                .overrides()
                .map(|resource| resource.public_id().as_str())
                .collect::<Vec<_>>(),
            ["vsock-secret"]
        );

        assert!(matches!(
            SnapshotRestoreManifest::try_new(vec![block.clone()], vec![block]),
            Err(SnapshotRestoreManifestError::UnsupportedOverrideClass)
        ));
        let pmem = key(
            4,
            0,
            "pmem-secret",
            SnapshotRestoreResourceClass::PmemBacking,
        );
        assert!(matches!(
            SnapshotRestoreManifest::try_new(vec![pmem.clone()], vec![pmem]),
            Err(SnapshotRestoreManifestError::UnsupportedOverrideClass)
        ));
        assert!(matches!(
            SnapshotRestoreManifest::try_new(
                vec![vsock.clone()],
                vec![vsock.clone(), vsock.clone()]
            ),
            Err(SnapshotRestoreManifestError::DuplicateOverride)
        ));
        assert!(matches!(
            SnapshotRestoreManifest::try_new(
                vec![vsock.clone()],
                vec![key(
                    9,
                    0,
                    "unknown",
                    SnapshotRestoreResourceClass::VsockEndpoint
                )]
            ),
            Err(SnapshotRestoreManifestError::UnknownOverride)
        ));
        let wrong_class = SnapshotRestoreResourceKey::new(
            vsock.device_key(),
            vsock.public_id().clone(),
            SnapshotRestoreResourceClass::BlockBacking,
        );
        assert!(matches!(
            SnapshotRestoreManifest::try_new(vec![vsock], vec![wrong_class]),
            Err(SnapshotRestoreManifestError::OverrideClassMismatch)
        ));
    }

    #[test]
    fn override_index_allocation_failure_is_redacted() {
        let secret = "override-allocation-secret";
        let vsock = key(1, 0, secret, SnapshotRestoreResourceClass::VsockEndpoint);
        let error = SnapshotRestoreManifest::try_new_with_reserve(
            vec![vsock.clone()],
            vec![vsock],
            move |_, _| Err(allocation_error()),
        )
        .expect_err("injected reserve failure should reject");
        assert!(matches!(
            error,
            SnapshotRestoreManifestError::AllocationFailed { .. }
        ));
        assert!(!format!("{error:?}").contains(secret));
        assert!(!error.to_string().contains(secret));
    }

    #[test]
    fn bindings_require_complete_exact_values_before_takes() {
        let block = key(1, 0, "block", SnapshotRestoreResourceClass::BlockBacking);
        let vsock = key(2, 0, "vsock", SnapshotRestoreResourceClass::VsockEndpoint);
        let manifest =
            SnapshotRestoreManifest::try_new(vec![vsock.clone(), block.clone()], Vec::new())
                .expect("manifest should validate");
        let mut bindings = manifest
            .try_into_bindings()
            .expect("binding slots should allocate");
        bindings
            .bind(&vsock, "vsock-value")
            .expect("vsock should bind");
        let incomplete = bindings
            .complete()
            .expect_err("missing block value should reject completion");
        assert_eq!(incomplete.missing_count(), 1);
        let mut bindings = incomplete.into_bindings();
        bindings
            .bind(&block, "block-value")
            .expect("block should bind");
        let mut prepared = bindings.complete().expect("all values should be bound");
        assert_eq!(
            prepared.take(&vsock).expect("vsock should take"),
            "vsock-value"
        );
        assert_eq!(
            prepared.take(&block).expect("block should take"),
            "block-value"
        );
        assert_eq!(
            prepared.take(&block),
            Err(SnapshotRestoreTakeError::AlreadyTaken)
        );
        prepared.finish().expect("all values should be consumed");
    }

    #[test]
    fn binding_rejections_preserve_values_and_categories() {
        let block = key(1, 0, "block", SnapshotRestoreResourceClass::BlockBacking);
        let manifest = SnapshotRestoreManifest::try_new(vec![block.clone()], Vec::new())
            .expect("manifest should validate");
        let mut bindings = manifest
            .try_into_bindings()
            .expect("binding slots should allocate");

        let extra = key(2, 0, "extra", SnapshotRestoreResourceClass::BlockBacking);
        let rejection = bindings
            .bind(&extra, 11)
            .expect_err("extra value should reject");
        assert_eq!(
            rejection.reason(),
            SnapshotRestoreBindingRejectionReason::ExtraBinding
        );
        assert_eq!(rejection.into_value(), 11);

        let wrong_class = SnapshotRestoreResourceKey::new(
            block.device_key(),
            block.public_id().clone(),
            SnapshotRestoreResourceClass::VsockEndpoint,
        );
        let rejection = bindings
            .bind(&wrong_class, 12)
            .expect_err("wrong class should reject");
        assert_eq!(
            rejection.reason(),
            SnapshotRestoreBindingRejectionReason::WrongClass
        );
        assert_eq!(rejection.into_value(), 12);

        bindings.bind(&block, 13).expect("block should bind");
        let rejection = bindings
            .bind(&block, 14)
            .expect_err("duplicate binding should reject");
        assert_eq!(
            rejection.reason(),
            SnapshotRestoreBindingRejectionReason::DuplicateBinding
        );
        assert_eq!(rejection.into_value(), 14);
    }

    #[test]
    fn prepared_takes_reject_unknown_and_wrong_class() {
        let block = key(1, 0, "block", SnapshotRestoreResourceClass::BlockBacking);
        let manifest = SnapshotRestoreManifest::try_new(vec![block.clone()], Vec::new())
            .expect("manifest should validate");
        let mut bindings = manifest
            .try_into_bindings()
            .expect("binding slots should allocate");
        bindings.bind(&block, 7).expect("block should bind");
        let mut prepared = bindings.complete().expect("set should complete");

        assert_eq!(
            prepared.take(&key(
                2,
                0,
                "unknown",
                SnapshotRestoreResourceClass::BlockBacking
            )),
            Err(SnapshotRestoreTakeError::UnknownResource)
        );
        let wrong_class = SnapshotRestoreResourceKey::new(
            block.device_key(),
            block.public_id().clone(),
            SnapshotRestoreResourceClass::VsockEndpoint,
        );
        assert_eq!(
            prepared.take(&wrong_class),
            Err(SnapshotRestoreTakeError::WrongClass)
        );
        assert_eq!(prepared.take(&block), Ok(7));
        prepared.finish().expect("consumed set should finish");
    }

    #[test]
    fn maximum_bindings_and_reverse_abort_are_deterministic() {
        let resources = maximum_keys();
        let manifest = SnapshotRestoreManifest::try_new(resources.clone(), Vec::new())
            .expect("maximum manifest should validate");
        let mut bindings = manifest
            .try_into_bindings()
            .expect("maximum slots should allocate");
        for (index, resource) in resources.iter().enumerate().rev() {
            bindings
                .bind(resource, index)
                .expect("reordered maximum value should bind");
        }
        let prepared = bindings.complete().expect("maximum set should complete");
        let unconsumed = prepared
            .finish()
            .expect_err("unconsumed maximum set should reject finish");
        assert_eq!(
            unconsumed.unconsumed_count(),
            MAX_SNAPSHOT_RESTORE_RESOURCES
        );
        assert_eq!(
            unconsumed
                .into_bindings()
                .into_values()
                .rev()
                .collect::<Vec<_>>(),
            (0..MAX_SNAPSHOT_RESTORE_RESOURCES)
                .rev()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn binding_allocation_failure_precedes_any_value() {
        let manifest = SnapshotRestoreManifest::try_new(
            vec![key(
                1,
                0,
                "allocation-secret",
                SnapshotRestoreResourceClass::BlockBacking,
            )],
            Vec::new(),
        )
        .expect("manifest should validate");
        let error = SnapshotRestoreBindings::<usize>::try_from_manifest_with_reserve(
            manifest,
            move |_, _| Err(allocation_error()),
        )
        .expect_err("injected binding allocation should fail");
        assert!(!format!("{error:?}").contains("allocation-secret"));
        assert!(!error.to_string().contains("allocation-secret"));
    }

    struct OpaqueOwner {
        secret: &'static str,
        callback_count: Arc<AtomicUsize>,
    }

    impl OpaqueOwner {
        fn host_operation(&self) {
            self.callback_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl fmt::Debug for OpaqueOwner {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            self.callback_count.fetch_add(1, Ordering::Relaxed);
            formatter.write_str(self.secret)
        }
    }

    #[test]
    fn binding_never_invokes_or_formats_owner_values() {
        let public_secret = "public-owner-secret";
        let owner_secret = "prepared-owner-secret";
        let resource = key(
            1,
            0,
            public_secret,
            SnapshotRestoreResourceClass::BlockBacking,
        );
        let manifest = SnapshotRestoreManifest::try_new(vec![resource.clone()], Vec::new())
            .expect("manifest should validate");
        let callback_count = Arc::new(AtomicUsize::new(0));
        let owner = OpaqueOwner {
            secret: owner_secret,
            callback_count: Arc::clone(&callback_count),
        };
        let mut bindings = manifest
            .try_into_bindings()
            .expect("binding slots should allocate");
        bindings.bind(&resource, owner).expect("owner should bind");
        let binding_debug = format!("{bindings:?}");
        let mut prepared = bindings.complete().expect("set should complete");
        let prepared_debug = format!("{prepared:?}");
        let owner = prepared.take(&resource).expect("owner should take");
        prepared.finish().expect("consumed set should finish");

        assert_eq!(callback_count.load(Ordering::Relaxed), 0);
        for output in [binding_debug, prepared_debug] {
            assert!(!output.contains(public_secret));
            assert!(!output.contains(owner_secret));
        }
        owner.host_operation();
        assert_eq!(callback_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn checked_runtime_topology_relationships_hold() {
        const {
            assert!(MAX_SNAPSHOT_RESTORE_RESOURCES >= NATIVE_V2_DEVICE_GRAPH_MAX_RECORDS as usize);
            assert!(MAX_SNAPSHOT_RESTORE_RESOURCES >= MAX_NETWORK_INTERFACE_COUNT);
            assert!(MAX_SNAPSHOT_RESTORE_RESOURCES == 64);
        }
    }
}
