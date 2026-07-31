//! Host-free exact native-v2 2.10 virtio-mem platform-product planning.

use std::fmt;

use bangbang_runtime::balloon::BalloonMmioLayout;
use bangbang_runtime::entropy::{EntropyMmioLayout, VIRTIO_RNG_QUEUE_SIZES};
use bangbang_runtime::fdt::{
    ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET, Arm64FdtPciHost, Arm64FdtRegion, Arm64FdtVirtioMmioDevice,
};
use bangbang_runtime::interrupt::GuestInterruptLine;
use bangbang_runtime::memory::{GuestAddress, GuestMemory, GuestMemoryRange};
use bangbang_runtime::memory_hotplug::{VIRTIO_MEM_QUEUE_SIZES, VirtioMemMmioLayout};
use bangbang_runtime::mmio::{MmioRegion, MmioRegionId};
use bangbang_runtime::pci::{Arm64PciAddressPlan, PciSbdf};
use bangbang_runtime::pvtime::ARM64_PVTIME_STRUCTURE_SIZE;
use bangbang_runtime::rtc::{RTC_MMIO_DEVICE_WINDOW_SIZE, RtcMmioLayout};
use bangbang_runtime::serial::SERIAL_MMIO_DEVICE_WINDOW_SIZE;
use bangbang_runtime::snapshot_balloon_v2_9::{
    PreparedSnapshotV2BalloonTransport, SnapshotV2BalloonRestorePlan,
};
use bangbang_runtime::snapshot_device_v2::{
    SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind, SnapshotV2PciDeviceState,
};
use bangbang_runtime::snapshot_device_v2_6::PreparedSnapshotV2StorageBundle;
use bangbang_runtime::snapshot_entropy_v2_8::{
    PreparedSnapshotV2EntropyTransport, SnapshotV2EntropyRestorePlan,
};
use bangbang_runtime::snapshot_memory_hotplug_v2_10::PreparedSnapshotV2MemoryHotplugTopology;
use bangbang_runtime::storage_capture::StorageDeviceOrigin;
use bangbang_runtime::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE;
use bangbang_runtime::virtio_pci::{VIRTIO_PCI_NO_VECTOR, VirtioPciEndpointPhase};

use crate::gic::{HvfGicInterruptLineAllocator, HvfGicMetadata, HvfGicMsiMetadata};
use crate::memory::{
    HvfSnapshotV2MemoryHotplugMappingPlan, PrepareHvfSnapshotV2MemoryHotplugMappingPlanError,
    prepare_hvf_snapshot_v2_memory_hotplug_mapping_plan,
};
use crate::snapshot_v2::HvfSnapshotV2PlatformState;
use crate::snapshot_v2_balloon_platform::{
    HvfSnapshotV2BalloonMmioEndpointPlan, HvfSnapshotV2BalloonPciEndpointPlan,
    PrepareHvfSnapshotV2BalloonPlatformPlanError,
    prepare_hvf_snapshot_v2_balloon_mmio_endpoint_plan,
    prepare_hvf_snapshot_v2_balloon_pci_endpoint_plan,
};
use crate::snapshot_v2_entropy_platform::{
    HvfSnapshotV2EntropyPciEndpointPlan, PrepareHvfSnapshotV2EntropyPciPlatformPlanError,
    prepare_hvf_snapshot_v2_entropy_pci_platform_plan_with_prefix,
    register_active_retained_pci_routes,
};
use crate::snapshot_v2_multi_block_platform::{
    snapshot_v2_pci_endpoint_placement, snapshot_v2_pci_endpoint_route_count,
};
use crate::snapshot_v2_platform::{
    PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID, PROCESS_SERIAL_MMIO_BASE,
};
use crate::snapshot_v2_storage_platform::{
    HvfSnapshotV2StorageMmioPlatformPlan, HvfSnapshotV2StorageMmioPlatformPrefix,
    HvfSnapshotV2StorageMmioProcessConfig, HvfSnapshotV2StoragePciPlatformPlan,
    HvfSnapshotV2StoragePciPlatformPrefix, PrepareHvfSnapshotV2StorageMmioPlatformPlanError,
    PrepareHvfSnapshotV2StoragePciPlatformPlanError, mmio_region_conflicts_with_platform,
    prepare_hvf_snapshot_v2_storage_mmio_platform_plan_with_prefix,
    prepare_hvf_snapshot_v2_storage_pci_platform_plan_with_prefix,
    queue_ranges_conflict_with_pci_platform, queue_ranges_conflict_with_platform,
    register_active_pci_routes,
};
use crate::startup::{PCI_ENDPOINT_SLOT_COUNT, pci_memory_hotplug_restore_gic_msi_configuration};

const REDACTED: &str = "<redacted>";
const MIB: u64 = 1024 * 1024;

/// One admitted exact-2.10 product with required virtio-mem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfSnapshotV2MemoryHotplugProductKind {
    SerialMemoryHotplug,
    SerialStorageMemoryHotplug,
    SerialEntropyMemoryHotplug,
    SerialStorageEntropyMemoryHotplug,
    SerialBalloonMemoryHotplug,
    SerialBalloonStorageMemoryHotplug,
    SerialBalloonEntropyMemoryHotplug,
    SerialBalloonStorageEntropyMemoryHotplug,
}

pub(crate) enum HvfSnapshotV2MemoryHotplugPreparedProductParts {
    Base {
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
    },
    Storage {
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        storage: PreparedSnapshotV2StorageBundle,
    },
    Entropy {
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        entropy: SnapshotV2EntropyRestorePlan,
    },
    StorageEntropy {
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        storage: PreparedSnapshotV2StorageBundle,
        entropy: SnapshotV2EntropyRestorePlan,
    },
    Balloon {
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        balloon: SnapshotV2BalloonRestorePlan,
    },
    BalloonStorage {
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
    },
    BalloonEntropy {
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        balloon: SnapshotV2BalloonRestorePlan,
        entropy: SnapshotV2EntropyRestorePlan,
    },
    BalloonStorageEntropy {
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
        entropy: SnapshotV2EntropyRestorePlan,
    },
}

/// One closed set of prepared exact-2.10 component continuations.
///
/// Construction consumes the topology, its materialized memory, and every
/// optional detached component into one tagged shape.
pub struct HvfSnapshotV2MemoryHotplugPreparedProduct {
    parts: HvfSnapshotV2MemoryHotplugPreparedProductParts,
}

