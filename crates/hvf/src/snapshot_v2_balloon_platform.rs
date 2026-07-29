//! Host-free exact native-v2 2.9 balloon platform-product planning.

use std::fmt;

use bangbang_runtime::balloon::BalloonMmioLayout;
use bangbang_runtime::entropy::{EntropyMmioLayout, VIRTIO_RNG_QUEUE_SIZES};
use bangbang_runtime::fdt::{Arm64FdtPciHost, Arm64FdtRegion, Arm64FdtVirtioMmioDevice};
use bangbang_runtime::interrupt::GuestInterruptLine;
use bangbang_runtime::memory::GuestMemoryRange;
use bangbang_runtime::mmio::{MmioRegion, MmioRegionId};
use bangbang_runtime::pci::{Arm64PciAddressPlan, PciSbdf};
use bangbang_runtime::rtc::RtcMmioLayout;
use bangbang_runtime::snapshot_balloon_v2_9::{
    PreparedSnapshotV2BalloonTransport, SnapshotV2BalloonRestorePlan,
};
use bangbang_runtime::snapshot_device_v2::{
    SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind,
};
use bangbang_runtime::snapshot_device_v2_6::PreparedSnapshotV2StorageBundle;
use bangbang_runtime::snapshot_entropy_v2_8::{
    PreparedSnapshotV2EntropyTransport, SnapshotV2EntropyRestorePlan,
};
use bangbang_runtime::storage_capture::StorageDeviceOrigin;
use bangbang_runtime::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE;
use bangbang_runtime::virtio_pci::VirtioPciEndpointPhase;

use crate::gic::{HvfGicInterruptLineAllocator, HvfGicMsiMetadata};
use crate::snapshot_v2::HvfSnapshotV2PlatformState;
use crate::snapshot_v2_entropy_platform::{
    HvfSnapshotV2EntropyPciEndpointPlan, PrepareHvfSnapshotV2EntropyPciPlatformPlanError,
    prepare_hvf_snapshot_v2_entropy_pci_platform_plan_with_prefix,
    register_active_retained_pci_routes,
};
use crate::snapshot_v2_multi_block_platform::{
    snapshot_v2_pci_endpoint_placement, snapshot_v2_pci_endpoint_route_count,
};
use crate::snapshot_v2_platform::{PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID};
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
use crate::startup::{PCI_ENDPOINT_SLOT_COUNT, pci_balloon_restore_gic_msi_configuration};

const REDACTED: &str = "<redacted>";

/// One supported exact-2.9 balloon product shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfSnapshotV2BalloonProductKind {
    /// Restored serial plus balloon.
    SerialBalloon,
    /// Restored serial, balloon, and storage.
    SerialBalloonStorage,
    /// Restored serial, balloon, and entropy.
    SerialBalloonEntropy,
    /// Restored serial, balloon, storage, and entropy.
    SerialBalloonStorageEntropy,
}

pub(crate) enum HvfSnapshotV2BalloonPreparedProductParts {
    Balloon {
        balloon: SnapshotV2BalloonRestorePlan,
    },
    Storage {
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
    },
    Entropy {
        balloon: SnapshotV2BalloonRestorePlan,
        entropy: SnapshotV2EntropyRestorePlan,
    },
    StorageEntropy {
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
        entropy: SnapshotV2EntropyRestorePlan,
    },
}

/// One closed set of detached exact-2.9 component continuations.
///
/// Construction consumes every component into one tagged shape. A successful
/// platform plan retains this value so independently prepared balloon,
/// storage, and entropy continuations cannot be recombined afterward.
pub struct HvfSnapshotV2BalloonPreparedProduct {
    parts: HvfSnapshotV2BalloonPreparedProductParts,
}

impl HvfSnapshotV2BalloonPreparedProduct {
    /// Closes one serial-plus-balloon product.
    pub fn serial_balloon(balloon: SnapshotV2BalloonRestorePlan) -> Self {
        Self {
            parts: HvfSnapshotV2BalloonPreparedProductParts::Balloon { balloon },
        }
    }

    /// Closes one serial-plus-balloon-and-storage product.
    pub fn serial_balloon_storage(
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2BalloonPreparedProductParts::Storage { balloon, storage },
        }
    }

    /// Closes one serial-plus-balloon-and-entropy product.
    pub fn serial_balloon_entropy(
        balloon: SnapshotV2BalloonRestorePlan,
        entropy: SnapshotV2EntropyRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2BalloonPreparedProductParts::Entropy { balloon, entropy },
        }
    }

    /// Closes one serial-plus-balloon-storage-and-entropy product.
    pub fn serial_balloon_storage_entropy(
        balloon: SnapshotV2BalloonRestorePlan,
        storage: PreparedSnapshotV2StorageBundle,
        entropy: SnapshotV2EntropyRestorePlan,
    ) -> Self {
        Self {
            parts: HvfSnapshotV2BalloonPreparedProductParts::StorageEntropy {
                balloon,
                storage,
                entropy,
            },
        }
    }

    /// Returns the closed product shape.
    pub const fn kind(&self) -> HvfSnapshotV2BalloonProductKind {
        match self.parts {
            HvfSnapshotV2BalloonPreparedProductParts::Balloon { .. } => {
                HvfSnapshotV2BalloonProductKind::SerialBalloon
            }
            HvfSnapshotV2BalloonPreparedProductParts::Storage { .. } => {
                HvfSnapshotV2BalloonProductKind::SerialBalloonStorage
            }
            HvfSnapshotV2BalloonPreparedProductParts::Entropy { .. } => {
                HvfSnapshotV2BalloonProductKind::SerialBalloonEntropy
            }
            HvfSnapshotV2BalloonPreparedProductParts::StorageEntropy { .. } => {
                HvfSnapshotV2BalloonProductKind::SerialBalloonStorageEntropy
            }
        }
    }

    /// Returns the owned detached balloon continuation.
    pub const fn balloon(&self) -> &SnapshotV2BalloonRestorePlan {
        match &self.parts {
            HvfSnapshotV2BalloonPreparedProductParts::Balloon { balloon }
            | HvfSnapshotV2BalloonPreparedProductParts::Storage { balloon, .. }
            | HvfSnapshotV2BalloonPreparedProductParts::Entropy { balloon, .. }
            | HvfSnapshotV2BalloonPreparedProductParts::StorageEntropy { balloon, .. } => balloon,
        }
    }

    /// Returns the owned detached storage continuation when present.
    pub const fn storage(&self) -> Option<&PreparedSnapshotV2StorageBundle> {
        match &self.parts {
            HvfSnapshotV2BalloonPreparedProductParts::Balloon { .. }
            | HvfSnapshotV2BalloonPreparedProductParts::Entropy { .. } => None,
            HvfSnapshotV2BalloonPreparedProductParts::Storage { storage, .. }
            | HvfSnapshotV2BalloonPreparedProductParts::StorageEntropy { storage, .. } => {
                Some(storage)
            }
        }
    }

    /// Returns the owned detached entropy continuation when present.
    pub const fn entropy(&self) -> Option<&SnapshotV2EntropyRestorePlan> {
        match &self.parts {
            HvfSnapshotV2BalloonPreparedProductParts::Balloon { .. }
            | HvfSnapshotV2BalloonPreparedProductParts::Storage { .. } => None,
            HvfSnapshotV2BalloonPreparedProductParts::Entropy { entropy, .. }
            | HvfSnapshotV2BalloonPreparedProductParts::StorageEntropy { entropy, .. } => {
                Some(entropy)
            }
        }
    }

    pub(crate) fn into_parts(self) -> HvfSnapshotV2BalloonPreparedProductParts {
        self.parts
    }
}

impl fmt::Debug for HvfSnapshotV2BalloonPreparedProduct {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2BalloonPreparedProduct")
            .field("kind", &self.kind())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Canonical destination layouts for one balloon-aware MMIO process.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2BalloonMmioProcessConfig {
    balloon_layout: BalloonMmioLayout,
    storage: HvfSnapshotV2StorageMmioProcessConfig,
    entropy_layout: EntropyMmioLayout,
}

impl HvfSnapshotV2BalloonMmioProcessConfig {
    /// Creates one closed destination MMIO placement policy.
    pub const fn new(
        balloon_layout: BalloonMmioLayout,
        storage: HvfSnapshotV2StorageMmioProcessConfig,
        entropy_layout: EntropyMmioLayout,
    ) -> Self {
        Self {
            balloon_layout,
            storage,
            entropy_layout,
        }
    }

    /// Returns the canonical balloon MMIO layout.
    pub const fn balloon_layout(self) -> BalloonMmioLayout {
        self.balloon_layout
    }

    /// Returns the canonical block/pmem MMIO layouts.
    pub const fn storage(self) -> HvfSnapshotV2StorageMmioProcessConfig {
        self.storage
    }

