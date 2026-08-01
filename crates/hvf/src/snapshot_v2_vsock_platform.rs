//! Host-free exact native-v2 2.12 vsock platform-product planning.

use std::fmt;

use bangbang_runtime::balloon::BalloonMmioLayout;
use bangbang_runtime::entropy::EntropyMmioLayout;
use bangbang_runtime::fdt::{Arm64FdtPciHost, Arm64FdtVirtioMmioDevice};
use bangbang_runtime::interrupt::GuestInterruptLine;
use bangbang_runtime::memory::{GuestMemory, GuestMemoryRange};
use bangbang_runtime::memory_hotplug::VirtioMemMmioLayout;
use bangbang_runtime::mmio::{MmioRegion, MmioRegionId};
use bangbang_runtime::network::NetworkMmioLayout;
use bangbang_runtime::snapshot_balloon_v2_9::SnapshotV2BalloonRestorePlan;
use bangbang_runtime::snapshot_device_v2::SnapshotV2DeviceTransportKind;
use bangbang_runtime::snapshot_device_v2_6::PreparedSnapshotV2StorageBundle;
use bangbang_runtime::snapshot_entropy_v2_8::SnapshotV2EntropyRestorePlan;
use bangbang_runtime::snapshot_memory_hotplug_v2_10::PreparedSnapshotV2MemoryHotplugTopology;
use bangbang_runtime::snapshot_memory_v2::SnapshotV2MemoryBinding;
use bangbang_runtime::snapshot_network_restore_v2_11::PreparedSnapshotV2NetworkRestoreTopology;
use bangbang_runtime::snapshot_restore::{
    MAX_SNAPSHOT_RESTORE_RESOURCES, NATIVE_V2_SERIAL_RESTORE_PUBLIC_ID,
    NATIVE_V2_VSOCK_RESTORE_PUBLIC_ID, SnapshotRestoreResourceClass, SnapshotRestoreResourceKey,
};
use bangbang_runtime::snapshot_vsock_restore_v2_12::{
    PreparedSnapshotV2VsockMmioState, PreparedSnapshotV2VsockPciState,
    PreparedSnapshotV2VsockRestoreState,
};
use bangbang_runtime::storage_capture::StorageDeviceOrigin;
use bangbang_runtime::virtio_mmio::{VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioQueueState};
use bangbang_runtime::vsock::{VIRTIO_VSOCK_QUEUE_COUNT, VsockConfig, VsockMmioLayout};

use crate::gic::HvfGicMsiMetadata;
use crate::memory::HvfSnapshotV2MemoryHotplugMappingPlan;
use crate::snapshot_v2::HvfSnapshotV2PlatformState;
use crate::snapshot_v2_balloon_platform::{
    HvfSnapshotV2BalloonMmioEndpointPlan, HvfSnapshotV2BalloonPciEndpointPlan,
};
use crate::snapshot_v2_network_platform::{
    HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan, HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan,
    HvfSnapshotV2NetworkMmioEndpointPlan, HvfSnapshotV2NetworkMmioFollowingEndpointInput,
    HvfSnapshotV2NetworkMmioFollowingEndpointPlan, HvfSnapshotV2NetworkMmioPlatformPlan,
    HvfSnapshotV2NetworkMmioProcessConfig, HvfSnapshotV2NetworkPciEndpointPlan,
    HvfSnapshotV2NetworkPciFollowingEndpointInput, HvfSnapshotV2NetworkPciFollowingEndpointPlan,
    HvfSnapshotV2NetworkPciPlatformPlan, HvfSnapshotV2NetworkPlatformPlanStage,
    HvfSnapshotV2NetworkPreparedProduct, HvfSnapshotV2NetworkProcessResourceIdentity,
    NetworkPlatformPlanReserve, PrepareHvfSnapshotV2NetworkPlatformPlanError,
    SystemNetworkPlatformPlanReserve, prepare_network_mmio_platform_plan,
    prepare_network_pci_platform_plan, validate_product,
};
use crate::snapshot_v2_storage_platform::{
    HvfSnapshotV2StorageMmioPlatformPlan, HvfSnapshotV2StorageMmioProcessConfig,
    HvfSnapshotV2StoragePciPlatformPlan,
};

const REDACTED: &str = "<redacted>";
const STORAGE_MASK: u8 = 1 << 0;
const ENTROPY_MASK: u8 = 1 << 1;
const BALLOON_MASK: u8 = 1 << 2;
const MEMORY_HOTPLUG_MASK: u8 = 1 << 3;
const NETWORK_MASK: u8 = 1 << 4;
const VSOCK_MASK: u8 = 1 << 5;
const SERIAL_DEVICE_KIND: u32 = 3;
const VSOCK_DEVICE_KIND: u32 = 5;

/// One of the 64 admitted exact-2.12 optional-family presence products.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct HvfSnapshotV2VsockProductKind {
    mask: u8,
}

impl HvfSnapshotV2VsockProductKind {
    #[allow(clippy::fn_params_excessive_bools)]
    pub const fn from_presence(
        has_storage: bool,
        has_entropy: bool,
        has_balloon: bool,
        has_memory_hotplug: bool,
        has_network: bool,
        has_vsock: bool,
    ) -> Self {
        Self {
            mask: ((has_storage as u8) * STORAGE_MASK)
                | ((has_entropy as u8) * ENTROPY_MASK)
                | ((has_balloon as u8) * BALLOON_MASK)
                | ((has_memory_hotplug as u8) * MEMORY_HOTPLUG_MASK)
                | ((has_network as u8) * NETWORK_MASK)
                | ((has_vsock as u8) * VSOCK_MASK),
        }
    }

    pub const fn has_storage(self) -> bool {
        self.mask & STORAGE_MASK != 0
    }

    pub const fn has_entropy(self) -> bool {
        self.mask & ENTROPY_MASK != 0
    }

    pub const fn has_balloon(self) -> bool {
        self.mask & BALLOON_MASK != 0
    }

    pub const fn has_memory_hotplug(self) -> bool {
        self.mask & MEMORY_HOTPLUG_MASK != 0
    }

    pub const fn has_network(self) -> bool {
        self.mask & NETWORK_MASK != 0
    }

    pub const fn has_vsock(self) -> bool {
        self.mask & VSOCK_MASK != 0
    }

    #[cfg(test)]
    const fn mask(self) -> u8 {
        self.mask
    }
}

impl fmt::Debug for HvfSnapshotV2VsockProductKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2VsockProductKind")
            .field("has_storage", &self.has_storage())
            .field("has_entropy", &self.has_entropy())
            .field("has_balloon", &self.has_balloon())
            .field("has_memory_hotplug", &self.has_memory_hotplug())
            .field("has_network", &self.has_network())
            .field("has_vsock", &self.has_vsock())
            .finish()
    }
}

/// Static or virtio-mem destination ownership retained by one exact-2.12 product.
pub enum HvfSnapshotV2VsockPreparedMemory {
    Static(SnapshotV2MemoryBinding),
    MemoryHotplug {
        topology: Box<PreparedSnapshotV2MemoryHotplugTopology>,
        memory: GuestMemory,
    },
}

