//! Host-free exact native-v2 2.6 storage platform planning.

use std::fmt;
use std::time::Instant;

use bangbang_runtime::block::{
    BlockMmioLayout, BlockMmioRegistrationError, VIRTIO_BLOCK_QUEUE_SIZES,
};
use bangbang_runtime::boot::{
    BootCommandLineError, canonical_process_block_command_line,
    canonical_process_root_block_command_line, canonical_process_root_pmem_command_line,
};
use bangbang_runtime::fdt::{Arm64FdtPciHost, Arm64FdtRegion, Arm64FdtVirtioMmioDevice};
use bangbang_runtime::interrupt::GuestInterruptLine;
use bangbang_runtime::memory::{GuestAddress, GuestMemoryRange};
use bangbang_runtime::mmio::{MmioRegion, MmioRegionId};
use bangbang_runtime::pci::{Arm64PciAddressPlan, PCI_FIRST_ENDPOINT_DEVICE, PciSbdf};
use bangbang_runtime::pmem::{PmemMmioLayout, PmemMmioRegistrationError, VIRTIO_PMEM_QUEUE_SIZES};
use bangbang_runtime::pvtime::ARM64_PVTIME_STRUCTURE_SIZE;
use bangbang_runtime::rtc::{RTC_MMIO_DEVICE_WINDOW_SIZE, RtcMmioLayout};
use bangbang_runtime::serial::SERIAL_MMIO_DEVICE_WINDOW_SIZE;
use bangbang_runtime::snapshot_device_v2::{
    SnapshotV2DeviceKey, SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind,
};
use bangbang_runtime::snapshot_device_v2_6::{
    NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS, PreparedSnapshotV2StorageBundle,
};
use bangbang_runtime::storage_capture::{StorageDeviceOrigin, StorageRetryState};

use crate::gic::{
    HvfGicInterruptLineAllocator, HvfGicMetadata, HvfGicMsiMetadata,
    HvfInterruptLineAllocationError,
};
use crate::snapshot_v2::HvfSnapshotV2PlatformState;
use crate::snapshot_v2_multi_block_platform::{
    snapshot_v2_pci_endpoint_placement, snapshot_v2_pci_endpoint_route_count,
    valid_snapshot_v2_pci_record,
};
use crate::snapshot_v2_platform::{
    PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID, PROCESS_SERIAL_MMIO_BASE,
    PROCESS_SERIAL_MMIO_REGION_ID,
};
use crate::startup::{PCI_ENDPOINT_SLOT_COUNT, pci_root_restore_gic_msi_configuration};

const REDACTED: &str = "<redacted>";

/// Destination MMIO policy for an exact profile-3 storage product.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2StorageMmioProcessConfig {
    block_layout: BlockMmioLayout,
    pmem_layout: PmemMmioLayout,
}

impl HvfSnapshotV2StorageMmioProcessConfig {
    /// Creates one closed MMIO destination policy.
    pub const fn new(block_layout: BlockMmioLayout, pmem_layout: PmemMmioLayout) -> Self {
        Self {
            block_layout,
            pmem_layout,
        }
    }

    pub const fn block_layout(self) -> BlockMmioLayout {
        self.block_layout
    }

    pub const fn pmem_layout(self) -> PmemMmioLayout {
        self.pmem_layout
    }
}

impl fmt::Debug for HvfSnapshotV2StorageMmioProcessConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2StorageMmioProcessConfig")
            .field("state", &REDACTED)
            .finish()
    }
}

/// One canonical MMIO record and semantic FDT node.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2StorageMmioRecordPlan {
    key: SnapshotV2DeviceKey,
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    fdt_device: Arm64FdtVirtioMmioDevice,
}

impl HvfSnapshotV2StorageMmioRecordPlan {
    pub const fn key(self) -> SnapshotV2DeviceKey {
        self.key
    }

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

impl fmt::Debug for HvfSnapshotV2StorageMmioRecordPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2StorageMmioRecordPlan")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Host-time projection of one exact storage retry owner.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2StorageRetryPlan {
    key: SnapshotV2DeviceKey,
    retry: StorageRetryState,
    retry_deadline: Option<Instant>,
}

impl HvfSnapshotV2StorageRetryPlan {
    pub const fn key(self) -> SnapshotV2DeviceKey {
        self.key
    }

    pub const fn retry(self) -> StorageRetryState {
        self.retry
    }

    pub const fn retry_deadline(self) -> Option<Instant> {
        self.retry_deadline
    }
}

impl fmt::Debug for HvfSnapshotV2StorageRetryPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2StorageRetryPlan")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete pre-HVF exact-2.6 MMIO storage proof.
pub struct HvfSnapshotV2StorageMmioPlatformPlan {
    root_key: Option<SnapshotV2DeviceKey>,
    command_line: String,
    block_metrics_ids: Vec<String>,
    pmem_metrics_ids: Vec<String>,
    block_retries: Vec<HvfSnapshotV2StorageRetryPlan>,
    pmem_retries: Vec<HvfSnapshotV2StorageRetryPlan>,
    block_records: Vec<HvfSnapshotV2StorageMmioRecordPlan>,
    pmem_records: Vec<HvfSnapshotV2StorageMmioRecordPlan>,
    serial_interrupt: GuestInterruptLine,
    vmgenid_interrupt: GuestInterruptLine,
    vmclock_interrupt: GuestInterruptLine,
}

pub(crate) struct HvfSnapshotV2StorageMmioPlatformPlanParts {
    pub(crate) root_key: Option<SnapshotV2DeviceKey>,
    pub(crate) command_line: String,
    pub(crate) block_metrics_ids: Vec<String>,
    pub(crate) pmem_metrics_ids: Vec<String>,
    pub(crate) block_retries: Vec<HvfSnapshotV2StorageRetryPlan>,
    pub(crate) pmem_retries: Vec<HvfSnapshotV2StorageRetryPlan>,
    pub(crate) block_records: Vec<HvfSnapshotV2StorageMmioRecordPlan>,
    pub(crate) pmem_records: Vec<HvfSnapshotV2StorageMmioRecordPlan>,
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

impl HvfSnapshotV2StorageMmioPlatformPlan {
    pub const fn root_key(&self) -> Option<SnapshotV2DeviceKey> {
        self.root_key
    }

    pub fn command_line(&self) -> &str {
        &self.command_line
    }

    pub fn block_metrics_ids(&self) -> &[String] {
        &self.block_metrics_ids
    }

    pub fn pmem_metrics_ids(&self) -> &[String] {
        &self.pmem_metrics_ids
    }

    pub fn block_retries(&self) -> &[HvfSnapshotV2StorageRetryPlan] {
        &self.block_retries
    }

    pub fn pmem_retries(&self) -> &[HvfSnapshotV2StorageRetryPlan] {
        &self.pmem_retries
    }

    pub fn earliest_block_retry_deadline(&self) -> Option<Instant> {
        self.block_retries
            .iter()
            .filter_map(|retry| retry.retry_deadline)
            .min()
    }

    pub fn earliest_pmem_retry_deadline(&self) -> Option<Instant> {
        self.pmem_retries
            .iter()
            .filter_map(|retry| retry.retry_deadline)
            .min()
    }

    pub fn block_records(&self) -> &[HvfSnapshotV2StorageMmioRecordPlan] {
        &self.block_records
    }

    pub fn pmem_records(&self) -> &[HvfSnapshotV2StorageMmioRecordPlan] {
        &self.pmem_records
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

    pub(crate) fn into_parts(self) -> HvfSnapshotV2StorageMmioPlatformPlanParts {
        HvfSnapshotV2StorageMmioPlatformPlanParts {
            root_key: self.root_key,
            command_line: self.command_line,
            block_metrics_ids: self.block_metrics_ids,
            pmem_metrics_ids: self.pmem_metrics_ids,
            block_retries: self.block_retries,
            pmem_retries: self.pmem_retries,
            block_records: self.block_records,
            pmem_records: self.pmem_records,
            serial_interrupt: self.serial_interrupt,
            vmgenid_interrupt: self.vmgenid_interrupt,
            vmclock_interrupt: self.vmclock_interrupt,
        }
    }
}

impl fmt::Debug for HvfSnapshotV2StorageMmioPlatformPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2StorageMmioPlatformPlan")
            .field("block_count", &self.block_records.len())
            .field("pmem_count", &self.pmem_records.len())
            .field("has_root", &self.root_key.is_some())
            .field("state", &REDACTED)
            .finish()
    }
}

/// One canonical exact-2.6 PCI endpoint in combined block-then-pmem order.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2StoragePciRecordPlan {
    key: SnapshotV2DeviceKey,
    origin: StorageDeviceOrigin,
    sbdf: PciSbdf,
    bar_region_id: MmioRegionId,
    bar_range: GuestMemoryRange,
    route_count: usize,
}

impl HvfSnapshotV2StoragePciRecordPlan {
    pub const fn key(self) -> SnapshotV2DeviceKey {
        self.key
    }

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
}

impl fmt::Debug for HvfSnapshotV2StoragePciRecordPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2StoragePciRecordPlan")
            .field("origin", &self.origin)
            .field("state", &REDACTED)
            .finish()
    }
}

/// One canonical PCI host shared by every exact-2.6 storage endpoint.
pub struct HvfSnapshotV2StoragePciHostPlan {
    host: Arm64FdtPciHost,
    msi: HvfGicMsiMetadata,
    route_demand: usize,
    block_records: Vec<HvfSnapshotV2StoragePciRecordPlan>,
    pmem_records: Vec<HvfSnapshotV2StoragePciRecordPlan>,
}