    /// Returns the canonical entropy MMIO layout.
    pub const fn entropy_layout(self) -> EntropyMmioLayout {
        self.entropy_layout
    }
}

impl fmt::Debug for HvfSnapshotV2BalloonMmioProcessConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2BalloonMmioProcessConfig")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Exact typed placement of one restored MMIO balloon endpoint.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2BalloonMmioEndpointPlan {
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    fdt_device: Arm64FdtVirtioMmioDevice,
}

impl HvfSnapshotV2BalloonMmioEndpointPlan {
    /// Returns the exact dispatcher region.
    pub const fn region(self) -> MmioRegion {
        self.region
    }

    /// Returns the first optional product SPI.
    pub const fn interrupt_line(self) -> GuestInterruptLine {
        self.interrupt_line
    }

    /// Returns the semantic FDT device fact.
    pub const fn fdt_device(self) -> Arm64FdtVirtioMmioDevice {
        self.fdt_device
    }
}

impl fmt::Debug for HvfSnapshotV2BalloonMmioEndpointPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2BalloonMmioEndpointPlan")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Exact typed placement of one restored MMIO entropy endpoint.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2BalloonEntropyMmioEndpointPlan {
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    fdt_device: Arm64FdtVirtioMmioDevice,
}

impl HvfSnapshotV2BalloonEntropyMmioEndpointPlan {
    /// Returns the exact dispatcher region.
    pub const fn region(self) -> MmioRegion {
        self.region
    }

    /// Returns the exact following optional SPI.
    pub const fn interrupt_line(self) -> GuestInterruptLine {
        self.interrupt_line
    }

    /// Returns the semantic FDT device fact.
    pub const fn fdt_device(self) -> Arm64FdtVirtioMmioDevice {
        self.fdt_device
    }
}

impl fmt::Debug for HvfSnapshotV2BalloonEntropyMmioEndpointPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2BalloonEntropyMmioEndpointPlan")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Exact typed placement of one restored PCI balloon endpoint.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2BalloonPciEndpointPlan {
    origin: StorageDeviceOrigin,
    sbdf: PciSbdf,
    bar_region_id: MmioRegionId,
    bar_range: GuestMemoryRange,
    route_count: usize,
    msi_interrupt_count: u32,
}

impl HvfSnapshotV2BalloonPciEndpointPlan {
    /// Returns the retained startup/runtime origin.
    pub const fn origin(self) -> StorageDeviceOrigin {
        self.origin
    }

    /// Returns the exact slot-zero function.
    pub const fn sbdf(self) -> PciSbdf {
        self.sbdf
    }

    /// Returns the exact dispatcher region ID.
    pub const fn bar_region_id(self) -> MmioRegionId {
        self.bar_region_id
    }

    /// Returns the exact retained capability BAR.
    pub const fn bar_range(self) -> GuestMemoryRange {
        self.bar_range
    }

    /// Returns queue count plus one configuration route.
    pub const fn route_count(self) -> usize {
        self.route_count
    }

    /// Returns the exact retained canonical GICv2m pool size.
    pub const fn msi_interrupt_count(self) -> u32 {
        self.msi_interrupt_count
    }
}

impl fmt::Debug for HvfSnapshotV2BalloonPciEndpointPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2BalloonPciEndpointPlan")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete immutable exact-2.9 balloon-aware MMIO product proof.
pub struct HvfSnapshotV2BalloonMmioPlatformPlan {
    product: HvfSnapshotV2BalloonPreparedProduct,
    balloon: HvfSnapshotV2BalloonMmioEndpointPlan,
    storage: Option<HvfSnapshotV2StorageMmioPlatformPlan>,
    entropy: Option<HvfSnapshotV2BalloonEntropyMmioEndpointPlan>,
    serial_interrupt: GuestInterruptLine,
    vmgenid_interrupt: GuestInterruptLine,
    vmclock_interrupt: GuestInterruptLine,
}

pub(crate) struct HvfSnapshotV2BalloonMmioPlatformPlanParts {
    pub(crate) product: HvfSnapshotV2BalloonPreparedProduct,
    pub(crate) balloon: HvfSnapshotV2BalloonMmioEndpointPlan,
    pub(crate) storage: Option<HvfSnapshotV2StorageMmioPlatformPlan>,
    pub(crate) entropy: Option<HvfSnapshotV2BalloonEntropyMmioEndpointPlan>,
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

impl HvfSnapshotV2BalloonMmioPlatformPlan {
    /// Returns the closed product shape.
    pub const fn kind(&self) -> HvfSnapshotV2BalloonProductKind {
        self.product.kind()
    }

    /// Returns the retained prepared components.
    pub const fn product(&self) -> &HvfSnapshotV2BalloonPreparedProduct {
        &self.product
    }

    /// Returns the checked balloon endpoint.
    pub const fn balloon(&self) -> HvfSnapshotV2BalloonMmioEndpointPlan {
        self.balloon
    }

    /// Returns the checked storage platform continuation when present.
    pub const fn storage(&self) -> Option<&HvfSnapshotV2StorageMmioPlatformPlan> {
        self.storage.as_ref()
    }

    /// Returns the checked entropy endpoint when present.
    pub const fn entropy(&self) -> Option<HvfSnapshotV2BalloonEntropyMmioEndpointPlan> {
        self.entropy
    }

    /// Returns the final exact serial SPI.
    pub const fn serial_interrupt(&self) -> GuestInterruptLine {
        self.serial_interrupt
    }

    /// Returns the final exact VMGenID SPI.
    pub const fn vmgenid_interrupt(&self) -> GuestInterruptLine {
        self.vmgenid_interrupt
    }

    /// Returns the final exact VMClock SPI.
    pub const fn vmclock_interrupt(&self) -> GuestInterruptLine {
        self.vmclock_interrupt
    }

    pub(crate) fn into_parts(self) -> HvfSnapshotV2BalloonMmioPlatformPlanParts {
        HvfSnapshotV2BalloonMmioPlatformPlanParts {
            product: self.product,
            balloon: self.balloon,
            storage: self.storage,
            entropy: self.entropy,
            serial_interrupt: self.serial_interrupt,
            vmgenid_interrupt: self.vmgenid_interrupt,
            vmclock_interrupt: self.vmclock_interrupt,
        }
    }
}

impl fmt::Debug for HvfSnapshotV2BalloonMmioPlatformPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2BalloonMmioPlatformPlan")
            .field("kind", &self.kind())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete immutable exact-2.9 balloon-aware PCI product proof.
pub struct HvfSnapshotV2BalloonPciPlatformPlan {
    product: HvfSnapshotV2BalloonPreparedProduct,
    balloon: HvfSnapshotV2BalloonPciEndpointPlan,
    storage: Option<HvfSnapshotV2StoragePciPlatformPlan>,
    entropy: Option<HvfSnapshotV2EntropyPciEndpointPlan>,
    host: Arm64FdtPciHost,
    msi: HvfGicMsiMetadata,
    route_demand: usize,
    serial_interrupt: GuestInterruptLine,
    vmgenid_interrupt: GuestInterruptLine,
    vmclock_interrupt: GuestInterruptLine,
}

pub(crate) struct HvfSnapshotV2BalloonPciPlatformPlanParts {
    pub(crate) product: HvfSnapshotV2BalloonPreparedProduct,
    pub(crate) balloon: HvfSnapshotV2BalloonPciEndpointPlan,
    pub(crate) storage: Option<HvfSnapshotV2StoragePciPlatformPlan>,
    pub(crate) entropy: Option<HvfSnapshotV2EntropyPciEndpointPlan>,
    pub(crate) host: Arm64FdtPciHost,
    pub(crate) msi: HvfGicMsiMetadata,
    pub(crate) route_demand: usize,
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

impl HvfSnapshotV2BalloonPciPlatformPlan {
    /// Returns the closed product shape.
    pub const fn kind(&self) -> HvfSnapshotV2BalloonProductKind {
        self.product.kind()
    }

    /// Returns the retained prepared components.
    pub const fn product(&self) -> &HvfSnapshotV2BalloonPreparedProduct {
        &self.product
    }

    /// Returns the checked slot-zero balloon endpoint.
    pub const fn balloon(&self) -> HvfSnapshotV2BalloonPciEndpointPlan {
        self.balloon
    }

    /// Returns the checked storage platform continuation when present.
    pub const fn storage(&self) -> Option<&HvfSnapshotV2StoragePciPlatformPlan> {
        self.storage.as_ref()
    }

    /// Returns the checked following entropy endpoint when present.
    pub const fn entropy(&self) -> Option<HvfSnapshotV2EntropyPciEndpointPlan> {
        self.entropy
    }