impl HvfSnapshotV2MemoryHotplugPreparedProduct {
    pub fn serial_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2MemoryHotplugPreparedProductParts::Base { topology, memory },
        }
    }

    pub fn serial_storage_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        storage: PreparedSnapshotV2StorageBundle,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2MemoryHotplugPreparedProductParts::Storage {
                topology,
                memory,
                storage,
            },
        }
    }

    pub fn serial_entropy_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        entropy: SnapshotV2EntropyRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2MemoryHotplugPreparedProductParts::Entropy {
                topology,
                memory,
                entropy,
            },
        }
    }

    pub fn serial_storage_entropy_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        storage: PreparedSnapshotV2StorageBundle,
        entropy: SnapshotV2EntropyRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2MemoryHotplugPreparedProductParts::StorageEntropy {
                topology,
                memory,
                storage,
                entropy,
            },
        }
    }

    pub fn serial_balloon_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        balloon: SnapshotV2BalloonRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2MemoryHotplugPreparedProductParts::Balloon {
                topology,
                memory,
                balloon,
            },
        }
    }

    pub fn serial_balloon_storage_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonStorage {
                topology,
                memory,
                balloon,
                storage,
            },
        }
    }

    pub fn serial_balloon_entropy_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        balloon: SnapshotV2BalloonRestorePlan,
        entropy: SnapshotV2EntropyRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonEntropy {
                topology,
                memory,
                balloon,
                entropy,
            },
        }
    }

    pub fn serial_balloon_storage_entropy_memory_hotplug(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
        entropy: SnapshotV2EntropyRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonStorageEntropy {
                topology,
                memory,
                balloon,
                storage,
                entropy,
            },
        }
    }

    pub const fn kind(&self) -> HvfSnapshotV2MemoryHotplugProductKind {
        match self.parts {
            HvfSnapshotV2MemoryHotplugPreparedProductParts::Base { .. } => {
                HvfSnapshotV2MemoryHotplugProductKind::SerialMemoryHotplug
            }
            HvfSnapshotV2MemoryHotplugPreparedProductParts::Storage { .. } => {
                HvfSnapshotV2MemoryHotplugProductKind::SerialStorageMemoryHotplug
            }
            HvfSnapshotV2MemoryHotplugPreparedProductParts::Entropy { .. } => {
                HvfSnapshotV2MemoryHotplugProductKind::SerialEntropyMemoryHotplug
            }
            HvfSnapshotV2MemoryHotplugPreparedProductParts::StorageEntropy { .. } => {
                HvfSnapshotV2MemoryHotplugProductKind::SerialStorageEntropyMemoryHotplug
            }
            HvfSnapshotV2MemoryHotplugPreparedProductParts::Balloon { .. } => {
                HvfSnapshotV2MemoryHotplugProductKind::SerialBalloonMemoryHotplug
            }
            HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonStorage { .. } => {
                HvfSnapshotV2MemoryHotplugProductKind::SerialBalloonStorageMemoryHotplug
            }
            HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonEntropy { .. } => {
                HvfSnapshotV2MemoryHotplugProductKind::SerialBalloonEntropyMemoryHotplug
            }
            HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonStorageEntropy { .. } => {
                HvfSnapshotV2MemoryHotplugProductKind::SerialBalloonStorageEntropyMemoryHotplug
            }
        }
    }

    fn topology(&self) -> &PreparedSnapshotV2MemoryHotplugTopology {
        match &self.parts {
            HvfSnapshotV2MemoryHotplugPreparedProductParts::Base { topology, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::Storage { topology, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::Entropy { topology, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::StorageEntropy { topology, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::Balloon { topology, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonStorage { topology, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonEntropy { topology, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonStorageEntropy {
                topology,
                ..
            } => topology,
        }
    }

    fn memory(&self) -> &GuestMemory {
        match &self.parts {
            HvfSnapshotV2MemoryHotplugPreparedProductParts::Base { memory, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::Storage { memory, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::Entropy { memory, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::StorageEntropy { memory, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::Balloon { memory, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonStorage { memory, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonEntropy { memory, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonStorageEntropy {
                memory,
                ..
            } => memory,
        }
    }

    fn storage(&self) -> Option<&PreparedSnapshotV2StorageBundle> {
        match &self.parts {
            HvfSnapshotV2MemoryHotplugPreparedProductParts::Storage { storage, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::StorageEntropy { storage, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonStorage { storage, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonStorageEntropy {
                storage,
                ..
            } => Some(storage),
            HvfSnapshotV2MemoryHotplugPreparedProductParts::Base { .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::Entropy { .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::Balloon { .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonEntropy { .. } => None,
        }
    }

    fn entropy(&self) -> Option<&SnapshotV2EntropyRestorePlan> {
        match &self.parts {
            HvfSnapshotV2MemoryHotplugPreparedProductParts::Entropy { entropy, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::StorageEntropy { entropy, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonEntropy { entropy, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonStorageEntropy {
                entropy,
                ..
            } => Some(entropy),
            HvfSnapshotV2MemoryHotplugPreparedProductParts::Base { .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::Storage { .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::Balloon { .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonStorage { .. } => None,
        }
    }

    fn balloon(&self) -> Option<&SnapshotV2BalloonRestorePlan> {
        match &self.parts {
            HvfSnapshotV2MemoryHotplugPreparedProductParts::Balloon { balloon, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonStorage { balloon, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonEntropy { balloon, .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::BalloonStorageEntropy {
                balloon,
                ..
            } => Some(balloon),
            HvfSnapshotV2MemoryHotplugPreparedProductParts::Base { .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::Storage { .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::Entropy { .. }
            | HvfSnapshotV2MemoryHotplugPreparedProductParts::StorageEntropy { .. } => None,
        }
    }

    pub(crate) fn into_parts(self) -> HvfSnapshotV2MemoryHotplugPreparedProductParts {
        self.parts
    }
}

impl fmt::Debug for HvfSnapshotV2MemoryHotplugPreparedProduct {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2MemoryHotplugPreparedProduct")
            .field("kind", &self.kind())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Canonical destination layouts for one memory-hotplug-aware MMIO process.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2MemoryHotplugMmioProcessConfig {
    balloon_layout: BalloonMmioLayout,
    storage: HvfSnapshotV2StorageMmioProcessConfig,
    entropy_layout: EntropyMmioLayout,
    memory_hotplug_layout: VirtioMemMmioLayout,
}

impl HvfSnapshotV2MemoryHotplugMmioProcessConfig {
    pub const fn new(
        balloon_layout: BalloonMmioLayout,
        storage: HvfSnapshotV2StorageMmioProcessConfig,
        entropy_layout: EntropyMmioLayout,
        memory_hotplug_layout: VirtioMemMmioLayout,
    ) -> Self {
        Self {
            balloon_layout,
            storage,
            entropy_layout,
            memory_hotplug_layout,
        }
    }

    pub const fn balloon_layout(self) -> BalloonMmioLayout {
        self.balloon_layout
    }

    pub const fn storage(self) -> HvfSnapshotV2StorageMmioProcessConfig {
        self.storage
    }

    pub const fn entropy_layout(self) -> EntropyMmioLayout {
        self.entropy_layout
    }

    pub const fn memory_hotplug_layout(self) -> VirtioMemMmioLayout {
        self.memory_hotplug_layout
    }
}

impl fmt::Debug for HvfSnapshotV2MemoryHotplugMmioProcessConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2MemoryHotplugMmioProcessConfig")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Exact typed placement of one restored MMIO entropy endpoint.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2MemoryHotplugEntropyMmioEndpointPlan {
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    fdt_device: Arm64FdtVirtioMmioDevice,
}

impl HvfSnapshotV2MemoryHotplugEntropyMmioEndpointPlan {
    pub const fn region(self) -> MmioRegion {
        self.region
    }

    pub const fn interrupt_line(self) -> GuestInterruptLine {
        self.interrupt_line
    }

    pub const fn fdt_device(self) -> Arm64FdtVirtioMmioDevice {
        self.fdt_device
    }
}

impl fmt::Debug for HvfSnapshotV2MemoryHotplugEntropyMmioEndpointPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2MemoryHotplugEntropyMmioEndpointPlan")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Exact typed placement of the required restored MMIO virtio-mem endpoint.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2MemoryHotplugMmioEndpointPlan {
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    fdt_device: Arm64FdtVirtioMmioDevice,
}

impl HvfSnapshotV2MemoryHotplugMmioEndpointPlan {
    pub const fn region(self) -> MmioRegion {
        self.region
    }

    pub const fn interrupt_line(self) -> GuestInterruptLine {
        self.interrupt_line
    }

    pub const fn fdt_device(self) -> Arm64FdtVirtioMmioDevice {
        self.fdt_device
    }
}

impl fmt::Debug for HvfSnapshotV2MemoryHotplugMmioEndpointPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2MemoryHotplugMmioEndpointPlan")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Exact typed placement of the required restored PCI virtio-mem endpoint.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2MemoryHotplugPciEndpointPlan {
    origin: StorageDeviceOrigin,
    sbdf: PciSbdf,
    bar_region_id: MmioRegionId,
    bar_range: GuestMemoryRange,
    route_count: usize,
    msi_interrupt_count: u32,
}

impl HvfSnapshotV2MemoryHotplugPciEndpointPlan {
    pub const fn origin(self) -> StorageDeviceOrigin {
        self.origin
    }

    pub const fn sbdf(self) -> PciSbdf {
        self.sbdf
    }

    pub const fn bar_region_id(self) -> MmioRegionId {
        self.bar_region_id
    }

    pub const fn bar_range(self) -> GuestMemoryRange {
        self.bar_range
    }

    pub const fn route_count(self) -> usize {
        self.route_count
    }

    pub const fn msi_interrupt_count(self) -> u32 {
        self.msi_interrupt_count
    }
}

impl fmt::Debug for HvfSnapshotV2MemoryHotplugPciEndpointPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2MemoryHotplugPciEndpointPlan")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete immutable exact-2.10 MMIO product proof.
pub struct HvfSnapshotV2MemoryHotplugMmioPlatformPlan {
    product: HvfSnapshotV2MemoryHotplugPreparedProduct,
    mapping: HvfSnapshotV2MemoryHotplugMappingPlan,
    balloon: Option<HvfSnapshotV2BalloonMmioEndpointPlan>,
    storage: Option<HvfSnapshotV2StorageMmioPlatformPlan>,
    entropy: Option<HvfSnapshotV2MemoryHotplugEntropyMmioEndpointPlan>,
    memory_hotplug: HvfSnapshotV2MemoryHotplugMmioEndpointPlan,
    serial_interrupt: GuestInterruptLine,
    vmgenid_interrupt: GuestInterruptLine,
    vmclock_interrupt: GuestInterruptLine,
}

pub(crate) struct HvfSnapshotV2MemoryHotplugMmioPlatformPlanParts {
    pub(crate) product: HvfSnapshotV2MemoryHotplugPreparedProduct,
    pub(crate) mapping: HvfSnapshotV2MemoryHotplugMappingPlan,
    pub(crate) balloon: Option<HvfSnapshotV2BalloonMmioEndpointPlan>,
    pub(crate) storage: Option<HvfSnapshotV2StorageMmioPlatformPlan>,
    pub(crate) entropy: Option<HvfSnapshotV2MemoryHotplugEntropyMmioEndpointPlan>,
    pub(crate) memory_hotplug: HvfSnapshotV2MemoryHotplugMmioEndpointPlan,
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

impl HvfSnapshotV2MemoryHotplugMmioPlatformPlan {
    pub const fn kind(&self) -> HvfSnapshotV2MemoryHotplugProductKind {
        self.product.kind()
    }

    pub const fn mapping(&self) -> &HvfSnapshotV2MemoryHotplugMappingPlan {
        &self.mapping
    }

    pub const fn balloon(&self) -> Option<HvfSnapshotV2BalloonMmioEndpointPlan> {
        self.balloon
    }

    pub const fn storage(&self) -> Option<&HvfSnapshotV2StorageMmioPlatformPlan> {
        self.storage.as_ref()
    }

    pub const fn entropy(&self) -> Option<HvfSnapshotV2MemoryHotplugEntropyMmioEndpointPlan> {
        self.entropy
    }

    pub const fn memory_hotplug(&self) -> HvfSnapshotV2MemoryHotplugMmioEndpointPlan {
        self.memory_hotplug
    }

    pub const fn serial_interrupt(&self) -> GuestInterruptLine {
        self.serial_interrupt
    }

    pub const fn vmgenid_interrupt(&self) -> GuestInterruptLine {
        self.vmgenid_interrupt
    }

    pub const fn vmclock_interrupt(&self) -> GuestInterruptLine {
        self.vmclock_interrupt
    }

    pub(crate) fn into_parts(self) -> HvfSnapshotV2MemoryHotplugMmioPlatformPlanParts {
        HvfSnapshotV2MemoryHotplugMmioPlatformPlanParts {
            product: self.product,
            mapping: self.mapping,
            balloon: self.balloon,
            storage: self.storage,
            entropy: self.entropy,
            memory_hotplug: self.memory_hotplug,
            serial_interrupt: self.serial_interrupt,
            vmgenid_interrupt: self.vmgenid_interrupt,
            vmclock_interrupt: self.vmclock_interrupt,
        }
    }
}

impl fmt::Debug for HvfSnapshotV2MemoryHotplugMmioPlatformPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2MemoryHotplugMmioPlatformPlan")
            .field("kind", &self.kind())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete immutable exact-2.10 PCI product proof.
pub struct HvfSnapshotV2MemoryHotplugPciPlatformPlan {
    product: HvfSnapshotV2MemoryHotplugPreparedProduct,
    mapping: HvfSnapshotV2MemoryHotplugMappingPlan,
    balloon: Option<HvfSnapshotV2BalloonPciEndpointPlan>,
    storage: Option<HvfSnapshotV2StoragePciPlatformPlan>,
    entropy: Option<HvfSnapshotV2EntropyPciEndpointPlan>,
    memory_hotplug: HvfSnapshotV2MemoryHotplugPciEndpointPlan,
    host: Arm64FdtPciHost,
    msi: HvfGicMsiMetadata,
    endpoint_count: usize,
    route_demand: usize,
    serial_interrupt: GuestInterruptLine,
    vmgenid_interrupt: GuestInterruptLine,
    vmclock_interrupt: GuestInterruptLine,
}

pub(crate) struct HvfSnapshotV2MemoryHotplugPciPlatformPlanParts {
    pub(crate) product: HvfSnapshotV2MemoryHotplugPreparedProduct,
    pub(crate) mapping: HvfSnapshotV2MemoryHotplugMappingPlan,
    pub(crate) balloon: Option<HvfSnapshotV2BalloonPciEndpointPlan>,
    pub(crate) storage: Option<HvfSnapshotV2StoragePciPlatformPlan>,
    pub(crate) entropy: Option<HvfSnapshotV2EntropyPciEndpointPlan>,
    pub(crate) memory_hotplug: HvfSnapshotV2MemoryHotplugPciEndpointPlan,
    pub(crate) host: Arm64FdtPciHost,
    pub(crate) msi: HvfGicMsiMetadata,
    pub(crate) endpoint_count: usize,
    pub(crate) route_demand: usize,
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

impl HvfSnapshotV2MemoryHotplugPciPlatformPlan {
    pub const fn kind(&self) -> HvfSnapshotV2MemoryHotplugProductKind {
        self.product.kind()
    }

    pub const fn mapping(&self) -> &HvfSnapshotV2MemoryHotplugMappingPlan {
        &self.mapping
    }

    pub const fn balloon(&self) -> Option<HvfSnapshotV2BalloonPciEndpointPlan> {
        self.balloon
    }

    pub const fn storage(&self) -> Option<&HvfSnapshotV2StoragePciPlatformPlan> {
        self.storage.as_ref()
    }

    pub const fn entropy(&self) -> Option<HvfSnapshotV2EntropyPciEndpointPlan> {
        self.entropy
    }

    pub const fn memory_hotplug(&self) -> HvfSnapshotV2MemoryHotplugPciEndpointPlan {
        self.memory_hotplug
    }

    pub const fn host(&self) -> Arm64FdtPciHost {
        self.host
    }

    pub const fn msi(&self) -> HvfGicMsiMetadata {
        self.msi
    }

    pub const fn endpoint_count(&self) -> usize {
        self.endpoint_count
    }

    pub const fn route_demand(&self) -> usize {
        self.route_demand
    }

    pub const fn serial_interrupt(&self) -> GuestInterruptLine {
        self.serial_interrupt
    }

    pub const fn vmgenid_interrupt(&self) -> GuestInterruptLine {
        self.vmgenid_interrupt
    }

    pub const fn vmclock_interrupt(&self) -> GuestInterruptLine {
        self.vmclock_interrupt
    }

    pub(crate) fn into_parts(self) -> HvfSnapshotV2MemoryHotplugPciPlatformPlanParts {
        HvfSnapshotV2MemoryHotplugPciPlatformPlanParts {
            product: self.product,
            mapping: self.mapping,
            balloon: self.balloon,
            storage: self.storage,
            entropy: self.entropy,
            memory_hotplug: self.memory_hotplug,
            host: self.host,
            msi: self.msi,
            endpoint_count: self.endpoint_count,
            route_demand: self.route_demand,
            serial_interrupt: self.serial_interrupt,
            vmgenid_interrupt: self.vmgenid_interrupt,
            vmclock_interrupt: self.vmclock_interrupt,
        }
    }
}

impl fmt::Debug for HvfSnapshotV2MemoryHotplugPciPlatformPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2MemoryHotplugPciPlatformPlan")
            .field("kind", &self.kind())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Redacted rejection from exact-2.10 memory-hotplug product planning.
pub enum PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError {
    PlatformProfile,
    Binding,
    TransportPolicy,
    Mapping(Box<PrepareHvfSnapshotV2MemoryHotplugMappingPlanError>),
    ResourcePlan,
    PciCapacity { count: usize, maximum: usize },
    RangeConflict,
    RouteConflict,
    Allocation,
    Balloon(Box<PrepareHvfSnapshotV2BalloonPlatformPlanError>),
    StorageMmio(Box<PrepareHvfSnapshotV2StorageMmioPlatformPlanError>),
    StoragePci(Box<PrepareHvfSnapshotV2StoragePciPlatformPlanError>),
    EntropyPci(Box<PrepareHvfSnapshotV2EntropyPciPlatformPlanError>),
}

impl fmt::Debug for PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self {
            Self::PlatformProfile => "platform-profile",
            Self::Binding => "binding",
            Self::TransportPolicy => "transport-policy",
            Self::Mapping(_) => "mapping",
            Self::ResourcePlan => "resource-plan",
            Self::PciCapacity { .. } => "pci-capacity",
            Self::RangeConflict => "range-conflict",
            Self::RouteConflict => "route-conflict",
            Self::Allocation => "allocation",
            Self::Balloon(_) => "balloon",
            Self::StorageMmio(_) => "storage-mmio",
            Self::StoragePci(_) => "storage-pci",
            Self::EntropyPci(_) => "entropy-pci",
        };
        formatter
            .debug_struct("PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError")
            .field("category", &category)
            .field("state", &REDACTED)
            .finish()
    }
}

impl fmt::Display for PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PlatformProfile => "native-v2 memory-hotplug platform profile is not canonical",
            Self::Binding => "native-v2 memory-hotplug platform binding is inconsistent",
            Self::TransportPolicy => "native-v2 memory-hotplug product transport is inconsistent",
            Self::Mapping(_) => "native-v2 memory-hotplug mapping planning failed",
            Self::ResourcePlan => "native-v2 memory-hotplug platform resources are inconsistent",
            Self::PciCapacity { .. } => {
                "native-v2 memory-hotplug PCI endpoint capacity is exceeded"
            }
            Self::RangeConflict => "native-v2 memory-hotplug product ranges overlap another owner",
            Self::RouteConflict => "native-v2 memory-hotplug active MSI routes are inconsistent",
            Self::Allocation => "native-v2 memory-hotplug temporary inventory allocation failed",
            Self::Balloon(_) => "native-v2 memory-hotplug balloon planning failed",
            Self::StorageMmio(_) => "native-v2 memory-hotplug MMIO storage planning failed",
            Self::StoragePci(_) => "native-v2 memory-hotplug PCI storage planning failed",
            Self::EntropyPci(_) => "native-v2 memory-hotplug PCI entropy planning failed",
        })
    }
}

impl std::error::Error for PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Mapping(source) => Some(source),
            Self::Balloon(source) => Some(source),
            Self::StorageMmio(source) => Some(source),
            Self::StoragePci(source) => Some(source),
            Self::EntropyPci(source) => Some(source),
            Self::PlatformProfile
            | Self::Binding
            | Self::TransportPolicy
            | Self::ResourcePlan
            | Self::PciCapacity { .. }
            | Self::RangeConflict
            | Self::RouteConflict
            | Self::Allocation => None,
        }
    }
}

trait MemoryHotplugPlatformPlanReserve {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError>;
}

struct SystemMemoryHotplugPlatformPlanReserve;

impl MemoryHotplugPlatformPlanReserve for SystemMemoryHotplugPlatformPlanReserve {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError> {
        values
            .try_reserve_exact(additional)
            .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::Allocation)
    }
}

/// Proves one complete virtio-mem-bearing MMIO product before live ownership.
pub fn prepare_hvf_snapshot_v2_memory_hotplug_mmio_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2MemoryHotplugPreparedProduct,
    process: HvfSnapshotV2MemoryHotplugMmioProcessConfig,
) -> Result<
    HvfSnapshotV2MemoryHotplugMmioPlatformPlan,
    PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError,
> {
    prepare_memory_hotplug_mmio_platform_plan(
        platform,
        product,
        process,
        &mut SystemMemoryHotplugPlatformPlanReserve,
    )
}

/// Proves one complete virtio-mem-bearing PCI product before live ownership.
pub fn prepare_hvf_snapshot_v2_memory_hotplug_pci_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2MemoryHotplugPreparedProduct,
) -> Result<
    HvfSnapshotV2MemoryHotplugPciPlatformPlan,
    PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError,
> {
    prepare_memory_hotplug_pci_platform_plan(
        platform,
        product,
        &mut SystemMemoryHotplugPlatformPlanReserve,
    )
}

fn prepare_memory_hotplug_mmio_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2MemoryHotplugPreparedProduct,
    process: HvfSnapshotV2MemoryHotplugMmioProcessConfig,
    reserve: &mut impl MemoryHotplugPlatformPlanReserve,
) -> Result<
    HvfSnapshotV2MemoryHotplugMmioPlatformPlan,
    PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError,
> {
    validate_product_profile_and_binding(platform, &product)?;
    let gic = platform.global().compatibility().gic_metadata();
    if gic.msi.is_some() || !product_has_transport(&product, SnapshotV2DeviceTransportKind::Mmio) {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::TransportPolicy);
    }

    let mapping = prepare_product_mapping(platform, &product)?;
    let aperture = mapping.reservation().range();
    if range_conflicts_with_fixed_platform(platform, aperture, &gic, None)? {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::RangeConflict);
    }

    let balloon = product
        .balloon()
        .map(|balloon| {
            prepare_hvf_snapshot_v2_balloon_mmio_endpoint_plan(
                platform,
                balloon,
                process.balloon_layout(),
            )
            .map_err(|source| {
                PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::Balloon(Box::new(source))
            })
        })
        .transpose()?;
    let entropy = product
        .entropy()
        .map(|entropy| prepare_entropy_mmio_endpoint(platform, entropy, process.entropy_layout()))
        .transpose()?;
    let memory_hotplug = prepare_memory_hotplug_mmio_endpoint(
        platform,
        product.topology(),
        process.memory_hotplug_layout(),
    )?;

    let prefix = balloon.map_or(HvfSnapshotV2StorageMmioPlatformPrefix::EMPTY, |endpoint| {
        HvfSnapshotV2StorageMmioPlatformPrefix::one(endpoint.region(), endpoint.interrupt_line())
    });
    let memory_hotplug_interrupts = [memory_hotplug.interrupt_line()];
    let entropy_memory_hotplug_interrupts =
        entropy.map(|entropy| [entropy.interrupt_line(), memory_hotplug.interrupt_line()]);
    let following_interrupts = entropy_memory_hotplug_interrupts
        .as_ref()
        .map_or(&memory_hotplug_interrupts[..], |interrupts| &interrupts[..]);
    let storage = product
        .storage()
        .map(|storage| {
            prepare_hvf_snapshot_v2_storage_mmio_platform_plan_with_prefix(
                platform,
                storage,
                process.storage(),
                prefix,
                following_interrupts,
            )
            .map_err(|source| {
                PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::StorageMmio(Box::new(source))
            })
        })
        .transpose()?;

    let (serial_interrupt, vmgenid_interrupt, vmclock_interrupt) =
        validate_mmio_interrupt_sequence(
            platform,
            balloon,
            storage.as_ref(),
            entropy,
            memory_hotplug,
        )?;

    let region_count = usize::from(balloon.is_some())
        .checked_add(storage_record_count_mmio(storage.as_ref()))
        .and_then(|count| count.checked_add(usize::from(entropy.is_some())))
        .and_then(|count| count.checked_add(1))
        .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let queue_count = product_queue_range_count(&product)?;
    let pmem_count = product
        .storage()
        .map_or(0, |storage| storage.pmem_records().len());
    let mut regions = Vec::new();
    let mut queues = Vec::new();
    let mut pmem = Vec::new();
    reserve.reserve(&mut regions, region_count)?;
    reserve.reserve(&mut queues, queue_count)?;
    reserve.reserve(&mut pmem, pmem_count)?;
    if let Some(balloon) = balloon {
        regions.push(balloon.region());
    }
    if let Some(storage) = &storage {
        regions.extend(
            storage
                .block_records()
                .iter()
                .chain(storage.pmem_records())
                .map(|record| record.region()),
        );
    }
    if let Some(entropy) = entropy {
        regions.push(entropy.region());
    }
    regions.push(memory_hotplug.region());
    append_product_memory_ranges(&product, &mut queues, &mut pmem);
    if !aggregate_memory_ranges_are_valid(&mapping, aperture, &regions, &queues, &pmem) {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::RangeConflict);
    }

    Ok(HvfSnapshotV2MemoryHotplugMmioPlatformPlan {
        product,
        mapping,
        balloon,
        storage,
        entropy,
        memory_hotplug,
        serial_interrupt,
        vmgenid_interrupt,
        vmclock_interrupt,
    })
}

fn validate_product_profile_and_binding(
    platform: &HvfSnapshotV2PlatformState,
    product: &HvfSnapshotV2MemoryHotplugPreparedProduct,
) -> Result<(), PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError> {
    if !platform.machine().fdt().is_product_process_profile()
        || !platform.machine().machine().track_dirty_pages()
        || platform.time().rtc_layout()
            != RtcMmioLayout::new(PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID)
    {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::PlatformProfile);
    }
    if platform.memory() != product.topology().memory().binding() {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::Binding);
    }
    Ok(())
}

fn prepare_product_mapping(
    platform: &HvfSnapshotV2PlatformState,
    product: &HvfSnapshotV2MemoryHotplugPreparedProduct,
) -> Result<HvfSnapshotV2MemoryHotplugMappingPlan, PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError>
{
    let expected_base_bytes = platform
        .machine()
        .machine()
        .mem_size_mib()
        .checked_mul(MIB)
        .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    prepare_hvf_snapshot_v2_memory_hotplug_mapping_plan(
        product.topology(),
        product.memory(),
        expected_base_bytes,
    )
    .map_err(|source| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::Mapping(Box::new(source)))
}

fn product_has_transport(
    product: &HvfSnapshotV2MemoryHotplugPreparedProduct,
    expected: SnapshotV2DeviceTransportKind,
) -> bool {
    product.topology().state().transport().kind() == expected
        && product
            .storage()
            .is_none_or(|storage| storage.transport_kind() == expected)
        && product
            .entropy()
            .is_none_or(|entropy| entropy.transport_kind() == expected)
        && product
            .balloon()
            .is_none_or(|balloon| balloon.transport_kind() == expected)
}

fn prepare_entropy_mmio_endpoint(
    platform: &HvfSnapshotV2PlatformState,
    entropy: &SnapshotV2EntropyRestorePlan,
    layout: EntropyMmioLayout,
) -> Result<
    HvfSnapshotV2MemoryHotplugEntropyMmioEndpointPlan,
    PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError,
> {
    let PreparedSnapshotV2EntropyTransport::Mmio(transport) = entropy.transport() else {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::TransportPolicy);
    };
    let expected_region = mmio_region(layout.region_id(), layout.address())?;
    if transport.region() != expected_region {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan);
    }
    if mmio_region_conflicts_with_platform(
        platform,
        expected_region,
        &platform.global().compatibility().gic_metadata(),
    )
    .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?
        || queue_ranges_conflict_with_platform(platform, entropy.queue_ranges())
            .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::RangeConflict);
    }
    Ok(HvfSnapshotV2MemoryHotplugEntropyMmioEndpointPlan {
        region: expected_region,
        interrupt_line: transport.interrupt_line(),
        fdt_device: mmio_fdt_device(expected_region, transport.interrupt_line()),
    })
}

fn prepare_memory_hotplug_mmio_endpoint(
    platform: &HvfSnapshotV2PlatformState,
    topology: &PreparedSnapshotV2MemoryHotplugTopology,
    layout: VirtioMemMmioLayout,
) -> Result<
    HvfSnapshotV2MemoryHotplugMmioEndpointPlan,
    PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError,
> {
    let SnapshotV2DeviceTransport::Mmio(transport) = topology.state().transport() else {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::TransportPolicy);
    };
    let expected_region = mmio_region(layout.region_id(), layout.address())?;
    if transport.region() != expected_region {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan);
    }
    if mmio_region_conflicts_with_platform(
        platform,
        expected_region,
        &platform.global().compatibility().gic_metadata(),
    )
    .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?
        || queue_ranges_conflict_with_platform(platform, topology.queue_ranges())
            .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::RangeConflict);
    }
    Ok(HvfSnapshotV2MemoryHotplugMmioEndpointPlan {
        region: expected_region,
        interrupt_line: transport.interrupt_line(),
        fdt_device: mmio_fdt_device(expected_region, transport.interrupt_line()),
    })
}

fn validate_mmio_interrupt_sequence(
    platform: &HvfSnapshotV2PlatformState,
    balloon: Option<HvfSnapshotV2BalloonMmioEndpointPlan>,
    storage: Option<&HvfSnapshotV2StorageMmioPlatformPlan>,
    entropy: Option<HvfSnapshotV2MemoryHotplugEntropyMmioEndpointPlan>,
    memory_hotplug: HvfSnapshotV2MemoryHotplugMmioEndpointPlan,
) -> Result<
    (GuestInterruptLine, GuestInterruptLine, GuestInterruptLine),
    PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError,
> {
    let mut allocator = HvfGicInterruptLineAllocator::from_metadata(
        &platform.global().compatibility().gic_metadata(),
    )
    .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    if let Some(balloon) = balloon
        && allocator
            .allocate()
            .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?
            != balloon.interrupt_line()
    {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan);
    }
    if let Some(storage) = storage {
        for record in storage.block_records().iter().chain(storage.pmem_records()) {
            if allocator
                .allocate()
                .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?
                != record.interrupt_line()
            {
                return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan);
            }
        }
    }
    if let Some(entropy) = entropy
        && allocator
            .allocate()
            .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?
            != entropy.interrupt_line()
    {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan);
    }
    if allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?
        != memory_hotplug.interrupt_line()
    {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan);
    }
    let serial = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let vmgenid = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let vmclock = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    if storage.is_some_and(|storage| {
        storage.serial_interrupt() != serial
            || storage.vmgenid_interrupt() != vmgenid
            || storage.vmclock_interrupt() != vmclock
    }) || platform.time().vmgenid().interrupt_line() != vmgenid
        || platform.time().vmclock().interrupt_line() != vmclock
    {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan);
    }
    Ok((serial, vmgenid, vmclock))
}