impl fmt::Debug for HvfSnapshotV2VsockPreparedMemory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2VsockPreparedMemory")
            .field(
                "has_memory_hotplug",
                &matches!(self, Self::MemoryHotplug { .. }),
            )
            .field("state", &REDACTED)
            .finish()
    }
}

/// Checked singleton continuation before destination platform placement.
pub struct HvfSnapshotV2VsockPreparedEndpoint {
    state: PreparedSnapshotV2VsockRestoreState,
    config: VsockConfig,
}

impl HvfSnapshotV2VsockPreparedEndpoint {
    pub const fn new(state: PreparedSnapshotV2VsockRestoreState, config: VsockConfig) -> Self {
        Self { state, config }
    }

    pub const fn state(&self) -> &PreparedSnapshotV2VsockRestoreState {
        &self.state
    }

    pub const fn config(&self) -> &VsockConfig {
        &self.config
    }

    #[doc(hidden)]
    pub fn into_parts(self) -> (PreparedSnapshotV2VsockRestoreState, VsockConfig) {
        (self.state, self.config)
    }
}

impl fmt::Debug for HvfSnapshotV2VsockPreparedEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2VsockPreparedEndpoint")
            .field("transport", &self.state.transport_kind())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Inputs consumed into one closed exact-2.12 platform product.
pub struct HvfSnapshotV2VsockPreparedProductParts {
    pub kind: HvfSnapshotV2VsockProductKind,
    pub memory: HvfSnapshotV2VsockPreparedMemory,
    pub storage: Option<PreparedSnapshotV2StorageBundle>,
    pub entropy: Option<SnapshotV2EntropyRestorePlan>,
    pub balloon: Option<SnapshotV2BalloonRestorePlan>,
    pub network: PreparedSnapshotV2NetworkRestoreTopology,
    pub vsock: Option<HvfSnapshotV2VsockPreparedEndpoint>,
    pub serial_resource_present: bool,
    pub binding_keys: Vec<SnapshotRestoreResourceKey>,
}

impl fmt::Debug for HvfSnapshotV2VsockPreparedProductParts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2VsockPreparedProductParts")
            .field("kind", &self.kind)
            .field("interface_count", &self.network.interfaces().len())
            .field("resource_count", &self.binding_keys.len())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete owner-free exact-2.12 component and resource-identity product.
pub struct HvfSnapshotV2VsockPreparedProduct {
    kind: HvfSnapshotV2VsockProductKind,
    base: HvfSnapshotV2NetworkPreparedProduct,
    vsock: Option<HvfSnapshotV2VsockPreparedEndpoint>,
    serial_resource_present: bool,
    binding_keys: Vec<SnapshotRestoreResourceKey>,
    vsock_key_index: Option<usize>,
}

impl HvfSnapshotV2VsockPreparedProduct {
    pub fn try_from_parts(
        parts: HvfSnapshotV2VsockPreparedProductParts,
    ) -> Result<Self, PrepareHvfSnapshotV2VsockPlatformPlanError> {
        let HvfSnapshotV2VsockPreparedProductParts {
            kind,
            memory,
            storage,
            entropy,
            balloon,
            network,
            vsock,
            serial_resource_present,
            binding_keys,
        } = parts;
        let has_memory_hotplug = matches!(
            &memory,
            HvfSnapshotV2VsockPreparedMemory::MemoryHotplug { .. }
        );
        let actual_kind = HvfSnapshotV2VsockProductKind::from_presence(
            storage.is_some(),
            entropy.is_some(),
            balloon.is_some(),
            has_memory_hotplug,
            !network.interfaces().is_empty(),
            vsock.is_some(),
        );
        if kind != actual_kind {
            return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Product);
        }
        validate_prepared_vsock(vsock.as_ref(), network.transport_kind())?;
        let vsock_key_index = validate_resource_manifest(
            storage.as_ref(),
            &network,
            vsock.as_ref(),
            serial_resource_present,
            &binding_keys,
        )?;
        let base = build_base_product(memory, network, storage, entropy, balloon);
        Ok(Self {
            kind,
            base,
            vsock,
            serial_resource_present,
            binding_keys,
            vsock_key_index,
        })
    }

    pub const fn kind(&self) -> HvfSnapshotV2VsockProductKind {
        self.kind
    }

    pub fn interface_count(&self) -> usize {
        self.base.interface_count()
    }

    pub fn resource_keys(&self) -> &[SnapshotRestoreResourceKey] {
        &self.binding_keys
    }

    pub const fn serial_resource_present(&self) -> bool {
        self.serial_resource_present
    }

    fn validate(&self) -> Result<(), PrepareHvfSnapshotV2VsockPlatformPlanError> {
        let actual = HvfSnapshotV2VsockProductKind::from_presence(
            self.base.has_storage(),
            self.base.has_entropy(),
            self.base.has_balloon(),
            self.base.has_memory_hotplug(),
            self.base.interface_count() != 0,
            self.vsock.is_some(),
        );
        if self.kind != actual {
            return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Product);
        }
        validate_prepared_vsock(self.vsock.as_ref(), self.base.network().transport_kind())?;
        let key_index = validate_resource_manifest(
            self.base.storage(),
            self.base.network(),
            self.vsock.as_ref(),
            self.serial_resource_present,
            &self.binding_keys,
        )?;
        if key_index != self.vsock_key_index {
            return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest);
        }
        Ok(())
    }
}

impl fmt::Debug for HvfSnapshotV2VsockPreparedProduct {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2VsockPreparedProduct")
            .field("kind", &self.kind)
            .field("interface_count", &self.base.interface_count())
            .field("resource_count", &self.binding_keys.len())
            .field("state", &REDACTED)
            .finish()
    }
}

fn validate_prepared_vsock(
    endpoint: Option<&HvfSnapshotV2VsockPreparedEndpoint>,
    expected_transport: SnapshotV2DeviceTransportKind,
) -> Result<(), PrepareHvfSnapshotV2VsockPlatformPlanError> {
    let Some(endpoint) = endpoint else {
        return Ok(());
    };
    if endpoint.state.transport_kind() != expected_transport {
        return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::TransportPolicy);
    }
    let device = match &endpoint.state {
        PreparedSnapshotV2VsockRestoreState::Mmio(state) => state.capture().device(),
        PreparedSnapshotV2VsockRestoreState::Pci(state) => state.capture().device(),
    };
    if device.guest_cid() != u64::from(endpoint.config.guest_cid())
        || endpoint
            .state
            .clone()
            .into_destination_normalized_state(&endpoint.config)
            .is_err()
    {
        return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Vsock);
    }
    Ok(())
}