impl HvfSnapshotV2StoragePciHostPlan {
    pub const fn host(&self) -> Arm64FdtPciHost {
        self.host
    }

    pub const fn msi(&self) -> HvfGicMsiMetadata {
        self.msi
    }

    pub const fn route_demand(&self) -> usize {
        self.route_demand
    }

    pub fn block_records(&self) -> &[HvfSnapshotV2StoragePciRecordPlan] {
        &self.block_records
    }

    pub fn pmem_records(&self) -> &[HvfSnapshotV2StoragePciRecordPlan] {
        &self.pmem_records
    }

    pub fn record_count(&self) -> usize {
        self.block_records.len() + self.pmem_records.len()
    }
}

impl fmt::Debug for HvfSnapshotV2StoragePciHostPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2StoragePciHostPlan")
            .field("block_count", &self.block_records.len())
            .field("pmem_count", &self.pmem_records.len())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete pre-HVF exact-2.6 heterogeneous PCI storage proof.
pub struct HvfSnapshotV2StoragePciPlatformPlan {
    root_key: Option<SnapshotV2DeviceKey>,
    command_line: String,
    block_metrics_ids: Vec<String>,
    pmem_metrics_ids: Vec<String>,
    block_retries: Vec<HvfSnapshotV2StorageRetryPlan>,
    pmem_retries: Vec<HvfSnapshotV2StorageRetryPlan>,
    pci: HvfSnapshotV2StoragePciHostPlan,
    serial_interrupt: GuestInterruptLine,
    vmgenid_interrupt: GuestInterruptLine,
    vmclock_interrupt: GuestInterruptLine,
}

pub(crate) struct HvfSnapshotV2StoragePciPlatformPlanParts {
    pub(crate) root_key: Option<SnapshotV2DeviceKey>,
    pub(crate) command_line: String,
    pub(crate) block_metrics_ids: Vec<String>,
    pub(crate) pmem_metrics_ids: Vec<String>,
    pub(crate) block_retries: Vec<HvfSnapshotV2StorageRetryPlan>,
    pub(crate) pmem_retries: Vec<HvfSnapshotV2StorageRetryPlan>,
    pub(crate) pci: HvfSnapshotV2StoragePciHostPlan,
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

impl HvfSnapshotV2StoragePciPlatformPlan {
    pub const fn root_key(&self) -> Option<SnapshotV2DeviceKey> {
        self.root_key
    }

    pub fn command_line(&self) -> &str {
        &self.command_line
    }

    pub fn block_metrics_ids(&self) -> &[String] {
        &self.block_metrics_ids
    }

    pub fn pmem_metrics_ids(&self) -> &[String] {
        &self.pmem_metrics_ids
    }

    pub fn block_retries(&self) -> &[HvfSnapshotV2StorageRetryPlan] {
        &self.block_retries
    }

    pub fn pmem_retries(&self) -> &[HvfSnapshotV2StorageRetryPlan] {
        &self.pmem_retries
    }

    pub fn earliest_block_retry_deadline(&self) -> Option<Instant> {
        self.block_retries
            .iter()
            .filter_map(|retry| retry.retry_deadline)
            .min()
    }

    pub fn earliest_pmem_retry_deadline(&self) -> Option<Instant> {
        self.pmem_retries
            .iter()
            .filter_map(|retry| retry.retry_deadline)
            .min()
    }

    pub const fn pci(&self) -> &HvfSnapshotV2StoragePciHostPlan {
        &self.pci
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

    pub(crate) fn into_parts(self) -> HvfSnapshotV2StoragePciPlatformPlanParts {
        HvfSnapshotV2StoragePciPlatformPlanParts {
            root_key: self.root_key,
            command_line: self.command_line,
            block_metrics_ids: self.block_metrics_ids,
            pmem_metrics_ids: self.pmem_metrics_ids,
            block_retries: self.block_retries,
            pmem_retries: self.pmem_retries,
            pci: self.pci,
            serial_interrupt: self.serial_interrupt,
            vmgenid_interrupt: self.vmgenid_interrupt,
            vmclock_interrupt: self.vmclock_interrupt,
        }
    }
}

impl fmt::Debug for HvfSnapshotV2StoragePciPlatformPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2StoragePciPlatformPlan")
            .field("block_count", &self.pci.block_records.len())
            .field("pmem_count", &self.pci.pmem_records.len())
            .field("has_root", &self.root_key.is_some())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Redacted rejection from exact-2.6 host-free MMIO platform planning.
pub enum PrepareHvfSnapshotV2StorageMmioPlatformPlanError {
    PlatformProfile,
    Cardinality,
    Allocation,
    RecordIdentity,
    RootIdentity,
    TransportPolicy,
    CommandLine(BootCommandLineError),
    QueuePlatformConflict,
    PmemRangeConflict,
    BlockMmioLayout(Box<BlockMmioRegistrationError>),
    PmemMmioLayout(Box<PmemMmioRegistrationError>),
    Interrupt(HvfInterruptLineAllocationError),
    ResourcePlan,
}

impl fmt::Debug for PrepareHvfSnapshotV2StorageMmioPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self {
            Self::PlatformProfile => "platform-profile",
            Self::Cardinality => "cardinality",
            Self::Allocation => "allocation",
            Self::RecordIdentity => "record-identity",
            Self::RootIdentity => "root-identity",
            Self::TransportPolicy => "transport-policy",
            Self::CommandLine(_) => "command-line",
            Self::QueuePlatformConflict => "queue-platform-conflict",
            Self::PmemRangeConflict => "pmem-range-conflict",
            Self::BlockMmioLayout(_) => "block-mmio-layout",
            Self::PmemMmioLayout(_) => "pmem-mmio-layout",
            Self::Interrupt(_) => "interrupt",
            Self::ResourcePlan => "resource-plan",
        };
        formatter
            .debug_struct("PrepareHvfSnapshotV2StorageMmioPlatformPlanError")
            .field("category", &category)
            .field("state", &REDACTED)
            .finish()
    }
}

impl fmt::Display for PrepareHvfSnapshotV2StorageMmioPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PlatformProfile => "native-v2 storage platform profile is not canonical",
            Self::Cardinality => "native-v2 storage platform cardinality is inconsistent",
            Self::Allocation => "native-v2 storage platform allocation failed",
            Self::RecordIdentity => "native-v2 storage record identity is inconsistent",
            Self::RootIdentity => "native-v2 storage root identity is inconsistent",
            Self::TransportPolicy => "native-v2 storage MMIO transport policy is inconsistent",
            Self::CommandLine(_) => "native-v2 storage process command line is invalid",
            Self::QueuePlatformConflict => "native-v2 storage queue overlaps platform-owned memory",
            Self::PmemRangeConflict => {
                "native-v2 storage pmem mapping overlaps platform-owned resources"
            }
            Self::BlockMmioLayout(_) => "native-v2 storage block MMIO layout is invalid",
            Self::PmemMmioLayout(_) => "native-v2 storage pmem MMIO layout is invalid",
            Self::Interrupt(_) => "native-v2 storage interrupt demand is invalid",
            Self::ResourcePlan => "native-v2 storage platform resources are inconsistent",
        })
    }
}

impl std::error::Error for PrepareHvfSnapshotV2StorageMmioPlatformPlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CommandLine(source) => Some(source),
            Self::BlockMmioLayout(source) => Some(source),
            Self::PmemMmioLayout(source) => Some(source),
            Self::Interrupt(source) => Some(source),
            Self::PlatformProfile
            | Self::Cardinality
            | Self::Allocation
            | Self::RecordIdentity
            | Self::RootIdentity
            | Self::TransportPolicy
            | Self::QueuePlatformConflict
            | Self::PmemRangeConflict
            | Self::ResourcePlan => None,
        }
    }
}

/// Redacted rejection from exact-2.6 host-free PCI platform planning.
pub enum PrepareHvfSnapshotV2StoragePciPlatformPlanError {
    PlatformProfile,
    Cardinality,
    Allocation,
    RecordIdentity,
    RootIdentity,
    TransportPolicy,
    CommandLine(BootCommandLineError),
    QueuePlatformConflict,
    PmemRangeConflict,
    Interrupt(HvfInterruptLineAllocationError),
    ResourcePlan,
    PciCapacity { count: usize, maximum: usize },
}

impl fmt::Debug for PrepareHvfSnapshotV2StoragePciPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self {
            Self::PlatformProfile => "platform-profile",
            Self::Cardinality => "cardinality",
            Self::Allocation => "allocation",
            Self::RecordIdentity => "record-identity",
            Self::RootIdentity => "root-identity",
            Self::TransportPolicy => "transport-policy",
            Self::CommandLine(_) => "command-line",
            Self::QueuePlatformConflict => "queue-platform-conflict",
            Self::PmemRangeConflict => "pmem-range-conflict",
            Self::Interrupt(_) => "interrupt",
            Self::ResourcePlan => "resource-plan",
            Self::PciCapacity { .. } => "pci-capacity",
        };
        formatter
            .debug_struct("PrepareHvfSnapshotV2StoragePciPlatformPlanError")
            .field("category", &category)
            .field("state", &REDACTED)
            .finish()
    }
}