    /// Returns the coherent retained PCI host.
    pub const fn host(&self) -> Arm64FdtPciHost {
        self.host
    }

    /// Returns the coherent retained GICv2m metadata.
    pub const fn msi(&self) -> HvfGicMsiMetadata {
        self.msi
    }

    /// Returns total configuration-plus-queue route demand.
    pub const fn route_demand(&self) -> usize {
        self.route_demand
    }

    /// Returns the final exact serial SPI.
    pub const fn serial_interrupt(&self) -> GuestInterruptLine {
        self.serial_interrupt
    }

    /// Returns the final exact VMGenID SPI.
    pub const fn vmgenid_interrupt(&self) -> GuestInterruptLine {
        self.vmgenid_interrupt
    }

    /// Returns the final exact VMClock SPI.
    pub const fn vmclock_interrupt(&self) -> GuestInterruptLine {
        self.vmclock_interrupt
    }

    pub(crate) fn into_parts(self) -> HvfSnapshotV2BalloonPciPlatformPlanParts {
        HvfSnapshotV2BalloonPciPlatformPlanParts {
            product: self.product,
            balloon: self.balloon,
            storage: self.storage,
            entropy: self.entropy,
            host: self.host,
            msi: self.msi,
            route_demand: self.route_demand,
            serial_interrupt: self.serial_interrupt,
            vmgenid_interrupt: self.vmgenid_interrupt,
            vmclock_interrupt: self.vmclock_interrupt,
        }
    }
}

impl fmt::Debug for HvfSnapshotV2BalloonPciPlatformPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2BalloonPciPlatformPlan")
            .field("kind", &self.kind())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Redacted rejection from exact-2.9 balloon platform-product planning.
pub enum PrepareHvfSnapshotV2BalloonPlatformPlanError {
    /// The captured product platform profile is not the process profile.
    PlatformProfile,
    /// Prepared components do not all select the requested transport.
    TransportPolicy,
    /// A retained region, interrupt, endpoint, BAR, or host identity diverged.
    ResourcePlan,
    /// The complete product exceeds the canonical PCI endpoint capacity.
    PciCapacity { count: usize, maximum: usize },
    /// A queue, device region, or pmem mapping conflicts with another owner.
    RangeConflict,
    /// Active MSI messages are malformed, out of range, or collide.
    RouteConflict,
    /// A temporary destination-only inventory could not reserve capacity.
    Allocation,
    /// Prefix-aware MMIO storage planning failed.
    StorageMmio(Box<PrepareHvfSnapshotV2StorageMmioPlatformPlanError>),
    /// Prefix-aware PCI storage planning failed.
    StoragePci(Box<PrepareHvfSnapshotV2StoragePciPlatformPlanError>),
    /// Prefix-aware PCI entropy planning failed.
    EntropyPci(Box<PrepareHvfSnapshotV2EntropyPciPlatformPlanError>),
}

impl fmt::Debug for PrepareHvfSnapshotV2BalloonPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self {
            Self::PlatformProfile => "platform-profile",
            Self::TransportPolicy => "transport-policy",
            Self::ResourcePlan => "resource-plan",
            Self::PciCapacity { .. } => "pci-capacity",
            Self::RangeConflict => "range-conflict",
            Self::RouteConflict => "route-conflict",
            Self::Allocation => "allocation",
            Self::StorageMmio(_) => "storage-mmio",
            Self::StoragePci(_) => "storage-pci",
            Self::EntropyPci(_) => "entropy-pci",
        };
        formatter
            .debug_struct("PrepareHvfSnapshotV2BalloonPlatformPlanError")
            .field("category", &category)
            .field("state", &REDACTED)
            .finish()
    }
}

impl fmt::Display for PrepareHvfSnapshotV2BalloonPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PlatformProfile => {
                "native-v2 balloon platform does not use the product process profile"
            }
            Self::TransportPolicy => "native-v2 balloon product transport policy is inconsistent",
            Self::ResourcePlan => "native-v2 balloon platform resources are inconsistent",
            Self::PciCapacity { .. } => {
                "native-v2 balloon product PCI endpoint capacity is exceeded"
            }
            Self::RangeConflict => {
                "native-v2 balloon product ranges overlap another platform owner"
            }
            Self::RouteConflict => "native-v2 balloon product active MSI routes are inconsistent",
            Self::Allocation => "native-v2 balloon platform temporary inventory allocation failed",
            Self::StorageMmio(_) => "native-v2 balloon-prefixed MMIO storage planning failed",
            Self::StoragePci(_) => "native-v2 balloon-prefixed PCI storage planning failed",
            Self::EntropyPci(_) => "native-v2 balloon-prefixed PCI entropy planning failed",
        })
    }
}

impl std::error::Error for PrepareHvfSnapshotV2BalloonPlatformPlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::StorageMmio(source) => Some(source),
            Self::StoragePci(source) => Some(source),
            Self::EntropyPci(source) => Some(source),
            Self::PlatformProfile
            | Self::TransportPolicy
            | Self::ResourcePlan
            | Self::PciCapacity { .. }
            | Self::RangeConflict
            | Self::RouteConflict
            | Self::Allocation => None,
        }
    }
}

/// Proves one complete balloon-aware MMIO product before live construction.
pub fn prepare_hvf_snapshot_v2_balloon_mmio_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2BalloonPreparedProduct,
    process: HvfSnapshotV2BalloonMmioProcessConfig,
) -> Result<HvfSnapshotV2BalloonMmioPlatformPlan, PrepareHvfSnapshotV2BalloonPlatformPlanError> {
    prepare_balloon_mmio_platform_plan(
        platform,
        product,
        process,
        &mut SystemBalloonPlatformPlanReserve,
    )
}

/// Proves one complete balloon-aware PCI product before live construction.
pub fn prepare_hvf_snapshot_v2_balloon_pci_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2BalloonPreparedProduct,
) -> Result<HvfSnapshotV2BalloonPciPlatformPlan, PrepareHvfSnapshotV2BalloonPlatformPlanError> {
    prepare_balloon_pci_platform_plan(platform, product, &mut SystemBalloonPlatformPlanReserve)
}

fn prepare_balloon_mmio_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2BalloonPreparedProduct,
    process: HvfSnapshotV2BalloonMmioProcessConfig,
    reserve: &mut impl BalloonPlatformPlanReserve,
) -> Result<HvfSnapshotV2BalloonMmioPlatformPlan, PrepareHvfSnapshotV2BalloonPlatformPlanError> {
    validate_product_profile(platform)?;
    if platform
        .global()
        .compatibility()
        .gic_metadata()
        .msi
        .is_some()
        || product.balloon().transport_kind() != SnapshotV2DeviceTransportKind::Mmio
        || product
            .storage()
            .is_some_and(|storage| storage.transport_kind() != SnapshotV2DeviceTransportKind::Mmio)
        || product
            .entropy()
            .is_some_and(|entropy| entropy.transport_kind() != SnapshotV2DeviceTransportKind::Mmio)
    {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::TransportPolicy);
    }

    let expected_balloon_region = mmio_region(
        process.balloon_layout().region_id(),
        process.balloon_layout().address(),
    )?;
    let PreparedSnapshotV2BalloonTransport::Mmio(balloon_transport) = product.balloon().transport()
    else {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::TransportPolicy);
    };
    if balloon_transport.region() != expected_balloon_region
        || mmio_region_conflicts_with_platform(
            platform,
            expected_balloon_region,
            &platform.global().compatibility().gic_metadata(),
        )
        .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan);
    }
    for ranges in product.balloon().queue_ranges() {
        if queue_ranges_conflict_with_platform(platform, Some(*ranges))
            .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?
        {
            return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::RangeConflict);
        }
    }
    let balloon_interrupt = balloon_transport.interrupt_line();

    let entropy_endpoint = product
        .entropy()
        .map(|entropy| prepare_entropy_mmio_endpoint(platform, entropy, process.entropy_layout()))
        .transpose()?;

    let storage_plan = product
        .storage()
        .map(|storage| {
            prepare_hvf_snapshot_v2_storage_mmio_platform_plan_with_prefix(
                platform,
                storage,
                process.storage(),
                HvfSnapshotV2StorageMmioPlatformPrefix::one(
                    expected_balloon_region,
                    balloon_interrupt,
                ),
                entropy_endpoint.map(|entropy| entropy.interrupt_line()),
            )
            .map_err(|source| {
                PrepareHvfSnapshotV2BalloonPlatformPlanError::StorageMmio(Box::new(source))
            })
        })
        .transpose()?;

    let (serial_interrupt, vmgenid_interrupt, vmclock_interrupt) =
        validate_mmio_interrupt_sequence(
            platform,
            balloon_interrupt,
            storage_plan.as_ref(),
            entropy_endpoint,
        )?;

    let region_count = 1_usize
        .checked_add(storage_record_count_mmio(storage_plan.as_ref()))
        .and_then(|count| count.checked_add(usize::from(entropy_endpoint.is_some())))
        .ok_or(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
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
    regions.push(expected_balloon_region);
    if let Some(storage) = &storage_plan {
        regions.extend(
            storage
                .block_records()
                .iter()
                .chain(storage.pmem_records())
                .map(|record| record.region()),
        );
    }
    if let Some(entropy) = entropy_endpoint {
        regions.push(entropy.region());
    }
    append_product_memory_ranges(&product, &mut queues, &mut pmem);
    if !aggregate_ranges_are_disjoint(&regions, &queues, &pmem) {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::RangeConflict);
    }

    Ok(HvfSnapshotV2BalloonMmioPlatformPlan {
        product,
        balloon: HvfSnapshotV2BalloonMmioEndpointPlan {
            region: expected_balloon_region,
            interrupt_line: balloon_interrupt,
            fdt_device: mmio_fdt_device(expected_balloon_region, balloon_interrupt),
        },
        storage: storage_plan,
        entropy: entropy_endpoint,
        serial_interrupt,
        vmgenid_interrupt,
        vmclock_interrupt,
    })
}