fn mmio_region(
    region_id: MmioRegionId,
    address: GuestAddress,
) -> Result<MmioRegion, PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError> {
    MmioRegion::new(region_id, address, VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
        .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)
}

fn mmio_fdt_device(
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
) -> Arm64FdtVirtioMmioDevice {
    Arm64FdtVirtioMmioDevice {
        region: Arm64FdtRegion {
            base: region.range().start().raw_value(),
            size: region.range().size(),
        },
        interrupt_line,
    }
}

fn storage_record_count_mmio(storage: Option<&HvfSnapshotV2StorageMmioPlatformPlan>) -> usize {
    storage.map_or(0, |storage| {
        storage.block_records().len() + storage.pmem_records().len()
    })
}

fn product_queue_range_count(
    product: &HvfSnapshotV2MemoryHotplugPreparedProduct,
) -> Result<usize, PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError> {
    let mut queue_count = product
        .balloon()
        .map_or(0, |balloon| balloon.queue_ranges().len())
        .checked_mul(3)
        .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    if let Some(storage) = product.storage() {
        let storage_queues = storage
            .block_bundle()
            .map_or(&[][..], |block| block.records())
            .iter()
            .filter(|record| record.queue_ranges().is_some())
            .count()
            .checked_add(
                storage
                    .pmem_records()
                    .iter()
                    .filter(|record| record.queue_ranges().is_some())
                    .count(),
            )
            .and_then(|count| count.checked_mul(3))
            .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
        queue_count = queue_count
            .checked_add(storage_queues)
            .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    }
    for configured in [
        product
            .entropy()
            .is_some_and(|entropy| entropy.queue_ranges().is_some()),
        product.topology().queue_ranges().is_some(),
    ] {
        if configured {
            queue_count = queue_count
                .checked_add(3)
                .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
        }
    }
    Ok(queue_count)
}

fn append_product_memory_ranges(
    product: &HvfSnapshotV2MemoryHotplugPreparedProduct,
    queues: &mut Vec<GuestMemoryRange>,
    pmem: &mut Vec<GuestMemoryRange>,
) {
    if let Some(balloon) = product.balloon() {
        for ranges in balloon.queue_ranges() {
            queues.extend_from_slice(ranges);
        }
    }
    if let Some(storage) = product.storage() {
        for record in storage
            .block_bundle()
            .map_or(&[][..], |block| block.records())
        {
            if let Some(ranges) = record.queue_ranges() {
                queues.extend_from_slice(&ranges);
            }
        }
        for record in storage.pmem_records() {
            if let Some(ranges) = record.queue_ranges() {
                queues.extend_from_slice(&ranges);
            }
            pmem.push(record.prepared_device().guest_range());
        }
    }
    if let Some(ranges) = product.entropy().and_then(|entropy| entropy.queue_ranges()) {
        queues.extend_from_slice(&ranges);
    }
    if let Some(ranges) = product.topology().queue_ranges() {
        queues.extend_from_slice(&ranges);
    }
}

fn aggregate_memory_ranges_are_valid(
    mapping: &HvfSnapshotV2MemoryHotplugMappingPlan,
    aperture: GuestMemoryRange,
    regions: &[MmioRegion],
    queues: &[GuestMemoryRange],
    pmem: &[GuestMemoryRange],
) -> bool {
    for (index, region) in regions.iter().enumerate() {
        if regions.iter().take(index).any(|previous| {
            previous.id() == region.id() || previous.range().overlaps(region.range())
        }) || region.range().overlaps(aperture)
            || mapping
                .static_ranges()
                .iter()
                .any(|range| region.range().overlaps(*range))
            || queues.iter().any(|queue| region.range().overlaps(*queue))
            || pmem.iter().any(|mapping| region.range().overlaps(*mapping))
        {
            return false;
        }
    }
    for (index, queue) in queues.iter().enumerate() {
        if queue.overlaps(aperture)
            || !mapping
                .static_ranges()
                .iter()
                .any(|base| range_contains(*base, *queue))
            || queues
                .iter()
                .take(index)
                .any(|previous| previous.overlaps(*queue))
            || pmem.iter().any(|mapping| mapping.overlaps(*queue))
        {
            return false;
        }
    }
    for (index, host_mapping) in pmem.iter().enumerate() {
        if host_mapping.overlaps(aperture)
            || mapping
                .static_ranges()
                .iter()
                .any(|base| base.overlaps(*host_mapping))
            || pmem
                .iter()
                .take(index)
                .any(|previous| previous.overlaps(*host_mapping))
        {
            return false;
        }
    }
    true
}

fn range_contains(container: GuestMemoryRange, candidate: GuestMemoryRange) -> bool {
    candidate.start() >= container.start() && candidate.end_exclusive() <= container.end_exclusive()
}

fn ranges_are_pairwise_disjoint(ranges: &[GuestMemoryRange]) -> bool {
    ranges.iter().enumerate().all(|(index, range)| {
        ranges
            .iter()
            .take(index)
            .all(|other| !range.overlaps(*other))
    })
}

fn range_conflicts_with_fixed_platform(
    platform: &HvfSnapshotV2PlatformState,
    range: GuestMemoryRange,
    gic: &HvfGicMetadata,
    pci: Option<Arm64PciAddressPlan>,
) -> Result<bool, PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError> {
    let fdt = platform.machine().fdt();
    let fdt_range = GuestMemoryRange::new(fdt.address(), u64::from(fdt.size()))
        .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let serial = GuestMemoryRange::new(PROCESS_SERIAL_MMIO_BASE, SERIAL_MMIO_DEVICE_WINDOW_SIZE)
        .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let rtc = GuestMemoryRange::new(PROCESS_RTC_MMIO_BASE, RTC_MMIO_DEVICE_WINDOW_SIZE)
        .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let distributor = gic_region_range(gic.distributor)?;
    let redistributor = gic_region_range(gic.redistributor.region)?;
    if [
        fdt_range,
        platform.time().vmgenid().range(),
        platform.time().vmclock().range(),
        serial,
        rtc,
        distributor,
        redistributor,
    ]
    .into_iter()
    .any(|fixed| range.overlaps(fixed))
    {
        return Ok(true);
    }
    let pvtime_size = u64::try_from(ARM64_PVTIME_STRUCTURE_SIZE)
        .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    for record in platform.time().pvtime_vcpus() {
        let pvtime = GuestMemoryRange::new(record.record_ipa(), pvtime_size)
            .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
        if range.overlaps(pvtime) {
            return Ok(true);
        }
    }
    if let Some(msi) = gic.msi
        && range.overlaps(gic_region_range(msi.region)?)
    {
        return Ok(true);
    }
    if let Some(pci) = pci
        && [pci.ecam_reservation(), pci.bar32(), pci.bar64()]
            .into_iter()
            .any(|fixed| range.overlaps(fixed))
    {
        return Ok(true);
    }
    Ok(false)
}

fn gic_region_range(
    region: crate::gic::HvfGicRegion,
) -> Result<GuestMemoryRange, PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError> {
    GuestMemoryRange::new(GuestAddress::new(region.base), region.size)
        .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)
}