impl fmt::Display for PrepareHvfSnapshotV2StoragePciPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PlatformProfile => "native-v2 storage PCI platform profile is not canonical",
            Self::Cardinality => "native-v2 storage PCI platform cardinality is inconsistent",
            Self::Allocation => "native-v2 storage PCI platform allocation failed",
            Self::RecordIdentity => "native-v2 storage PCI record identity is inconsistent",
            Self::RootIdentity => "native-v2 storage PCI root identity is inconsistent",
            Self::TransportPolicy => "native-v2 storage PCI transport policy is inconsistent",
            Self::CommandLine(_) => "native-v2 storage PCI process command line is invalid",
            Self::QueuePlatformConflict => {
                "native-v2 storage PCI queue overlaps platform-owned memory"
            }
            Self::PmemRangeConflict => {
                "native-v2 storage PCI pmem mapping overlaps platform-owned resources"
            }
            Self::Interrupt(_) => "native-v2 storage PCI interrupt demand is invalid",
            Self::ResourcePlan => "native-v2 storage PCI platform resources are inconsistent",
            Self::PciCapacity { .. } => "native-v2 storage PCI endpoint capacity is exceeded",
        })
    }
}

impl std::error::Error for PrepareHvfSnapshotV2StoragePciPlatformPlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CommandLine(source) => Some(source),
            Self::Interrupt(source) => Some(source),
            Self::PlatformProfile
            | Self::Cardinality
            | Self::Allocation
            | Self::RecordIdentity
            | Self::RootIdentity
            | Self::TransportPolicy
            | Self::QueuePlatformConflict
            | Self::PmemRangeConflict
            | Self::ResourcePlan
            | Self::PciCapacity { .. } => None,
        }
    }
}

enum StorageRoot<'a> {
    Block {
        partuuid: Option<&'a str>,
        read_only: bool,
    },
    Pmem {
        index: usize,
        read_only: bool,
    },
}

/// Proves a complete exact-2.6 block+pmem MMIO product before live HVF
/// construction.
pub fn prepare_hvf_snapshot_v2_storage_mmio_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    bundle: &PreparedSnapshotV2StorageBundle,
    process: HvfSnapshotV2StorageMmioProcessConfig,
) -> Result<HvfSnapshotV2StorageMmioPlatformPlan, PrepareHvfSnapshotV2StorageMmioPlatformPlanError>
{
    prepare_hvf_snapshot_v2_storage_mmio_platform_plan_with(
        platform,
        bundle,
        process,
        &mut SystemStoragePlatformPlanReserve,
    )
}

fn prepare_hvf_snapshot_v2_storage_mmio_platform_plan_with(
    platform: &HvfSnapshotV2PlatformState,
    bundle: &PreparedSnapshotV2StorageBundle,
    process: HvfSnapshotV2StorageMmioProcessConfig,
    reserve: &mut impl StoragePlatformPlanReserve,
) -> Result<HvfSnapshotV2StorageMmioPlatformPlan, PrepareHvfSnapshotV2StorageMmioPlatformPlanError>
{
    validate_platform_profile(platform)?;
    if bundle.transport_kind() != SnapshotV2DeviceTransportKind::Mmio {
        return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::TransportPolicy);
    }

    let block_records = bundle
        .block_bundle()
        .map_or(&[][..], |block| block.records());
    let block_configs = bundle
        .block_bundle()
        .map_or(&[][..], |block| block.drive_configs().as_slice());
    let block_retry_projection = bundle
        .block_bundle()
        .map_or(&[][..], |block| block.retry_projection());
    let pmem_records = bundle.pmem_records();
    let pmem_configs = bundle.pmem_configs().as_slice();
    let record_count = block_records
        .len()
        .checked_add(pmem_records.len())
        .ok_or(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::Cardinality)?;
    if record_count == 0
        || record_count > usize::from(NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS)
        || block_records.len() != block_configs.len()
        || block_records.len() != block_retry_projection.len()
        || pmem_records.len() != pmem_configs.len()
    {
        return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::Cardinality);
    }

    let mut block_metrics_ids = Vec::new();
    let mut pmem_metrics_ids = Vec::new();
    let mut block_retries = Vec::new();
    let mut pmem_retries = Vec::new();
    let mut planned_block = Vec::new();
    let mut planned_pmem = Vec::new();
    let mut planned_regions = Vec::new();
    reserve.reserve(&mut block_metrics_ids, block_records.len())?;
    reserve.reserve(&mut pmem_metrics_ids, pmem_records.len())?;
    reserve.reserve(&mut block_retries, block_records.len())?;
    reserve.reserve(&mut pmem_retries, pmem_records.len())?;
    reserve.reserve(&mut planned_block, block_records.len())?;
    reserve.reserve(&mut planned_pmem, pmem_records.len())?;
    reserve.reserve(&mut planned_regions, record_count)?;

    let mut root_key = None;
    let mut root = None;
    for (index, ((record, config), projected_retry)) in block_records
        .iter()
        .zip(block_configs)
        .zip(block_retry_projection)
        .enumerate()
    {
        let read_only = config
            .is_read_only()
            .ok_or(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::RecordIdentity)?;
        if record.transport().kind() != SnapshotV2DeviceTransportKind::Mmio
            || record.key() != projected_retry.key()
            || record.retry() != projected_retry.retry()
            || record.retry_deadline() != projected_retry.retry_deadline()
            || record.drive_id() != config.drive_id()
            || record.is_root_device() != config.is_root_device()
            || record.config_space().is_read_only() != read_only
            || record.device().device().io_engine() != config.io_engine()
            || record.device().cache_type() != config.cache_type()
            || config.is_vhost_user()
            || block_metrics_ids
                .iter()
                .any(|candidate| candidate == config.drive_id())
        {
            return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::RecordIdentity);
        }
        if queue_ranges_conflict_with_platform(platform, record.queue_ranges())? {
            return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::QueuePlatformConflict);
        }
        if config.is_root_device() {
            if root_key.replace(record.key()).is_some() || root.is_some() {
                return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::RootIdentity);
            }
            root = Some(StorageRoot::Block {
                partuuid: config.partuuid(),
                read_only,
            });
        }
        block_metrics_ids.push(try_clone(config.drive_id())?);
        block_retries.push(HvfSnapshotV2StorageRetryPlan {
            key: record.key(),
            retry: record.retry(),
            retry_deadline: record.retry_deadline(),
        });
        let planned = plan_mmio_record(
            platform,
            record.key(),
            record.transport(),
            process.block_layout.region_at(index).map_err(|source| {
                PrepareHvfSnapshotV2StorageMmioPlatformPlanError::BlockMmioLayout(Box::new(source))
            })?,
            &mut planned_regions,
            None,
        )?;
        planned_block.push(planned);
    }

    let gic = platform.global().compatibility().gic_metadata();
    if gic.msi.is_some() {
        return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::ResourcePlan);
    }
    let mut interrupt_allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
        .map_err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::Interrupt)?;
    for (record, planned) in block_records.iter().zip(&mut planned_block) {
        let interrupt = interrupt_allocator
            .allocate()
            .map_err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::Interrupt)?;
        validate_and_set_interrupt(record.transport(), interrupt, planned)?;
    }

    for (index, (record, config)) in pmem_records.iter().zip(pmem_configs).enumerate() {
        let prepared = record.prepared_device();
        let guest_range = prepared.guest_range();
        if record.transport().kind() != SnapshotV2DeviceTransportKind::Mmio
            || record.pmem_id() != config.id()
            || record.is_root_device() != config.root_device()
            || prepared.id() != config.id()
            || prepared.backing().is_read_only() != config.read_only()
            || prepared.mapping().is_read_only() != config.read_only()
            || prepared.rate_limiter() != config.rate_limiter()
            || prepared.config_space().available_features() != record.virtio().available_features()
            || prepared.mapping().mapped_len() != guest_range.size()
            || pmem_metrics_ids
                .iter()
                .any(|candidate| candidate == config.id())
        {
            return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::RecordIdentity);
        }
        if mapping_range_conflicts_with_platform(platform, guest_range, &gic)?
            || planned_regions
                .iter()
                .any(|region: &MmioRegion| region.range().overlaps(guest_range))
        {
            return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::PmemRangeConflict);
        }
        if queue_ranges_conflict_with_platform(platform, record.queue_ranges())? {
            return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::QueuePlatformConflict);
        }
        if config.root_device() {
            if root_key.replace(record.key()).is_some() || root.is_some() {
                return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::RootIdentity);
            }
            root = Some(StorageRoot::Pmem {
                index,
                read_only: config.read_only(),
            });
        }
        pmem_metrics_ids.push(try_clone(config.id())?);
        pmem_retries.push(HvfSnapshotV2StorageRetryPlan {
            key: record.key(),
            retry: record.retry(),
            retry_deadline: record.retry_deadline(),
        });
        let region = process.pmem_layout.region_at(index).map_err(|source| {
            PrepareHvfSnapshotV2StorageMmioPlatformPlanError::PmemMmioLayout(Box::new(source))
        })?;
        if region.range().overlaps(guest_range) {
            return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::PmemRangeConflict);
        }
        let interrupt = interrupt_allocator
            .allocate()
            .map_err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::Interrupt)?;
        let planned = plan_mmio_record(
            platform,
            record.key(),
            record.transport(),
            region,
            &mut planned_regions,
            Some(interrupt),
        )?;
        planned_pmem.push(planned);
    }

    if bundle.root_key() != root_key || bundle.root_key().is_some() != root.is_some() {
        return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::RootIdentity);
    }
    for range in bundle
        .pmem_records()
        .iter()
        .map(|record| record.prepared_device().guest_range())
    {
        if planned_regions
            .iter()
            .any(|region| region.range().overlaps(range))
        {
            return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::PmemRangeConflict);
        }
    }

    let base_arguments = platform.machine().boot().boot_arguments();
    let command_line = match root {
        Some(StorageRoot::Block {
            partuuid,
            read_only,
        }) => canonical_process_root_block_command_line(base_arguments, false, partuuid, read_only),
        Some(StorageRoot::Pmem { index, read_only }) => {
            canonical_process_root_pmem_command_line(base_arguments, false, index, read_only)
        }
        None => canonical_process_block_command_line(base_arguments, false),
    }
    .map_err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::CommandLine)?;

    let serial_interrupt = interrupt_allocator
        .allocate()
        .map_err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::Interrupt)?;
    let vmgenid_interrupt = interrupt_allocator
        .allocate()
        .map_err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::Interrupt)?;
    let vmclock_interrupt = interrupt_allocator
        .allocate()
        .map_err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::Interrupt)?;
    if platform.time().vmgenid().interrupt_line() != vmgenid_interrupt
        || platform.time().vmclock().interrupt_line() != vmclock_interrupt
    {
        return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::ResourcePlan);
    }

    Ok(HvfSnapshotV2StorageMmioPlatformPlan {
        root_key,
        command_line,
        block_metrics_ids,
        pmem_metrics_ids,
        block_retries,
        pmem_retries,
        block_records: planned_block,
        pmem_records: planned_pmem,
        serial_interrupt,
        vmgenid_interrupt,
        vmclock_interrupt,
    })
}