fn prepare_balloon_pci_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    product: HvfSnapshotV2BalloonPreparedProduct,
    reserve: &mut impl BalloonPlatformPlanReserve,
) -> Result<HvfSnapshotV2BalloonPciPlatformPlan, PrepareHvfSnapshotV2BalloonPlatformPlanError> {
    validate_product_profile(platform)?;
    if product.balloon().transport_kind() != SnapshotV2DeviceTransportKind::Pci
        || product
            .storage()
            .is_some_and(|storage| storage.transport_kind() != SnapshotV2DeviceTransportKind::Pci)
        || product
            .entropy()
            .is_some_and(|entropy| entropy.transport_kind() != SnapshotV2DeviceTransportKind::Pci)
    {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::TransportPolicy);
    }
    let PreparedSnapshotV2BalloonTransport::Pci(balloon_transport) = product.balloon().transport()
    else {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::TransportPolicy);
    };
    let address_plan = Arm64PciAddressPlan::firecracker_v1_16()
        .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    let host = Arm64FdtPciHost::from_address_plan(address_plan);
    let gic = platform.global().compatibility().gic_metadata();
    let msi = gic
        .msi
        .ok_or(PrepareHvfSnapshotV2BalloonPlatformPlanError::TransportPolicy)?;
    let queue_count = balloon_transport.device().queue_layout().queue_count();
    let balloon_route_count = snapshot_v2_pci_endpoint_route_count(queue_count)
        .filter(|count| (3..=6).contains(count))
        .ok_or(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    let expected_msi =
        pci_balloon_restore_gic_msi_configuration(queue_count, product.entropy().is_some())
            .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    let expected_msi_interrupt_count = expected_msi.interrupt_count().get();
    let balloon_placement = snapshot_v2_pci_endpoint_placement(address_plan, 0)
        .ok_or(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    if msi.interrupt_range.count != expected_msi_interrupt_count
        || balloon_transport.origin() != StorageDeviceOrigin::Startup
        || balloon_transport.sbdf() != balloon_placement.sbdf
        || balloon_transport.bar_range() != balloon_placement.bar_range
        || balloon_transport.retained().phase() != VirtioPciEndpointPhase::Active
    {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan);
    }
    let balloon_origin = balloon_transport.origin();
    for ranges in product.balloon().queue_ranges() {
        if queue_ranges_conflict_with_pci_platform(platform, Some(*ranges), &gic, address_plan)
            .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?
        {
            return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::RangeConflict);
        }
    }

    let reserved_entropy = usize::from(product.entropy().is_some());
    let storage_plan = product
        .storage()
        .map(|storage| {
            prepare_hvf_snapshot_v2_storage_pci_platform_plan_with_prefix(
                platform,
                storage,
                HvfSnapshotV2StoragePciPlatformPrefix::exact(
                    1,
                    reserved_entropy,
                    expected_msi_interrupt_count,
                ),
            )
            .map_err(|source| {
                PrepareHvfSnapshotV2BalloonPlatformPlanError::StoragePci(Box::new(source))
            })
        })
        .transpose()?;
    let storage_count = storage_plan
        .as_ref()
        .map_or(0, |storage| storage.pci().record_count());
    let preceding_entropy = 1_usize
        .checked_add(storage_count)
        .ok_or(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    let storage_pair = match (product.storage(), storage_plan.as_ref()) {
        (Some(storage), Some(plan)) => Some((storage, plan)),
        (None, None) => None,
        _ => return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan),
    };
    let entropy_endpoint = product
        .entropy()
        .map(|entropy| {
            prepare_hvf_snapshot_v2_entropy_pci_platform_plan_with_prefix(
                platform,
                storage_pair,
                entropy,
                1,
                preceding_entropy,
                expected_msi_interrupt_count,
            )
            .map_err(|source| {
                PrepareHvfSnapshotV2BalloonPlatformPlanError::EntropyPci(Box::new(source))
            })
        })
        .transpose()?;

    let endpoint_count = preceding_entropy
        .checked_add(usize::from(entropy_endpoint.is_some()))
        .ok_or(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    if endpoint_count > PCI_ENDPOINT_SLOT_COUNT {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::PciCapacity {
            count: endpoint_count,
            maximum: PCI_ENDPOINT_SLOT_COUNT,
        });
    }
    let storage_route_demand = storage_plan
        .as_ref()
        .map_or(0, |storage| storage.pci().route_demand());
    let entropy_route_demand = entropy_endpoint.map_or(0, |entropy| entropy.route_count());
    let route_demand = balloon_route_count
        .checked_add(storage_route_demand)
        .and_then(|count| count.checked_add(entropy_route_demand))
        .ok_or(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    if route_demand
        > usize::try_from(msi.interrupt_range.count)
            .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::RouteConflict);
    }

    let queue_range_count = product_queue_range_count(&product)?;
    let pmem_count = product
        .storage()
        .map_or(0, |storage| storage.pmem_records().len());
    let mut queues = Vec::new();
    let mut pmem = Vec::new();
    let mut active_routes = Vec::new();
    let mut endpoint_bars = Vec::new();
    reserve.reserve(&mut queues, queue_range_count)?;
    reserve.reserve(&mut pmem, pmem_count)?;
    reserve.reserve(&mut active_routes, route_demand)?;
    reserve.reserve(&mut endpoint_bars, endpoint_count)?;
    append_product_memory_ranges(&product, &mut queues, &mut pmem);
    if !aggregate_ranges_are_disjoint(&[], &queues, &pmem) {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::RangeConflict);
    }

    endpoint_bars.push(balloon_placement.bar_range);
    if let Some(storage) = &storage_plan {
        if storage.pci().host() != host || storage.pci().msi() != msi {
            return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan);
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
    if let Some(entropy) = entropy_endpoint {
        if entropy.msi_interrupt_count() != expected_msi_interrupt_count {
            return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan);
        }
        endpoint_bars.push(entropy.bar_range());
    }
    if !ranges_are_pairwise_disjoint(&endpoint_bars) {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan);
    }

    if !register_active_retained_pci_routes(
        balloon_transport.retained().msix_state(),
        msi,
        queue_count,
        balloon_route_count,
        &mut active_routes,
    ) {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::RouteConflict);
    }
    if let Some(storage) = product.storage() {
        for record in storage
            .block_bundle()
            .map_or(&[][..], |block| block.records())
            .iter()
        {
            let SnapshotV2DeviceTransport::Pci(captured) = record.transport() else {
                return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::TransportPolicy);
            };
            if !register_active_pci_routes(captured, &mut active_routes) {
                return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::RouteConflict);
            }
        }
        for record in storage.pmem_records() {
            let SnapshotV2DeviceTransport::Pci(captured) = record.transport() else {
                return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::TransportPolicy);
            };
            if !register_active_pci_routes(captured, &mut active_routes) {
                return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::RouteConflict);
            }
        }
    }
    if let Some(entropy) = product.entropy() {
        let PreparedSnapshotV2EntropyTransport::Pci(entropy_transport) = entropy.transport() else {
            return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::TransportPolicy);
        };
        let route_count = snapshot_v2_pci_endpoint_route_count(VIRTIO_RNG_QUEUE_SIZES.len())
            .ok_or(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
        if !register_active_retained_pci_routes(
            entropy_transport.retained().msix_state(),
            msi,
            VIRTIO_RNG_QUEUE_SIZES.len(),
            route_count,
            &mut active_routes,
        ) {
            return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::RouteConflict);
        }
    }

    let (serial_interrupt, vmgenid_interrupt, vmclock_interrupt) =
        validate_pci_interrupt_sequence(platform, storage_plan.as_ref())?;
    Ok(HvfSnapshotV2BalloonPciPlatformPlan {
        product,
        balloon: HvfSnapshotV2BalloonPciEndpointPlan {
            origin: balloon_origin,
            sbdf: balloon_placement.sbdf,
            bar_region_id: balloon_placement.bar_region_id,
            bar_range: balloon_placement.bar_range,
            route_count: balloon_route_count,
            msi_interrupt_count: expected_msi_interrupt_count,
        },
        storage: storage_plan,
        entropy: entropy_endpoint,
        host,
        msi,
        route_demand,
        serial_interrupt,
        vmgenid_interrupt,
        vmclock_interrupt,
    })
}