fn prepare_memory_hotplug_pci_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2MemoryHotplugPreparedProduct,
    reserve: &mut impl MemoryHotplugPlatformPlanReserve,
) -> Result<
    HvfSnapshotV2MemoryHotplugPciPlatformPlan,
    PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError,
> {
    validate_product_profile_and_binding(platform, &product)?;
    if !product_has_transport(&product, SnapshotV2DeviceTransportKind::Pci) {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::TransportPolicy);
    }
    let mapping = prepare_product_mapping(platform, &product)?;
    let address_plan = Arm64PciAddressPlan::firecracker_v1_16()
        .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let host = Arm64FdtPciHost::from_address_plan(address_plan);
    let gic = platform.global().compatibility().gic_metadata();
    let msi = gic
        .msi
        .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::TransportPolicy)?;
    let aperture = mapping.reservation().range();
    if range_conflicts_with_fixed_platform(platform, aperture, &gic, Some(address_plan))? {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::RangeConflict);
    }

    let balloon_queue_count = product
        .balloon()
        .map(|balloon| {
            let PreparedSnapshotV2BalloonTransport::Pci(transport) = balloon.transport() else {
                return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::TransportPolicy);
            };
            Ok(transport.device().queue_layout().queue_count())
        })
        .transpose()?;
    let expected_msi = pci_memory_hotplug_restore_gic_msi_configuration(
        balloon_queue_count,
        product.entropy().is_some(),
    )
    .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let expected_msi_interrupt_count = expected_msi.interrupt_count().get();
    if msi.interrupt_range.count != expected_msi_interrupt_count {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan);
    }

    let balloon = product
        .balloon()
        .map(|balloon| {
            prepare_hvf_snapshot_v2_balloon_pci_endpoint_plan(
                platform,
                balloon,
                address_plan,
                msi,
                expected_msi_interrupt_count,
            )
            .map_err(|source| {
                PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::Balloon(Box::new(source))
            })
        })
        .transpose()?;
    let storage_start_slot = usize::from(balloon.is_some());
    let reserved_following = 1_usize
        .checked_add(usize::from(product.entropy().is_some()))
        .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let storage = product
        .storage()
        .map(|storage| {
            prepare_hvf_snapshot_v2_storage_pci_platform_plan_with_prefix(
                platform,
                storage,
                HvfSnapshotV2StoragePciPlatformPrefix::exact(
                    storage_start_slot,
                    reserved_following,
                    expected_msi_interrupt_count,
                ),
            )
            .map_err(|source| {
                PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::StoragePci(Box::new(source))
            })
        })
        .transpose()?;
    let storage_count = storage
        .as_ref()
        .map_or(0, |storage| storage.pci().record_count());
    let preceding_entropy = storage_start_slot
        .checked_add(storage_count)
        .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let storage_pair = match (product.storage(), storage.as_ref()) {
        (Some(bundle), Some(plan)) => Some((bundle, plan)),
        (None, None) => None,
        _ => return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan),
    };
    let entropy = product
        .entropy()
        .map(|entropy| {
            prepare_hvf_snapshot_v2_entropy_pci_platform_plan_with_prefix(
                platform,
                storage_pair,
                entropy,
                storage_start_slot,
                preceding_entropy,
                expected_msi_interrupt_count,
            )
            .map_err(|source| {
                PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::EntropyPci(Box::new(source))
            })
        })
        .transpose()?;
    let memory_hotplug_slot = preceding_entropy
        .checked_add(usize::from(entropy.is_some()))
        .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let endpoint_count = memory_hotplug_slot
        .checked_add(1)
        .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    if endpoint_count > PCI_ENDPOINT_SLOT_COUNT {
        return Err(
            PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::PciCapacity {
                count: endpoint_count,
                maximum: PCI_ENDPOINT_SLOT_COUNT,
            },
        );
    }
    let memory_hotplug = prepare_memory_hotplug_pci_endpoint(
        platform,
        product.topology(),
        address_plan,
        msi,
        memory_hotplug_slot,
        expected_msi_interrupt_count,
    )?;

    let balloon_route_demand = balloon.map_or(0, |endpoint| endpoint.route_count());
    let storage_route_demand = storage
        .as_ref()
        .map_or(0, |storage| storage.pci().route_demand());
    let entropy_route_demand = entropy.map_or(0, |endpoint| endpoint.route_count());
    let route_demand = balloon_route_demand
        .checked_add(storage_route_demand)
        .and_then(|count| count.checked_add(entropy_route_demand))
        .and_then(|count| count.checked_add(memory_hotplug.route_count()))
        .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    if route_demand
        > usize::try_from(msi.interrupt_range.count)
            .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::RouteConflict);
    }

    let queue_count = product_queue_range_count(&product)?;
    let pmem_count = product
        .storage()
        .map_or(0, |storage| storage.pmem_records().len());
    let mut queues = Vec::new();
    let mut pmem = Vec::new();
    let mut active_routes = Vec::new();
    let mut endpoint_bars = Vec::new();
    reserve.reserve(&mut queues, queue_count)?;
    reserve.reserve(&mut pmem, pmem_count)?;
    reserve.reserve(&mut active_routes, route_demand)?;
    reserve.reserve(&mut endpoint_bars, endpoint_count)?;
    append_product_memory_ranges(&product, &mut queues, &mut pmem);
    if !aggregate_memory_ranges_are_valid(&mapping, aperture, &[], &queues, &pmem) {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::RangeConflict);
    }

    if let Some(balloon) = balloon {
        endpoint_bars.push(balloon.bar_range());
    }
    if let Some(storage) = &storage {
        if storage.pci().host() != host || storage.pci().msi() != msi {
            return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan);
        }
        endpoint_bars.extend(
            storage
                .pci()
                .block_records()
                .iter()
                .chain(storage.pci().pmem_records())
                .map(|record| record.bar_range()),
        );
    }
    if let Some(entropy) = entropy {
        if entropy.msi_interrupt_count() != expected_msi_interrupt_count {
            return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan);
        }
        endpoint_bars.push(entropy.bar_range());
    }
    endpoint_bars.push(memory_hotplug.bar_range());
    if endpoint_bars.len() != endpoint_count
        || !ranges_are_pairwise_disjoint(&endpoint_bars)
        || endpoint_bars.iter().any(|bar| {
            bar.overlaps(aperture)
                || mapping
                    .static_ranges()
                    .iter()
                    .any(|base| bar.overlaps(*base))
                || queues.iter().any(|queue| bar.overlaps(*queue))
                || pmem.iter().any(|host_mapping| bar.overlaps(*host_mapping))
        })
    {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::RangeConflict);
    }

    register_product_active_pci_routes(
        &product,
        msi,
        memory_hotplug.route_count(),
        &mut active_routes,
    )?;
    let (serial_interrupt, vmgenid_interrupt, vmclock_interrupt) =
        validate_pci_interrupt_sequence(platform, storage.as_ref())?;

    Ok(HvfSnapshotV2MemoryHotplugPciPlatformPlan {
        product,
        mapping,
        balloon,
        storage,
        entropy,
        memory_hotplug,
        host,
        msi,
        endpoint_count,
        route_demand,
        serial_interrupt,
        vmgenid_interrupt,
        vmclock_interrupt,
    })
}

fn prepare_memory_hotplug_pci_endpoint(
    platform: &HvfSnapshotV2PlatformState,
    topology: &PreparedSnapshotV2MemoryHotplugTopology,
    address_plan: Arm64PciAddressPlan,
    msi: HvfGicMsiMetadata,
    slot: usize,
    expected_msi_interrupt_count: u32,
) -> Result<
    HvfSnapshotV2MemoryHotplugPciEndpointPlan,
    PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError,
> {
    let SnapshotV2DeviceTransport::Pci(transport) = topology.state().transport() else {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::TransportPolicy);
    };
    let placement = snapshot_v2_pci_endpoint_placement(address_plan, slot)
        .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let route_count = snapshot_v2_pci_endpoint_route_count(VIRTIO_MEM_QUEUE_SIZES.len())
        .filter(|count| *count == 2)
        .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let gic = platform.global().compatibility().gic_metadata();
    if gic.msi != Some(msi)
        || msi.interrupt_range.count != expected_msi_interrupt_count
        || transport.origin() != StorageDeviceOrigin::Startup
        || transport.phase() != VirtioPciEndpointPhase::Active
        || transport.sbdf() != placement.sbdf
        || transport.bar_range() != placement.bar_range
    {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan);
    }
    if queue_ranges_conflict_with_pci_platform(
        platform,
        topology.queue_ranges(),
        &gic,
        address_plan,
    )
    .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::RangeConflict);
    }
    Ok(HvfSnapshotV2MemoryHotplugPciEndpointPlan {
        origin: transport.origin(),
        sbdf: placement.sbdf,
        bar_region_id: placement.bar_region_id,
        bar_range: placement.bar_range,
        route_count,
        msi_interrupt_count: expected_msi_interrupt_count,
    })
}

fn register_product_active_pci_routes(
    product: &HvfSnapshotV2MemoryHotplugPreparedProduct,
    msi: HvfGicMsiMetadata,
    memory_hotplug_route_count: usize,
    active_routes: &mut Vec<(u64, u32)>,
) -> Result<(), PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError> {
    if let Some(balloon) = product.balloon() {
        let PreparedSnapshotV2BalloonTransport::Pci(transport) = balloon.transport() else {
            return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::TransportPolicy);
        };
        let queue_count = transport.device().queue_layout().queue_count();
        let route_count = snapshot_v2_pci_endpoint_route_count(queue_count)
            .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
        if !register_active_retained_pci_routes(
            transport.retained().msix_state(),
            msi,
            queue_count,
            route_count,
            active_routes,
        ) {
            return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::RouteConflict);
        }
    }
    if let Some(storage) = product.storage() {
        for record in storage
            .block_bundle()
            .map_or(&[][..], |block| block.records())
        {
            let SnapshotV2DeviceTransport::Pci(transport) = record.transport() else {
                return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::TransportPolicy);
            };
            if !register_active_pci_routes(transport, active_routes) {
                return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::RouteConflict);
            }
        }
        for record in storage.pmem_records() {
            let SnapshotV2DeviceTransport::Pci(transport) = record.transport() else {
                return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::TransportPolicy);
            };
            if !register_active_pci_routes(transport, active_routes) {
                return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::RouteConflict);
            }
        }
    }
    if let Some(entropy) = product.entropy() {
        let PreparedSnapshotV2EntropyTransport::Pci(transport) = entropy.transport() else {
            return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::TransportPolicy);
        };
        let route_count = snapshot_v2_pci_endpoint_route_count(VIRTIO_RNG_QUEUE_SIZES.len())
            .ok_or(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
        if !register_active_retained_pci_routes(
            transport.retained().msix_state(),
            msi,
            VIRTIO_RNG_QUEUE_SIZES.len(),
            route_count,
            active_routes,
        ) {
            return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::RouteConflict);
        }
    }
    let SnapshotV2DeviceTransport::Pci(memory_hotplug) = product.topology().state().transport()
    else {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::TransportPolicy);
    };
    if !register_active_snapshot_v2_pci_routes(
        memory_hotplug,
        msi,
        VIRTIO_MEM_QUEUE_SIZES.len(),
        memory_hotplug_route_count,
        active_routes,
    ) {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::RouteConflict);
    }
    Ok(())
}

pub(crate) fn register_active_snapshot_v2_pci_routes(
    state: &SnapshotV2PciDeviceState,
    msi: HvfGicMsiMetadata,
    queue_count: usize,
    route_count: usize,
    active_routes: &mut Vec<(u64, u32)>,
) -> bool {
    let Some(expected_address) = msi
        .region
        .base
        .checked_add(ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET)
    else {
        return false;
    };
    let Some(interrupt_end) = msi
        .interrupt_range
        .base
        .checked_add(msi.interrupt_range.count)
    else {
        return false;
    };
    let pending_mask = match route_count {
        0 => return false,
        count if count < u64::BITS as usize => (1_u64 << count) - 1,
        count if count == u64::BITS as usize => u64::MAX,
        _ => return false,
    };
    let msix = state.msix();
    if msix.entries().len() != route_count
        || msix.pending_words().len() != 1
        || msix.queue_vectors().len() != queue_count
        || msix
            .pending_words()
            .first()
            .is_none_or(|pending| pending & !pending_mask != 0)
        || !valid_vector(msix.config_vector(), route_count)
        || !msix
            .queue_vectors()
            .iter()
            .copied()
            .all(|vector| valid_vector(vector, route_count))
    {
        return false;
    }
    msix.entries().iter().enumerate().all(|(index, entry)| {
        if entry.vector_control() & !1 != 0 {
            return false;
        }
        let Ok(vector) = u16::try_from(index) else {
            return false;
        };
        let referenced = msix.config_vector() == vector || msix.queue_vectors().contains(&vector);
        let pending = msix
            .pending_words()
            .get(index / u64::BITS as usize)
            .is_some_and(|word| word & (1_u64 << (index % u64::BITS as usize)) != 0);
        if entry.vector_control() & 1 != 0 || (!referenced && !pending) {
            return true;
        }
        let address = (u64::from(entry.message_address_high()) << 32)
            | u64::from(entry.message_address_low());
        let data = entry.message_data();
        if address != expected_address
            || data < msi.interrupt_range.base
            || data >= interrupt_end
            || active_routes.contains(&(address, data))
        {
            return false;
        }
        active_routes.push((address, data));
        true
    })
}

fn valid_vector(vector: u16, route_count: usize) -> bool {
    vector == VIRTIO_PCI_NO_VECTOR || usize::from(vector) < route_count
}

fn validate_pci_interrupt_sequence(
    platform: &HvfSnapshotV2PlatformState,
    storage: Option<&HvfSnapshotV2StoragePciPlatformPlan>,
) -> Result<
    (GuestInterruptLine, GuestInterruptLine, GuestInterruptLine),
    PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError,