fn validate_resource_manifest(
    storage: Option<&PreparedSnapshotV2StorageBundle>,
    network: &PreparedSnapshotV2NetworkRestoreTopology,
    vsock: Option<&HvfSnapshotV2VsockPreparedEndpoint>,
    serial_resource_present: bool,
    binding_keys: &[SnapshotRestoreResourceKey],
) -> Result<Option<usize>, PrepareHvfSnapshotV2VsockPlatformPlanError> {
    let block_count = storage
        .and_then(PreparedSnapshotV2StorageBundle::block_bundle)
        .map_or(0, |block| block.records().len());
    let pmem_count = storage.map_or(0, |storage| storage.pmem_records().len());
    let expected_count = block_count
        .checked_add(pmem_count)
        .and_then(|count| count.checked_add(usize::from(serial_resource_present)))
        .and_then(|count| count.checked_add(network.interfaces().len()))
        .and_then(|count| count.checked_add(usize::from(vsock.is_some())))
        .ok_or(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest)?;
    if expected_count != binding_keys.len()
        || binding_keys.len() > MAX_SNAPSHOT_RESTORE_RESOURCES
        || binding_keys.windows(2).any(|pair| match pair {
            [left, right] => left >= right,
            _ => false,
        })
    {
        return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest);
    }

    let mut keys = binding_keys.iter().enumerate();
    if let Some(block) = storage.and_then(PreparedSnapshotV2StorageBundle::block_bundle) {
        for record in block.records() {
            let Some((_, key)) = keys.next() else {
                return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest);
            };
            if key.resource_class() != SnapshotRestoreResourceClass::BlockBacking
                || key.device_key() != record.key()
                || key.public_id().as_str() != record.drive_id()
            {
                return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest);
            }
        }
    }
    if let Some(storage) = storage {
        for record in storage.pmem_records() {
            let Some((_, key)) = keys.next() else {
                return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest);
            };
            if key.resource_class() != SnapshotRestoreResourceClass::PmemBacking
                || key.device_key() != record.key()
                || key.public_id().as_str() != record.pmem_id()
            {
                return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest);
            }
        }
    }
    if serial_resource_present {
        let Some((_, key)) = keys.next() else {
            return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest);
        };
        if key.resource_class() != SnapshotRestoreResourceClass::SerialSink
            || key.device_key().kind() != SERIAL_DEVICE_KIND
            || key.device_key().instance() != 0
            || key.public_id().as_str() != NATIVE_V2_SERIAL_RESTORE_PUBLIC_ID
        {
            return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest);
        }
    }
    for interface in network.interfaces() {
        let Some((_, key)) = keys.next() else {
            return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest);
        };
        if key != interface.resource_key() {
            return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest);
        }
    }
    let vsock_key_index = if vsock.is_some() {
        let Some((index, key)) = keys.next() else {
            return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest);
        };
        if key.resource_class() != SnapshotRestoreResourceClass::VsockEndpoint
            || key.device_key().kind() != VSOCK_DEVICE_KIND
            || key.device_key().instance() != 0
            || key.public_id().as_str() != NATIVE_V2_VSOCK_RESTORE_PUBLIC_ID
        {
            return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest);
        }
        Some(index)
    } else {
        None
    };
    if keys.next().is_some() {
        return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest);
    }
    Ok(vsock_key_index)
}

fn build_base_product(
    memory: HvfSnapshotV2VsockPreparedMemory,
    network: PreparedSnapshotV2NetworkRestoreTopology,
    storage: Option<PreparedSnapshotV2StorageBundle>,
    entropy: Option<SnapshotV2EntropyRestorePlan>,
    balloon: Option<SnapshotV2BalloonRestorePlan>,
) -> HvfSnapshotV2NetworkPreparedProduct {
    match (memory, balloon, storage, entropy) {
        (HvfSnapshotV2VsockPreparedMemory::Static(binding), None, None, None) => {
            HvfSnapshotV2NetworkPreparedProduct::serial_network(binding, network)
        }
        (HvfSnapshotV2VsockPreparedMemory::Static(binding), None, Some(storage), None) => {
            HvfSnapshotV2NetworkPreparedProduct::serial_storage_network(
                binding, network, storage,
            )
        }
        (HvfSnapshotV2VsockPreparedMemory::Static(binding), None, None, Some(entropy)) => {
            HvfSnapshotV2NetworkPreparedProduct::serial_entropy_network(
                binding, network, entropy,
            )
        }
        (
            HvfSnapshotV2VsockPreparedMemory::Static(binding),
            None,
            Some(storage),
            Some(entropy),
        ) => HvfSnapshotV2NetworkPreparedProduct::serial_storage_entropy_network(
            binding, network, storage, entropy,
        ),
        (HvfSnapshotV2VsockPreparedMemory::Static(binding), Some(balloon), None, None) => {
            HvfSnapshotV2NetworkPreparedProduct::serial_balloon_network(
                binding, network, balloon,
            )
        }
        (
            HvfSnapshotV2VsockPreparedMemory::Static(binding),
            Some(balloon),
            Some(storage),
            None,
        ) => HvfSnapshotV2NetworkPreparedProduct::serial_balloon_storage_network(
            binding, network, balloon, storage,
        ),
        (
            HvfSnapshotV2VsockPreparedMemory::Static(binding),
            Some(balloon),
            None,
            Some(entropy),
        ) => HvfSnapshotV2NetworkPreparedProduct::serial_balloon_entropy_network(
            binding, network, balloon, entropy,
        ),
        (
            HvfSnapshotV2VsockPreparedMemory::Static(binding),
            Some(balloon),
            Some(storage),
            Some(entropy),
        ) => HvfSnapshotV2NetworkPreparedProduct::serial_balloon_storage_entropy_network(
            binding, network, balloon, storage, entropy,
        ),
        (
            HvfSnapshotV2VsockPreparedMemory::MemoryHotplug { topology, memory },
            None,
            None,
            None,
        ) => HvfSnapshotV2NetworkPreparedProduct::serial_network_memory_hotplug(
            *topology, memory, network,
        ),
        (
            HvfSnapshotV2VsockPreparedMemory::MemoryHotplug { topology, memory },
            None,
            Some(storage),
            None,
        ) => HvfSnapshotV2NetworkPreparedProduct::serial_storage_network_memory_hotplug(
            *topology, memory, network, storage,
        ),
        (
            HvfSnapshotV2VsockPreparedMemory::MemoryHotplug { topology, memory },
            None,
            None,
            Some(entropy),
        ) => HvfSnapshotV2NetworkPreparedProduct::serial_entropy_network_memory_hotplug(
            *topology, memory, network, entropy,
        ),
        (
            HvfSnapshotV2VsockPreparedMemory::MemoryHotplug { topology, memory },
            None,
            Some(storage),
            Some(entropy),
        ) => HvfSnapshotV2NetworkPreparedProduct::serial_storage_entropy_network_memory_hotplug(
            *topology, memory, network, storage, entropy,
        ),
        (
            HvfSnapshotV2VsockPreparedMemory::MemoryHotplug { topology, memory },
            Some(balloon),
            None,
            None,
        ) => HvfSnapshotV2NetworkPreparedProduct::serial_balloon_network_memory_hotplug(
            *topology, memory, network, balloon,
        ),
        (
            HvfSnapshotV2VsockPreparedMemory::MemoryHotplug { topology, memory },
            Some(balloon),
            Some(storage),
            None,
        ) => HvfSnapshotV2NetworkPreparedProduct::serial_balloon_storage_network_memory_hotplug(
            *topology, memory, network, balloon, storage,
        ),
        (
            HvfSnapshotV2VsockPreparedMemory::MemoryHotplug { topology, memory },
            Some(balloon),
            None,
            Some(entropy),
        ) => HvfSnapshotV2NetworkPreparedProduct::serial_balloon_entropy_network_memory_hotplug(
            *topology, memory, network, balloon, entropy,
        ),
        (
            HvfSnapshotV2VsockPreparedMemory::MemoryHotplug { topology, memory },
            Some(balloon),
            Some(storage),
            Some(entropy),
        ) => HvfSnapshotV2NetworkPreparedProduct::
            serial_balloon_storage_entropy_network_memory_hotplug(
                *topology, memory, network, balloon, storage, entropy,
            ),
    }
}