fn validate_product_profile(
    platform: &HvfSnapshotV2PlatformState,
) -> Result<(), PrepareHvfSnapshotV2BalloonPlatformPlanError> {
    if !platform.machine().fdt().is_product_process_profile()
        || platform.time().rtc_layout()
            != RtcMmioLayout::new(PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID)
    {
        Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::PlatformProfile)
    } else {
        Ok(())
    }
}

fn prepare_entropy_mmio_endpoint(
    platform: &HvfSnapshotV2PlatformState,
    entropy: &SnapshotV2EntropyRestorePlan,
    layout: EntropyMmioLayout,
) -> Result<HvfSnapshotV2BalloonEntropyMmioEndpointPlan, PrepareHvfSnapshotV2BalloonPlatformPlanError>
{
    let PreparedSnapshotV2EntropyTransport::Mmio(transport) = entropy.transport() else {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::TransportPolicy);
    };
    let expected_region = mmio_region(layout.region_id(), layout.address())?;
    if transport.region() != expected_region {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan);
    }
    if mmio_region_conflicts_with_platform(
        platform,
        expected_region,
        &platform.global().compatibility().gic_metadata(),
    )
    .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?
        || queue_ranges_conflict_with_platform(platform, entropy.queue_ranges())
            .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::RangeConflict);
    }
    Ok(HvfSnapshotV2BalloonEntropyMmioEndpointPlan {
        region: expected_region,
        interrupt_line: transport.interrupt_line(),
        fdt_device: mmio_fdt_device(expected_region, transport.interrupt_line()),
    })
}

fn validate_mmio_interrupt_sequence(
    platform: &HvfSnapshotV2PlatformState,
    balloon_interrupt: GuestInterruptLine,
    storage: Option<&HvfSnapshotV2StorageMmioPlatformPlan>,
    entropy: Option<HvfSnapshotV2BalloonEntropyMmioEndpointPlan>,
) -> Result<
    (GuestInterruptLine, GuestInterruptLine, GuestInterruptLine),
    PrepareHvfSnapshotV2BalloonPlatformPlanError,
> {
    let mut allocator = HvfGicInterruptLineAllocator::from_metadata(
        &platform.global().compatibility().gic_metadata(),
    )
    .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    if allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?
        != balloon_interrupt
    {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan);
    }
    if let Some(storage) = storage {
        for record in storage.block_records().iter().chain(storage.pmem_records()) {
            if allocator
                .allocate()
                .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?
                != record.interrupt_line()
            {
                return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan);
            }
        }
    }
    if let Some(entropy) = entropy
        && allocator
            .allocate()
            .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?
            != entropy.interrupt_line()
    {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan);
    }
    let serial = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    let vmgenid = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    let vmclock = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    if storage.is_some_and(|storage| {
        storage.serial_interrupt() != serial
            || storage.vmgenid_interrupt() != vmgenid
            || storage.vmclock_interrupt() != vmclock
    }) || platform.time().vmgenid().interrupt_line() != vmgenid
        || platform.time().vmclock().interrupt_line() != vmclock
    {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan);
    }
    Ok((serial, vmgenid, vmclock))
}

fn validate_pci_interrupt_sequence(
    platform: &HvfSnapshotV2PlatformState,
    storage: Option<&HvfSnapshotV2StoragePciPlatformPlan>,
) -> Result<
    (GuestInterruptLine, GuestInterruptLine, GuestInterruptLine),
    PrepareHvfSnapshotV2BalloonPlatformPlanError,
> {
    let mut allocator = HvfGicInterruptLineAllocator::from_metadata(
        &platform.global().compatibility().gic_metadata(),
    )
    .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    let serial = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    let vmgenid = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    let vmclock = allocator
        .allocate()
        .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    if storage.is_some_and(|storage| {
        storage.serial_interrupt() != serial
            || storage.vmgenid_interrupt() != vmgenid
            || storage.vmclock_interrupt() != vmclock
    }) || platform.time().vmgenid().interrupt_line() != vmgenid
        || platform.time().vmclock().interrupt_line() != vmclock
    {
        return Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan);
    }
    Ok((serial, vmgenid, vmclock))
}