/// Proves one complete exact-2.6 block-then-pmem PCI product before any live
/// Hypervisor.framework construction.
pub fn prepare_hvf_snapshot_v2_storage_pci_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    bundle: &PreparedSnapshotV2StorageBundle,
) -> Result<HvfSnapshotV2StoragePciPlatformPlan, PrepareHvfSnapshotV2StoragePciPlatformPlanError> {
    prepare_hvf_snapshot_v2_storage_pci_platform_plan_with(
        platform,
        bundle,
        &mut SystemStoragePciPlatformPlanReserve,
    )
}

fn prepare_hvf_snapshot_v2_storage_pci_platform_plan_with(
    platform: &HvfSnapshotV2PlatformState,
    bundle: &PreparedSnapshotV2StorageBundle,
    reserve: &mut impl StoragePciPlatformPlanReserve,
) -> Result<HvfSnapshotV2StoragePciPlatformPlan, PrepareHvfSnapshotV2StoragePciPlatformPlanError> {
    if !platform.machine().fdt().is_product_process_profile()
        || platform.time().rtc_layout()
            != RtcMmioLayout::new(PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID)
    {
        return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::PlatformProfile);
    }
    if bundle.transport_kind() != SnapshotV2DeviceTransportKind::Pci {
        return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::TransportPolicy);
    }

    let block_records = bundle
        .block_bundle()
        .map_or(&[][..], |block| block.records());
    let block_configs = bundle
        .block_bundle()
        .map_or(&[][..], |block| block.drive_configs().as_slice());
    let block_retry_projection = bundle
        .block_bundle()
        .map_or(&[][..], |block| block.retry_projection());
    let pmem_records = bundle.pmem_records();
    let pmem_configs = bundle.pmem_configs().as_slice();
    let record_count = block_records
        .len()
        .checked_add(pmem_records.len())
        .ok_or(PrepareHvfSnapshotV2StoragePciPlatformPlanError::Cardinality)?;
    if record_count == 0
        || record_count > usize::from(NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS)
        || block_records.len() != block_configs.len()
        || block_records.len() != block_retry_projection.len()
        || pmem_records.len() != pmem_configs.len()
    {
        return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::Cardinality);
    }
    if record_count > PCI_ENDPOINT_SLOT_COUNT {
        return Err(
            PrepareHvfSnapshotV2StoragePciPlatformPlanError::PciCapacity {
                count: record_count,
                maximum: PCI_ENDPOINT_SLOT_COUNT,
            },
        );
    }

    let gic = platform.global().compatibility().gic_metadata();
    let msi = gic
        .msi
        .ok_or(PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;
    let expected_msi = pci_root_restore_gic_msi_configuration()
        .map_err(|_| PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;
    if msi.interrupt_range.count != expected_msi.interrupt_count().get() {
        return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan);
    }
    let address_plan = Arm64PciAddressPlan::firecracker_v1_16()
        .map_err(|_| PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;

    let block_route_count = snapshot_v2_pci_endpoint_route_count(VIRTIO_BLOCK_QUEUE_SIZES.len())
        .ok_or(PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;
    let pmem_route_count = snapshot_v2_pci_endpoint_route_count(VIRTIO_PMEM_QUEUE_SIZES.len())
        .ok_or(PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;
    let route_demand = block_records
        .len()
        .checked_mul(block_route_count)
        .and_then(|demand| {
            pmem_records
                .len()
                .checked_mul(pmem_route_count)
                .and_then(|pmem| demand.checked_add(pmem))
        })
        .ok_or(PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;
    if route_demand
        > usize::try_from(msi.interrupt_range.count)
            .map_err(|_| PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan);
    }

    let mut block_metrics_ids = Vec::new();
    let mut pmem_metrics_ids = Vec::new();
    let mut block_retries = Vec::new();
    let mut pmem_retries = Vec::new();
    let mut planned_block = Vec::new();
    let mut planned_pmem = Vec::new();
    let mut planned_pmem_ranges = Vec::new();
    let mut all_queue_ranges = Vec::new();
    let mut active_msi_routes = Vec::new();
    let mut used_pci_slots = Vec::new();
    reserve.reserve(&mut block_metrics_ids, block_records.len())?;
    reserve.reserve(&mut pmem_metrics_ids, pmem_records.len())?;
    reserve.reserve(&mut block_retries, block_records.len())?;
    reserve.reserve(&mut pmem_retries, pmem_records.len())?;
    reserve.reserve(&mut planned_block, block_records.len())?;
    reserve.reserve(&mut planned_pmem, pmem_records.len())?;
    reserve.reserve(&mut planned_pmem_ranges, pmem_records.len())?;
    reserve.reserve(&mut all_queue_ranges, record_count.saturating_mul(3))?;
    reserve.reserve(&mut active_msi_routes, route_demand)?;
    reserve.reserve(&mut used_pci_slots, record_count)?;

    for ranges in block_records
        .iter()
        .filter_map(|record| record.queue_ranges())
        .chain(
            pmem_records
                .iter()
                .filter_map(|record| record.queue_ranges()),
        )
    {
        all_queue_ranges.extend(ranges);
    }

    let mut root_key = None;
    let mut root = None;
    for ((record, config), projected_retry) in block_records
        .iter()
        .zip(block_configs)
        .zip(block_retry_projection)
    {
        let read_only = config
            .is_read_only()
            .ok_or(PrepareHvfSnapshotV2StoragePciPlatformPlanError::RecordIdentity)?;
        if record.transport().kind() != SnapshotV2DeviceTransportKind::Pci
            || record.key() != projected_retry.key()
            || record.retry() != projected_retry.retry()
            || record.retry_deadline() != projected_retry.retry_deadline()
            || record.drive_id() != config.drive_id()
            || record.is_root_device() != config.is_root_device()
            || record.config_space().is_read_only() != read_only
            || record.device().device().io_engine() != config.io_engine()
            || record.device().cache_type() != config.cache_type()
            || config.is_vhost_user()
            || block_metrics_ids
                .iter()
                .any(|candidate| candidate == config.drive_id())
        {
            return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::RecordIdentity);
        }
        if queue_ranges_conflict_with_pci_platform(
            platform,
            record.queue_ranges(),
            &gic,
            address_plan,
        )? {
            return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::QueuePlatformConflict);
        }
        if config.is_root_device() {
            if root_key.replace(record.key()).is_some() || root.is_some() {
                return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::RootIdentity);
            }
            root = Some(StorageRoot::Block {
                partuuid: config.partuuid(),
                read_only,
            });
        }
        block_metrics_ids.push(pci_try_clone(config.drive_id())?);
        block_retries.push(HvfSnapshotV2StorageRetryPlan {
            key: record.key(),
            retry: record.retry(),
            retry_deadline: record.retry_deadline(),
        });
        let planned = plan_pci_record(
            record.key(),
            record.transport(),
            block_route_count,
            VIRTIO_BLOCK_QUEUE_SIZES.len(),
            msi,
            address_plan,
            &mut active_msi_routes,
            &mut used_pci_slots,
        )?;
        planned_block.push(planned);
    }

    for (pmem_index, (record, config)) in pmem_records.iter().zip(pmem_configs).enumerate() {
        let prepared = record.prepared_device();
        let guest_range = prepared.guest_range();
        if record.transport().kind() != SnapshotV2DeviceTransportKind::Pci
            || record.pmem_id() != config.id()
            || record.is_root_device() != config.root_device()
            || prepared.id() != config.id()
            || prepared.backing().is_read_only() != config.read_only()
            || prepared.mapping().is_read_only() != config.read_only()
            || prepared.rate_limiter() != config.rate_limiter()
            || prepared.config_space().available_features() != record.virtio().available_features()
            || prepared.mapping().mapped_len() != guest_range.size()
            || pmem_metrics_ids
                .iter()
                .any(|candidate| candidate == config.id())
        {
            return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::RecordIdentity);
        }
        if mapping_range_conflicts_with_pci_platform(platform, guest_range, &gic, address_plan)?
            || planned_pmem_ranges
                .iter()
                .any(|planned: &GuestMemoryRange| planned.overlaps(guest_range))
            || all_queue_ranges
                .iter()
                .any(|queue| queue.overlaps(guest_range))
        {
            return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::PmemRangeConflict);
        }
        if queue_ranges_conflict_with_pci_platform(
            platform,
            record.queue_ranges(),
            &gic,
            address_plan,
        )? {
            return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::QueuePlatformConflict);
        }
        if config.root_device() {
            if root_key.replace(record.key()).is_some() || root.is_some() {
                return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::RootIdentity);
            }
            root = Some(StorageRoot::Pmem {
                index: pmem_index,
                read_only: config.read_only(),
            });
        }
        pmem_metrics_ids.push(pci_try_clone(config.id())?);
        pmem_retries.push(HvfSnapshotV2StorageRetryPlan {
            key: record.key(),
            retry: record.retry(),
            retry_deadline: record.retry_deadline(),
        });
        let planned = plan_pci_record(
            record.key(),
            record.transport(),
            pmem_route_count,
            VIRTIO_PMEM_QUEUE_SIZES.len(),
            msi,
            address_plan,
            &mut active_msi_routes,
            &mut used_pci_slots,
        )?;
        planned_pmem_ranges.push(guest_range);
        planned_pmem.push(planned);
    }

    if bundle.root_key() != root_key || bundle.root_key().is_some() != root.is_some() {
        return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::RootIdentity);
    }
    let base_arguments = platform.machine().boot().boot_arguments();
    let command_line = match root {
        Some(StorageRoot::Block {
            partuuid,
            read_only,
        }) => canonical_process_root_block_command_line(base_arguments, true, partuuid, read_only),
        Some(StorageRoot::Pmem { index, read_only }) => {
            canonical_process_root_pmem_command_line(base_arguments, true, index, read_only)
        }
        None => canonical_process_block_command_line(base_arguments, true),
    }
    .map_err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::CommandLine)?;

    let mut interrupt_allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
        .map_err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::Interrupt)?;
    let serial_interrupt = interrupt_allocator
        .allocate()
        .map_err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::Interrupt)?;
    let vmgenid_interrupt = interrupt_allocator
        .allocate()
        .map_err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::Interrupt)?;
    let vmclock_interrupt = interrupt_allocator
        .allocate()
        .map_err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::Interrupt)?;
    if platform.time().vmgenid().interrupt_line() != vmgenid_interrupt
        || platform.time().vmclock().interrupt_line() != vmclock_interrupt
        || planned_block.len() + planned_pmem.len() != record_count
    {
        return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan);
    }

    Ok(HvfSnapshotV2StoragePciPlatformPlan {
        root_key,
        command_line,
        block_metrics_ids,
        pmem_metrics_ids,
        block_retries,
        pmem_retries,
        pci: HvfSnapshotV2StoragePciHostPlan {
            host: Arm64FdtPciHost::from_address_plan(address_plan),
            msi,
            route_demand,
            block_records: planned_block,
            pmem_records: planned_pmem,
        },
        serial_interrupt,
        vmgenid_interrupt,
        vmclock_interrupt,
    })
}