/// Canonical destination layouts for one exact-2.12 MMIO process.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2VsockMmioProcessConfig {
    network: HvfSnapshotV2NetworkMmioProcessConfig,
    vsock_layout: VsockMmioLayout,
}

impl HvfSnapshotV2VsockMmioProcessConfig {
    pub const fn new(
        network: HvfSnapshotV2NetworkMmioProcessConfig,
        vsock_layout: VsockMmioLayout,
    ) -> Self {
        Self {
            network,
            vsock_layout,
        }
    }

    pub const fn from_layouts(
        balloon_layout: BalloonMmioLayout,
        storage: HvfSnapshotV2StorageMmioProcessConfig,
        network_layout: NetworkMmioLayout,
        vsock_layout: VsockMmioLayout,
        entropy_layout: EntropyMmioLayout,
        memory_hotplug_layout: VirtioMemMmioLayout,
    ) -> Self {
        Self::new(
            HvfSnapshotV2NetworkMmioProcessConfig::new(
                balloon_layout,
                storage,
                network_layout,
                entropy_layout,
                memory_hotplug_layout,
            ),
            vsock_layout,
        )
    }

    pub const fn network(self) -> HvfSnapshotV2NetworkMmioProcessConfig {
        self.network
    }

    pub const fn vsock_layout(self) -> VsockMmioLayout {
        self.vsock_layout
    }
}

impl fmt::Debug for HvfSnapshotV2VsockMmioProcessConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2VsockMmioProcessConfig")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Borrowed singleton process projection for value-only identity preflight.
#[derive(Clone, Copy)]
#[doc(hidden)]
pub struct HvfSnapshotV2VsockProcessResourceIdentity<'a> {
    resource_key: &'a SnapshotRestoreResourceKey,
    config: &'a VsockConfig,
}

impl<'a> HvfSnapshotV2VsockProcessResourceIdentity<'a> {
    pub const fn new(
        resource_key: &'a SnapshotRestoreResourceKey,
        config: &'a VsockConfig,
    ) -> Self {
        Self {
            resource_key,
            config,
        }
    }
}

impl fmt::Debug for HvfSnapshotV2VsockProcessResourceIdentity<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2VsockProcessResourceIdentity")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete fixed placement and continuation for one MMIO vsock endpoint.
pub struct HvfSnapshotV2VsockMmioEndpointPlan {
    resource_key: SnapshotRestoreResourceKey,
    config: VsockConfig,
    state: PreparedSnapshotV2VsockMmioState,
    queue_ranges: [Option<[GuestMemoryRange; 3]>; VIRTIO_VSOCK_QUEUE_COUNT],
    placement: HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan,
}

impl HvfSnapshotV2VsockMmioEndpointPlan {
    pub const fn resource_key(&self) -> &SnapshotRestoreResourceKey {
        &self.resource_key
    }

    pub const fn config(&self) -> &VsockConfig {
        &self.config
    }

    pub const fn state(&self) -> &PreparedSnapshotV2VsockMmioState {
        &self.state
    }

    pub const fn queue_ranges(&self) -> &[Option<[GuestMemoryRange; 3]>; VIRTIO_VSOCK_QUEUE_COUNT] {
        &self.queue_ranges
    }

    pub const fn region(&self) -> MmioRegion {
        self.placement.region()
    }

    pub const fn dispatcher_region_id(&self) -> MmioRegionId {
        self.placement.dispatcher_region_id()
    }

    pub const fn interrupt_line(&self) -> GuestInterruptLine {
        self.placement.interrupt_line()
    }

    pub const fn fdt_device(&self) -> Arm64FdtVirtioMmioDevice {
        self.placement.fdt_device()
    }
}

impl fmt::Debug for HvfSnapshotV2VsockMmioEndpointPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2VsockMmioEndpointPlan")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete fixed placement and continuation for one PCI vsock endpoint.
pub struct HvfSnapshotV2VsockPciEndpointPlan {
    resource_key: SnapshotRestoreResourceKey,
    config: VsockConfig,
    state: PreparedSnapshotV2VsockPciState,
    queue_ranges: [Option<[GuestMemoryRange; 3]>; VIRTIO_VSOCK_QUEUE_COUNT],
    placement: HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan,
    queue_vectors: [u16; VIRTIO_VSOCK_QUEUE_COUNT],
    config_vector: u16,
}

impl HvfSnapshotV2VsockPciEndpointPlan {
    pub const fn resource_key(&self) -> &SnapshotRestoreResourceKey {
        &self.resource_key
    }

    pub const fn config(&self) -> &VsockConfig {
        &self.config
    }

    pub const fn state(&self) -> &PreparedSnapshotV2VsockPciState {
        &self.state
    }

    pub const fn queue_ranges(&self) -> &[Option<[GuestMemoryRange; 3]>; VIRTIO_VSOCK_QUEUE_COUNT] {
        &self.queue_ranges
    }

    pub const fn origin(&self) -> StorageDeviceOrigin {
        self.placement.origin()
    }

    pub const fn sbdf(&self) -> bangbang_runtime::pci::PciSbdf {
        self.placement.sbdf()
    }

    pub const fn bar_region_id(&self) -> MmioRegionId {
        self.placement.bar_region_id()
    }

    pub const fn dispatcher_region_id(&self) -> MmioRegionId {
        self.placement.dispatcher_region_id()
    }

    pub const fn bar_range(&self) -> GuestMemoryRange {
        self.placement.bar_range()
    }

    pub const fn route_count(&self) -> usize {
        self.placement.route_count()
    }

    pub const fn queue_vectors(&self) -> &[u16; VIRTIO_VSOCK_QUEUE_COUNT] {
        &self.queue_vectors
    }

    pub const fn config_vector(&self) -> u16 {
        self.config_vector
    }

    pub const fn msi_interrupt_count(&self) -> u32 {
        self.placement.msi_interrupt_count()
    }
}

impl fmt::Debug for HvfSnapshotV2VsockPciEndpointPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2VsockPciEndpointPlan")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete immutable exact-2.12 MMIO product proof.
pub struct HvfSnapshotV2VsockMmioPlatformPlan {
    kind: HvfSnapshotV2VsockProductKind,
    base: HvfSnapshotV2NetworkMmioPlatformPlan,
    vsock: Option<HvfSnapshotV2VsockMmioEndpointPlan>,
    serial_resource_present: bool,
    binding_keys: Vec<SnapshotRestoreResourceKey>,
}

impl HvfSnapshotV2VsockMmioPlatformPlan {
    pub const fn kind(&self) -> HvfSnapshotV2VsockProductKind {
        self.kind
    }