fn mmio_region(
    region_id: MmioRegionId,
    address: bangbang_runtime::memory::GuestAddress,
) -> Result<MmioRegion, PrepareHvfSnapshotV2BalloonPlatformPlanError> {
    MmioRegion::new(region_id, address, VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
        .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)
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
    product: &HvfSnapshotV2BalloonPreparedProduct,
) -> Result<usize, PrepareHvfSnapshotV2BalloonPlatformPlanError> {
    let mut count = product
        .balloon()
        .queue_ranges()
        .len()
        .checked_mul(3)
        .ok_or(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    if let Some(storage) = product.storage() {
        let active_block = storage
            .block_bundle()
            .map_or(&[][..], |block| block.records())
            .iter()
            .filter(|record| record.queue_ranges().is_some())
            .count();
        let active_pmem = storage
            .pmem_records()
            .iter()
            .filter(|record| record.queue_ranges().is_some())
            .count();
        let active_storage = active_block
            .checked_add(active_pmem)
            .ok_or(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
        count = count
            .checked_add(
                active_storage
                    .checked_mul(3)
                    .ok_or(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?,
            )
            .ok_or(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    }
    if product
        .entropy()
        .is_some_and(|entropy| entropy.queue_ranges().is_some())
    {
        count = count
            .checked_add(3)
            .ok_or(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)?;
    }
    Ok(count)
}

fn append_product_memory_ranges(
    product: &HvfSnapshotV2BalloonPreparedProduct,
    queues: &mut Vec<GuestMemoryRange>,
    pmem: &mut Vec<GuestMemoryRange>,
) {
    for ranges in product.balloon().queue_ranges() {
        queues.extend(*ranges);
    }
    if let Some(storage) = product.storage() {
        for record in storage
            .block_bundle()
            .map_or(&[][..], |block| block.records())
            .iter()
        {
            if let Some(ranges) = record.queue_ranges() {
                queues.extend(ranges);
            }
        }
        for record in storage.pmem_records() {
            if let Some(ranges) = record.queue_ranges() {
                queues.extend(ranges);
            }
        }
        pmem.extend(
            storage
                .pmem_records()
                .iter()
                .map(|record| record.prepared_device().guest_range()),
        );
    }
    if let Some(ranges) = product
        .entropy()
        .and_then(SnapshotV2EntropyRestorePlan::queue_ranges)
    {
        queues.extend(ranges);
    }
}

fn aggregate_ranges_are_disjoint(
    regions: &[MmioRegion],
    queues: &[GuestMemoryRange],
    pmem: &[GuestMemoryRange],
) -> bool {
    for (index, region) in regions.iter().enumerate() {
        if regions
            .iter()
            .skip(index.saturating_add(1))
            .any(|other| region.id() == other.id() || region.range().overlaps(other.range()))
            || queues.iter().any(|queue| queue.overlaps(region.range()))
            || pmem.iter().any(|mapping| mapping.overlaps(region.range()))
        {
            return false;
        }
    }
    ranges_are_pairwise_disjoint(queues)
        && ranges_are_pairwise_disjoint(pmem)
        && !queues
            .iter()
            .any(|queue| pmem.iter().any(|mapping| queue.overlaps(*mapping)))
}

fn ranges_are_pairwise_disjoint(ranges: &[GuestMemoryRange]) -> bool {
    ranges.iter().enumerate().all(|(index, range)| {
        ranges
            .iter()
            .skip(index.saturating_add(1))
            .all(|other| !range.overlaps(*other))
    })
}

trait BalloonPlatformPlanReserve {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), PrepareHvfSnapshotV2BalloonPlatformPlanError>;
}

struct SystemBalloonPlatformPlanReserve;

impl BalloonPlatformPlanReserve for SystemBalloonPlatformPlanReserve {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), PrepareHvfSnapshotV2BalloonPlatformPlanError> {
        values
            .try_reserve_exact(additional)
            .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::Allocation)
    }
}

#[cfg(test)]
mod tests {
    use bangbang_runtime::balloon::VirtioBalloonQueueLayout;
    use bangbang_runtime::fdt::ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, GuestMemoryLayout};
    use bangbang_runtime::snapshot_balloon_v2_9::{
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, SnapshotV2BalloonState,
    };
    use bangbang_runtime::snapshot_device_v2::SnapshotV2MmioDeviceState;

    use super::*;

    const DIRECTORY_OFFSET: usize = 64;
    const DIRECTORY_ENTRY_BYTES: usize = 32;
    const DIRECTORY_PAYLOAD_OFFSET: usize = 8;
    const RESTORE_LOW_MEMORY_SIZE: u64 = 0x10_0000;
    const RESTORE_QUEUE_MEMORY_START: u64 = 0x8000_0000;
    const RESTORE_QUEUE_STRIDE: u64 = 0x1_0000;
    const AVAILABLE_INDEX_OFFSET: u64 = 2;
    const AVAILABLE_RING_OFFSET: u64 = 4;
    const USED_INDEX_OFFSET: u64 = 2;
    const ACTIVE_BALLOON_PCI_HEX: &str =
        include_str!("../../runtime/src/snapshot_balloon_v2_9/fixtures/active-pci.hex");
    const INACTIVE_BALLOON_MMIO_HEX: &str =
        include_str!("../../runtime/src/snapshot_balloon_v2_9/fixtures/inactive-mmio.hex");
    const BALLOON_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_8000);
    const BALLOON_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(4000);
    const ENTROPY_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_7000);
    const ENTROPY_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(3000);

    fn process_config() -> HvfSnapshotV2BalloonMmioProcessConfig {
        HvfSnapshotV2BalloonMmioProcessConfig::new(
            BalloonMmioLayout::new(BALLOON_MMIO_BASE, BALLOON_MMIO_REGION_ID),
            crate::snapshot_v2_storage_platform::tests::process_config(),
            EntropyMmioLayout::new(ENTROPY_MMIO_BASE, ENTROPY_MMIO_REGION_ID),
        )
    }

    fn fixture_bytes(hex: &str) -> Vec<u8> {
        let compact: Vec<u8> = hex
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect();
        compact
            .chunks_exact(2)
            .map(|pair| {
                u8::from_str_radix(
                    std::str::from_utf8(pair).expect("fixture hex should be UTF-8"),
                    16,
                )
                .expect("fixture hex should decode")
            })
            .collect()
    }

    fn inactive_memory() -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), 0x20_0000)
                .expect("test memory range should validate"),
        ])
        .expect("test memory layout should validate");
        GuestMemory::allocate(&layout).expect("test memory should allocate")
    }

    fn balloon_mmio_plan(
        queue_count: usize,
        interrupt: u32,
        region: MmioRegion,
    ) -> SnapshotV2BalloonRestorePlan {
        assert_eq!(queue_count, 2);
        let state = SnapshotV2BalloonState::decode(
            NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
            &fixture_bytes(INACTIVE_BALLOON_MMIO_HEX),
        )
        .expect("inactive MMIO balloon fixture should decode");
        let SnapshotV2DeviceTransport::Mmio(captured) = state.transport() else {
            panic!("inactive MMIO balloon fixture should select MMIO");
        };
        let state = SnapshotV2BalloonState::try_new(
            state.config(),
            state.config_space(),
            *state.continuation(),
            state.accounting().clone(),
            state.virtio().clone(),
            SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
                captured.device_feature_select(),
                captured.driver_feature_select(),
                captured.queue_select(),
                region,
                GuestInterruptLine::new(interrupt).expect("interrupt should validate"),
            )),
        )
        .expect("MMIO balloon state should validate");
        SnapshotV2BalloonRestorePlan::prepare(state, &inactive_memory())
            .expect("MMIO balloon plan should prepare")
    }

    fn balloon_pci_plan(
        queue_count: usize,
        platform: &HvfSnapshotV2PlatformState,
        slot: usize,
        route_offset: u32,
        active_route: bool,
    ) -> SnapshotV2BalloonRestorePlan {
        assert_eq!(queue_count, 5);
        let address_plan =
            Arm64PciAddressPlan::firecracker_v1_16().expect("address plan should validate");
        let placement = snapshot_v2_pci_endpoint_placement(address_plan, slot)
            .expect("balloon placement should validate");
        let msi = platform
            .global()
            .compatibility()
            .gic_metadata()
            .msi
            .expect("PCI platform should retain MSI metadata");
        let mut bytes = fixture_bytes(ACTIVE_BALLOON_PCI_HEX);
        let transport_offset = section_payload_offset(&bytes, 3);
        bytes[transport_offset + 11] = placement.sbdf.device();
        bytes[transport_offset + 16..transport_offset + 24]
            .copy_from_slice(&placement.bar_range.start().raw_value().to_le_bytes());
        let message_address = msi
            .region
            .base
            .checked_add(ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET)
            .expect("MSI address should fit");
        for index in 0..=queue_count {
            let entry = transport_offset + 96 + index * 16;
            bytes[entry..entry + 4].copy_from_slice(
                &u32::try_from(message_address & u64::from(u32::MAX))
                    .expect("low message address should fit")
                    .to_le_bytes(),
            );
            bytes[entry + 4..entry + 8].copy_from_slice(
                &u32::try_from(message_address >> 32)
                    .expect("high message address should fit")
                    .to_le_bytes(),
            );
            let data = msi
                .interrupt_range
                .base
                .checked_add(route_offset)
                .and_then(|base| base.checked_add(u32::try_from(index).expect("index should fit")))
                .expect("message data should fit");
            bytes[entry + 8..entry + 12].copy_from_slice(&data.to_le_bytes());
            if !active_route {
                bytes[entry + 12..entry + 16].copy_from_slice(&1_u32.to_le_bytes());
            }
        }
        let state =
            SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &bytes)
                .expect("relocated PCI balloon fixture should decode");
        let mut memory = balloon_restore_memory(&state);
        initialize_balloon_restore_memory(&mut memory, &state);
        SnapshotV2BalloonRestorePlan::prepare(state, &memory)
            .expect("PCI balloon plan should prepare")
    }

    fn entropy_mmio_plan(interrupt: u32) -> SnapshotV2EntropyRestorePlan {
        let region = mmio_region(ENTROPY_MMIO_REGION_ID, ENTROPY_MMIO_BASE)
            .expect("entropy region should validate");
        crate::snapshot_v2_entropy_platform::tests::entropy_mmio_plan_at(
            region,
            GuestInterruptLine::new(interrupt).expect("interrupt should validate"),
        )
    }

    fn entropy_pci_plan(
        platform: &HvfSnapshotV2PlatformState,
        slot: usize,
        route_offset: u32,
        active_route: bool,
    ) -> SnapshotV2EntropyRestorePlan {
        let msi = platform
            .global()
            .compatibility()
            .gic_metadata()
            .msi
            .expect("PCI platform should retain MSI metadata");
        let first_message_data = msi
            .interrupt_range
            .base
            .checked_add(route_offset)
            .expect("entropy message data should fit");
        crate::snapshot_v2_entropy_platform::tests::entropy_plan_at_with_routes(
            slot,
            first_message_data,
            active_route,
        )
    }

    fn unreferenced_entropy_pci_plan(
        platform: &HvfSnapshotV2PlatformState,
        slot: usize,
        route_offset: u32,
    ) -> SnapshotV2EntropyRestorePlan {
        let msi = platform
            .global()
            .compatibility()
            .gic_metadata()
            .msi
            .expect("PCI platform should retain MSI metadata");
        let first_message_data = msi
            .interrupt_range
            .base
            .checked_add(route_offset)
            .expect("entropy message data should fit");
        crate::snapshot_v2_entropy_platform::tests::entropy_plan_at_with_unreferenced_routes(
            slot,
            first_message_data,
        )
    }

    fn section_payload_offset(bytes: &[u8], section_index: usize) -> usize {
        let entry = DIRECTORY_OFFSET + section_index * DIRECTORY_ENTRY_BYTES;
        usize::try_from(u64::from_le_bytes(
            bytes[entry + DIRECTORY_PAYLOAD_OFFSET..entry + DIRECTORY_PAYLOAD_OFFSET + 8]
                .try_into()
                .expect("section payload offset should exist"),
        ))
        .expect("section payload offset should fit")
    }

    fn balloon_restore_memory(state: &SnapshotV2BalloonState) -> GuestMemory {
        let mut ranges = vec![
            GuestMemoryRange::new(GuestAddress::new(0), RESTORE_LOW_MEMORY_SIZE)
                .expect("low restore memory should validate"),
        ];
        if state.virtio().is_activated() {
            ranges.push(
                GuestMemoryRange::new(
                    GuestAddress::new(RESTORE_QUEUE_MEMORY_START),
                    u64::try_from(state.virtio().queues().len()).expect("queue count should fit")
                        * RESTORE_QUEUE_STRIDE,
                )
                .expect("queue restore memory should validate"),
            );
        }
        let layout = GuestMemoryLayout::new(ranges).expect("restore layout should validate");
        GuestMemory::allocate(&layout).expect("restore memory should allocate")
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
                    .expect("available index should fit"),
                cursor.next_available(),
            );
            write_memory_u16(
                memory,
                queue
                    .device_ring()
                    .checked_add(USED_INDEX_OFFSET)
                    .expect("used index should fit"),
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
                    .expect("pending statistics entry should fit"),
                pending_head,
            );
        }
    }

    fn write_memory_u16(memory: &mut GuestMemory, address: GuestAddress, value: u16) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("restore fixture write should succeed");
    }

    fn platform_with_msi_count(
        platform: HvfSnapshotV2PlatformState,
        interrupt_count: u32,
    ) -> HvfSnapshotV2PlatformState {
        let (memory, machine, global, topology, vcpus, time) = platform.into_parts();
        let (compatibility, gic_device) = global.into_parts();
        let mut gic = compatibility.gic_metadata();
        let mut msi = gic.msi.expect("PCI platform should retain MSI metadata");
        msi.interrupt_range.count = interrupt_count;
        gic.msi = Some(msi);
        let compatibility = crate::snapshot_bundle::HvfSnapshotV1CompatibilityState::new(
            compatibility.identification(),
            compatibility.optional_sve_sme_identification(),
            compatibility.cache_manifest(),
            compatibility.primary_mpidr(),
            gic,
            compatibility.rtc_mmio_layout(),
        );
        let global =
            crate::snapshot_v2::HvfSnapshotV2GlobalState::try_new(compatibility, gic_device)
                .expect("mutated PCI global state should validate");
        HvfSnapshotV2PlatformState::try_new(memory, machine, global, topology, vcpus, time)
            .expect("mutated PCI platform should validate")
    }

    fn pci_platform(queue_count: usize, entropy: bool) -> HvfSnapshotV2PlatformState {
        let platform = crate::snapshot_v2_multi_block_platform::tests::product_pci_platform();
        let expected = pci_balloon_restore_gic_msi_configuration(queue_count, entropy)
            .expect("balloon-aware MSI profile should validate")
            .interrupt_count()
            .get();
        platform_with_msi_count(platform, expected)
    }

    #[test]
    fn all_four_mmio_product_shapes_close_balloon_first_order() {
        let balloon_region = mmio_region(BALLOON_MMIO_REGION_ID, BALLOON_MMIO_BASE)
            .expect("balloon region should validate");

        let platform = crate::snapshot_v2_multi_block_platform::tests::product_mmio_platform(1);
        let plan = prepare_hvf_snapshot_v2_balloon_mmio_platform_plan(
            &platform,
            HvfSnapshotV2BalloonPreparedProduct::serial_balloon(balloon_mmio_plan(
                2,
                32,
                balloon_region,
            )),
            process_config(),
        )
        .expect("serial balloon MMIO product should plan");
        assert_eq!(plan.kind(), HvfSnapshotV2BalloonProductKind::SerialBalloon);
        assert_eq!(plan.balloon().interrupt_line().raw_value(), 32);
        assert_eq!(plan.serial_interrupt().raw_value(), 33);

        let platform = crate::snapshot_v2_multi_block_platform::tests::product_mmio_platform(2);
        let plan = prepare_hvf_snapshot_v2_balloon_mmio_platform_plan(
            &platform,
            HvfSnapshotV2BalloonPreparedProduct::serial_balloon_entropy(
                balloon_mmio_plan(2, 32, balloon_region),
                entropy_mmio_plan(33),
            ),
            process_config(),
        )
        .expect("serial balloon entropy MMIO product should plan");
        assert_eq!(
            plan.kind(),
            HvfSnapshotV2BalloonProductKind::SerialBalloonEntropy
        );
        assert_eq!(
            plan.entropy()
                .expect("entropy endpoint should exist")
                .interrupt_line()
                .raw_value(),
            33
        );
        assert_eq!(plan.serial_interrupt().raw_value(), 34);

        let fixture =
            crate::snapshot_v2_storage_platform::tests::balloon_prefixed_rootless_block_mmio_fixture(
                false,
            );
        let plan = prepare_hvf_snapshot_v2_balloon_mmio_platform_plan(
            &fixture.platform,
            HvfSnapshotV2BalloonPreparedProduct::serial_balloon_storage(
                balloon_mmio_plan(2, 32, balloon_region),
                fixture.bundle,
            ),
            process_config(),
        )
        .expect("serial balloon storage MMIO product should plan");
        assert_eq!(
            plan.kind(),
            HvfSnapshotV2BalloonProductKind::SerialBalloonStorage
        );
        assert_eq!(
            plan.storage()
                .expect("storage plan should exist")
                .block_records()[0]
                .interrupt_line()
                .raw_value(),
            33
        );
        assert_eq!(plan.serial_interrupt().raw_value(), 35);

        let fixture =
            crate::snapshot_v2_storage_platform::tests::balloon_prefixed_rootless_block_mmio_fixture(
                true,
            );
        let plan = prepare_hvf_snapshot_v2_balloon_mmio_platform_plan(
            &fixture.platform,
            HvfSnapshotV2BalloonPreparedProduct::serial_balloon_storage_entropy(
                balloon_mmio_plan(2, 32, balloon_region),
                fixture.bundle,
                entropy_mmio_plan(35),
            ),
            process_config(),
        )
        .expect("complete balloon MMIO product should plan");
        assert_eq!(
            plan.kind(),
            HvfSnapshotV2BalloonProductKind::SerialBalloonStorageEntropy
        );
        assert_eq!(
            plan.entropy()
                .expect("entropy endpoint should exist")
                .interrupt_line()
                .raw_value(),
            35
        );
        assert_eq!(plan.serial_interrupt().raw_value(), 36);
    }

    #[test]
    fn pci_balloon_route_demand_tracks_two_through_five_queues() {
        for (queue_count, expected_without_entropy, expected_with_entropy) in
            [(2, 93, 92), (3, 94, 93), (4, 95, 94), (5, 96, 95)]
        {
            assert_eq!(
                snapshot_v2_pci_endpoint_route_count(queue_count),
                Some(queue_count + 1)
            );
            assert_eq!(
                pci_balloon_restore_gic_msi_configuration(queue_count, false)
                    .expect("MSI profile should validate")
                    .interrupt_count()
                    .get(),
                expected_without_entropy
            );
            assert_eq!(
                pci_balloon_restore_gic_msi_configuration(queue_count, true)
                    .expect("MSI profile should validate")
                    .interrupt_count()
                    .get(),
                expected_with_entropy
            );
        }

        let platform = pci_platform(5, false);
        let plan = prepare_hvf_snapshot_v2_balloon_pci_platform_plan(
            &platform,
            HvfSnapshotV2BalloonPreparedProduct::serial_balloon(balloon_pci_plan(
                5, &platform, 0, 0, true,
            )),
        )
        .expect("five-queue balloon PCI product should plan");
        assert_eq!(plan.balloon().route_count(), 6);
        assert_eq!(plan.route_demand(), 6);
        assert_eq!(
            plan.balloon().msi_interrupt_count(),
            pci_balloon_restore_gic_msi_configuration(5, false)
                .expect("MSI profile should validate")
                .interrupt_count()
                .get()
        );
    }

    #[test]
    fn all_four_pci_product_shapes_close_balloon_slot_zero_order() {
        let platform = pci_platform(5, false);
        let plan = prepare_hvf_snapshot_v2_balloon_pci_platform_plan(
            &platform,
            HvfSnapshotV2BalloonPreparedProduct::serial_balloon(balloon_pci_plan(
                5, &platform, 0, 0, true,
            )),
        )
        .expect("serial balloon PCI product should plan");
        assert_eq!(plan.balloon().sbdf().device(), 1);

        let platform = pci_platform(5, true);
        let plan = prepare_hvf_snapshot_v2_balloon_pci_platform_plan(
            &platform,
            HvfSnapshotV2BalloonPreparedProduct::serial_balloon_entropy(
                balloon_pci_plan(5, &platform, 0, 0, true),
                entropy_pci_plan(&platform, 1, 16, true),
            ),
        )
        .expect("serial balloon entropy PCI product should plan");
        assert_eq!(
            plan.entropy()
                .expect("entropy endpoint should exist")
                .preceding_endpoint_count(),
            1
        );

        let fixture =
            crate::snapshot_v2_storage_platform::tests::balloon_prefixed_rootless_block_pci_fixture(
            );
        let platform = platform_with_msi_count(
            fixture.platform,
            pci_balloon_restore_gic_msi_configuration(5, false)
                .expect("MSI profile should validate")
                .interrupt_count()
                .get(),
        );
        let plan = prepare_hvf_snapshot_v2_balloon_pci_platform_plan(
            &platform,
            HvfSnapshotV2BalloonPreparedProduct::serial_balloon_storage(
                balloon_pci_plan(5, &platform, 0, 0, true),
                fixture.bundle,
            ),
        )
        .expect("serial balloon storage PCI product should plan");
        assert_eq!(
            plan.storage()
                .expect("storage plan should exist")
                .pci()
                .block_records()[0]
                .sbdf()
                .device(),
            2
        );

        let fixture =
            crate::snapshot_v2_storage_platform::tests::balloon_prefixed_rootless_block_pci_fixture(
            );
        let platform = platform_with_msi_count(
            fixture.platform,
            pci_balloon_restore_gic_msi_configuration(5, true)
                .expect("MSI profile should validate")
                .interrupt_count()
                .get(),
        );
        let plan = prepare_hvf_snapshot_v2_balloon_pci_platform_plan(
            &platform,
            HvfSnapshotV2BalloonPreparedProduct::serial_balloon_storage_entropy(
                balloon_pci_plan(5, &platform, 0, 0, true),
                fixture.bundle,
                entropy_pci_plan(&platform, 2, 16, true),
            ),
        )
        .expect("complete balloon PCI product should plan");
        assert_eq!(
            plan.entropy()
                .expect("entropy endpoint should exist")
                .preceding_endpoint_count(),
            2
        );
        assert_eq!(plan.route_demand(), 10);
    }

    #[test]
    fn wrong_mmio_and_pci_order_are_rejected() {
        let balloon_region = mmio_region(BALLOON_MMIO_REGION_ID, BALLOON_MMIO_BASE)
            .expect("balloon region should validate");
        let platform = crate::snapshot_v2_multi_block_platform::tests::product_mmio_platform(1);
        assert!(matches!(
            prepare_hvf_snapshot_v2_balloon_mmio_platform_plan(
                &platform,
                HvfSnapshotV2BalloonPreparedProduct::serial_balloon(balloon_mmio_plan(
                    2,
                    33,
                    balloon_region,
                )),
                process_config(),
            ),
            Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)
        ));

        let platform = pci_platform(5, false);
        assert!(matches!(
            prepare_hvf_snapshot_v2_balloon_pci_platform_plan(
                &platform,
                HvfSnapshotV2BalloonPreparedProduct::serial_balloon(balloon_pci_plan(
                    5, &platform, 1, 0, true,
                )),
            ),
            Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan)
        ));
    }

    #[test]
    fn genuine_active_route_collisions_are_rejected() {
        let platform = pci_platform(5, true);
        assert!(matches!(
            prepare_hvf_snapshot_v2_balloon_pci_platform_plan(
                &platform,
                HvfSnapshotV2BalloonPreparedProduct::serial_balloon_entropy(
                    balloon_pci_plan(5, &platform, 0, 0, true),
                    entropy_pci_plan(&platform, 1, 1, true),
                ),
            ),
            Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::RouteConflict)
        ));
    }

    #[test]
    fn masked_and_unreferenced_routes_do_not_create_collisions() {
        let platform = pci_platform(5, true);
        let plan = prepare_hvf_snapshot_v2_balloon_pci_platform_plan(
            &platform,
            HvfSnapshotV2BalloonPreparedProduct::serial_balloon_entropy(
                balloon_pci_plan(5, &platform, 0, 0, true),
                entropy_pci_plan(&platform, 1, 1, false),
            ),
        )
        .expect("masked duplicate messages should not create an active collision");
        assert_eq!(
            plan.kind(),
            HvfSnapshotV2BalloonProductKind::SerialBalloonEntropy
        );

        let plan = prepare_hvf_snapshot_v2_balloon_pci_platform_plan(
            &platform,
            HvfSnapshotV2BalloonPreparedProduct::serial_balloon_entropy(
                balloon_pci_plan(5, &platform, 0, 0, true),
                unreferenced_entropy_pci_plan(&platform, 1, 1),
            ),
        )
        .expect("unreferenced duplicate messages should not create an active collision");
        assert_eq!(
            plan.kind(),
            HvfSnapshotV2BalloonProductKind::SerialBalloonEntropy
        );
    }

    #[test]
    fn aggregate_ranges_and_every_inventory_reservation_are_checked() {
        let first = MmioRegion::new(MmioRegionId::new(1), GuestAddress::new(0x1000), 0x1000)
            .expect("first region should validate");
        let duplicate_id = MmioRegion::new(MmioRegionId::new(1), GuestAddress::new(0x4000), 0x1000)
            .expect("duplicate-ID region should validate");
        assert!(!aggregate_ranges_are_disjoint(
            &[first, duplicate_id],
            &[],
            &[]
        ));
        let queue = GuestMemoryRange::new(GuestAddress::new(0x1800), 0x100)
            .expect("queue range should validate");
        assert!(!aggregate_ranges_are_disjoint(&[first], &[queue], &[]));

        struct FailingReserve {
            calls: usize,
            fail_at: usize,
        }
        impl BalloonPlatformPlanReserve for FailingReserve {
            fn reserve<T>(
                &mut self,
                values: &mut Vec<T>,
                additional: usize,
            ) -> Result<(), PrepareHvfSnapshotV2BalloonPlatformPlanError> {
                let call = self.calls;
                self.calls = self.calls.saturating_add(1);
                if call == self.fail_at {
                    Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::Allocation)
                } else {
                    values
                        .try_reserve_exact(additional)
                        .map_err(|_| PrepareHvfSnapshotV2BalloonPlatformPlanError::Allocation)
                }
            }
        }

        let balloon_region = mmio_region(BALLOON_MMIO_REGION_ID, BALLOON_MMIO_BASE)
            .expect("balloon region should validate");
        for fail_at in 0..3 {
            let platform = crate::snapshot_v2_multi_block_platform::tests::product_mmio_platform(1);
            assert!(matches!(
                prepare_balloon_mmio_platform_plan(
                    &platform,
                    HvfSnapshotV2BalloonPreparedProduct::serial_balloon(balloon_mmio_plan(
                        2,
                        32,
                        balloon_region,
                    )),
                    process_config(),
                    &mut FailingReserve { calls: 0, fail_at },
                ),
                Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::Allocation)
            ));
        }
        for fail_at in 0..4 {
            let platform = pci_platform(5, false);
            assert!(matches!(
                prepare_balloon_pci_platform_plan(
                    &platform,
                    HvfSnapshotV2BalloonPreparedProduct::serial_balloon(balloon_pci_plan(
                        5, &platform, 0, 0, true,
                    )),
                    &mut FailingReserve { calls: 0, fail_at },
                ),
                Err(PrepareHvfSnapshotV2BalloonPlatformPlanError::Allocation)
            ));
        }
    }

    #[test]
    fn product_and_errors_are_value_redacted() {
        let platform = crate::snapshot_v2_multi_block_platform::tests::product_mmio_platform(1);
        let balloon_region = mmio_region(BALLOON_MMIO_REGION_ID, BALLOON_MMIO_BASE)
            .expect("balloon region should validate");
        let product = HvfSnapshotV2BalloonPreparedProduct::serial_balloon(balloon_mmio_plan(
            2,
            32,
            balloon_region,
        ));
        let product_debug = format!("{product:?}");
        assert!(product_debug.contains(REDACTED));
        assert!(!product_debug.contains("40008000"));
        let plan = prepare_hvf_snapshot_v2_balloon_mmio_platform_plan(
            &platform,
            product,
            process_config(),
        )
        .expect("redaction fixture should plan");
        assert!(format!("{plan:?}").contains(REDACTED));
        for error in [
            PrepareHvfSnapshotV2BalloonPlatformPlanError::PlatformProfile,
            PrepareHvfSnapshotV2BalloonPlatformPlanError::TransportPolicy,
            PrepareHvfSnapshotV2BalloonPlatformPlanError::ResourcePlan,
            PrepareHvfSnapshotV2BalloonPlatformPlanError::PciCapacity {
                count: 32,
                maximum: 31,
            },
            PrepareHvfSnapshotV2BalloonPlatformPlanError::RangeConflict,
            PrepareHvfSnapshotV2BalloonPlatformPlanError::RouteConflict,
            PrepareHvfSnapshotV2BalloonPlatformPlanError::Allocation,
        ] {
            assert!(format!("{error:?}").contains(REDACTED));
            assert!(!format!("{error:?}").contains("40008000"));
        }
    }
}