> {
    let mut allocator = HvfGicInterruptLineAllocator::from_metadata(
        &platform.global().compatibility().gic_metadata(),
    )
    .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let serial = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let vmgenid = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    let vmclock = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan)?;
    if storage.is_some_and(|storage| {
        storage.serial_interrupt() != serial
            || storage.vmgenid_interrupt() != vmgenid
            || storage.vmclock_interrupt() != vmclock
    }) || platform.time().vmgenid().interrupt_line() != vmgenid
        || platform.time().vmclock().interrupt_line() != vmclock
    {
        return Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::ResourcePlan);
    }
    Ok((serial, vmgenid, vmclock))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs::{self, File, OpenOptions};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use bangbang_runtime::BackendError;
    use bangbang_runtime::balloon::VirtioBalloonQueueLayout;
    use bangbang_runtime::block::{BlockFileBacking, BlockMmioLayout};
    use bangbang_runtime::memory::{GuestMemoryLayout, aarch64};
    use bangbang_runtime::pmem::{PmemFileBacking, PmemMmioLayout};
    use bangbang_runtime::serial::{
        SerialMmioDevice, SharedSerialOutput, SharedSerialOutputBuffer,
    };
    use bangbang_runtime::snapshot_balloon_v2_9::{
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, SnapshotV2BalloonRestorePlan,
        SnapshotV2BalloonState,
    };
    use bangbang_runtime::snapshot_device::SnapshotV1PlatformDeviceMetadata;
    use bangbang_runtime::snapshot_device_v2::SnapshotV2DeviceTransportKind;
    use bangbang_runtime::snapshot_device_v2_6::{
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION, PreparedSnapshotV2StorageBundle,
        SnapshotV2StorageDeviceGraph, SnapshotV2StorageRestorePlan,
    };
    use bangbang_runtime::snapshot_entropy_v2_8::{
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION, SnapshotV2EntropyRestorePlan,
        SnapshotV2EntropyState,
    };
    use bangbang_runtime::snapshot_memory_hotplug_v2_10::{
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        PreparedSnapshotV2MemoryHotplugTopology, SnapshotV2MemoryHotplugState,
    };
    use bangbang_runtime::snapshot_memory_v2::{
        materialize_snapshot_v2_memory_hotplug_file,
        write_snapshot_v2_memory_image_with_compatibility_version,
    };
    use bangbang_runtime::storage_capture::StorageRetryState;

    use super::*;
    use crate::gic::{HvfGicInterruptRange, HvfGicRegion};
    use crate::memory::{
        HvfGuestMemoryMapping, HvfMappedGuestMemoryRegion, HvfMemoryMapRequest, HvfMemoryMapper,
        HvfMemoryPermissions, HvfSnapshotV2MemoryHotplugMappingPlanFailureStage,
        PrepareHvfSnapshotV2MemoryHotplugMappingPlanError,
        prepare_hvf_snapshot_v2_memory_hotplug_mapping_plan_with_failure,
    };
    use crate::snapshot_bundle::HvfSnapshotV1CompatibilityState;
    use crate::snapshot_v2::{
        HvfSnapshotV2GlobalState, HvfSnapshotV2MachineState,
        HvfSnapshotV2MemoryHotplugPlatformState, HvfSnapshotV2TimeState,
        tests::{
            FIXTURE_MEMORY_MIB, memory_hotplug_active_ranges, memory_hotplug_capture_fixture,
            memory_hotplug_product_entropy_fixture, product_balloon_fixture,
            product_memory_hotplug_fixture, product_serial_fixture, product_storage_fixture,
            try_exact_minor_ten_platform,
        },
    };
    use crate::snapshot_v2_platform::HvfSnapshotV2RestoredSerialShell;
    use crate::startup::{HvfSnapshotV2MemoryHotplugMmioRestoreFault, OwnedHvfArm64BootSession};

    const MIB: u64 = 1024 * 1024;
    const COMPONENT_HEADER_BYTES: usize = 64;
    const COMPONENT_SECTION_ENTRY_BYTES: usize = 32;
    const STORAGE_SECTION_ENTRY_BYTES: usize = 32;
    const PCI_FIXED_BYTES: usize = 72;
    const PCI_WRITABLE_ENTRY_BYTES: usize = 4;
    const PCI_BAR_PROBE_ENTRY_BYTES: usize = 4;
    const PCI_MSIX_ENTRY_BYTES: usize = 16;
    const PCI_MSI_REGION_BASE: u64 = 0x0800_0000;
    const PCI_MSI_REGION_SIZE: u64 = 0x1_0000;
    const BALLOON_MMIO_BASE: GuestAddress = GuestAddress::new(0x1000_0000);
    const BALLOON_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(1);
    const STORAGE_PMEM_MMIO_BASE: GuestAddress = GuestAddress::new(0xd000_0000);
    const STORAGE_PMEM_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(100);
    const UNUSED_BLOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0xd100_0000);
    const UNUSED_BLOCK_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(200);
    const ENTROPY_MMIO_BASE: GuestAddress = GuestAddress::new(0xd001_0000);
    const ENTROPY_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(101);
    const MEMORY_HOTPLUG_MMIO_BASE: GuestAddress = GuestAddress::new(0xd002_0000);
    const MEMORY_HOTPLUG_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(102);
    const BALLOON_PCI_QUEUE_COUNT: usize = 5;
    const BALLOON_RESTORE_LOW_MEMORY_SIZE: u64 = 0x10_0000;
    const AVAILABLE_INDEX_OFFSET: u64 = 2;
    const AVAILABLE_RING_OFFSET: u64 = 4;
    const USED_INDEX_OFFSET: u64 = 2;
    const ENTROPY_DATA_BUFFER: GuestAddress = GuestAddress::new(0x8007_0000);
    static NEXT_IMAGE: AtomicU64 = AtomicU64::new(0);
    static NEXT_BACKING: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug, Default)]
    struct MemoryHotplugMappingTestMapper {
        state: Mutex<MemoryHotplugMappingTestMapperState>,
    }

    impl MemoryHotplugMappingTestMapper {
        fn failing(fail_map_on: Option<usize>, fail_unmap_on: Option<usize>) -> Self {
            Self {
                state: Mutex::new(MemoryHotplugMappingTestMapperState {
                    fail_map_on,
                    fail_unmap_on,
                    ..MemoryHotplugMappingTestMapperState::default()
                }),
            }
        }

        fn mapped_ranges(&self) -> Vec<GuestMemoryRange> {
            self.state
                .lock()
                .expect("mapping test state should not be poisoned")
                .mapped_ranges
                .clone()
        }

        fn unmapped_ranges(&self) -> Vec<GuestMemoryRange> {
            self.state
                .lock()
                .expect("mapping test state should not be poisoned")
                .unmapped_ranges
                .clone()
        }

        fn allow_unmaps(&self) {
            self.state
                .lock()
                .expect("mapping test state should not be poisoned")
                .fail_unmap_on = None;
        }
    }

    impl HvfMemoryMapper for MemoryHotplugMappingTestMapper {
        fn map_region(
            &self,
            request: HvfMemoryMapRequest,
            _permissions: HvfMemoryPermissions,
        ) -> Result<(), BackendError> {
            let mut state = self
                .state
                .lock()
                .expect("mapping test state should not be poisoned");
            state.mapped_ranges.push(request.range());
            if state.fail_map_on == Some(state.mapped_ranges.len()) {
                Err(BackendError::Hypervisor(
                    "injected mixed map failure".to_string(),
                ))
            } else {
                Ok(())
            }
        }

        fn unmap_region(
            &self,
            mapped_region: HvfMappedGuestMemoryRegion,
        ) -> Result<(), BackendError> {
            let mut state = self
                .state
                .lock()
                .expect("mapping test state should not be poisoned");
            state.unmapped_ranges.push(mapped_region.range);
            if state.fail_unmap_on == Some(state.unmapped_ranges.len()) {
                Err(BackendError::Hypervisor(
                    "injected mixed unmap failure".to_string(),
                ))
            } else {
                Ok(())
            }
        }

        fn protect_region(
            &self,
            _range: GuestMemoryRange,
            _permissions: HvfMemoryPermissions,
        ) -> Result<(), BackendError> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct MemoryHotplugMappingTestMapperState {
        mapped_ranges: Vec<GuestMemoryRange>,
        unmapped_ranges: Vec<GuestMemoryRange>,
        fail_map_on: Option<usize>,
        fail_unmap_on: Option<usize>,
    }

    pub(crate) struct TestImage {
        path: PathBuf,
    }

    impl TestImage {
        fn create() -> Self {
            let sequence = NEXT_IMAGE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-memory-hotplug-platform-{}-{sequence}",
                std::process::id()
            ));
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("memory-hotplug image should create");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestImage {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    pub(crate) struct MaterializedFixture {
        pub(crate) platform: HvfSnapshotV2PlatformState,
        pub(crate) topology: PreparedSnapshotV2MemoryHotplugTopology,
        pub(crate) memory: GuestMemory,
        pub(crate) _image: TestImage,
    }

    pub(crate) fn materialized_fixture(state: SnapshotV2MemoryHotplugState) -> MaterializedFixture {
        let base_bytes = FIXTURE_MEMORY_MIB * MIB;
        let mut ranges = aarch64::dram_layout(base_bytes)
            .expect("base layout should validate")
            .ranges()
            .to_vec();
        ranges.extend(memory_hotplug_active_ranges(&state));
        ranges.sort_by_key(|range| range.start());
        let layout = GuestMemoryLayout::new(ranges).expect("mixed layout should validate");
        let source = GuestMemory::allocate(&layout).expect("source memory should allocate");
        let image = TestImage::create();
        let mut writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(image.path())
            .expect("memory-hotplug image should open for writing");
        let binding = write_snapshot_v2_memory_image_with_compatibility_version(
            &source,
            &mut writer,
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
        )
        .expect("memory-hotplug image should encode");
        let capture = memory_hotplug_capture_fixture(state.clone());
        let platform = try_exact_minor_ten_platform(binding.clone(), Some(capture))
            .expect("exact-2.10 platform should validate")
            .platform()
            .clone();
        let (memory_binding, machine, global, stable, vcpus, time) = platform.into_parts();
        let machine = HvfSnapshotV2MachineState::try_new(
            machine.machine().with_track_dirty_pages(true),
            machine.boot().clone(),
            machine.fdt(),
            machine.cpu_template().cloned(),
        )
        .expect("tracked exact-2.10 machine should validate");
        let platform = HvfSnapshotV2MemoryHotplugPlatformState::try_new(
            memory_binding,
            machine,
            global,
            stable,
            vcpus,
            time,
            Some(memory_hotplug_capture_fixture(state.clone())),
        )
        .expect("tracked exact-2.10 platform should validate")
        .platform()
        .clone();
        let topology = PreparedSnapshotV2MemoryHotplugTopology::prepare(state, binding)
            .expect("memory-hotplug topology should prepare");
        let memory = materialize_snapshot_v2_memory_hotplug_file(
            &topology,
            File::open(image.path()).expect("memory-hotplug image should open for materialization"),
        )
        .expect("memory-hotplug memory should materialize");
        MaterializedFixture {
            platform,
            topology,
            memory,
            _image: image,
        }
    }

    pub(crate) struct TestBacking {
        path: PathBuf,
    }

    impl TestBacking {
        fn create(name: &str, len: u64) -> Self {
            let sequence = NEXT_BACKING.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-memory-hotplug-{name}-{}-{sequence}",
                std::process::id()
            ));
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("component backing should create");
            file.set_len(len).expect("component backing should resize");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestBacking {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn read_wire_u16(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(
            bytes[offset..offset + 2]
                .try_into()
                .expect("wire u16 should fit"),
        )
    }

    fn read_wire_u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(
            bytes[offset..offset + 8]
                .try_into()
                .expect("wire u64 should fit"),
        )
    }

    fn write_wire_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_wire_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn component_transport_offset(bytes: &[u8], section_index: usize) -> usize {
        let entry = COMPONENT_HEADER_BYTES + section_index * COMPONENT_SECTION_ENTRY_BYTES;
        usize::try_from(read_wire_u64(bytes, entry + 8))
            .expect("component transport offset should fit")
    }

    fn storage_transport_offsets(bytes: &[u8]) -> Vec<usize> {
        let record_count = usize::from(read_wire_u16(bytes, 14));
        let directory =
            usize::try_from(read_wire_u64(bytes, 48)).expect("storage directory should fit");
        (0..record_count)
            .map(|index| {
                let entry = directory + (index * 4 + 3) * STORAGE_SECTION_ENTRY_BYTES;
                usize::try_from(read_wire_u64(bytes, entry + 16))
                    .expect("storage transport offset should fit")
            })
            .collect()
    }

    fn relocate_mmio_interrupt(bytes: &mut [u8], transport_offset: usize, interrupt: u32) {
        write_wire_u32(bytes, transport_offset + 12, interrupt);
    }

    fn relocate_pci_transport(
        bytes: &mut [u8],
        transport_offset: usize,
        slot: usize,
        msi: HvfGicMsiMetadata,
        route_offset: u32,
    ) {
        let address_plan =
            Arm64PciAddressPlan::firecracker_v1_16().expect("PCI address plan should validate");
        let placement = snapshot_v2_pci_endpoint_placement(address_plan, slot)
            .expect("PCI endpoint placement should validate");
        bytes[transport_offset + 11] = placement.sbdf.device();
        write_wire_u64(
            bytes,
            transport_offset + 16,
            placement.bar_range.start().raw_value(),
        );

        let writable_count = usize::from(read_wire_u16(bytes, transport_offset + 42));
        let probe_count = usize::from(read_wire_u16(bytes, transport_offset + 44));
        let entry_count = usize::from(read_wire_u16(bytes, transport_offset + 46));
        let mut entry_offset = transport_offset
            + PCI_FIXED_BYTES
            + writable_count * PCI_WRITABLE_ENTRY_BYTES
            + probe_count * PCI_BAR_PROBE_ENTRY_BYTES;
        let message_address = msi
            .region
            .base
            .checked_add(ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET)
            .expect("MSI message address should fit");
        let message_address_low =
            u32::try_from(message_address & u64::from(u32::MAX)).expect("low MSI address fits");
        let message_address_high =
            u32::try_from(message_address >> 32).expect("high MSI address fits");
        for index in 0..entry_count {
            let message_data = msi
                .interrupt_range
                .base
                .checked_add(route_offset)
                .and_then(|base| {
                    base.checked_add(u32::try_from(index).expect("route index should fit"))
                })
                .expect("MSI message data should fit");
            write_wire_u32(bytes, entry_offset, message_address_low);
            write_wire_u32(bytes, entry_offset + 4, message_address_high);
            write_wire_u32(bytes, entry_offset + 8, message_data);
            entry_offset += PCI_MSIX_ENTRY_BYTES;
        }
    }

    pub(crate) fn memory_hotplug_mmio_state(interrupt: u32) -> SnapshotV2MemoryHotplugState {
        let state = product_memory_hotplug_fixture(SnapshotV2DeviceTransportKind::Mmio);
        let mut bytes = state
            .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
            .expect("MMIO memory-hotplug state should encode");
        let transport_offset = component_transport_offset(&bytes, 3);
        relocate_mmio_interrupt(&mut bytes, transport_offset, interrupt);
        SnapshotV2MemoryHotplugState::decode(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &bytes,
        )
        .expect("relocated MMIO memory-hotplug state should decode")
    }

    pub(crate) fn memory_hotplug_pci_state(
        slot: usize,
        msi: HvfGicMsiMetadata,
        route_offset: u32,
    ) -> SnapshotV2MemoryHotplugState {
        let state = product_memory_hotplug_fixture(SnapshotV2DeviceTransportKind::Pci);
        let mut bytes = state
            .encode(NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION)
            .expect("PCI memory-hotplug state should encode");
        let transport_offset = component_transport_offset(&bytes, 3);
        relocate_pci_transport(&mut bytes, transport_offset, slot, msi, route_offset);
        SnapshotV2MemoryHotplugState::decode(
            NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION,
            &bytes,
        )
        .expect("relocated PCI memory-hotplug state should decode")
    }

    pub(crate) fn storage_mmio_graph(first_interrupt: u32) -> SnapshotV2StorageDeviceGraph {
        storage_mmio_graph_with_gap(first_interrupt, 0)
    }

    pub(crate) fn storage_mmio_graph_with_gap(
        first_interrupt: u32,
        inserted_interrupt_count: usize,
    ) -> SnapshotV2StorageDeviceGraph {
        let graph = product_storage_fixture(SnapshotV2DeviceTransportKind::Mmio);
        let block_count = graph.block_records().len();
        let mut bytes = graph
            .encode(NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION)
            .expect("MMIO storage graph should encode");
        for (index, transport_offset) in storage_transport_offsets(&bytes).into_iter().enumerate() {
            let inserted = usize::from(index >= block_count)
                .checked_mul(inserted_interrupt_count)
                .expect("inserted interrupt count should fit");
            let interrupt = first_interrupt
                .checked_add(
                    u32::try_from(
                        index
                            .checked_add(inserted)
                            .expect("storage interrupt index should fit"),
                    )
                    .expect("storage interrupt index should fit"),
                )
                .expect("storage interrupt should fit");
            relocate_mmio_interrupt(&mut bytes, transport_offset, interrupt);
        }
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes,
        )
        .expect("relocated MMIO storage graph should decode")
    }

    fn storage_pci_graph(
        first_slot: usize,
        msi: HvfGicMsiMetadata,
        first_route_offset: u32,
    ) -> SnapshotV2StorageDeviceGraph {
        storage_pci_graph_with_gap(first_slot, 0, msi, first_route_offset)
    }

    pub(crate) fn storage_pci_graph_with_gap(
        first_slot: usize,
        inserted_endpoint_count: usize,
        msi: HvfGicMsiMetadata,
        first_route_offset: u32,
    ) -> SnapshotV2StorageDeviceGraph {
        let graph = product_storage_fixture(SnapshotV2DeviceTransportKind::Pci);
        let block_count = graph.block_records().len();
        let mut bytes = graph
            .encode(NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION)
            .expect("PCI storage graph should encode");
        for (index, transport_offset) in storage_transport_offsets(&bytes).into_iter().enumerate() {
            let slot = first_slot
                .checked_add(index)
                .and_then(|slot| {
                    if index < block_count {
                        Some(slot)
                    } else {
                        slot.checked_add(inserted_endpoint_count)
                    }
                })
                .expect("storage slot should fit");
            let route_offset = first_route_offset
                .checked_add(
                    u32::try_from(
                        index
                            .checked_mul(2)
                            .expect("storage route index should fit"),
                    )
                    .expect("storage route index should fit u32"),
                )
                .and_then(|route_offset| {
                    if index < block_count {
                        Some(route_offset)
                    } else {
                        u32::try_from(
                            inserted_endpoint_count
                                .checked_mul(3)
                                .expect("inserted route count should fit"),
                        )
                        .ok()
                        .and_then(|inserted| route_offset.checked_add(inserted))
                    }
                })
                .expect("storage route offset should fit");
            relocate_pci_transport(&mut bytes, transport_offset, slot, msi, route_offset);
        }
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes,
        )
        .expect("relocated PCI storage graph should decode")
    }

    pub(crate) fn balloon_mmio_plan(
        memory: &GuestMemory,
        interrupt: u32,
    ) -> SnapshotV2BalloonRestorePlan {
        let state = product_balloon_fixture(SnapshotV2DeviceTransportKind::Mmio);
        let mut bytes = state
            .encode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION)
            .expect("MMIO balloon state should encode");
        let transport_offset = component_transport_offset(&bytes, 3);
        relocate_mmio_interrupt(&mut bytes, transport_offset, interrupt);
        let state =
            SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &bytes)
                .expect("relocated MMIO balloon state should decode");
        SnapshotV2BalloonRestorePlan::prepare(state, memory)
            .expect("MMIO balloon restore plan should prepare")
    }

    pub(crate) fn entropy_mmio_plan(
        memory: &GuestMemory,
        interrupt: u32,
    ) -> SnapshotV2EntropyRestorePlan {
        let state = memory_hotplug_product_entropy_fixture(SnapshotV2DeviceTransportKind::Mmio);
        let mut bytes = state
            .encode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION)
            .expect("MMIO entropy state should encode");
        let transport_offset = component_transport_offset(&bytes, 2);
        relocate_mmio_interrupt(&mut bytes, transport_offset, interrupt);
        let state =
            SnapshotV2EntropyState::decode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION, &bytes)
                .expect("relocated MMIO entropy state should decode");
        SnapshotV2EntropyRestorePlan::prepare(state, memory, Instant::now())
            .expect("MMIO entropy restore plan should prepare")
    }

    pub(crate) fn balloon_pci_plan(
        slot: usize,
        msi: HvfGicMsiMetadata,
        route_offset: u32,
    ) -> SnapshotV2BalloonRestorePlan {
        let state = product_balloon_fixture(SnapshotV2DeviceTransportKind::Pci);
        let mut bytes = state
            .encode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION)
            .expect("PCI balloon state should encode");
        let transport_offset = component_transport_offset(&bytes, 3);
        relocate_pci_transport(&mut bytes, transport_offset, slot, msi, route_offset);
        let state =
            SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &bytes)
                .expect("relocated PCI balloon state should decode");
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), BALLOON_RESTORE_LOW_MEMORY_SIZE)
                .expect("balloon low-memory range should validate"),
            GuestMemoryRange::new(
                GuestAddress::new(aarch64::DRAM_MEM_START),
                FIXTURE_MEMORY_MIB * MIB,
            )
            .expect("balloon queue-memory range should validate"),
        ])
        .expect("balloon restore layout should validate");
        let mut memory = GuestMemory::allocate(&layout).expect("balloon memory should allocate");
        initialize_balloon_restore_memory(&mut memory, &state);
        SnapshotV2BalloonRestorePlan::prepare(state, &memory)
            .expect("PCI balloon restore plan should prepare")
    }

    fn initialize_balloon_restore_memory(memory: &mut GuestMemory, state: &SnapshotV2BalloonState) {
        let Some(active) = state.continuation().active_queues() else {
            return;
        };
        let layout = VirtioBalloonQueueLayout::from_config(state.config());
        let cursors = [
            Some(active.inflate()),
            Some(active.deflate()),
            active.statistics(),
            active.free_page_hinting(),
            active.free_page_reporting(),
        ];
        for (queue, cursor) in state
            .virtio()
            .queues()
            .iter()
            .zip(cursors.into_iter().flatten())
        {
            write_memory_u16(
                memory,
                queue
                    .driver_ring()
                    .checked_add(AVAILABLE_INDEX_OFFSET)
                    .expect("balloon available index should fit"),
                cursor.next_available(),
            );
            write_memory_u16(
                memory,
                queue
                    .device_ring()
                    .checked_add(USED_INDEX_OFFSET)
                    .expect("balloon used index should fit"),
                cursor.next_used(),
            );
        }
        if let Some(pending_head) = state.continuation().statistics_pending_descriptor_head() {
            let statistics = layout
                .statistics()
                .expect("pending statistics require statistics queue");
            let cursor = active
                .statistics()
                .expect("pending statistics require retained cursors");
            let queue = &state.virtio().queues()[statistics.index()];
            let ring_index = cursor.next_available().wrapping_sub(1) % queue.size();
            write_memory_u16(
                memory,
                queue
                    .driver_ring()
                    .checked_add(AVAILABLE_RING_OFFSET + u64::from(ring_index) * 2)
                    .expect("balloon pending entry should fit"),
                pending_head,
            );
        }
    }

    pub(crate) fn entropy_pci_plan(
        slot: usize,
        msi: HvfGicMsiMetadata,
        route_offset: u32,
    ) -> SnapshotV2EntropyRestorePlan {
        let state = memory_hotplug_product_entropy_fixture(SnapshotV2DeviceTransportKind::Pci);
        let mut bytes = state
            .encode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION)
            .expect("PCI entropy state should encode");
        let transport_offset = component_transport_offset(&bytes, 2);
        relocate_pci_transport(&mut bytes, transport_offset, slot, msi, route_offset);
        let state =
            SnapshotV2EntropyState::decode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION, &bytes)
                .expect("relocated PCI entropy state should decode");
        let layout = aarch64::dram_layout(FIXTURE_MEMORY_MIB * MIB)
            .expect("entropy restore layout should validate");
        let mut memory = GuestMemory::allocate(&layout).expect("entropy memory should allocate");
        initialize_entropy_restore_memory(&mut memory, &state);
        SnapshotV2EntropyRestorePlan::prepare(state, &memory, Instant::now())
            .expect("PCI entropy restore plan should prepare")
    }

    fn initialize_entropy_restore_memory(memory: &mut GuestMemory, state: &SnapshotV2EntropyState) {
        let Some(cursor) = state.active_queue() else {
            return;
        };
        let queue = state
            .virtio()
            .queues()
            .first()
            .expect("active entropy queue should exist");
        memory
            .write_slice(
                &ENTROPY_DATA_BUFFER.raw_value().to_le_bytes(),
                queue.descriptor_table(),
            )
            .expect("entropy descriptor address should write");
        memory
            .write_slice(
                &8_u32.to_le_bytes(),
                queue
                    .descriptor_table()
                    .checked_add(8)
                    .expect("entropy descriptor length address should fit"),
            )
            .expect("entropy descriptor length should write");
        memory
            .write_slice(
                &2_u16.to_le_bytes(),
                queue
                    .descriptor_table()
                    .checked_add(12)
                    .expect("entropy descriptor flags address should fit"),
            )
            .expect("entropy descriptor flags should write");
        memory
            .write_slice(
                &0_u16.to_le_bytes(),
                queue
                    .descriptor_table()
                    .checked_add(14)
                    .expect("entropy descriptor next address should fit"),
            )
            .expect("entropy descriptor next should write");
        let ring_index = cursor.next_available().wrapping_sub(1) % queue.size();
        write_memory_u16(
            memory,
            queue
                .driver_ring()
                .checked_add(AVAILABLE_RING_OFFSET + u64::from(ring_index) * 2)
                .expect("entropy available entry should fit"),
            0,
        );
        write_memory_u16(
            memory,
            queue
                .driver_ring()
                .checked_add(AVAILABLE_INDEX_OFFSET)
                .expect("entropy available index should fit"),
            cursor.next_available(),
        );
        write_memory_u16(
            memory,
            queue
                .device_ring()
                .checked_add(USED_INDEX_OFFSET)
                .expect("entropy used index should fit"),
            cursor.next_used(),
        );
    }

    fn write_memory_u16(memory: &mut GuestMemory, address: GuestAddress, value: u16) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("restore fixture write should succeed");
    }

    fn storage_restore_memory(graph: &SnapshotV2StorageDeviceGraph) -> GuestMemory {
        let layout = aarch64::dram_layout(FIXTURE_MEMORY_MIB * MIB)
            .expect("storage restore layout should validate");
        let mut memory = GuestMemory::allocate(&layout).expect("storage memory should allocate");
        for record in graph.block_records() {
            let Some(cursor) = record.block().continuation().active_queue() else {
                continue;
            };
            let queue = &record.virtio().queues()[0];
            let available = if record.block().continuation().retry() == StorageRetryState::None {
                cursor.next_available()
            } else {
                cursor.next_available().wrapping_add(1)
            };
            memory
                .write_slice(
                    &available.to_le_bytes(),
                    queue
                        .driver_ring()
                        .checked_add(2)
                        .expect("block available address should fit"),
                )
                .expect("block available cursor should write");
            memory
                .write_slice(
                    &cursor.next_used().to_le_bytes(),
                    queue
                        .device_ring()
                        .checked_add(2)
                        .expect("block used address should fit"),
                )
                .expect("block used cursor should write");
        }
        for record in graph.pmem_records() {
            let Some(cursor) = record.pmem().active_queue() else {
                continue;
            };
            let queue = &record.virtio().queues()[0];
            let available = if record.pmem().retry() == StorageRetryState::None {
                cursor.next_available()
            } else {
                cursor.next_available().wrapping_add(1)
            };
            memory
                .write_slice(
                    &available.to_le_bytes(),
                    queue
                        .driver_ring()
                        .checked_add(2)
                        .expect("pmem available address should fit"),
                )
                .expect("pmem available cursor should write");
            memory
                .write_slice(
                    &cursor.next_used().to_le_bytes(),
                    queue
                        .device_ring()
                        .checked_add(2)
                        .expect("pmem used address should fit"),
                )
                .expect("pmem used cursor should write");
        }
        memory
    }

    pub(crate) fn prepared_storage_bundle(
        graph: SnapshotV2StorageDeviceGraph,
    ) -> (PreparedSnapshotV2StorageBundle, Vec<TestBacking>) {
        let memory = storage_restore_memory(&graph);
        let mut files = Vec::new();
        let mut block_backings = Vec::new();
        let mut pmem_backings = Vec::new();
        for (index, record) in graph.block_records().iter().enumerate() {
            let file =
                TestBacking::create(&format!("block-{index}"), record.block().backing_bytes());
            let backing =
                BlockFileBacking::open_snapshot(file.path(), record.config().is_read_only())
                    .expect("block backing should open")
                    .0;
            files.push(file);
            block_backings.push(backing);
        }
        for (index, record) in graph.pmem_records().iter().enumerate() {
            let file = TestBacking::create(&format!("pmem-{index}"), record.pmem().file_bytes());
            let host_file = OpenOptions::new()
                .read(true)
                .write(!record.config().is_read_only())
                .open(file.path())
                .expect("pmem backing should open");
            let backing = PmemFileBacking::from_file(host_file, record.config().is_read_only())
                .expect("pmem backing should validate");
            files.push(file);
            pmem_backings.push(backing);
        }
        let bundle = SnapshotV2StorageRestorePlan::prepare(graph, &memory, Instant::now())
            .expect("storage restore plan should prepare")
            .prepare_backings(block_backings, pmem_backings, || false)
            .expect("storage bundle should prepare");
        (bundle, files)
    }

    pub(crate) fn mmio_platform(
        platform: HvfSnapshotV2PlatformState,
        state: &SnapshotV2MemoryHotplugState,
        device_count: usize,
    ) -> HvfSnapshotV2PlatformState {
        let spi_count = u32::try_from(
            device_count
                .checked_add(3)
                .expect("MMIO interrupt count should fit"),
        )
        .expect("MMIO interrupt count should fit u32");
        let (memory, machine, global, topology, vcpus, time) = platform.into_parts();
        let (compatibility, gic_device) = global.into_parts();
        let mut gic = compatibility.gic_metadata();
        gic.spi_interrupt_range.count = spi_count;
        let compatibility = HvfSnapshotV1CompatibilityState::new(
            compatibility.identification(),
            compatibility.optional_sve_sme_identification(),
            compatibility.cache_manifest(),
            compatibility.primary_mpidr(),
            gic,
            compatibility.rtc_mmio_layout(),
        );
        let global = HvfSnapshotV2GlobalState::try_new(compatibility, gic_device)
            .expect("MMIO global state should validate");

        let mut allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
            .expect("MMIO interrupt allocator should validate");
        for _ in 0..device_count {
            allocator
                .allocate()
                .expect("MMIO device interrupt should allocate");
        }
        let _serial = allocator
            .allocate()
            .expect("serial interrupt should allocate");
        let vmgenid_interrupt = allocator
            .allocate()
            .expect("VMGenID interrupt should allocate");
        let vmclock_interrupt = allocator
            .allocate()
            .expect("VMClock interrupt should allocate");
        let (rtc, vmgenid, vmclock, vmclock_abi, pvtime) = time.into_parts();
        let vmgenid = SnapshotV1PlatformDeviceMetadata::new(
            vmgenid.range(),
            vmgenid.fdt_region(),
            vmgenid_interrupt,
        );
        let vmclock = SnapshotV1PlatformDeviceMetadata::new(
            vmclock.range(),
            vmclock.fdt_region(),
            vmclock_interrupt,
        );
        let time = HvfSnapshotV2TimeState::try_new(rtc, vmgenid, vmclock, vmclock_abi, pvtime)
            .expect("MMIO time state should validate");
        let capture = memory_hotplug_capture_fixture(state.clone());
        HvfSnapshotV2MemoryHotplugPlatformState::try_new(
            memory,
            machine,
            global,
            topology,
            vcpus,
            time,
            Some(capture),
        )
        .expect("MMIO memory-hotplug platform should validate")
        .platform()
        .clone()
    }

    pub(crate) fn pci_platform(
        platform: HvfSnapshotV2PlatformState,
        state: &SnapshotV2MemoryHotplugState,
        interrupt_count: u32,
    ) -> HvfSnapshotV2PlatformState {
        let (memory, machine, global, topology, vcpus, time) = platform.into_parts();
        let (compatibility, gic_device) = global.into_parts();
        let mut gic = compatibility.gic_metadata();
        let legacy_interrupt_count = 3;
        let msi_interrupt_base = gic
            .spi_interrupt_range
            .base
            .checked_add(legacy_interrupt_count)
            .expect("MSI interrupt base should fit");
        gic.spi_interrupt_range.count = legacy_interrupt_count;
        gic.msi = Some(HvfGicMsiMetadata {
            region: HvfGicRegion {
                base: PCI_MSI_REGION_BASE,
                size: PCI_MSI_REGION_SIZE,
            },
            interrupt_range: HvfGicInterruptRange {
                base: msi_interrupt_base,
                count: interrupt_count,
            },
        });
        let compatibility = HvfSnapshotV1CompatibilityState::new(
            compatibility.identification(),
            compatibility.optional_sve_sme_identification(),
            compatibility.cache_manifest(),
            compatibility.primary_mpidr(),
            gic,
            compatibility.rtc_mmio_layout(),
        );
        let global = HvfSnapshotV2GlobalState::try_new(compatibility, gic_device)
            .expect("PCI global state should validate");

        let mut allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
            .expect("PCI legacy interrupt allocator should validate");
        let _serial = allocator
            .allocate()
            .expect("serial interrupt should allocate");
        let vmgenid_interrupt = allocator
            .allocate()
            .expect("VMGenID interrupt should allocate");
        let vmclock_interrupt = allocator
            .allocate()
            .expect("VMClock interrupt should allocate");
        let (rtc, vmgenid, vmclock, vmclock_abi, pvtime) = time.into_parts();
        let vmgenid = SnapshotV1PlatformDeviceMetadata::new(
            vmgenid.range(),
            vmgenid.fdt_region(),
            vmgenid_interrupt,
        );
        let vmclock = SnapshotV1PlatformDeviceMetadata::new(
            vmclock.range(),
            vmclock.fdt_region(),
            vmclock_interrupt,
        );
        let time = HvfSnapshotV2TimeState::try_new(rtc, vmgenid, vmclock, vmclock_abi, pvtime)
            .expect("PCI time state should validate");
        let capture = memory_hotplug_capture_fixture(state.clone());
        HvfSnapshotV2MemoryHotplugPlatformState::try_new(
            memory,
            machine,
            global,
            topology,
            vcpus,
            time,
            Some(capture),
        )
        .expect("PCI memory-hotplug platform should validate")
        .platform()
        .clone()
    }

    fn mmio_process_config() -> HvfSnapshotV2MemoryHotplugMmioProcessConfig {
        HvfSnapshotV2MemoryHotplugMmioProcessConfig::new(
            BalloonMmioLayout::new(BALLOON_MMIO_BASE, BALLOON_MMIO_REGION_ID),
            HvfSnapshotV2StorageMmioProcessConfig::new(
                BlockMmioLayout::new(UNUSED_BLOCK_MMIO_BASE, UNUSED_BLOCK_MMIO_REGION_ID),
                PmemMmioLayout::new(STORAGE_PMEM_MMIO_BASE, STORAGE_PMEM_MMIO_REGION_ID),
            ),
            EntropyMmioLayout::new(ENTROPY_MMIO_BASE, ENTROPY_MMIO_REGION_ID),
            VirtioMemMmioLayout::new(MEMORY_HOTPLUG_MMIO_BASE, MEMORY_HOTPLUG_MMIO_REGION_ID),
        )
    }

    fn prepared_product(
        topology: PreparedSnapshotV2MemoryHotplugTopology,
        memory: GuestMemory,
        balloon: Option<SnapshotV2BalloonRestorePlan>,
        storage: Option<PreparedSnapshotV2StorageBundle>,
        entropy: Option<SnapshotV2EntropyRestorePlan>,
    ) -> HvfSnapshotV2MemoryHotplugPreparedProduct {
        match (balloon, storage, entropy) {
            (None, None, None) => {
                HvfSnapshotV2MemoryHotplugPreparedProduct::serial_memory_hotplug(topology, memory)
            }
            (None, Some(storage), None) => {
                HvfSnapshotV2MemoryHotplugPreparedProduct::serial_storage_memory_hotplug(
                    topology, memory, storage,
                )
            }
            (None, None, Some(entropy)) => {
                HvfSnapshotV2MemoryHotplugPreparedProduct::serial_entropy_memory_hotplug(
                    topology, memory, entropy,
                )
            }
            (None, Some(storage), Some(entropy)) => {
                HvfSnapshotV2MemoryHotplugPreparedProduct::serial_storage_entropy_memory_hotplug(
                    topology, memory, storage, entropy,
                )
            }
            (Some(balloon), None, None) => {
                HvfSnapshotV2MemoryHotplugPreparedProduct::serial_balloon_memory_hotplug(
                    topology, memory, balloon,
                )
            }
            (Some(balloon), Some(storage), None) => {
                HvfSnapshotV2MemoryHotplugPreparedProduct::serial_balloon_storage_memory_hotplug(
                    topology, memory, balloon, storage,
                )
            }
            (Some(balloon), None, Some(entropy)) => {
                HvfSnapshotV2MemoryHotplugPreparedProduct::serial_balloon_entropy_memory_hotplug(
                    topology, memory, balloon, entropy,
                )
            }
            (Some(balloon), Some(storage), Some(entropy)) => {
                HvfSnapshotV2MemoryHotplugPreparedProduct::
                    serial_balloon_storage_entropy_memory_hotplug(
                        topology, memory, balloon, storage, entropy,
                    )
            }
        }
    }

    fn expected_kind(
        has_balloon: bool,
        has_storage: bool,
        has_entropy: bool,
    ) -> HvfSnapshotV2MemoryHotplugProductKind {
        match (has_balloon, has_storage, has_entropy) {
            (false, false, false) => HvfSnapshotV2MemoryHotplugProductKind::SerialMemoryHotplug,
            (false, true, false) => {
                HvfSnapshotV2MemoryHotplugProductKind::SerialStorageMemoryHotplug
            }
            (false, false, true) => {
                HvfSnapshotV2MemoryHotplugProductKind::SerialEntropyMemoryHotplug
            }
            (false, true, true) => {
                HvfSnapshotV2MemoryHotplugProductKind::SerialStorageEntropyMemoryHotplug
            }
            (true, false, false) => {
                HvfSnapshotV2MemoryHotplugProductKind::SerialBalloonMemoryHotplug
            }
            (true, true, false) => {
                HvfSnapshotV2MemoryHotplugProductKind::SerialBalloonStorageMemoryHotplug
            }
            (true, false, true) => {
                HvfSnapshotV2MemoryHotplugProductKind::SerialBalloonEntropyMemoryHotplug
            }
            (true, true, true) => {
                HvfSnapshotV2MemoryHotplugProductKind::SerialBalloonStorageEntropyMemoryHotplug
            }
        }
    }

    #[test]
    fn all_eight_mmio_products_close_contiguous_interrupt_and_memory_order() {
        let storage_record_count =
            product_storage_fixture(SnapshotV2DeviceTransportKind::Mmio).record_count();
        for (has_balloon, has_storage, has_entropy) in [
            (false, false, false),
            (false, true, false),
            (false, false, true),
            (false, true, true),
            (true, false, false),
            (true, true, false),
            (true, false, true),
            (true, true, true),
        ] {
            let first_interrupt = 32_u32;
            let storage_count = usize::from(has_storage) * storage_record_count;
            let storage_interrupt = first_interrupt + u32::from(has_balloon);
            let entropy_interrupt =
                storage_interrupt + u32::try_from(storage_count).expect("storage count should fit");
            let memory_hotplug_interrupt = entropy_interrupt + u32::from(has_entropy);
            let device_count =
                usize::from(has_balloon) + storage_count + usize::from(has_entropy) + 1;

            let fixture = materialized_fixture(memory_hotplug_mmio_state(memory_hotplug_interrupt));
            let MaterializedFixture {
                platform,
                topology,
                memory,
                _image,
            } = fixture;
            let platform = mmio_platform(platform, topology.state(), device_count);
            let balloon = has_balloon.then(|| balloon_mmio_plan(&memory, first_interrupt));
            let entropy = has_entropy.then(|| entropy_mmio_plan(&memory, entropy_interrupt));
            let (storage, _backings) = if has_storage {
                let graph = storage_mmio_graph(storage_interrupt);
                let (bundle, files) = prepared_storage_bundle(graph);
                (Some(bundle), files)
            } else {
                (None, Vec::new())
            };
            let product = prepared_product(topology, memory, balloon, storage, entropy);
            let plan = prepare_hvf_snapshot_v2_memory_hotplug_mmio_platform_plan(
                &platform,
                product,
                mmio_process_config(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "MMIO balloon={has_balloon} storage={has_storage} entropy={has_entropy} failed: {error:?}"
                )
            });

            assert_eq!(
                plan.kind(),
                expected_kind(has_balloon, has_storage, has_entropy)
            );
            assert_eq!(plan.balloon().is_some(), has_balloon);
            assert_eq!(plan.storage().is_some(), has_storage);
            assert_eq!(plan.entropy().is_some(), has_entropy);
            assert_eq!(
                plan.memory_hotplug().interrupt_line().raw_value(),
                memory_hotplug_interrupt
            );
            assert_eq!(
                plan.serial_interrupt().raw_value(),
                first_interrupt + u32::try_from(device_count).expect("device count should fit")
            );
            assert_eq!(
                plan.vmgenid_interrupt().raw_value(),
                plan.serial_interrupt().raw_value() + 1
            );
            assert_eq!(
                plan.vmclock_interrupt().raw_value(),
                plan.serial_interrupt().raw_value() + 2
            );
            assert_eq!(plan.mapping().base_bytes(), FIXTURE_MEMORY_MIB * MIB);
            assert_eq!(plan.mapping().dirty_epoch(), 0);

            let serial = product_serial_fixture();
            let shell = HvfSnapshotV2RestoredSerialShell::new(
                SerialMmioDevice::from_capture_state_with_shared_output(
                    SharedSerialOutput::from(SharedSerialOutputBuffer::default()),
                    serial.device().clone(),
                ),
            );
            let error =
                OwnedHvfArm64BootSession::restore_snapshot_v2_memory_hotplug_mmio_with_fault(
                    platform,
                    shell,
                    None,
                    plan,
                    HvfSnapshotV2MemoryHotplugMmioRestoreFault::InterruptSetup,
                )
                .expect_err("all eight MMIO product tags should reach owner preflight");
            assert!(!error.is_terminal());
            assert!(!error.has_incomplete_cleanup());
        }
    }

    #[test]
    fn all_eight_pci_products_close_slots_routes_and_exact_msi_capacity() {
        let storage_record_count =
            product_storage_fixture(SnapshotV2DeviceTransportKind::Pci).record_count();
        for (has_balloon, has_storage, has_entropy) in [
            (false, false, false),
            (false, true, false),
            (false, false, true),
            (false, true, true),
            (true, false, false),
            (true, true, false),
            (true, false, true),
            (true, true, true),
        ] {
            let storage_count = usize::from(has_storage) * storage_record_count;
            let storage_slot = usize::from(has_balloon);
            let entropy_slot = storage_slot + storage_count;
            let memory_hotplug_slot = entropy_slot + usize::from(has_entropy);
            let endpoint_count = memory_hotplug_slot + 1;

            let balloon_routes = usize::from(has_balloon) * (BALLOON_PCI_QUEUE_COUNT + 1);
            let storage_routes = storage_count * 2;
            let entropy_routes = usize::from(has_entropy) * 2;
            let storage_route_offset =
                u32::try_from(balloon_routes).expect("balloon routes should fit");
            let entropy_route_offset = u32::try_from(balloon_routes + storage_routes)
                .expect("entropy route offset should fit");
            let memory_hotplug_route_offset =
                u32::try_from(balloon_routes + storage_routes + entropy_routes)
                    .expect("memory-hotplug route offset should fit");
            let route_demand = balloon_routes + storage_routes + entropy_routes + 2;

            let expected_msi_interrupt_count = pci_memory_hotplug_restore_gic_msi_configuration(
                has_balloon.then_some(BALLOON_PCI_QUEUE_COUNT),
                has_entropy,
            )
            .expect("memory-hotplug MSI capacity should validate")
            .interrupt_count()
            .get();
            let msi = HvfGicMsiMetadata {
                region: HvfGicRegion {
                    base: PCI_MSI_REGION_BASE,
                    size: PCI_MSI_REGION_SIZE,
                },
                interrupt_range: HvfGicInterruptRange {
                    base: 35,
                    count: expected_msi_interrupt_count,
                },
            };
            let state =
                memory_hotplug_pci_state(memory_hotplug_slot, msi, memory_hotplug_route_offset);
            let fixture = materialized_fixture(state);
            let MaterializedFixture {
                platform,
                topology,
                memory,
                _image,
            } = fixture;
            let platform = pci_platform(platform, topology.state(), expected_msi_interrupt_count);
            assert_eq!(
                platform.global().compatibility().gic_metadata().msi,
                Some(msi)
            );

            let balloon = has_balloon.then(|| balloon_pci_plan(0, msi, 0));
            let entropy =
                has_entropy.then(|| entropy_pci_plan(entropy_slot, msi, entropy_route_offset));
            let (storage, _backings) = if has_storage {
                let graph = storage_pci_graph(storage_slot, msi, storage_route_offset);
                let (bundle, files) = prepared_storage_bundle(graph);
                (Some(bundle), files)
            } else {
                (None, Vec::new())
            };
            let product = prepared_product(topology, memory, balloon, storage, entropy);
            let plan = prepare_hvf_snapshot_v2_memory_hotplug_pci_platform_plan(
                &platform, product,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "PCI balloon={has_balloon} storage={has_storage} entropy={has_entropy} failed: {error:?}"
                )
            });

            assert_eq!(
                plan.kind(),
                expected_kind(has_balloon, has_storage, has_entropy)
            );
            assert_eq!(plan.balloon().is_some(), has_balloon);
            assert_eq!(plan.storage().is_some(), has_storage);
            assert_eq!(plan.entropy().is_some(), has_entropy);
            assert_eq!(plan.endpoint_count(), endpoint_count);
            assert_eq!(plan.route_demand(), route_demand);
            assert_eq!(
                plan.memory_hotplug().sbdf(),
                snapshot_v2_pci_endpoint_placement(
                    Arm64PciAddressPlan::firecracker_v1_16()
                        .expect("PCI address plan should validate"),
                    memory_hotplug_slot,
                )
                .expect("memory-hotplug placement should validate")
                .sbdf
            );
            assert_eq!(
                plan.memory_hotplug().msi_interrupt_count(),
                expected_msi_interrupt_count
            );
            assert_eq!(plan.mapping().base_bytes(), FIXTURE_MEMORY_MIB * MIB);
            assert_eq!(plan.mapping().dirty_epoch(), 0);
        }
    }

    #[test]
    fn aggregate_relationships_capacity_and_diagnostics_are_closed() {
        let fixture = materialized_fixture(product_memory_hotplug_fixture(
            SnapshotV2DeviceTransportKind::Mmio,
        ));
        let mapping = prepare_hvf_snapshot_v2_memory_hotplug_mapping_plan(
            &fixture.topology,
            &fixture.memory,
            FIXTURE_MEMORY_MIB * MIB,
        )
        .expect("mapping fixture should plan");
        let aperture = mapping.reservation().range();
        let aperture_region = MmioRegion::new(
            MmioRegionId::new(9000),
            aperture.start(),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("aperture-overlapping MMIO region should validate locally");
        assert!(!aggregate_memory_ranges_are_valid(
            &mapping,
            aperture,
            &[aperture_region],
            &[],
            &[],
        ));
        let aperture_queue =
            GuestMemoryRange::new(aperture.start(), 4096).expect("aperture queue should validate");
        assert!(!aggregate_memory_ranges_are_valid(
            &mapping,
            aperture,
            &[],
            &[aperture_queue],
            &[],
        ));
        assert!(!aggregate_memory_ranges_are_valid(
            &mapping,
            aperture,
            &[],
            &[],
            &[mapping.static_ranges()[0]],
        ));
        assert!(!ranges_are_pairwise_disjoint(&[
            mapping.static_ranges()[0],
            mapping.static_ranges()[0],
        ]));
        assert_eq!(PCI_ENDPOINT_SLOT_COUNT, 31);
        assert_eq!(
            PCI_ENDPOINT_SLOT_COUNT
                .checked_sub(3)
                .expect("balloon, entropy, and memory consume three slots"),
            28
        );

        let product_debug = format!(
            "{:?}",
            HvfSnapshotV2MemoryHotplugPreparedProduct::serial_memory_hotplug(
                fixture.topology,
                fixture.memory,
            )
        );
        assert!(product_debug.contains(REDACTED));
        assert!(!product_debug.contains(&aperture.start().to_string()));
        let error = PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::PciCapacity {
            count: 32,
            maximum: 31,
        };
        let diagnostics = format!("{error:?} {error}");
        assert!(diagnostics.contains(REDACTED));
        assert!(!diagnostics.contains("32"));
        assert!(!diagnostics.contains("31"));
    }

    #[test]
    fn every_product_inventory_reservation_reports_allocation_failure() {
        struct FailingReserve {
            calls: usize,
            fail_at: usize,
        }

        impl MemoryHotplugPlatformPlanReserve for FailingReserve {
            fn reserve<T>(
                &mut self,
                values: &mut Vec<T>,
                additional: usize,
            ) -> Result<(), PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError> {
                let call = self.calls;
                self.calls = self.calls.saturating_add(1);
                if call == self.fail_at {
                    Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::Allocation)
                } else {
                    values
                        .try_reserve_exact(additional)
                        .map_err(|_| PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::Allocation)
                }
            }
        }

        for fail_at in 0..3 {
            let fixture = materialized_fixture(memory_hotplug_mmio_state(32));
            let MaterializedFixture {
                platform,
                topology,
                memory,
                _image,
            } = fixture;
            let platform = mmio_platform(platform, topology.state(), 1);
            let product =
                HvfSnapshotV2MemoryHotplugPreparedProduct::serial_memory_hotplug(topology, memory);
            assert!(matches!(
                prepare_memory_hotplug_mmio_platform_plan(
                    &platform,
                    product,
                    mmio_process_config(),
                    &mut FailingReserve { calls: 0, fail_at },
                ),
                Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::Allocation)
            ));
        }

        let expected_msi_interrupt_count =
            pci_memory_hotplug_restore_gic_msi_configuration(None, false)
                .expect("memory-only MSI capacity should validate")
                .interrupt_count()
                .get();
        let msi = HvfGicMsiMetadata {
            region: HvfGicRegion {
                base: PCI_MSI_REGION_BASE,
                size: PCI_MSI_REGION_SIZE,
            },
            interrupt_range: HvfGicInterruptRange {
                base: 35,
                count: expected_msi_interrupt_count,
            },
        };
        for fail_at in 0..4 {
            let fixture = materialized_fixture(memory_hotplug_pci_state(0, msi, 0));
            let MaterializedFixture {
                platform,
                topology,
                memory,
                _image,
            } = fixture;
            let platform = pci_platform(platform, topology.state(), expected_msi_interrupt_count);
            let product =
                HvfSnapshotV2MemoryHotplugPreparedProduct::serial_memory_hotplug(topology, memory);
            assert!(matches!(
                prepare_memory_hotplug_pci_platform_plan(
                    &platform,
                    product,
                    &mut FailingReserve { calls: 0, fail_at },
                ),
                Err(PrepareHvfSnapshotV2MemoryHotplugPlatformPlanError::Allocation)
            ));
        }
    }

    #[test]
    fn mixed_mapping_plan_classifies_base_and_aperture_and_redacts_failures() {
        let fixture = materialized_fixture(product_memory_hotplug_fixture(
            SnapshotV2DeviceTransportKind::Mmio,
        ));
        let plan = prepare_hvf_snapshot_v2_memory_hotplug_mapping_plan(
            &fixture.topology,
            &fixture.memory,
            FIXTURE_MEMORY_MIB * MIB,
        )
        .expect("materialized mixed mapping should plan");
        assert_eq!(
            fixture.platform.memory(),
            fixture.topology.memory().binding()
        );
        assert_eq!(plan.static_ranges().len(), 1);
        assert_eq!(plan.dynamic_ranges(), fixture.topology.plugged_ranges());
        assert_eq!(plan.base_bytes(), FIXTURE_MEMORY_MIB * MIB);
        assert_eq!(
            plan.current_memory_bytes(),
            plan.base_bytes() + plan.active_bytes()
        );
        assert_eq!(
            plan.active_bytes() + plan.offline_bytes(),
            plan.reservation().range().size()
        );
        assert_eq!(plan.dirty_epoch(), 0);
        assert!(plan.dirty_page_size().is_power_of_two());

        for (stage, expected) in [
            (
                HvfSnapshotV2MemoryHotplugMappingPlanFailureStage::StaticRanges,
                "allocation",
            ),
            (
                HvfSnapshotV2MemoryHotplugMappingPlanFailureStage::DynamicRanges,
                "allocation",
            ),
            (
                HvfSnapshotV2MemoryHotplugMappingPlanFailureStage::ActiveRanges,
                "allocation",
            ),
            (
                HvfSnapshotV2MemoryHotplugMappingPlanFailureStage::MappingPreflight,
                "dirty-access",
            ),
            (
                HvfSnapshotV2MemoryHotplugMappingPlanFailureStage::DirtySnapshot,
                "dirty-access",
            ),
        ] {
            let error = prepare_hvf_snapshot_v2_memory_hotplug_mapping_plan_with_failure(
                &fixture.topology,
                &fixture.memory,
                FIXTURE_MEMORY_MIB * MIB,
                stage,
            )
            .expect_err("injected mapping checkpoint should fail");
            assert!(matches!(
                (&error, expected),
                (
                    PrepareHvfSnapshotV2MemoryHotplugMappingPlanError::Allocation,
                    "allocation"
                ) | (
                    PrepareHvfSnapshotV2MemoryHotplugMappingPlanError::DirtyAccess,
                    "dirty-access"
                )
            ));
            let diagnostics = format!("{error:?} {error}");
            assert!(diagnostics.contains("<redacted>"));
            assert!(!diagnostics.contains(&plan.reservation().range().start().to_string()));
        }
    }

    #[test]
    fn mixed_mapping_materializes_static_and_dynamic_owners_and_survives_remap() {
        let fixture = materialized_fixture(product_memory_hotplug_fixture(
            SnapshotV2DeviceTransportKind::Mmio,
        ));
        let MaterializedFixture {
            platform: _,
            topology,
            memory,
            _image,
        } = fixture;
        let config = topology.state().config();
        let plan = prepare_hvf_snapshot_v2_memory_hotplug_mapping_plan(
            &topology,
            &memory,
            FIXTURE_MEMORY_MIB * MIB,
        )
        .expect("mixed mapping should plan");
        let aperture = plan.reservation().range();
        let dynamic_range = memory
            .regions()
            .iter()
            .map(|region| region.range())
            .find(|range| {
                range.start() >= aperture.start()
                    && range.end_exclusive() <= aperture.end_exclusive()
            })
            .expect("materialized mapping should have a block-granular dynamic owner");
        let restored = topology
            .into_mmio_handler(&memory)
            .expect("inactive MMIO handler should reconstruct");
        let expected_ranges = memory
            .regions()
            .iter()
            .map(|region| region.range())
            .collect::<Vec<_>>();
        let mapper = Arc::new(MemoryHotplugMappingTestMapper::default());
        let mut mapping = HvfGuestMemoryMapping::map_snapshot_v2_memory_hotplug_with_mapper(
            memory,
            &plan,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("mixed mapping should materialize");

        assert_eq!(mapper.mapped_ranges(), expected_ranges);
        assert!(mapping.has_dynamic_regions());
        mapping
            .start_dirty_write_tracking()
            .expect("restored mixed mapping should start HVF dirty tracking");
        let capture = restored
            .handler()
            .capture_memory_hotplug_state(
                config,
                mapping
                    .memory()
                    .expect("restored mapping should retain guest memory"),
            )
            .expect("restored virtio-mem handler should capture");
        let mapping_capture = mapping
            .capture_virtio_mem_mapping_state(capture.device())
            .expect("restored owners should close over the mapping proof");
        assert!(plan.matches_capture(&mapping_capture));

        mapping
            .unmap_dynamic_region(dynamic_range)
            .expect("restored dynamic owner should unplug");
        mapping
            .map_dynamic_region(dynamic_range, HvfMemoryPermissions::GUEST_RAM)
            .expect("restored dynamic owner should remap");
        let remapped_capture = mapping
            .capture_virtio_mem_mapping_state(capture.device())
            .expect("remapped owners should close over the original proof");
        assert!(plan.matches_capture(&remapped_capture));

        mapping.unmap_all().expect("mixed mapping should unmap");
        assert!(!mapping.has_mapped_regions());
        assert!(!mapping.has_dynamic_regions());
    }

    #[test]
    fn mixed_mapping_rolls_back_every_map_boundary_in_reverse_order() {
        let region_count = {
            let fixture = materialized_fixture(product_memory_hotplug_fixture(
                SnapshotV2DeviceTransportKind::Mmio,
            ));
            fixture.memory.regions().len()
        };
        assert!(region_count >= 3);

        for fail_map_on in 1..=region_count {
            let fixture = materialized_fixture(product_memory_hotplug_fixture(
                SnapshotV2DeviceTransportKind::Mmio,
            ));
            let plan = prepare_hvf_snapshot_v2_memory_hotplug_mapping_plan(
                &fixture.topology,
                &fixture.memory,
                FIXTURE_MEMORY_MIB * MIB,
            )
            .expect("mixed mapping should plan");
            let expected_ranges = fixture
                .memory
                .regions()
                .iter()
                .map(|region| region.range())
                .collect::<Vec<_>>();
            let mapper = Arc::new(MemoryHotplugMappingTestMapper::failing(
                Some(fail_map_on),
                None,
            ));
            let failure = HvfGuestMemoryMapping::map_snapshot_v2_memory_hotplug_with_mapper(
                fixture.memory,
                &plan,
                HvfMemoryPermissions::GUEST_RAM,
                mapper.clone(),
            )
            .expect_err("injected map boundary should fail");
            let mut expected_unmaps = expected_ranges[..fail_map_on - 1].to_vec();
            expected_unmaps.reverse();

            assert_eq!(
                mapper.mapped_ranges(),
                expected_ranges[..fail_map_on].to_vec()
            );
            assert_eq!(mapper.unmapped_ranges(), expected_unmaps);
            assert!(matches!(
                failure.error,
                crate::memory::HvfGuestMemoryMappingError::MapFailed {
                    range,
                    ref cleanup_failures,
                    ..
                } if range == expected_ranges[fail_map_on - 1] && cleanup_failures.is_empty()
            ));
            assert!(!failure.mapping.has_mapped_regions());
            assert!(!failure.mapping.has_dynamic_regions());
        }
    }

    #[test]
    fn mixed_mapping_retains_only_failed_cleanup_owners_until_retry() {
        let fixture = materialized_fixture(product_memory_hotplug_fixture(
            SnapshotV2DeviceTransportKind::Mmio,
        ));
        let plan = prepare_hvf_snapshot_v2_memory_hotplug_mapping_plan(
            &fixture.topology,
            &fixture.memory,
            FIXTURE_MEMORY_MIB * MIB,
        )
        .expect("mixed mapping should plan");
        let region_count = fixture.memory.regions().len();
        assert!(region_count >= 3);
        let expected_ranges = fixture
            .memory
            .regions()
            .iter()
            .map(|region| region.range())
            .collect::<Vec<_>>();
        let retained_dynamic_range = expected_ranges[region_count - 2];
        assert!(plan.dynamic_ranges().iter().any(|range| {
            range.start() <= retained_dynamic_range.start()
                && range.end_exclusive() >= retained_dynamic_range.end_exclusive()
        }));
        let mapper = Arc::new(MemoryHotplugMappingTestMapper::failing(
            Some(region_count),
            Some(1),
        ));
        let mut failure = HvfGuestMemoryMapping::map_snapshot_v2_memory_hotplug_with_mapper(
            fixture.memory,
            &plan,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect_err("map plus reverse-cleanup failure should retain authority");

        assert!(matches!(
            failure.error,
            crate::memory::HvfGuestMemoryMappingError::MapFailed {
                ref cleanup_failures,
                ..
            } if cleanup_failures.len() == 1
                && cleanup_failures[0].range() == retained_dynamic_range
        ));
        assert!(failure.mapping.has_mapped_regions());
        assert!(failure.mapping.has_dynamic_regions());

        mapper.allow_unmaps();
        failure
            .mapping
            .unmap_all()
            .expect("retained cleanup owner should retry");
        assert!(!failure.mapping.has_mapped_regions());
        assert!(!failure.mapping.has_dynamic_regions());
        assert_eq!(
            mapper.unmapped_ranges().last(),
            Some(&retained_dynamic_range)
        );
    }
}