    pub const fn mapping(&self) -> Option<&HvfSnapshotV2MemoryHotplugMappingPlan> {
        self.base.mapping()
    }

    pub const fn balloon(&self) -> Option<HvfSnapshotV2BalloonMmioEndpointPlan> {
        self.base.balloon()
    }

    pub const fn storage(&self) -> Option<&HvfSnapshotV2StorageMmioPlatformPlan> {
        self.base.storage()
    }

    pub fn network(&self) -> &[HvfSnapshotV2NetworkMmioEndpointPlan] {
        self.base.network()
    }

    pub const fn vsock(&self) -> Option<&HvfSnapshotV2VsockMmioEndpointPlan> {
        self.vsock.as_ref()
    }

    pub const fn entropy(&self) -> Option<HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan> {
        self.base.entropy()
    }

    pub const fn memory_hotplug(&self) -> Option<HvfSnapshotV2NetworkAuxiliaryMmioEndpointPlan> {
        self.base.memory_hotplug()
    }

    pub const fn serial_interrupt(&self) -> GuestInterruptLine {
        self.base.serial_interrupt()
    }

    pub const fn vmgenid_interrupt(&self) -> GuestInterruptLine {
        self.base.vmgenid_interrupt()
    }

    pub const fn vmclock_interrupt(&self) -> GuestInterruptLine {
        self.base.vmclock_interrupt()
    }

    pub fn resource_keys(&self) -> &[SnapshotRestoreResourceKey] {
        &self.binding_keys
    }

    pub const fn serial_resource_present(&self) -> bool {
        self.serial_resource_present
    }

    #[doc(hidden)]
    pub fn preflight_process_resource_identity<'a>(
        &self,
        resources: impl ExactSizeIterator<Item = HvfSnapshotV2NetworkProcessResourceIdentity<'a>>,
        mmds_state: Option<&bangbang_runtime::snapshot_network_v2_11::SnapshotV2MmdsState>,
        mmds_controller: Option<&bangbang_runtime::mmds::MmdsConfig>,
        vsock: Option<HvfSnapshotV2VsockProcessResourceIdentity<'a>>,
        binding_keys: &[SnapshotRestoreResourceKey],
    ) -> bool {
        self.base
            .preflight_process_resource_identity(resources, mmds_state, mmds_controller)
            && matches_vsock_identity_mmio(self.vsock.as_ref(), vsock)
            && self.binding_keys == binding_keys
    }

    pub(crate) fn into_owner_parts(self) -> HvfSnapshotV2VsockMmioPlatformOwnerParts {
        HvfSnapshotV2VsockMmioPlatformOwnerParts {
            kind: self.kind,
            base: self.base,
            vsock: self.vsock,
            serial_resource_present: self.serial_resource_present,
            binding_keys: self.binding_keys,
        }
    }
}

pub(crate) struct HvfSnapshotV2VsockMmioPlatformOwnerParts {
    pub(crate) kind: HvfSnapshotV2VsockProductKind,
    pub(crate) base: HvfSnapshotV2NetworkMmioPlatformPlan,
    pub(crate) vsock: Option<HvfSnapshotV2VsockMmioEndpointPlan>,
    pub(crate) serial_resource_present: bool,
    pub(crate) binding_keys: Vec<SnapshotRestoreResourceKey>,
}

impl fmt::Debug for HvfSnapshotV2VsockMmioPlatformPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2VsockMmioPlatformPlan")
            .field("kind", &self.kind)
            .field("interface_count", &self.base.network().len())
            .field("resource_count", &self.binding_keys.len())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete immutable exact-2.12 PCI product proof.
pub struct HvfSnapshotV2VsockPciPlatformPlan {
    kind: HvfSnapshotV2VsockProductKind,
    base: HvfSnapshotV2NetworkPciPlatformPlan,
    vsock: Option<HvfSnapshotV2VsockPciEndpointPlan>,
    serial_resource_present: bool,
    binding_keys: Vec<SnapshotRestoreResourceKey>,
}

impl HvfSnapshotV2VsockPciPlatformPlan {
    pub const fn kind(&self) -> HvfSnapshotV2VsockProductKind {
        self.kind
    }

    pub const fn mapping(&self) -> Option<&HvfSnapshotV2MemoryHotplugMappingPlan> {
        self.base.mapping()
    }

    pub const fn balloon(&self) -> Option<HvfSnapshotV2BalloonPciEndpointPlan> {
        self.base.balloon()
    }

    pub const fn storage(&self) -> Option<&HvfSnapshotV2StoragePciPlatformPlan> {
        self.base.storage()
    }

    pub fn network(&self) -> &[HvfSnapshotV2NetworkPciEndpointPlan] {
        self.base.network()
    }

    pub const fn vsock(&self) -> Option<&HvfSnapshotV2VsockPciEndpointPlan> {
        self.vsock.as_ref()
    }

    pub const fn entropy(&self) -> Option<HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan> {
        self.base.entropy()
    }

    pub const fn memory_hotplug(&self) -> Option<HvfSnapshotV2NetworkAuxiliaryPciEndpointPlan> {
        self.base.memory_hotplug()
    }

    pub const fn host(&self) -> Arm64FdtPciHost {
        self.base.host()
    }

    pub const fn msi(&self) -> HvfGicMsiMetadata {
        self.base.msi()
    }

    pub const fn endpoint_count(&self) -> usize {
        self.base.endpoint_count()
    }

    pub const fn route_demand(&self) -> usize {
        self.base.route_demand()
    }

    pub const fn serial_interrupt(&self) -> GuestInterruptLine {
        self.base.serial_interrupt()
    }

    pub const fn vmgenid_interrupt(&self) -> GuestInterruptLine {
        self.base.vmgenid_interrupt()
    }

    pub const fn vmclock_interrupt(&self) -> GuestInterruptLine {
        self.base.vmclock_interrupt()
    }

    pub fn resource_keys(&self) -> &[SnapshotRestoreResourceKey] {
        &self.binding_keys
    }

    pub const fn serial_resource_present(&self) -> bool {
        self.serial_resource_present
    }

    #[doc(hidden)]
    pub fn preflight_process_resource_identity<'a>(
        &self,
        resources: impl ExactSizeIterator<Item = HvfSnapshotV2NetworkProcessResourceIdentity<'a>>,
        mmds_state: Option<&bangbang_runtime::snapshot_network_v2_11::SnapshotV2MmdsState>,
        mmds_controller: Option<&bangbang_runtime::mmds::MmdsConfig>,
        vsock: Option<HvfSnapshotV2VsockProcessResourceIdentity<'a>>,
        binding_keys: &[SnapshotRestoreResourceKey],
    ) -> bool {
        self.base
            .preflight_process_resource_identity(resources, mmds_state, mmds_controller)
            && matches_vsock_identity_pci(self.vsock.as_ref(), vsock)
            && self.binding_keys == binding_keys
    }

    pub(crate) fn into_owner_parts(self) -> HvfSnapshotV2VsockPciPlatformOwnerParts {
        HvfSnapshotV2VsockPciPlatformOwnerParts {
            kind: self.kind,
            base: self.base,
            vsock: self.vsock,
            serial_resource_present: self.serial_resource_present,
            binding_keys: self.binding_keys,
        }
    }
}