#[allow(clippy::too_many_arguments)]
fn plan_pci_record(
    key: SnapshotV2DeviceKey,
    transport: &SnapshotV2DeviceTransport,
    route_count: usize,
    queue_count: usize,
    msi: HvfGicMsiMetadata,
    address_plan: Arm64PciAddressPlan,
    active_routes: &mut Vec<(u64, u32)>,
    used_slots: &mut Vec<usize>,
) -> Result<HvfSnapshotV2StoragePciRecordPlan, PrepareHvfSnapshotV2StoragePciPlatformPlanError> {
    let SnapshotV2DeviceTransport::Pci(captured) = transport else {
        return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::TransportPolicy);
    };
    let slot = captured
        .sbdf()
        .device()
        .checked_sub(PCI_FIRST_ENDPOINT_DEVICE)
        .map(usize::from)
        .ok_or(PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;
    let placement = snapshot_v2_pci_endpoint_placement(address_plan, slot)
        .ok_or(PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;
    if captured.sbdf() != placement.sbdf
        || captured.bar_range() != placement.bar_range
        || used_slots.contains(&slot)
        || !valid_snapshot_v2_pci_record(captured, msi, queue_count)
        || !register_active_pci_routes(captured, active_routes)
    {
        return Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan);
    }
    used_slots.push(slot);
    Ok(HvfSnapshotV2StoragePciRecordPlan {
        key,
        origin: captured.origin(),
        sbdf: placement.sbdf,
        bar_region_id: placement.bar_region_id,
        bar_range: placement.bar_range,
        route_count,
    })
}

fn register_active_pci_routes(
    state: &bangbang_runtime::snapshot_device_v2::SnapshotV2PciDeviceState,
    active_routes: &mut Vec<(u64, u32)>,
) -> bool {
    state
        .msix()
        .entries()
        .iter()
        .enumerate()
        .all(|(index, entry)| {
            let Ok(vector) = u16::try_from(index) else {
                return false;
            };
            let referenced = state.msix().config_vector() == vector
                || state.msix().queue_vectors().contains(&vector);
            let pending = state
                .msix()
                .pending_words()
                .get(index / u64::BITS as usize)
                .is_some_and(|word| word & (1_u64 << (index % u64::BITS as usize)) != 0);
            if entry.vector_control() & 1 != 0 || (!referenced && !pending) {
                return true;
            }
            let address = (u64::from(entry.message_address_high()) << 32)
                | u64::from(entry.message_address_low());
            let route = (address, entry.message_data());
            if active_routes.contains(&route) {
                return false;
            }
            active_routes.push(route);
            true
        })
}

fn validate_platform_profile(
    platform: &HvfSnapshotV2PlatformState,
) -> Result<(), PrepareHvfSnapshotV2StorageMmioPlatformPlanError> {
    if !platform.machine().fdt().is_product_process_profile()
        || platform.time().rtc_layout()
            != RtcMmioLayout::new(PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID)
    {
        Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::PlatformProfile)
    } else {
        Ok(())
    }
}

fn plan_mmio_record(
    platform: &HvfSnapshotV2PlatformState,
    key: SnapshotV2DeviceKey,
    captured: &SnapshotV2DeviceTransport,
    region: MmioRegion,
    planned_regions: &mut Vec<MmioRegion>,
    expected_interrupt: Option<GuestInterruptLine>,
) -> Result<HvfSnapshotV2StorageMmioRecordPlan, PrepareHvfSnapshotV2StorageMmioPlatformPlanError> {
    let SnapshotV2DeviceTransport::Mmio(captured) = captured else {
        return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::TransportPolicy);
    };
    if captured.region() != region
        || expected_interrupt.is_some_and(|line| captured.interrupt_line() != line)
        || mmio_region_conflicts_with_platform(
            platform,
            region,
            &platform.global().compatibility().gic_metadata(),
        )?
        || planned_regions
            .iter()
            .any(|planned| planned.id() == region.id() || planned.range().overlaps(region.range()))
    {
        return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::ResourcePlan);
    }
    planned_regions.push(region);
    let interrupt_line = captured.interrupt_line();
    Ok(HvfSnapshotV2StorageMmioRecordPlan {
        key,
        region,
        interrupt_line,
        fdt_device: Arm64FdtVirtioMmioDevice {
            region: Arm64FdtRegion {
                base: region.range().start().raw_value(),
                size: region.range().size(),
            },
            interrupt_line,
        },
    })
}

fn validate_and_set_interrupt(
    captured: &SnapshotV2DeviceTransport,
    interrupt: GuestInterruptLine,
    planned: &mut HvfSnapshotV2StorageMmioRecordPlan,
) -> Result<(), PrepareHvfSnapshotV2StorageMmioPlatformPlanError> {
    let SnapshotV2DeviceTransport::Mmio(captured) = captured else {
        return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::TransportPolicy);
    };
    if captured.interrupt_line() != interrupt {
        return Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::ResourcePlan);
    }
    planned.interrupt_line = interrupt;
    planned.fdt_device.interrupt_line = interrupt;
    Ok(())
}