pub(crate) struct HvfSnapshotV2VsockPciPlatformOwnerParts {
    pub(crate) kind: HvfSnapshotV2VsockProductKind,
    pub(crate) base: HvfSnapshotV2NetworkPciPlatformPlan,
    pub(crate) vsock: Option<HvfSnapshotV2VsockPciEndpointPlan>,
    pub(crate) serial_resource_present: bool,
    pub(crate) binding_keys: Vec<SnapshotRestoreResourceKey>,
}

impl fmt::Debug for HvfSnapshotV2VsockPciPlatformPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2VsockPciPlatformPlan")
            .field("kind", &self.kind)
            .field("interface_count", &self.base.network().len())
            .field("endpoint_count", &self.base.endpoint_count())
            .field("resource_count", &self.binding_keys.len())
            .field("state", &REDACTED)
            .finish()
    }
}

fn matches_vsock_identity_mmio(
    endpoint: Option<&HvfSnapshotV2VsockMmioEndpointPlan>,
    identity: Option<HvfSnapshotV2VsockProcessResourceIdentity<'_>>,
) -> bool {
    match (endpoint, identity) {
        (None, None) => true,
        (Some(endpoint), Some(identity)) => {
            endpoint.resource_key() == identity.resource_key && endpoint.config() == identity.config
        }
        _ => false,
    }
}

fn matches_vsock_identity_pci(
    endpoint: Option<&HvfSnapshotV2VsockPciEndpointPlan>,
    identity: Option<HvfSnapshotV2VsockProcessResourceIdentity<'_>>,
) -> bool {
    match (endpoint, identity) {
        (None, None) => true,
        (Some(endpoint), Some(identity)) => {
            endpoint.resource_key() == identity.resource_key && endpoint.config() == identity.config
        }
        _ => false,
    }
}

/// Stable cancellation checkpoints before an exact-2.12 plan is published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfSnapshotV2VsockPlatformPlanStage {
    Start,
    Product,
    Interface,
    Vsock,
    Components,
    Inventory,
    Completion,
}

/// Redacted rejection from exact-2.12 vsock platform-product planning.
pub enum PrepareHvfSnapshotV2VsockPlatformPlanError {
    Product,
    Manifest,
    Vsock,
    TransportPolicy,
    Placement,
    ResourcePlan,
    Allocation,
    Network(Box<PrepareHvfSnapshotV2NetworkPlatformPlanError>),
    Cancelled {
        stage: HvfSnapshotV2VsockPlatformPlanStage,
    },
}

impl fmt::Debug for PrepareHvfSnapshotV2VsockPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self {
            Self::Product => "product",
            Self::Manifest => "manifest",
            Self::Vsock => "vsock",
            Self::TransportPolicy => "transport-policy",
            Self::Placement => "placement",
            Self::ResourcePlan => "resource-plan",
            Self::Allocation => "allocation",
            Self::Network(_) => "network",
            Self::Cancelled { .. } => "cancelled",
        };
        formatter
            .debug_struct("PrepareHvfSnapshotV2VsockPlatformPlanError")
            .field("category", &category)
            .field("state", &REDACTED)
            .finish()
    }
}

impl fmt::Display for PrepareHvfSnapshotV2VsockPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Product => "native-v2 vsock product presence is inconsistent",
            Self::Manifest => "native-v2 vsock process resource manifest is inconsistent",
            Self::Vsock => "native-v2 vsock endpoint projection is inconsistent",
            Self::TransportPolicy => "native-v2 vsock product transport is inconsistent",
            Self::Placement => "native-v2 vsock endpoint placement is inconsistent",
            Self::ResourcePlan => "native-v2 vsock platform resources are inconsistent",
            Self::Allocation => "native-v2 vsock temporary planning allocation failed",
            Self::Network(_) => "native-v2 vsock aggregate platform planning failed",
            Self::Cancelled { .. } => "native-v2 vsock platform planning was cancelled",
        })
    }
}

impl std::error::Error for PrepareHvfSnapshotV2VsockPlatformPlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Network(source) => Some(source),
            Self::Product
            | Self::Manifest
            | Self::Vsock
            | Self::TransportPolicy
            | Self::Placement
            | Self::ResourcePlan
            | Self::Allocation
            | Self::Cancelled { .. } => None,
        }
    }
}

fn check_cancelled(
    is_cancelled: &mut impl FnMut(HvfSnapshotV2VsockPlatformPlanStage) -> bool,
    stage: HvfSnapshotV2VsockPlatformPlanStage,
) -> Result<(), PrepareHvfSnapshotV2VsockPlatformPlanError> {
    if is_cancelled(stage) {
        Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Cancelled { stage })
    } else {
        Ok(())
    }
}

fn exact_stage(
    stage: HvfSnapshotV2NetworkPlatformPlanStage,
) -> Option<HvfSnapshotV2VsockPlatformPlanStage> {
    match stage {
        HvfSnapshotV2NetworkPlatformPlanStage::Start
        | HvfSnapshotV2NetworkPlatformPlanStage::Product => None,
        HvfSnapshotV2NetworkPlatformPlanStage::Interface => {
            Some(HvfSnapshotV2VsockPlatformPlanStage::Interface)
        }
        HvfSnapshotV2NetworkPlatformPlanStage::Components => {
            Some(HvfSnapshotV2VsockPlatformPlanStage::Components)
        }
        HvfSnapshotV2NetworkPlatformPlanStage::Inventory => {
            Some(HvfSnapshotV2VsockPlatformPlanStage::Inventory)
        }
        HvfSnapshotV2NetworkPlatformPlanStage::Completion => {
            Some(HvfSnapshotV2VsockPlatformPlanStage::Completion)
        }
    }
}

fn map_network_error(
    error: PrepareHvfSnapshotV2NetworkPlatformPlanError,
) -> PrepareHvfSnapshotV2VsockPlatformPlanError {
    match error {
        PrepareHvfSnapshotV2NetworkPlatformPlanError::Allocation => {
            PrepareHvfSnapshotV2VsockPlatformPlanError::Allocation
        }
        PrepareHvfSnapshotV2NetworkPlatformPlanError::Cancelled { stage } => {
            match exact_stage(stage) {
                Some(stage) => PrepareHvfSnapshotV2VsockPlatformPlanError::Cancelled { stage },
                None => PrepareHvfSnapshotV2VsockPlatformPlanError::Network(Box::new(
                    PrepareHvfSnapshotV2NetworkPlatformPlanError::Cancelled { stage },
                )),
            }
        }
        error => PrepareHvfSnapshotV2VsockPlatformPlanError::Network(Box::new(error)),
    }
}

fn queue_ranges(
    queue: VirtioMmioQueueState,
) -> Result<Option<[GuestMemoryRange; 3]>, PrepareHvfSnapshotV2VsockPlatformPlanError> {
    if queue.size() == 0 {
        return Ok(None);
    }
    let descriptor_size = u64::from(queue.size())
        .checked_mul(16)
        .ok_or(PrepareHvfSnapshotV2VsockPlatformPlanError::ResourcePlan)?;
    let available_size = u64::from(queue.size())
        .checked_mul(2)
        .and_then(|size| size.checked_add(6))
        .ok_or(PrepareHvfSnapshotV2VsockPlatformPlanError::ResourcePlan)?;
    let used_size = u64::from(queue.size())
        .checked_mul(8)
        .and_then(|size| size.checked_add(6))
        .ok_or(PrepareHvfSnapshotV2VsockPlatformPlanError::ResourcePlan)?;
    Ok(Some([
        GuestMemoryRange::new(queue.descriptor_table(), descriptor_size)
            .map_err(|_| PrepareHvfSnapshotV2VsockPlatformPlanError::ResourcePlan)?,
        GuestMemoryRange::new(queue.driver_ring(), available_size)
            .map_err(|_| PrepareHvfSnapshotV2VsockPlatformPlanError::ResourcePlan)?,
        GuestMemoryRange::new(queue.device_ring(), used_size)
            .map_err(|_| PrepareHvfSnapshotV2VsockPlatformPlanError::ResourcePlan)?,
    ]))
}

fn mmio_vsock_queue_ranges(
    state: &PreparedSnapshotV2VsockMmioState,
) -> Result<
    [Option<[GuestMemoryRange; 3]>; VIRTIO_VSOCK_QUEUE_COUNT],
    PrepareHvfSnapshotV2VsockPlatformPlanError,
> {
    let queues = state.capture().transport().queues();
    let [rx, tx, event] = queues else {
        return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Vsock);
    };
    Ok([
        queue_ranges(*rx)?,
        queue_ranges(*tx)?,
        queue_ranges(*event)?,
    ])
}

fn pci_vsock_queue_ranges(
    state: &PreparedSnapshotV2VsockPciState,
) -> Result<
    [Option<[GuestMemoryRange; 3]>; VIRTIO_VSOCK_QUEUE_COUNT],
    PrepareHvfSnapshotV2VsockPlatformPlanError,
> {
    let queues = state.capture().transport().queues();
    if queues.queue_count() != VIRTIO_VSOCK_QUEUE_COUNT {
        return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Vsock);
    }
    let queue = |index: u32| {
        queues
            .queue(index)
            .copied()
            .map_err(|_| PrepareHvfSnapshotV2VsockPlatformPlanError::Vsock)
    };
    Ok([
        queue_ranges(queue(0)?)?,
        queue_ranges(queue(1)?)?,
        queue_ranges(queue(2)?)?,
    ])
}

fn project_mmio_vsock(
    endpoint: &HvfSnapshotV2VsockPreparedEndpoint,
    layout: VsockMmioLayout,
) -> Result<
    HvfSnapshotV2NetworkMmioFollowingEndpointInput,
    PrepareHvfSnapshotV2VsockPlatformPlanError,
> {
    let PreparedSnapshotV2VsockRestoreState::Mmio(state) = &endpoint.state else {
        return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::TransportPolicy);
    };
    let region = MmioRegion::new(
        layout.region_id(),
        layout.address(),
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
    )
    .map_err(|_| PrepareHvfSnapshotV2VsockPlatformPlanError::ResourcePlan)?;
    if state.region() != region {
        return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::Placement);
    }
    Ok(HvfSnapshotV2NetworkMmioFollowingEndpointInput {
        region,
        interrupt_line: state.interrupt_line(),
        queue_ranges: mmio_vsock_queue_ranges(state)?,
    })
}

fn project_pci_vsock(
    endpoint: &HvfSnapshotV2VsockPreparedEndpoint,
) -> Result<
    HvfSnapshotV2NetworkPciFollowingEndpointInput<'_>,
    PrepareHvfSnapshotV2VsockPlatformPlanError,
> {
    let PreparedSnapshotV2VsockRestoreState::Pci(state) = &endpoint.state else {
        return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::TransportPolicy);
    };
    Ok(HvfSnapshotV2NetworkPciFollowingEndpointInput {
        origin: state.origin(),
        phase: state.capture().transport().phase(),
        sbdf: state.sbdf(),
        bar_range: state.bar_range(),
        queue_ranges: pci_vsock_queue_ranges(state)?,
        msix: state.capture().transport().msix_state(),
    })
}

/// Proves one complete exact-2.12 MMIO product before live ownership.
pub fn prepare_hvf_snapshot_v2_vsock_mmio_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2VsockPreparedProduct,
    process: HvfSnapshotV2VsockMmioProcessConfig,
) -> Result<HvfSnapshotV2VsockMmioPlatformPlan, PrepareHvfSnapshotV2VsockPlatformPlanError> {
    prepare_vsock_mmio_platform_plan(
        platform,
        product,
        process,
        &mut SystemNetworkPlatformPlanReserve,
        &mut |_| false,
    )
}

/// Proves one exact-2.12 MMIO product with stable cancellation checkpoints.
pub fn prepare_hvf_snapshot_v2_vsock_mmio_platform_plan_with_cancel<C>(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2VsockPreparedProduct,
    process: HvfSnapshotV2VsockMmioProcessConfig,
    mut is_cancelled: C,
) -> Result<HvfSnapshotV2VsockMmioPlatformPlan, PrepareHvfSnapshotV2VsockPlatformPlanError>
where
    C: FnMut(HvfSnapshotV2VsockPlatformPlanStage) -> bool,
{
    prepare_vsock_mmio_platform_plan(
        platform,
        product,
        process,
        &mut SystemNetworkPlatformPlanReserve,
        &mut is_cancelled,
    )
}

pub(crate) fn prepare_vsock_mmio_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2VsockPreparedProduct,
    process: HvfSnapshotV2VsockMmioProcessConfig,
    reserve: &mut impl NetworkPlatformPlanReserve,
    is_cancelled: &mut impl FnMut(HvfSnapshotV2VsockPlatformPlanStage) -> bool,
) -> Result<HvfSnapshotV2VsockMmioPlatformPlan, PrepareHvfSnapshotV2VsockPlatformPlanError> {
    check_cancelled(is_cancelled, HvfSnapshotV2VsockPlatformPlanStage::Start)?;
    product.validate()?;
    if validate_product(platform, &product.base).map_err(map_network_error)?
        != SnapshotV2DeviceTransportKind::Mmio
    {
        return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::TransportPolicy);
    }
    check_cancelled(is_cancelled, HvfSnapshotV2VsockPlatformPlanStage::Product)?;

    let following_input = product
        .vsock
        .as_ref()
        .map(|endpoint| project_mmio_vsock(endpoint, process.vsock_layout()))
        .transpose()?;
    check_cancelled(is_cancelled, HvfSnapshotV2VsockPlatformPlanStage::Vsock)?;
    let resource_key = product
        .vsock_key_index
        .map(|index| {
            product
                .binding_keys
                .get(index)
                .ok_or(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest)
                .and_then(|key| reserve.clone_key(key).map_err(map_network_error))
        })
        .transpose()?;

    let HvfSnapshotV2VsockPreparedProduct {
        kind,
        base,
        vsock,
        serial_resource_present,
        binding_keys,
        vsock_key_index: _,
    } = product;
    let mut mapped_cancel = |stage| exact_stage(stage).is_some_and(&mut *is_cancelled);
    let (base, following) = prepare_network_mmio_platform_plan(
        platform,
        base,
        process.network(),
        following_input,
        reserve,
        &mut mapped_cancel,
    )
    .map_err(map_network_error)?;
    let vsock = finish_mmio_vsock(vsock, resource_key, following)?;
    Ok(HvfSnapshotV2VsockMmioPlatformPlan {
        kind,
        base,
        vsock,
        serial_resource_present,
        binding_keys,
    })
}