fn queue_ranges_conflict_with_platform(
    platform: &HvfSnapshotV2PlatformState,
    queue_ranges: Option<[GuestMemoryRange; 3]>,
) -> Result<bool, PrepareHvfSnapshotV2StorageMmioPlatformPlanError> {
    let Some(queue_ranges) = queue_ranges else {
        return Ok(false);
    };
    let fdt = platform.machine().fdt();
    let fdt_range = GuestMemoryRange::new(fdt.address(), u64::from(fdt.size()))
        .map_err(|_| PrepareHvfSnapshotV2StorageMmioPlatformPlanError::ResourcePlan)?;
    let fixed = [
        fdt_range,
        platform.time().vmgenid().range(),
        platform.time().vmclock().range(),
    ];
    if queue_ranges
        .iter()
        .any(|queue| fixed.into_iter().any(|reserved| queue.overlaps(reserved)))
    {
        return Ok(true);
    }
    let pvtime_size = u64::try_from(ARM64_PVTIME_STRUCTURE_SIZE)
        .map_err(|_| PrepareHvfSnapshotV2StorageMmioPlatformPlanError::ResourcePlan)?;
    for record in platform.time().pvtime_vcpus() {
        let range = GuestMemoryRange::new(record.record_ipa(), pvtime_size)
            .map_err(|_| PrepareHvfSnapshotV2StorageMmioPlatformPlanError::ResourcePlan)?;
        if queue_ranges.iter().any(|queue| queue.overlaps(range)) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn mapping_range_conflicts_with_platform(
    platform: &HvfSnapshotV2PlatformState,
    range: GuestMemoryRange,
    gic: &HvfGicMetadata,
) -> Result<bool, PrepareHvfSnapshotV2StorageMmioPlatformPlanError> {
    if platform
        .memory()
        .extents()
        .iter()
        .any(|extent| range.overlaps(extent.range()))
    {
        return Ok(true);
    }
    let fdt = platform.machine().fdt();
    let fixed = [
        GuestMemoryRange::new(fdt.address(), u64::from(fdt.size())),
        Ok(platform.time().vmgenid().range()),
        Ok(platform.time().vmclock().range()),
        GuestMemoryRange::new(PROCESS_SERIAL_MMIO_BASE, SERIAL_MMIO_DEVICE_WINDOW_SIZE),
        GuestMemoryRange::new(PROCESS_RTC_MMIO_BASE, RTC_MMIO_DEVICE_WINDOW_SIZE),
        gic_region_range(gic.distributor),
        gic_region_range(gic.redistributor.region),
    ];
    for fixed in fixed {
        let fixed =
            fixed.map_err(|_| PrepareHvfSnapshotV2StorageMmioPlatformPlanError::ResourcePlan)?;
        if range.overlaps(fixed) {
            return Ok(true);
        }
    }
    let pvtime_size = u64::try_from(ARM64_PVTIME_STRUCTURE_SIZE)
        .map_err(|_| PrepareHvfSnapshotV2StorageMmioPlatformPlanError::ResourcePlan)?;
    for record in platform.time().pvtime_vcpus() {
        let pvtime = GuestMemoryRange::new(record.record_ipa(), pvtime_size)
            .map_err(|_| PrepareHvfSnapshotV2StorageMmioPlatformPlanError::ResourcePlan)?;
        if range.overlaps(pvtime) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn mmio_region_conflicts_with_platform(
    platform: &HvfSnapshotV2PlatformState,
    region: MmioRegion,
    gic: &HvfGicMetadata,
) -> Result<bool, PrepareHvfSnapshotV2StorageMmioPlatformPlanError> {
    if matches!(
        region.id(),
        PROCESS_SERIAL_MMIO_REGION_ID | PROCESS_RTC_MMIO_REGION_ID
    ) {
        return Ok(true);
    }
    let range = region.range();
    if platform
        .memory()
        .extents()
        .iter()
        .any(|extent| range.overlaps(extent.range()))
    {
        return Ok(true);
    }
    let fixed = [
        GuestMemoryRange::new(PROCESS_SERIAL_MMIO_BASE, SERIAL_MMIO_DEVICE_WINDOW_SIZE),
        GuestMemoryRange::new(PROCESS_RTC_MMIO_BASE, RTC_MMIO_DEVICE_WINDOW_SIZE),
        gic_region_range(gic.distributor),
        gic_region_range(gic.redistributor.region),
    ];
    for fixed in fixed {
        let fixed =
            fixed.map_err(|_| PrepareHvfSnapshotV2StorageMmioPlatformPlanError::ResourcePlan)?;
        if range.overlaps(fixed) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn queue_ranges_conflict_with_pci_platform(
    platform: &HvfSnapshotV2PlatformState,
    queue_ranges: Option<[GuestMemoryRange; 3]>,
    gic: &HvfGicMetadata,
    address_plan: Arm64PciAddressPlan,
) -> Result<bool, PrepareHvfSnapshotV2StoragePciPlatformPlanError> {
    let Some(queue_ranges) = queue_ranges else {
        return Ok(false);
    };
    let fdt = platform.machine().fdt();
    let fdt_range = GuestMemoryRange::new(fdt.address(), u64::from(fdt.size()))
        .map_err(|_| PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;
    let msi = gic
        .msi
        .ok_or(PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;
    let fixed = [
        fdt_range,
        platform.time().vmgenid().range(),
        platform.time().vmclock().range(),
        gic_region_range(gic.distributor)
            .map_err(|_| PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?,
        gic_region_range(gic.redistributor.region)
            .map_err(|_| PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?,
        gic_region_range(msi.region)
            .map_err(|_| PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?,
        address_plan.ecam_reservation(),
        address_plan.bar32(),
        address_plan.bar64(),
    ];
    if queue_ranges
        .iter()
        .any(|queue| fixed.into_iter().any(|reserved| queue.overlaps(reserved)))
    {
        return Ok(true);
    }
    let pvtime_size = u64::try_from(ARM64_PVTIME_STRUCTURE_SIZE)
        .map_err(|_| PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;
    for record in platform.time().pvtime_vcpus() {
        let range = GuestMemoryRange::new(record.record_ipa(), pvtime_size)
            .map_err(|_| PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;
        if queue_ranges.iter().any(|queue| queue.overlaps(range)) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn mapping_range_conflicts_with_pci_platform(
    platform: &HvfSnapshotV2PlatformState,
    range: GuestMemoryRange,
    gic: &HvfGicMetadata,
    address_plan: Arm64PciAddressPlan,
) -> Result<bool, PrepareHvfSnapshotV2StoragePciPlatformPlanError> {
    if platform
        .memory()
        .extents()
        .iter()
        .any(|extent| range.overlaps(extent.range()))
    {
        return Ok(true);
    }
    let fdt = platform.machine().fdt();
    let msi = gic
        .msi
        .ok_or(PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;
    let fixed = [
        GuestMemoryRange::new(fdt.address(), u64::from(fdt.size())),
        Ok(platform.time().vmgenid().range()),
        Ok(platform.time().vmclock().range()),
        GuestMemoryRange::new(PROCESS_SERIAL_MMIO_BASE, SERIAL_MMIO_DEVICE_WINDOW_SIZE),
        GuestMemoryRange::new(PROCESS_RTC_MMIO_BASE, RTC_MMIO_DEVICE_WINDOW_SIZE),
        gic_region_range(gic.distributor),
        gic_region_range(gic.redistributor.region),
        gic_region_range(msi.region),
        Ok(address_plan.ecam_reservation()),
        Ok(address_plan.bar32()),
        Ok(address_plan.bar64()),
    ];
    for fixed in fixed {
        let fixed =
            fixed.map_err(|_| PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;
        if range.overlaps(fixed) {
            return Ok(true);
        }
    }
    let pvtime_size = u64::try_from(ARM64_PVTIME_STRUCTURE_SIZE)
        .map_err(|_| PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;
    for record in platform.time().pvtime_vcpus() {
        let pvtime = GuestMemoryRange::new(record.record_ipa(), pvtime_size)
            .map_err(|_| PrepareHvfSnapshotV2StoragePciPlatformPlanError::ResourcePlan)?;
        if range.overlaps(pvtime) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn gic_region_range(
    region: crate::gic::HvfGicRegion,
) -> Result<GuestMemoryRange, bangbang_runtime::memory::GuestMemoryError> {
    GuestMemoryRange::new(GuestAddress::new(region.base), region.size)
}

fn try_clone(value: &str) -> Result<String, PrepareHvfSnapshotV2StorageMmioPlatformPlanError> {
    let mut cloned = String::new();
    cloned
        .try_reserve_exact(value.len())
        .map_err(|_| PrepareHvfSnapshotV2StorageMmioPlatformPlanError::Allocation)?;
    cloned.push_str(value);
    Ok(cloned)
}

fn pci_try_clone(value: &str) -> Result<String, PrepareHvfSnapshotV2StoragePciPlatformPlanError> {
    let mut cloned = String::new();
    cloned
        .try_reserve_exact(value.len())
        .map_err(|_| PrepareHvfSnapshotV2StoragePciPlatformPlanError::Allocation)?;
    cloned.push_str(value);
    Ok(cloned)
}

trait StoragePlatformPlanReserve {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), PrepareHvfSnapshotV2StorageMmioPlatformPlanError>;
}

struct SystemStoragePlatformPlanReserve;

impl StoragePlatformPlanReserve for SystemStoragePlatformPlanReserve {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), PrepareHvfSnapshotV2StorageMmioPlatformPlanError> {
        values
            .try_reserve_exact(additional)
            .map_err(|_| PrepareHvfSnapshotV2StorageMmioPlatformPlanError::Allocation)
    }
}

trait StoragePciPlatformPlanReserve {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), PrepareHvfSnapshotV2StoragePciPlatformPlanError>;
}

struct SystemStoragePciPlatformPlanReserve;

impl StoragePciPlatformPlanReserve for SystemStoragePciPlatformPlanReserve {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), PrepareHvfSnapshotV2StoragePciPlatformPlanError> {
        values
            .try_reserve_exact(additional)
            .map_err(|_| PrepareHvfSnapshotV2StoragePciPlatformPlanError::Allocation)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs::{self, OpenOptions};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    use bangbang_runtime::block::{BlockFileBacking, BlockMmioLayout};
    use bangbang_runtime::memory::{GuestMemory, GuestMemoryLayout};
    use bangbang_runtime::pmem::{PmemFileBacking, PmemMmioLayout};
    use bangbang_runtime::snapshot_device_v2::{
        SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind, SnapshotV2MmioDeviceState,
    };
    use bangbang_runtime::snapshot_device_v2_5::{
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2MultiBlockDeviceGraph,
    };
    use bangbang_runtime::snapshot_device_v2_6::{
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION, PreparedSnapshotV2StorageBundle,
        SnapshotV2PmemDeviceRecord, SnapshotV2StorageDeviceGraph, SnapshotV2StorageRestorePlan,
    };
    use bangbang_runtime::storage_capture::StorageRetryState;
    use bangbang_runtime::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE;

    use super::*;

    const BLOCK_ROOT_MMIO_HEX: &str =
        include_str!("../../runtime/src/snapshot_device_v2_6/fixtures/block-root-mmio.hex");
    const PMEM_ROOT_MMIO_HEX: &str =
        include_str!("../../runtime/src/snapshot_device_v2_6/fixtures/pmem-root-mmio.hex");
    const MIXED_BLOCK_ROOT_MMIO_HEX: &str =
        include_str!("../../runtime/src/snapshot_device_v2_6/fixtures/mixed-block-root-mmio.hex");
    const PMEM_ROOTLESS_PCI_HEX: &str =
        include_str!("../../runtime/src/snapshot_device_v2_6/fixtures/pmem-rootless-pci.hex");
    const BLOCK_ROOTLESS_PCI_HEX: &str =
        include_str!("../../runtime/src/snapshot_device_v2_6/fixtures/block-rootless-pci.hex");
    const MIXED_PMEM_ROOT_PCI_HEX: &str =
        include_str!("../../runtime/src/snapshot_device_v2_6/fixtures/mixed-pmem-root-pci.hex");
    const PROFILE_2_ROOTLESS_MMIO_HEX: &str =
        include_str!("../../runtime/src/snapshot_device_v2_5/fixtures/rootless-mmio.hex");

    const BLOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0xd000_0000);
    const PMEM_MMIO_BASE: GuestAddress = GuestAddress::new(0xd100_0000);
    const BLOCK_MMIO_REGION_ID: bangbang_runtime::mmio::MmioRegionId =
        bangbang_runtime::mmio::MmioRegionId::new(100);
    const PMEM_MMIO_REGION_ID: bangbang_runtime::mmio::MmioRegionId =
        bangbang_runtime::mmio::MmioRegionId::new(200);

    static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

    struct TempBacking {
        path: PathBuf,
    }

    impl TempBacking {
        fn new(name: &str, len: u64) -> Self {
            let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-profile-3-platform-{name}-{}-{sequence}",
                std::process::id()
            ));
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("storage platform backing should create");
            file.set_len(len)
                .expect("storage platform backing should resize");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempBacking {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    #[derive(Clone, Copy)]
    enum FixtureShape {
        BlockRoot,
        PmemRoot,
        MixedBlockRoot,
        BlockRootless,
        PmemRootless,
    }

    struct StorageFixture {
        platform: HvfSnapshotV2PlatformState,
        bundle: PreparedSnapshotV2StorageBundle,
        _files: Vec<TempBacking>,
    }

    fn fixture_bytes(hex: &str) -> Vec<u8> {
        hex.trim()
            .as_bytes()
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

    fn storage_graph(hex: &str) -> SnapshotV2StorageDeviceGraph {
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &fixture_bytes(hex),
        )
        .expect("profile-3 fixture should decode")
    }

    fn rootless_blocks()
    -> Vec<bangbang_runtime::snapshot_device_v2_5::SnapshotV2MultiBlockDeviceRecord> {
        SnapshotV2MultiBlockDeviceGraph::decode(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &fixture_bytes(PROFILE_2_ROOTLESS_MMIO_HEX),
        )
        .expect("rootless profile-2 fixture should decode")
        .records()
        .to_vec()
    }

    fn base_graph(shape: FixtureShape) -> SnapshotV2StorageDeviceGraph {
        match shape {
            FixtureShape::BlockRoot => storage_graph(BLOCK_ROOT_MMIO_HEX),
            FixtureShape::PmemRoot => storage_graph(PMEM_ROOT_MMIO_HEX),
            FixtureShape::MixedBlockRoot => storage_graph(MIXED_BLOCK_ROOT_MMIO_HEX),
            FixtureShape::BlockRootless => SnapshotV2StorageDeviceGraph::try_from_parts(
                None,
                SnapshotV2DeviceTransportKind::Mmio,
                rootless_blocks(),
                Vec::new(),
            )
            .expect("rootless block graph should validate"),
            FixtureShape::PmemRootless => {
                let mixed = storage_graph(MIXED_BLOCK_ROOT_MMIO_HEX);
                SnapshotV2StorageDeviceGraph::try_from_parts(
                    None,
                    SnapshotV2DeviceTransportKind::Mmio,
                    Vec::new(),
                    mixed.pmem_records().to_vec(),
                )
                .expect("rootless pmem graph should validate")
            }
        }
    }

    fn canonicalize_pmem_placement(
        graph: SnapshotV2StorageDeviceGraph,
        first_interrupt: u32,
    ) -> SnapshotV2StorageDeviceGraph {
        let root_key = graph.root_key();
        let block_records = graph.block_records().to_vec();
        for (index, record) in block_records.iter().enumerate() {
            let SnapshotV2DeviceTransport::Mmio(mmio) = record.transport() else {
                panic!("canonical storage fixture should use MMIO");
            };
            assert_eq!(
                mmio.interrupt_line().raw_value(),
                first_interrupt + u32::try_from(index).expect("block index should fit")
            );
        }
        let block_count = block_records.len();
        let layout = PmemMmioLayout::new(PMEM_MMIO_BASE, PMEM_MMIO_REGION_ID);
        let pmem_records = graph
            .pmem_records()
            .iter()
            .enumerate()
            .map(|(index, record)| {
                let SnapshotV2DeviceTransport::Mmio(captured) = record.transport() else {
                    panic!("canonical storage fixture should use MMIO");
                };
                let region = layout
                    .region_at(index)
                    .expect("canonical pmem region should validate");
                let interrupt = GuestInterruptLine::new(
                    first_interrupt
                        + u32::try_from(block_count + index)
                            .expect("storage interrupt index should fit"),
                )
                .expect("canonical pmem interrupt should validate");
                let transport =
                    SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
                        captured.device_feature_select(),
                        captured.driver_feature_select(),
                        captured.queue_select(),
                        region,
                        interrupt,
                    ));
                SnapshotV2PmemDeviceRecord::try_new(
                    record.key().instance(),
                    record.config().clone(),
                    record.pmem().clone(),
                    record.virtio().clone(),
                    transport,
                )
                .expect("canonical pmem record should validate")
            })
            .collect();
        SnapshotV2StorageDeviceGraph::try_from_parts(
            root_key,
            SnapshotV2DeviceTransportKind::Mmio,
            block_records,
            pmem_records,
        )
        .expect("canonical storage graph should validate")
    }

    fn restore_memory(graph: &SnapshotV2StorageDeviceGraph) -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), 0x80_0000)
                .expect("storage restore memory range should validate"),
        ])
        .expect("storage restore memory layout should validate");
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
                    GuestAddress::new(queue.driver_ring().raw_value() + 2),
                )
                .expect("block available cursor should write");
            memory
                .write_slice(
                    &cursor.next_used().to_le_bytes(),
                    GuestAddress::new(queue.device_ring().raw_value() + 2),
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
                    GuestAddress::new(queue.driver_ring().raw_value() + 2),
                )
                .expect("pmem available cursor should write");
            memory
                .write_slice(
                    &cursor.next_used().to_le_bytes(),
                    GuestAddress::new(queue.device_ring().raw_value() + 2),
                )
                .expect("pmem used cursor should write");
        }
        memory
    }

    fn bundle_from_graph(
        graph: SnapshotV2StorageDeviceGraph,
    ) -> (PreparedSnapshotV2StorageBundle, Vec<TempBacking>) {
        let memory = restore_memory(&graph);
        let mut files = Vec::new();
        let mut block_backings = Vec::new();
        let mut pmem_backings = Vec::new();
        for (index, record) in graph.block_records().iter().enumerate() {
            let file = TempBacking::new(&format!("block-{index}"), record.block().backing_bytes());
            let backing =
                BlockFileBacking::open_snapshot(file.path(), record.config().is_read_only())
                    .expect("block backing should open")
                    .0;
            files.push(file);
            block_backings.push(backing);
        }
        for (index, record) in graph.pmem_records().iter().enumerate() {
            let file = TempBacking::new(&format!("pmem-{index}"), record.pmem().file_bytes());
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
            .expect("storage restore bundle should prepare");
        (bundle, files)
    }

    fn fixture(shape: FixtureShape) -> StorageFixture {
        let base = base_graph(shape);
        let platform = crate::snapshot_v2_multi_block_platform::tests::product_mmio_platform(
            base.record_count(),
        );
        let first_interrupt = platform
            .global()
            .compatibility()
            .gic_metadata()
            .spi_interrupt_range
            .base;
        let graph = canonicalize_pmem_placement(base, first_interrupt);
        let (bundle, files) = bundle_from_graph(graph);
        StorageFixture {
            platform,
            bundle,
            _files: files,
        }
    }

    fn pci_fixture(hex: &str) -> StorageFixture {
        let graph = storage_graph(hex);
        let platform = crate::snapshot_v2_multi_block_platform::tests::product_pci_platform();
        let (bundle, files) = bundle_from_graph(graph);
        StorageFixture {
            platform,
            bundle,
            _files: files,
        }
    }

    pub(crate) fn pci_fdt_plan_fixture() -> (
        HvfSnapshotV2PlatformState,
        HvfSnapshotV2StoragePciPlatformPlan,
    ) {
        let fixture = pci_fixture(MIXED_PMEM_ROOT_PCI_HEX);
        let plan =
            prepare_hvf_snapshot_v2_storage_pci_platform_plan(&fixture.platform, &fixture.bundle)
                .expect("storage PCI FDT plan should validate");
        fixture
            .bundle
            .abort()
            .expect("storage PCI FDT bundle should abort");
        (fixture.platform, plan)
    }

    const fn process_config() -> HvfSnapshotV2StorageMmioProcessConfig {
        HvfSnapshotV2StorageMmioProcessConfig::new(
            BlockMmioLayout::new(BLOCK_MMIO_BASE, BLOCK_MMIO_REGION_ID),
            PmemMmioLayout::new(PMEM_MMIO_BASE, PMEM_MMIO_REGION_ID),
        )
    }

    #[test]
    fn shape_and_root_matrix_projects_one_ordered_mmio_product() {
        for (shape, block_count, pmem_count, root_kind) in [
            (FixtureShape::BlockRoot, 1, 0, Some("block")),
            (FixtureShape::PmemRoot, 0, 1, Some("pmem")),
            (FixtureShape::MixedBlockRoot, 1, 1, Some("block")),
            (FixtureShape::BlockRootless, 2, 0, None),
            (FixtureShape::PmemRootless, 0, 1, None),
        ] {
            let fixture = fixture(shape);
            let plan = prepare_hvf_snapshot_v2_storage_mmio_platform_plan(
                &fixture.platform,
                &fixture.bundle,
                process_config(),
            )
            .expect("storage platform matrix entry should plan");
            assert_eq!(plan.block_records().len(), block_count);
            assert_eq!(plan.pmem_records().len(), pmem_count);
            assert_eq!(plan.block_metrics_ids().len(), block_count);
            assert_eq!(plan.pmem_metrics_ids().len(), pmem_count);
            assert_eq!(plan.block_retries().len(), block_count);
            assert_eq!(plan.pmem_retries().len(), pmem_count);
            assert_eq!(plan.root_key().is_some(), root_kind.is_some());
            match root_kind {
                Some("block") => {
                    assert!(plan.command_line().contains("root=PARTUUID="));
                    assert!(!plan.command_line().contains("root=/dev/pmem"));
                }
                Some("pmem") => assert!(plan.command_line().contains("root=/dev/pmem0")),
                None => assert!(!plan.command_line().contains("root=")),
                Some(other) => panic!("unexpected root kind {other}"),
            }

            let first_interrupt = fixture
                .platform
                .global()
                .compatibility()
                .gic_metadata()
                .spi_interrupt_range
                .base;
            for (index, record) in plan
                .block_records()
                .iter()
                .chain(plan.pmem_records())
                .enumerate()
            {
                assert_eq!(
                    record.interrupt_line().raw_value(),
                    first_interrupt + u32::try_from(index).expect("record index should fit")
                );
                assert_eq!(record.fdt_device().interrupt_line, record.interrupt_line());
            }
            assert_eq!(
                plan.serial_interrupt().raw_value(),
                first_interrupt + u32::try_from(block_count + pmem_count).unwrap()
            );
            assert_eq!(
                plan.vmgenid_interrupt().raw_value(),
                plan.serial_interrupt().raw_value() + 1
            );
            assert_eq!(
                plan.vmclock_interrupt().raw_value(),
                plan.serial_interrupt().raw_value() + 2
            );
            let debug = format!("{plan:?}");
            for secret in ["rootfs", "pmem_0", "PARTUUID", "root=/dev/pmem"] {
                assert!(!debug.contains(secret));
            }
            fixture
                .bundle
                .abort()
                .expect("planned storage bundle should abort cleanly");
        }
    }

    #[test]
    fn pci_is_explicitly_unavailable_at_the_mmio_platform_boundary() {
        let graph = storage_graph(PMEM_ROOTLESS_PCI_HEX);
        let (bundle, _files) = bundle_from_graph(graph);
        let platform = crate::snapshot_v2_multi_block_platform::tests::product_mmio_platform(1);
        assert!(matches!(
            prepare_hvf_snapshot_v2_storage_mmio_platform_plan(
                &platform,
                &bundle,
                process_config(),
            ),
            Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::TransportPolicy)
        ));
        bundle
            .abort()
            .expect("rejected PCI bundle should abort cleanly");
    }

    #[test]
    fn pci_shape_matrix_projects_one_combined_host_and_endpoint_order() {
        for (hex, block_count, pmem_count, has_root) in [
            (BLOCK_ROOTLESS_PCI_HEX, 1, 0, false),
            (PMEM_ROOTLESS_PCI_HEX, 0, 1, false),
            (MIXED_PMEM_ROOT_PCI_HEX, 1, 1, true),
        ] {
            let fixture = pci_fixture(hex);
            let plan = prepare_hvf_snapshot_v2_storage_pci_platform_plan(
                &fixture.platform,
                &fixture.bundle,
            )
            .expect("storage PCI matrix entry should plan");
            assert_eq!(plan.pci().block_records().len(), block_count);
            assert_eq!(plan.pci().pmem_records().len(), pmem_count);
            assert_eq!(plan.pci().record_count(), block_count + pmem_count);
            assert_eq!(plan.root_key().is_some(), has_root);
            assert!(!plan.command_line().contains("pci=off"));
            if has_root {
                assert!(plan.command_line().contains("root=/dev/pmem0"));
            } else {
                assert!(!plan.command_line().contains("root="));
            }
            let address_plan =
                Arm64PciAddressPlan::firecracker_v1_16().expect("PCI plan should build");
            let mut expected_routes = 0;
            for (index, record) in plan
                .pci()
                .block_records()
                .iter()
                .chain(plan.pci().pmem_records())
                .enumerate()
            {
                let placement = snapshot_v2_pci_endpoint_placement(address_plan, index)
                    .expect("combined endpoint placement should exist");
                assert_eq!(record.sbdf(), placement.sbdf);
                assert_eq!(record.bar_region_id(), placement.bar_region_id);
                assert_eq!(record.bar_range(), placement.bar_range);
                expected_routes += record.route_count();
            }
            assert_eq!(plan.pci().route_demand(), expected_routes);
            assert_eq!(
                plan.pci().host(),
                Arm64FdtPciHost::from_address_plan(address_plan)
            );
            let debug = format!("{plan:?}");
            for secret in ["rootfs", "pmem_0", "PARTUUID", "root=/dev/pmem"] {
                assert!(!debug.contains(secret));
            }
            fixture
                .bundle
                .abort()
                .expect("planned PCI storage bundle should abort cleanly");
        }
    }

    #[test]
    fn placement_interrupt_and_fixed_range_conflicts_fail_before_construction() {
        let fixture = fixture(FixtureShape::MixedBlockRoot);
        let shifted = HvfSnapshotV2StorageMmioProcessConfig::new(
            BlockMmioLayout::new(
                BLOCK_MMIO_BASE
                    .checked_add(VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
                    .expect("shifted block base should fit"),
                BLOCK_MMIO_REGION_ID,
            ),
            PmemMmioLayout::new(PMEM_MMIO_BASE, PMEM_MMIO_REGION_ID),
        );
        assert!(matches!(
            prepare_hvf_snapshot_v2_storage_mmio_platform_plan(
                &fixture.platform,
                &fixture.bundle,
                shifted,
            ),
            Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::ResourcePlan)
        ));

        let gic = fixture.platform.global().compatibility().gic_metadata();
        let fdt = fixture.platform.machine().fdt();
        let fdt_range = GuestMemoryRange::new(fdt.address(), u64::from(fdt.size()))
            .expect("FDT range should validate");
        assert!(
            mapping_range_conflicts_with_platform(&fixture.platform, fdt_range, &gic)
                .expect("mapping conflict should be checked")
        );
        assert!(
            queue_ranges_conflict_with_platform(&fixture.platform, Some([fdt_range; 3]))
                .expect("queue conflict should be checked")
        );
        let serial_region = MmioRegion::new(
            bangbang_runtime::mmio::MmioRegionId::new(999),
            PROCESS_SERIAL_MMIO_BASE,
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("serial conflict region should validate");
        assert!(
            mmio_region_conflicts_with_platform(&fixture.platform, serial_region, &gic)
                .expect("MMIO conflict should be checked")
        );
        fixture
            .bundle
            .abort()
            .expect("conflict fixture should abort cleanly");
    }

    struct FailingReserve {
        calls: usize,
        fail_at: usize,
    }

    impl StoragePlatformPlanReserve for FailingReserve {
        fn reserve<T>(
            &mut self,
            values: &mut Vec<T>,
            additional: usize,
        ) -> Result<(), PrepareHvfSnapshotV2StorageMmioPlatformPlanError> {
            let call = self.calls;
            self.calls = self.calls.saturating_add(1);
            if call == self.fail_at {
                Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::Allocation)
            } else {
                values
                    .try_reserve_exact(additional)
                    .map_err(|_| PrepareHvfSnapshotV2StorageMmioPlatformPlanError::Allocation)
            }
        }
    }

    impl StoragePciPlatformPlanReserve for FailingReserve {
        fn reserve<T>(
            &mut self,
            values: &mut Vec<T>,
            additional: usize,
        ) -> Result<(), PrepareHvfSnapshotV2StoragePciPlatformPlanError> {
            let call = self.calls;
            self.calls = self.calls.saturating_add(1);
            if call == self.fail_at {
                Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::Allocation)
            } else {
                values
                    .try_reserve_exact(additional)
                    .map_err(|_| PrepareHvfSnapshotV2StoragePciPlatformPlanError::Allocation)
            }
        }
    }

    #[test]
    fn every_platform_inventory_reservation_reports_explicit_allocation_failure() {
        for fail_at in 0..7 {
            let fixture = fixture(FixtureShape::MixedBlockRoot);
            assert!(matches!(
                prepare_hvf_snapshot_v2_storage_mmio_platform_plan_with(
                    &fixture.platform,
                    &fixture.bundle,
                    process_config(),
                    &mut FailingReserve { calls: 0, fail_at },
                ),
                Err(PrepareHvfSnapshotV2StorageMmioPlatformPlanError::Allocation)
            ));
            fixture
                .bundle
                .abort()
                .expect("allocation fixture should abort cleanly");
        }
    }

    #[test]
    fn every_pci_platform_inventory_reservation_reports_explicit_allocation_failure() {
        for fail_at in 0..10 {
            let fixture = pci_fixture(MIXED_PMEM_ROOT_PCI_HEX);
            assert!(matches!(
                prepare_hvf_snapshot_v2_storage_pci_platform_plan_with(
                    &fixture.platform,
                    &fixture.bundle,
                    &mut FailingReserve { calls: 0, fail_at },
                ),
                Err(PrepareHvfSnapshotV2StoragePciPlatformPlanError::Allocation)
            ));
            fixture
                .bundle
                .abort()
                .expect("PCI allocation fixture should abort cleanly");
        }
    }
}