fn finish_mmio_vsock(
    endpoint: Option<HvfSnapshotV2VsockPreparedEndpoint>,
    resource_key: Option<SnapshotRestoreResourceKey>,
    following: Option<HvfSnapshotV2NetworkMmioFollowingEndpointPlan>,
) -> Result<Option<HvfSnapshotV2VsockMmioEndpointPlan>, PrepareHvfSnapshotV2VsockPlatformPlanError>
{
    match (endpoint, resource_key, following) {
        (None, None, None) => Ok(None),
        (Some(endpoint), Some(resource_key), Some(following)) => {
            let PreparedSnapshotV2VsockRestoreState::Mmio(state) = endpoint.state else {
                return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::TransportPolicy);
            };
            Ok(Some(HvfSnapshotV2VsockMmioEndpointPlan {
                resource_key,
                config: endpoint.config,
                state,
                queue_ranges: following.queue_ranges,
                placement: following.placement,
            }))
        }
        _ => Err(PrepareHvfSnapshotV2VsockPlatformPlanError::ResourcePlan),
    }
}

/// Proves one complete exact-2.12 PCI product before live ownership.
pub fn prepare_hvf_snapshot_v2_vsock_pci_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2VsockPreparedProduct,
) -> Result<HvfSnapshotV2VsockPciPlatformPlan, PrepareHvfSnapshotV2VsockPlatformPlanError> {
    prepare_vsock_pci_platform_plan(
        platform,
        product,
        &mut SystemNetworkPlatformPlanReserve,
        &mut |_| false,
    )
}

/// Proves one exact-2.12 PCI product with stable cancellation checkpoints.
pub fn prepare_hvf_snapshot_v2_vsock_pci_platform_plan_with_cancel<C>(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2VsockPreparedProduct,
    mut is_cancelled: C,
) -> Result<HvfSnapshotV2VsockPciPlatformPlan, PrepareHvfSnapshotV2VsockPlatformPlanError>
where
    C: FnMut(HvfSnapshotV2VsockPlatformPlanStage) -> bool,
{
    prepare_vsock_pci_platform_plan(
        platform,
        product,
        &mut SystemNetworkPlatformPlanReserve,
        &mut is_cancelled,
    )
}

pub(crate) fn prepare_vsock_pci_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2VsockPreparedProduct,
    reserve: &mut impl NetworkPlatformPlanReserve,
    is_cancelled: &mut impl FnMut(HvfSnapshotV2VsockPlatformPlanStage) -> bool,
) -> Result<HvfSnapshotV2VsockPciPlatformPlan, PrepareHvfSnapshotV2VsockPlatformPlanError> {
    check_cancelled(is_cancelled, HvfSnapshotV2VsockPlatformPlanStage::Start)?;
    product.validate()?;
    if validate_product(platform, &product.base).map_err(map_network_error)?
        != SnapshotV2DeviceTransportKind::Pci
    {
        return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::TransportPolicy);
    }
    check_cancelled(is_cancelled, HvfSnapshotV2VsockPlatformPlanStage::Product)?;

    let HvfSnapshotV2VsockPreparedProduct {
        kind,
        base,
        vsock,
        serial_resource_present,
        binding_keys,
        vsock_key_index,
    } = product;
    let following_input = vsock.as_ref().map(project_pci_vsock).transpose()?;
    check_cancelled(is_cancelled, HvfSnapshotV2VsockPlatformPlanStage::Vsock)?;
    let resource_key = vsock_key_index
        .map(|index| {
            binding_keys
                .get(index)
                .ok_or(PrepareHvfSnapshotV2VsockPlatformPlanError::Manifest)
                .and_then(|key| reserve.clone_key(key).map_err(map_network_error))
        })
        .transpose()?;
    let mut mapped_cancel = |stage| exact_stage(stage).is_some_and(&mut *is_cancelled);
    let (base, following) = prepare_network_pci_platform_plan(
        platform,
        base,
        following_input,
        reserve,
        &mut mapped_cancel,
    )
    .map_err(map_network_error)?;
    let vsock = finish_pci_vsock(vsock, resource_key, following)?;
    Ok(HvfSnapshotV2VsockPciPlatformPlan {
        kind,
        base,
        vsock,
        serial_resource_present,
        binding_keys,
    })
}

fn finish_pci_vsock(
    endpoint: Option<HvfSnapshotV2VsockPreparedEndpoint>,
    resource_key: Option<SnapshotRestoreResourceKey>,
    following: Option<HvfSnapshotV2NetworkPciFollowingEndpointPlan>,
) -> Result<Option<HvfSnapshotV2VsockPciEndpointPlan>, PrepareHvfSnapshotV2VsockPlatformPlanError> {
    match (endpoint, resource_key, following) {
        (None, None, None) => Ok(None),
        (Some(endpoint), Some(resource_key), Some(following)) => {
            let PreparedSnapshotV2VsockRestoreState::Pci(state) = endpoint.state else {
                return Err(PrepareHvfSnapshotV2VsockPlatformPlanError::TransportPolicy);
            };
            Ok(Some(HvfSnapshotV2VsockPciEndpointPlan {
                resource_key,
                config: endpoint.config,
                state,
                queue_ranges: following.queue_ranges,
                placement: following.placement,
                queue_vectors: following.queue_vectors,
                config_vector: following.config_vector,
            }))
        }
        _ => Err(PrepareHvfSnapshotV2VsockPlatformPlanError::ResourcePlan),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_sixty_four_presence_kinds_are_distinct_and_exact() {
        let mut seen = [false; 64];
        for mask in 0_u8..64 {
            let kind = HvfSnapshotV2VsockProductKind::from_presence(
                mask & STORAGE_MASK != 0,
                mask & ENTROPY_MASK != 0,
                mask & BALLOON_MASK != 0,
                mask & MEMORY_HOTPLUG_MASK != 0,
                mask & NETWORK_MASK != 0,
                mask & VSOCK_MASK != 0,
            );
            assert_eq!(kind.mask(), mask);
            assert!(!seen[usize::from(kind.mask())]);
            seen[usize::from(kind.mask())] = true;
            assert_eq!(kind.has_storage(), mask & STORAGE_MASK != 0);
            assert_eq!(kind.has_entropy(), mask & ENTROPY_MASK != 0);
            assert_eq!(kind.has_balloon(), mask & BALLOON_MASK != 0);
            assert_eq!(kind.has_memory_hotplug(), mask & MEMORY_HOTPLUG_MASK != 0);
            assert_eq!(kind.has_network(), mask & NETWORK_MASK != 0);
            assert_eq!(kind.has_vsock(), mask & VSOCK_MASK != 0);
        }
        assert!(seen.into_iter().all(|present| present));
    }
}
