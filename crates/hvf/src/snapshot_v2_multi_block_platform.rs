//! Host-free native-v2 profile-2 block platform planning.

use std::fmt;

use bangbang_runtime::block::{
    BlockMmioLayout, BlockMmioRegistrationError, VIRTIO_BLOCK_QUEUE_SIZES,
};
use bangbang_runtime::boot::{
    BootCommandLineError, canonical_process_block_command_line,
    canonical_process_root_block_command_line,
};
use bangbang_runtime::fdt::{Arm64FdtPciHost, Arm64FdtRegion, Arm64FdtVirtioMmioDevice};
use bangbang_runtime::interrupt::GuestInterruptLine;
use bangbang_runtime::memory::{GuestAddress, GuestMemoryRange};
use bangbang_runtime::mmio::{MmioRegion, MmioRegionId};
use bangbang_runtime::pci::{
    Arm64PciAddressPlan, PCI_BUS_ZERO, PCI_FIRST_ENDPOINT_DEVICE, PCI_FUNCTION_ZERO,
    PCI_SEGMENT_ZERO, PciBarAddressSpace, PciBarPrefetchable, PciSbdf,
};
use bangbang_runtime::pvtime::ARM64_PVTIME_STRUCTURE_SIZE;
use bangbang_runtime::rtc::{RTC_MMIO_DEVICE_WINDOW_SIZE, RtcMmioLayout};
use bangbang_runtime::serial::SERIAL_MMIO_DEVICE_WINDOW_SIZE;
use bangbang_runtime::snapshot_device_v2::{
    SnapshotV2DeviceKey, SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind,
    SnapshotV2PciDeviceState, SnapshotV2PciMsixState,
};
use bangbang_runtime::snapshot_device_v2_5::{
    NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_RECORDS, PreparedSnapshotV2MultiBlockBundle,
};
use bangbang_runtime::storage_capture::{StorageDeviceOrigin, StorageRetryState};
use bangbang_runtime::virtio_pci::{
    VIRTIO_PCI_CAPABILITY_BAR_INDEX, VIRTIO_PCI_CAPABILITY_BAR_SIZE, VIRTIO_PCI_MAX_MSIX_VECTORS,
    VIRTIO_PCI_NO_VECTOR, VirtioPciEndpointPhase,
};

use crate::gic::{
    HvfGicInterruptLineAllocator, HvfGicMetadata, HvfGicMsiMetadata,
    HvfInterruptLineAllocationError,
};
use crate::snapshot_v2::HvfSnapshotV2PlatformState;
use crate::snapshot_v2_platform::{
    PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID, PROCESS_SERIAL_MMIO_BASE,
    PROCESS_SERIAL_MMIO_REGION_ID, pci_msix_routes_match_gic,
};
use crate::startup::{
    PCI_ENDPOINT_SLOT_COUNT, pci_data_region_id, pci_root_restore_gic_msi_configuration,
};

const REDACTED: &str = "<redacted>";
const PCI_GENERIC_WRITABLE_OFFSETS: [u16; 4] = [0x04, 0x05, 0x0c, 0x3c];

/// Destination process policy for a profile-2 block vector.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2MultiBlockProcessConfig {
    block_mmio_layout: BlockMmioLayout,
    pci_enabled: bool,
}

impl HvfSnapshotV2MultiBlockProcessConfig {
    /// Creates one closed block-only destination policy.
    pub const fn new(block_mmio_layout: BlockMmioLayout, pci_enabled: bool) -> Self {
        Self {
            block_mmio_layout,
            pci_enabled,
        }
    }

    /// Returns the configured MMIO allocation sequence.
    pub const fn block_mmio_layout(self) -> BlockMmioLayout {
        self.block_mmio_layout
    }

    /// Returns whether the all-virtio PCI transport is selected.
    pub const fn pci_enabled(self) -> bool {
        self.pci_enabled
    }
}

impl fmt::Debug for HvfSnapshotV2MultiBlockProcessConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2MultiBlockProcessConfig")
            .field("state", &REDACTED)
            .finish()
    }
}

/// One canonical MMIO record and its semantic FDT node.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2MultiBlockMmioRecordPlan {
    key: SnapshotV2DeviceKey,
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    fdt_device: Arm64FdtVirtioMmioDevice,
}

impl HvfSnapshotV2MultiBlockMmioRecordPlan {
    /// Returns the canonical graph key.
    pub const fn key(self) -> SnapshotV2DeviceKey {
        self.key
    }

    /// Returns the exact dispatcher region.
    pub const fn region(self) -> MmioRegion {
        self.region
    }

    /// Returns the exact GIC SPI.
    pub const fn interrupt_line(self) -> GuestInterruptLine {
        self.interrupt_line
    }

    /// Returns the exact semantic FDT node.
    pub const fn fdt_device(self) -> Arm64FdtVirtioMmioDevice {
        self.fdt_device
    }
}

impl fmt::Debug for HvfSnapshotV2MultiBlockMmioRecordPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2MultiBlockMmioRecordPlan")
            .field("state", &REDACTED)
            .finish()
    }
}

/// One canonical PCI record and its destination dispatcher identity.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2MultiBlockPciRecordPlan {
    key: SnapshotV2DeviceKey,
    origin: StorageDeviceOrigin,
    sbdf: PciSbdf,
    bar_region_id: MmioRegionId,
    bar_range: GuestMemoryRange,
    route_count: usize,
}

impl HvfSnapshotV2MultiBlockPciRecordPlan {
    /// Returns the canonical graph key.
    pub const fn key(self) -> SnapshotV2DeviceKey {
        self.key
    }

    /// Returns the captured startup/runtime origin.
    pub const fn origin(self) -> StorageDeviceOrigin {
        self.origin
    }

    /// Returns the exact endpoint identity.
    pub const fn sbdf(self) -> PciSbdf {
        self.sbdf
    }

    /// Returns the destination dispatcher region ID.
    pub const fn bar_region_id(self) -> MmioRegionId {
        self.bar_region_id
    }

    /// Returns the exact capability BAR range.
    pub const fn bar_range(self) -> GuestMemoryRange {
        self.bar_range
    }

    /// Returns configuration-plus-queue MSI-X route demand.
    pub const fn route_count(self) -> usize {
        self.route_count
    }
}

impl fmt::Debug for HvfSnapshotV2MultiBlockPciRecordPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2MultiBlockPciRecordPlan")
            .field("origin", &self.origin)
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete PCI host, endpoint vector, and shared MSI demand.
pub struct HvfSnapshotV2MultiBlockPciPlan {
    host: Arm64FdtPciHost,
    msi: HvfGicMsiMetadata,
    route_demand: usize,
    records: Vec<HvfSnapshotV2MultiBlockPciRecordPlan>,
}

impl HvfSnapshotV2MultiBlockPciPlan {
    /// Returns the single canonical PCI FDT host.
    pub const fn host(&self) -> Arm64FdtPciHost {
        self.host
    }

    /// Returns captured shared GICv2m metadata.
    pub const fn msi(&self) -> HvfGicMsiMetadata {
        self.msi
    }

    /// Returns complete block MSI-X route demand.
    pub const fn route_demand(&self) -> usize {
        self.route_demand
    }

    /// Returns endpoints in canonical graph order.
    pub fn records(&self) -> &[HvfSnapshotV2MultiBlockPciRecordPlan] {
        &self.records
    }
}

impl fmt::Debug for HvfSnapshotV2MultiBlockPciPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2MultiBlockPciPlan")
            .field("record_count", &self.records.len())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Transport-tagged complete block resource vector.
pub enum HvfSnapshotV2MultiBlockTransportPlan {
    /// Ordered MMIO records, each also representing one FDT node.
    Mmio(Vec<HvfSnapshotV2MultiBlockMmioRecordPlan>),
    /// One PCI host plus ordered endpoint records.
    Pci(HvfSnapshotV2MultiBlockPciPlan),
}

impl HvfSnapshotV2MultiBlockTransportPlan {
    /// Returns the selected transport kind.
    pub const fn kind(&self) -> SnapshotV2DeviceTransportKind {
        match self {
            Self::Mmio(_) => SnapshotV2DeviceTransportKind::Mmio,
            Self::Pci(_) => SnapshotV2DeviceTransportKind::Pci,
        }
    }

    /// Returns the number of canonical resource records.
    pub fn len(&self) -> usize {
        match self {
            Self::Mmio(records) => records.len(),
            Self::Pci(plan) => plan.records.len(),
        }
    }

    /// Returns whether the resource vector is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns MMIO records when MMIO is selected.
    pub fn mmio_records(&self) -> Option<&[HvfSnapshotV2MultiBlockMmioRecordPlan]> {
        match self {
            Self::Mmio(records) => Some(records),
            Self::Pci(_) => None,
        }
    }

    /// Returns the PCI plan when PCI is selected.
    pub const fn pci(&self) -> Option<&HvfSnapshotV2MultiBlockPciPlan> {
        match self {
            Self::Pci(plan) => Some(plan),
            Self::Mmio(_) => None,
        }
    }
}

impl fmt::Debug for HvfSnapshotV2MultiBlockTransportPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2MultiBlockTransportPlan")
            .field("kind", &self.kind())
            .field("record_count", &self.len())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Host-time-free retry state for one canonical record.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2MultiBlockRetryPlan {
    key: SnapshotV2DeviceKey,
    retry: StorageRetryState,
}

impl HvfSnapshotV2MultiBlockRetryPlan {
    /// Returns the canonical graph key.
    pub const fn key(self) -> SnapshotV2DeviceKey {
        self.key
    }

    /// Returns the logical retry disposition.
    pub const fn retry(self) -> StorageRetryState {
        self.retry
    }
}

impl fmt::Debug for HvfSnapshotV2MultiBlockRetryPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2MultiBlockRetryPlan")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete pre-HVF profile-2 block platform proof.
pub struct HvfSnapshotV2MultiBlockPlatformPlan {
    root_key: Option<SnapshotV2DeviceKey>,
    command_line: String,
    metrics_ids: Vec<String>,
    retries: Vec<HvfSnapshotV2MultiBlockRetryPlan>,
    earliest_retry_index: Option<usize>,
    transport: HvfSnapshotV2MultiBlockTransportPlan,
    serial_interrupt: GuestInterruptLine,
    vmgenid_interrupt: GuestInterruptLine,
    vmclock_interrupt: GuestInterruptLine,
}

pub(crate) struct HvfSnapshotV2MultiBlockPlatformPlanParts {
    pub(crate) root_key: Option<SnapshotV2DeviceKey>,
    pub(crate) command_line: String,
    pub(crate) metrics_ids: Vec<String>,
    pub(crate) retries: Vec<HvfSnapshotV2MultiBlockRetryPlan>,
    pub(crate) earliest_retry_index: Option<usize>,
    pub(crate) transport: HvfSnapshotV2MultiBlockTransportPlan,
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

impl HvfSnapshotV2MultiBlockPlatformPlan {
    /// Returns the optional root graph key.
    pub const fn root_key(&self) -> Option<SnapshotV2DeviceKey> {
        self.root_key
    }

    /// Returns the exact process command line.
    pub fn command_line(&self) -> &str {
        &self.command_line
    }

    /// Returns complete metrics IDs in graph order.
    pub fn metrics_ids(&self) -> &[String] {
        &self.metrics_ids
    }

    /// Returns every logical retry state in graph order.
    pub fn retries(&self) -> &[HvfSnapshotV2MultiBlockRetryPlan] {
        &self.retries
    }

    /// Returns the earliest deterministic logical scheduler input.
    pub fn earliest_retry(&self) -> Option<HvfSnapshotV2MultiBlockRetryPlan> {
        self.earliest_retry_index
            .and_then(|index| self.retries.get(index).copied())
    }

    /// Returns the complete transport-specific vector.
    pub const fn transport(&self) -> &HvfSnapshotV2MultiBlockTransportPlan {
        &self.transport
    }

    /// Returns the canonical serial SPI.
    pub const fn serial_interrupt(&self) -> GuestInterruptLine {
        self.serial_interrupt
    }

    /// Returns the canonical VMGenID SPI.
    pub const fn vmgenid_interrupt(&self) -> GuestInterruptLine {
        self.vmgenid_interrupt
    }

    /// Returns the canonical VMClock SPI.
    pub const fn vmclock_interrupt(&self) -> GuestInterruptLine {
        self.vmclock_interrupt
    }

    pub(crate) fn into_parts(self) -> HvfSnapshotV2MultiBlockPlatformPlanParts {
        HvfSnapshotV2MultiBlockPlatformPlanParts {
            root_key: self.root_key,
            command_line: self.command_line,
            metrics_ids: self.metrics_ids,
            retries: self.retries,
            earliest_retry_index: self.earliest_retry_index,
            transport: self.transport,
            serial_interrupt: self.serial_interrupt,
            vmgenid_interrupt: self.vmgenid_interrupt,
            vmclock_interrupt: self.vmclock_interrupt,
        }
    }
}

impl fmt::Debug for HvfSnapshotV2MultiBlockPlatformPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2MultiBlockPlatformPlan")
            .field("record_count", &self.transport.len())
            .field("transport", &self.transport.kind())
            .field("has_root", &self.root_key.is_some())
            .field("has_retry", &self.earliest_retry_index.is_some())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Redacted rejection from profile-2 host-free platform planning.
pub enum PrepareHvfSnapshotV2MultiBlockPlatformPlanError {
    /// Captured platform metadata is not the canonical product profile.
    PlatformProfile,
    /// Bundle, controller, retry, metrics, or resource cardinality disagrees.
    Cardinality,
    /// Bounded destination metadata allocation failed.
    Allocation,
    /// Root role, public ID, configuration, or retry identity disagrees.
    RecordIdentity,
    /// Destination transport policy disagrees with the bundle.
    TransportPolicy,
    /// Canonical process arguments could not be constructed.
    CommandLine(BootCommandLineError),
    /// A queue overlaps retained platform-owned memory.
    QueuePlatformConflict,
    /// MMIO layout arithmetic or policy is invalid.
    MmioLayout(Box<BlockMmioRegistrationError>),
    /// Captured GIC metadata cannot satisfy deterministic SPI demand.
    Interrupt(HvfInterruptLineAllocationError),
    /// Captured placement or transport-global state is noncanonical.
    ResourcePlan,
    /// The PCI vector exceeds the shared product endpoint capacity.
    PciCapacity {
        /// Requested endpoint count.
        count: usize,
        /// Product endpoint maximum.
        maximum: usize,
    },
}

impl fmt::Debug for PrepareHvfSnapshotV2MultiBlockPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self {
            Self::PlatformProfile => "platform profile",
            Self::Cardinality => "cardinality",
            Self::Allocation => "allocation",
            Self::RecordIdentity => "record identity",
            Self::TransportPolicy => "transport policy",
            Self::CommandLine(_) => "command line",
            Self::QueuePlatformConflict => "queue/platform conflict",
            Self::MmioLayout(_) => "MMIO layout",
            Self::Interrupt(_) => "interrupt demand",
            Self::ResourcePlan => "resource plan",
            Self::PciCapacity { .. } => "PCI capacity",
        };
        formatter
            .debug_struct("PrepareHvfSnapshotV2MultiBlockPlatformPlanError")
            .field("category", &category)
            .field("state", &REDACTED)
            .finish()
    }
}

impl fmt::Display for PrepareHvfSnapshotV2MultiBlockPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PlatformProfile => "native-v2 multi-block platform profile is not canonical",
            Self::Cardinality => "native-v2 multi-block platform cardinality is inconsistent",
            Self::Allocation => "native-v2 multi-block platform allocation failed",
            Self::RecordIdentity => {
                "native-v2 multi-block platform record identity is inconsistent"
            }
            Self::TransportPolicy => {
                "native-v2 multi-block platform transport policy is inconsistent"
            }
            Self::CommandLine(_) => "native-v2 multi-block process command line is invalid",
            Self::QueuePlatformConflict => {
                "native-v2 multi-block queue overlaps platform-owned memory"
            }
            Self::MmioLayout(_) => "native-v2 multi-block MMIO layout is invalid",
            Self::Interrupt(_) => "native-v2 multi-block interrupt demand is invalid",
            Self::ResourcePlan => "native-v2 multi-block platform resources are inconsistent",
            Self::PciCapacity { .. } => "native-v2 multi-block PCI endpoint capacity is exceeded",
        })
    }
}

impl std::error::Error for PrepareHvfSnapshotV2MultiBlockPlatformPlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CommandLine(source) => Some(source),
            Self::MmioLayout(source) => Some(source),
            Self::Interrupt(source) => Some(source),
            Self::PlatformProfile
            | Self::Cardinality
            | Self::Allocation
            | Self::RecordIdentity
            | Self::TransportPolicy
            | Self::QueuePlatformConflict
            | Self::ResourcePlan
            | Self::PciCapacity { .. } => None,
        }
    }
}

/// Proves a complete profile-2 block vector before any live HVF construction.
pub fn prepare_hvf_snapshot_v2_multi_block_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    bundle: &PreparedSnapshotV2MultiBlockBundle,
    process: HvfSnapshotV2MultiBlockProcessConfig,
) -> Result<HvfSnapshotV2MultiBlockPlatformPlan, PrepareHvfSnapshotV2MultiBlockPlatformPlanError> {
    prepare_hvf_snapshot_v2_multi_block_platform_plan_with(
        platform,
        bundle,
        process,
        &mut SystemPlatformPlanReserve,
    )
}

fn prepare_hvf_snapshot_v2_multi_block_platform_plan_with(
    platform: &HvfSnapshotV2PlatformState,
    bundle: &PreparedSnapshotV2MultiBlockBundle,
    process: HvfSnapshotV2MultiBlockProcessConfig,
    reserve: &mut impl PlatformPlanReserve,
) -> Result<HvfSnapshotV2MultiBlockPlatformPlan, PrepareHvfSnapshotV2MultiBlockPlatformPlanError> {
    validate_platform_profile(platform)?;
    let records = bundle.records();
    let configs = bundle.drive_configs().as_slice();
    let projected_retries = bundle.retry_projection();
    let maximum = usize::from(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_RECORDS);
    if records.is_empty()
        || records.len() > maximum
        || configs.len() != records.len()
        || projected_retries.len() != records.len()
    {
        return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::Cardinality);
    }

    let expected_kind = if process.pci_enabled {
        SnapshotV2DeviceTransportKind::Pci
    } else {
        SnapshotV2DeviceTransportKind::Mmio
    };
    if records
        .first()
        .is_none_or(|record| record.transport().kind() != expected_kind)
    {
        return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::TransportPolicy);
    }
    if expected_kind == SnapshotV2DeviceTransportKind::Pci
        && records.len() > PCI_ENDPOINT_SLOT_COUNT
    {
        return Err(
            PrepareHvfSnapshotV2MultiBlockPlatformPlanError::PciCapacity {
                count: records.len(),
                maximum: PCI_ENDPOINT_SLOT_COUNT,
            },
        );
    }

    let mut metrics_ids = Vec::new();
    let mut retries = Vec::new();
    reserve.reserve(&mut metrics_ids, records.len())?;
    reserve.reserve(&mut retries, records.len())?;
    let mut root_key = None;
    let mut root_config = None;
    let mut earliest_retry_index = None;

    for (index, ((record, config), projected_retry)) in records
        .iter()
        .zip(configs)
        .zip(projected_retries)
        .enumerate()
    {
        let read_only = config
            .is_read_only()
            .ok_or(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::RecordIdentity)?;
        if record.transport().kind() != expected_kind
            || record.key() != projected_retry.key()
            || record.retry() != projected_retry.retry()
            || record.retry_deadline() != projected_retry.retry_deadline()
            || record.drive_id() != config.drive_id()
            || record.is_root_device() != config.is_root_device()
            || record.config_space().is_read_only() != read_only
            || record.device().device().io_engine() != config.io_engine()
            || record.device().cache_type() != config.cache_type()
            || config.is_vhost_user()
            || (config.is_root_device() && index != 0)
            || metrics_ids
                .iter()
                .any(|candidate| candidate == config.drive_id())
        {
            return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::RecordIdentity);
        }
        if config.is_root_device() {
            if root_key.replace(record.key()).is_some() {
                return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::RecordIdentity);
            }
            root_config = Some((config.partuuid(), read_only));
        }
        if queue_ranges_conflict_with_platform(platform, record.queue_ranges())? {
            return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::QueuePlatformConflict);
        }

        metrics_ids.push(try_clone(config.drive_id())?);
        retries.push(HvfSnapshotV2MultiBlockRetryPlan {
            key: record.key(),
            retry: record.retry(),
        });
        if retry_precedes(
            record.retry(),
            earliest_retry_index.and_then(|candidate| retries.get(candidate).copied()),
        ) {
            earliest_retry_index = Some(index);
        }
    }

    if root_key.is_some() != root_config.is_some()
        || metrics_ids.len() != records.len()
        || retries.len() != records.len()
    {
        return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::Cardinality);
    }
    let base_arguments = platform.machine().boot().boot_arguments();
    let base_command_line =
        canonical_process_block_command_line(base_arguments, process.pci_enabled)
            .map_err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::CommandLine)?;
    if command_line_has_root_argument(&base_command_line) {
        return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::RecordIdentity);
    }
    let command_line = match root_config {
        Some((partuuid, read_only)) => canonical_process_root_block_command_line(
            base_arguments,
            process.pci_enabled,
            partuuid,
            read_only,
        ),
        None => Ok(base_command_line),
    }
    .map_err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::CommandLine)?;

    let gic = platform.global().compatibility().gic_metadata();
    let mut interrupt_allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
        .map_err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::Interrupt)?;
    let transport = match expected_kind {
        SnapshotV2DeviceTransportKind::Mmio => prepare_mmio_plan(
            platform,
            records,
            process.block_mmio_layout,
            &gic,
            &mut interrupt_allocator,
            reserve,
        )?,
        SnapshotV2DeviceTransportKind::Pci => prepare_pci_plan(records, &gic, reserve)?,
    };
    let serial_interrupt = interrupt_allocator
        .allocate()
        .map_err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::Interrupt)?;
    let vmgenid_interrupt = interrupt_allocator
        .allocate()
        .map_err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::Interrupt)?;
    let vmclock_interrupt = interrupt_allocator
        .allocate()
        .map_err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::Interrupt)?;
    if platform.time().vmgenid().interrupt_line() != vmgenid_interrupt
        || platform.time().vmclock().interrupt_line() != vmclock_interrupt
        || transport.len() != records.len()
    {
        return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan);
    }

    Ok(HvfSnapshotV2MultiBlockPlatformPlan {
        root_key,
        command_line,
        metrics_ids,
        retries,
        earliest_retry_index,
        transport,
        serial_interrupt,
        vmgenid_interrupt,
        vmclock_interrupt,
    })
}

fn validate_platform_profile(
    platform: &HvfSnapshotV2PlatformState,
) -> Result<(), PrepareHvfSnapshotV2MultiBlockPlatformPlanError> {
    if !platform.machine().fdt().is_product_process_profile()
        || platform.time().rtc_layout()
            != RtcMmioLayout::new(PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID)
    {
        Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::PlatformProfile)
    } else {
        Ok(())
    }
}

fn queue_ranges_conflict_with_platform(
    platform: &HvfSnapshotV2PlatformState,
    queue_ranges: Option<[GuestMemoryRange; 3]>,
) -> Result<bool, PrepareHvfSnapshotV2MultiBlockPlatformPlanError> {
    let Some(queue_ranges) = queue_ranges else {
        return Ok(false);
    };
    let fdt = platform.machine().fdt();
    let fdt_range = GuestMemoryRange::new(fdt.address(), u64::from(fdt.size()))
        .map_err(|_| PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?;
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
        .map_err(|_| PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?;
    for record in platform.time().pvtime_vcpus() {
        let range = GuestMemoryRange::new(record.record_ipa(), pvtime_size)
            .map_err(|_| PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?;
        if queue_ranges.iter().any(|queue| queue.overlaps(range)) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn prepare_mmio_plan(
    platform: &HvfSnapshotV2PlatformState,
    records: &[bangbang_runtime::snapshot_device_v2_5::PreparedSnapshotV2MultiBlockRecord],
    layout: BlockMmioLayout,
    gic: &HvfGicMetadata,
    allocator: &mut HvfGicInterruptLineAllocator,
    reserve: &mut impl PlatformPlanReserve,
) -> Result<HvfSnapshotV2MultiBlockTransportPlan, PrepareHvfSnapshotV2MultiBlockPlatformPlanError> {
    if gic.msi.is_some() {
        return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan);
    }
    let mut planned = Vec::new();
    reserve.reserve(&mut planned, records.len())?;
    for (index, record) in records.iter().enumerate() {
        let region = layout.region_at(index).map_err(|source| {
            PrepareHvfSnapshotV2MultiBlockPlatformPlanError::MmioLayout(Box::new(source))
        })?;
        let interrupt_line = allocator
            .allocate()
            .map_err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::Interrupt)?;
        let SnapshotV2DeviceTransport::Mmio(captured) = record.transport() else {
            return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::TransportPolicy);
        };
        if captured.region() != region
            || captured.interrupt_line() != interrupt_line
            || mmio_region_conflicts_with_platform(platform, region, gic)?
        {
            return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan);
        }
        let fdt_device = Arm64FdtVirtioMmioDevice {
            region: Arm64FdtRegion {
                base: region.range().start().raw_value(),
                size: region.range().size(),
            },
            interrupt_line,
        };
        planned.push(HvfSnapshotV2MultiBlockMmioRecordPlan {
            key: record.key(),
            region,
            interrupt_line,
            fdt_device,
        });
    }
    Ok(HvfSnapshotV2MultiBlockTransportPlan::Mmio(planned))
}

fn mmio_region_conflicts_with_platform(
    platform: &HvfSnapshotV2PlatformState,
    region: MmioRegion,
    gic: &HvfGicMetadata,
) -> Result<bool, PrepareHvfSnapshotV2MultiBlockPlatformPlanError> {
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
    let fixed_ranges = [
        GuestMemoryRange::new(PROCESS_SERIAL_MMIO_BASE, SERIAL_MMIO_DEVICE_WINDOW_SIZE),
        GuestMemoryRange::new(PROCESS_RTC_MMIO_BASE, RTC_MMIO_DEVICE_WINDOW_SIZE),
        gic_region_range(gic.distributor),
        gic_region_range(gic.redistributor.region),
    ];
    for fixed in fixed_ranges {
        let fixed =
            fixed.map_err(|_| PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?;
        if range.overlaps(fixed) {
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

fn prepare_pci_plan(
    records: &[bangbang_runtime::snapshot_device_v2_5::PreparedSnapshotV2MultiBlockRecord],
    gic: &HvfGicMetadata,
    reserve: &mut impl PlatformPlanReserve,
) -> Result<HvfSnapshotV2MultiBlockTransportPlan, PrepareHvfSnapshotV2MultiBlockPlatformPlanError> {
    let msi = gic
        .msi
        .ok_or(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?;
    let expected_msi = pci_root_restore_gic_msi_configuration()
        .map_err(|_| PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?;
    if msi.interrupt_range.count != expected_msi.interrupt_count().get() {
        return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan);
    }
    let (routes_per_record, route_demand) =
        pci_route_demand(records.len(), msi.interrupt_range.count)?;

    let address_plan = Arm64PciAddressPlan::firecracker_v1_16()
        .map_err(|_| PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?;
    let mut planned = Vec::new();
    reserve.reserve(&mut planned, records.len())?;
    for (index, record) in records.iter().enumerate() {
        let device = PCI_FIRST_ENDPOINT_DEVICE
            .checked_add(
                u8::try_from(index)
                    .map_err(|_| PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?,
            )
            .ok_or(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?;
        let sbdf = PciSbdf::new(PCI_SEGMENT_ZERO, PCI_BUS_ZERO, device, PCI_FUNCTION_ZERO)
            .map_err(|_| PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?;
        let offset = u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_mul(VIRTIO_PCI_CAPABILITY_BAR_SIZE))
            .ok_or(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?;
        let bar_start = address_plan
            .bar64()
            .start()
            .checked_add(offset)
            .ok_or(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?;
        let bar_range = GuestMemoryRange::new(bar_start, VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .map_err(|_| PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?;
        if bar_range.end_exclusive().raw_value() > address_plan.bar64().end_exclusive().raw_value()
        {
            return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan);
        }
        let bar_region_id = pci_data_region_id(index)
            .map_err(|_| PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?;
        let SnapshotV2DeviceTransport::Pci(captured) = record.transport() else {
            return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::TransportPolicy);
        };
        if captured.sbdf() != sbdf
            || captured.bar_range() != bar_range
            || !valid_pci_record(captured, msi)
        {
            return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan);
        }
        planned.push(HvfSnapshotV2MultiBlockPciRecordPlan {
            key: record.key(),
            origin: captured.origin(),
            sbdf,
            bar_region_id,
            bar_range,
            route_count: routes_per_record,
        });
    }
    Ok(HvfSnapshotV2MultiBlockTransportPlan::Pci(
        HvfSnapshotV2MultiBlockPciPlan {
            host: Arm64FdtPciHost::from_address_plan(address_plan),
            msi,
            route_demand,
            records: planned,
        },
    ))
}

fn pci_route_demand(
    record_count: usize,
    available_routes: u32,
) -> Result<(usize, usize), PrepareHvfSnapshotV2MultiBlockPlatformPlanError> {
    if record_count > PCI_ENDPOINT_SLOT_COUNT {
        return Err(
            PrepareHvfSnapshotV2MultiBlockPlatformPlanError::PciCapacity {
                count: record_count,
                maximum: PCI_ENDPOINT_SLOT_COUNT,
            },
        );
    }
    let routes_per_record = VIRTIO_BLOCK_QUEUE_SIZES
        .len()
        .checked_add(1)
        .ok_or(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?;
    let route_demand = record_count
        .checked_mul(routes_per_record)
        .ok_or(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?;
    if route_demand
        > usize::try_from(available_routes)
            .map_err(|_| PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan);
    }
    Ok((routes_per_record, route_demand))
}

fn valid_pci_record(state: &SnapshotV2PciDeviceState, msi: HvfGicMsiMetadata) -> bool {
    state.phase() == VirtioPciEndpointPhase::Active
        && state.bar_index() == VIRTIO_PCI_CAPABILITY_BAR_INDEX
        && state.bar_address_space() == PciBarAddressSpace::Memory64
        && state.bar_prefetchable() == PciBarPrefetchable::No
        && state
            .writable_bytes()
            .iter()
            .map(|byte| byte.offset())
            .eq(PCI_GENERIC_WRITABLE_OFFSETS)
        && state
            .bar_probes()
            .iter()
            .map(|probe| probe.index())
            .eq([0, 1])
        && valid_pci_msix(state.msix())
        && pci_msix_routes_match_gic(state.msix(), msi)
}

fn valid_pci_msix(state: &SnapshotV2PciMsixState) -> bool {
    state.entries().len() == VIRTIO_BLOCK_QUEUE_SIZES.len() + 1
        && state.entries().len() <= VIRTIO_PCI_MAX_MSIX_VECTORS
        && state.pending_words().len() == 1
        && state.queue_vectors().len() == VIRTIO_BLOCK_QUEUE_SIZES.len()
        && state
            .pending_words()
            .first()
            .copied()
            .is_some_and(|pending| pending & !0b11 == 0)
        && valid_pci_vector(state.config_vector(), state.entries().len())
        && state
            .queue_vectors()
            .iter()
            .copied()
            .all(|vector| valid_pci_vector(vector, state.entries().len()))
        && state
            .entries()
            .iter()
            .all(|entry| entry.vector_control() & !1 == 0)
}

fn valid_pci_vector(vector: u16, count: usize) -> bool {
    vector == VIRTIO_PCI_NO_VECTOR || usize::from(vector) < count
}

fn retry_precedes(
    candidate: StorageRetryState,
    current: Option<HvfSnapshotV2MultiBlockRetryPlan>,
) -> bool {
    let Some(candidate_rank) = retry_rank(candidate) else {
        return false;
    };
    current
        .and_then(|current| retry_rank(current.retry))
        .is_none_or(|current_rank| candidate_rank < current_rank)
}

const fn retry_rank(retry: StorageRetryState) -> Option<(u8, u64)> {
    match retry {
        StorageRetryState::None => None,
        StorageRetryState::Immediate => Some((0, 0)),
        StorageRetryState::After { remaining_nanos } => Some((1, remaining_nanos)),
    }
}

fn command_line_has_root_argument(command_line: &str) -> bool {
    command_line
        .split_once(" -- ")
        .map_or(command_line, |(kernel, _)| kernel)
        .split_ascii_whitespace()
        .any(|argument| argument.starts_with("root="))
}

fn try_clone(value: &str) -> Result<String, PrepareHvfSnapshotV2MultiBlockPlatformPlanError> {
    let mut cloned = String::new();
    cloned
        .try_reserve_exact(value.len())
        .map_err(|_| PrepareHvfSnapshotV2MultiBlockPlatformPlanError::Allocation)?;
    cloned.push_str(value);
    Ok(cloned)
}

trait PlatformPlanReserve {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), PrepareHvfSnapshotV2MultiBlockPlatformPlanError>;
}

struct SystemPlatformPlanReserve;

impl PlatformPlanReserve for SystemPlatformPlanReserve {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), PrepareHvfSnapshotV2MultiBlockPlatformPlanError> {
        values
            .try_reserve_exact(additional)
            .map_err(|_| PrepareHvfSnapshotV2MultiBlockPlatformPlanError::Allocation)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs::{self, OpenOptions};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    use bangbang_runtime::block::{
        BlockFileBacking, DriveConfigInput, DriveIoEngine, PreparedBlockDevice,
        VIRTIO_BLOCK_DEVICE_ID,
    };
    use bangbang_runtime::memory::{GuestMemory, GuestMemoryLayout};
    use bangbang_runtime::snapshot_device::SnapshotV1PlatformDeviceMetadata;
    use bangbang_runtime::snapshot_device_v2_5::{
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2MultiBlockDeviceGraph,
        SnapshotV2MultiBlockRestorePlan,
    };
    use bangbang_runtime::storage_capture::{
        CaptureReadyBlockDeviceState, StorageMmioTransportState, StorageTransportState,
    };
    use bangbang_runtime::virtio_mmio::{
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioRegisterHandler,
    };

    use crate::snapshot_bundle::HvfSnapshotV1CompatibilityState;
    use crate::snapshot_v2::{
        HvfSnapshotV2FdtState, HvfSnapshotV2GlobalState, HvfSnapshotV2MachineState,
        HvfSnapshotV2TimeState,
    };

    use super::*;

    static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

    struct TempBacking {
        path: PathBuf,
    }

    impl TempBacking {
        fn new(name: &str, len: u64) -> Self {
            let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-profile-2-platform-{name}-{}-{sequence}",
                std::process::id()
            ));
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("test backing should create");
            file.set_len(len).expect("test backing should resize");
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

    struct BundleFixture {
        bundle: PreparedSnapshotV2MultiBlockBundle,
        _files: Vec<TempBacking>,
    }

    fn fixture_graph(
        transport: SnapshotV2DeviceTransportKind,
        with_root: bool,
    ) -> SnapshotV2MultiBlockDeviceGraph {
        let hex = match (transport, with_root) {
            (SnapshotV2DeviceTransportKind::Mmio, false) => {
                include_str!("../../runtime/src/snapshot_device_v2_5/fixtures/rootless-mmio.hex")
            }
            (SnapshotV2DeviceTransportKind::Mmio, true) => {
                include_str!("../../runtime/src/snapshot_device_v2_5/fixtures/root-mmio.hex")
            }
            (SnapshotV2DeviceTransportKind::Pci, false) => {
                include_str!("../../runtime/src/snapshot_device_v2_5/fixtures/rootless-pci.hex")
            }
            (SnapshotV2DeviceTransportKind::Pci, true) => {
                include_str!("../../runtime/src/snapshot_device_v2_5/fixtures/root-pci.hex")
            }
        };
        SnapshotV2MultiBlockDeviceGraph::decode(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &fixture_bytes(hex),
        )
        .expect("profile-2 fixture graph should decode")
    }

    fn fixture_bytes(hex: &str) -> Vec<u8> {
        hex.trim()
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let pair = std::str::from_utf8(pair).expect("fixture hex should be UTF-8");
                u8::from_str_radix(pair, 16).expect("fixture hex should decode")
            })
            .collect()
    }

    fn mutate_unique_window(
        bytes: &mut [u8],
        needle: &[u8],
        replacement_offset: usize,
        replacement: &[u8],
    ) {
        let position = {
            let mut matches = bytes
                .windows(needle.len())
                .enumerate()
                .filter_map(|(index, window)| (window == needle).then_some(index));
            let position = matches.next().expect("mutation window should exist");
            assert!(matches.next().is_none(), "mutation window should be unique");
            position
        };
        let start = position
            .checked_add(replacement_offset)
            .expect("replacement start should fit");
        let end = start
            .checked_add(replacement.len())
            .expect("replacement end should fit");
        bytes[start..end].copy_from_slice(replacement);
    }

    fn mutate_first_pci_message_data(
        graph: &SnapshotV2MultiBlockDeviceGraph,
        message_data: u32,
    ) -> SnapshotV2MultiBlockDeviceGraph {
        let SnapshotV2DeviceTransport::Pci(pci) = graph.records()[0].transport() else {
            panic!("fixture record should use PCI");
        };
        let entry = pci
            .msix()
            .entries()
            .first()
            .expect("fixture MSI-X entry should exist");
        let mut encoded_entry = Vec::with_capacity(16);
        encoded_entry.extend_from_slice(&entry.message_address_low().to_le_bytes());
        encoded_entry.extend_from_slice(&entry.message_address_high().to_le_bytes());
        encoded_entry.extend_from_slice(&entry.message_data().to_le_bytes());
        encoded_entry.extend_from_slice(&entry.vector_control().to_le_bytes());

        let mut bytes = graph
            .encode(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION)
            .expect("fixture graph should encode");
        mutate_unique_window(&mut bytes, &encoded_entry, 8, &message_data.to_le_bytes());
        SnapshotV2MultiBlockDeviceGraph::decode(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes,
        )
        .expect("mutated MSI-X graph should remain graph-valid")
    }

    fn mutate_second_pci_bar(
        graph: &SnapshotV2MultiBlockDeviceGraph,
    ) -> SnapshotV2MultiBlockDeviceGraph {
        let SnapshotV2DeviceTransport::Pci(pci) = graph.records()[1].transport() else {
            panic!("fixture record should use PCI");
        };
        let range = pci.bar_range();
        let mut encoded_range = Vec::with_capacity(16);
        encoded_range.extend_from_slice(&range.start().raw_value().to_le_bytes());
        encoded_range.extend_from_slice(&range.size().to_le_bytes());
        let replacement = range
            .start()
            .checked_add(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .expect("replacement BAR start should fit")
            .raw_value()
            .to_le_bytes();

        let mut bytes = graph
            .encode(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION)
            .expect("fixture graph should encode");
        mutate_unique_window(&mut bytes, &encoded_range, 0, &replacement);
        SnapshotV2MultiBlockDeviceGraph::decode(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes,
        )
        .expect("mutated BAR graph should remain graph-valid")
    }

    fn memory_for(graph: &SnapshotV2MultiBlockDeviceGraph) -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), 0x80_0000)
                .expect("test memory range should validate"),
        ])
        .expect("test memory layout should validate");
        let mut memory = GuestMemory::allocate(&layout).expect("test memory should allocate");
        for record in graph.records() {
            let Some(cursor) = record.block().continuation().active_queue() else {
                continue;
            };
            let queue = record
                .virtio()
                .queues()
                .first()
                .expect("fixture queue should exist");
            let available_index =
                if record.block().continuation().retry() == StorageRetryState::None {
                    cursor.next_available()
                } else {
                    cursor.next_available().wrapping_add(1)
                };
            memory
                .write_slice(
                    &available_index.to_le_bytes(),
                    GuestAddress::new(queue.driver_ring().raw_value() + 2),
                )
                .expect("available cursor should write");
            memory
                .write_slice(
                    &cursor.next_used().to_le_bytes(),
                    GuestAddress::new(queue.device_ring().raw_value() + 2),
                )
                .expect("used cursor should write");
        }
        memory
    }

    fn bundle_from_graph(graph: SnapshotV2MultiBlockDeviceGraph) -> BundleFixture {
        let memory = memory_for(&graph);
        let configs = graph
            .project_drive_configs()
            .expect("fixture configs should project");
        let files: Vec<_> = graph
            .records()
            .iter()
            .enumerate()
            .map(|(index, record)| {
                TempBacking::new(&format!("bundle-{index}"), record.block().backing_bytes())
            })
            .collect();
        let backings = files
            .iter()
            .zip(graph.records())
            .map(|(file, record)| {
                BlockFileBacking::open_snapshot(file.path(), record.config().is_read_only())
                    .expect("snapshot backing should open")
                    .0
            })
            .collect();
        let bundle = SnapshotV2MultiBlockRestorePlan::prepare(graph, &memory, Instant::now())
            .expect("fixture restore plan should prepare")
            .prepare_backings(configs, backings)
            .expect("fixture bundle should prepare");
        BundleFixture {
            bundle,
            _files: files,
        }
    }

    fn captured_mmio_graph(
        record_count: usize,
        root_read_only: Option<bool>,
        root_partuuid: Option<&str>,
        mutated_region_index: Option<usize>,
        mutated_interrupt_index: Option<usize>,
    ) -> SnapshotV2MultiBlockDeviceGraph {
        let mut files = Vec::new();
        let mut states = Vec::new();
        for index in 0..record_count {
            let file = TempBacking::new(&format!("capture-{index}"), 4096);
            let is_root = index == 0 && root_read_only.is_some();
            let drive_id = if is_root {
                "rootfs".to_string()
            } else {
                format!("data_{index}")
            };
            let mut input = DriveConfigInput::new(drive_id.clone(), drive_id, file.path(), is_root)
                .with_is_read_only(if is_root {
                    root_read_only.expect("root mode should exist")
                } else {
                    true
                })
                .with_io_engine(DriveIoEngine::Sync);
            if is_root && let Some(partuuid) = root_partuuid {
                input = input.with_partuuid(partuuid);
            }
            let config = input.validate().expect("capture config should validate");
            let prepared = PreparedBlockDevice::from_config_with_backing(&config, None)
                .expect("capture block should prepare");
            let (_, _, config_space, device) = prepared.into_parts();
            let handler = VirtioMmioRegisterHandler::with_device_config_and_activation(
                VIRTIO_BLOCK_DEVICE_ID,
                config_space.available_features(),
                &VIRTIO_BLOCK_QUEUE_SIZES,
                config_space,
                device,
            )
            .expect("capture handler should construct");
            let captured = handler
                .capture_block_device_state_at(&config, Instant::now())
                .expect("block state should capture");
            let slot = if mutated_region_index == Some(index) {
                index.checked_add(1).expect("mutated slot should fit")
            } else {
                index
            };
            let interrupt_slot = if mutated_interrupt_index == Some(index) {
                index
                    .checked_add(1)
                    .expect("mutated interrupt slot should fit")
            } else {
                index
            };
            let offset = u64::try_from(slot)
                .expect("slot should fit")
                .checked_mul(VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
                .expect("MMIO offset should fit");
            let region = MmioRegion::new(
                MmioRegionId::new(
                    100_u64
                        .checked_add(u64::try_from(index).expect("index should fit"))
                        .expect("region ID should fit"),
                ),
                GuestAddress::new(0xd000_0000 + offset),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("capture region should validate");
            states.push(CaptureReadyBlockDeviceState::new(
                config,
                StorageTransportState::Mmio(StorageMmioTransportState::new(
                    region,
                    GuestInterruptLine::new(
                        32_u32
                            .checked_add(
                                u32::try_from(interrupt_slot).expect("interrupt slot should fit"),
                            )
                            .expect("interrupt should fit"),
                    )
                    .expect("capture interrupt should validate"),
                    handler.transport_state(),
                )),
                StorageRetryState::None,
                captured,
            ));
            files.push(file);
        }
        let graph = SnapshotV2MultiBlockDeviceGraph::from_capture_ready_blocks(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &states,
        )
        .expect("captured profile-2 graph should validate");
        drop(files);
        graph
    }

    fn product_mmio_platform(record_count: usize) -> HvfSnapshotV2PlatformState {
        let (platform, root, _process) =
            crate::snapshot_v2_platform::tests::mmio_root_plan_fixture();
        drop(root);
        rebuild_product_platform(platform, record_count, None, true)
    }

    pub(crate) fn mmio_fdt_plan_fixture() -> (
        HvfSnapshotV2PlatformState,
        HvfSnapshotV2MultiBlockPlatformPlan,
    ) {
        let fixture = bundle_from_graph(fixture_graph(SnapshotV2DeviceTransportKind::Mmio, false));
        let platform = product_mmio_platform(fixture.bundle.records().len());
        let plan = prepare_hvf_snapshot_v2_multi_block_platform_plan(
            &platform,
            &fixture.bundle,
            mmio_process(),
        )
        .expect("multi-block MMIO FDT plan should validate");
        (platform, plan)
    }

    fn product_pci_platform() -> HvfSnapshotV2PlatformState {
        let (platform, root, _process) =
            crate::snapshot_v2_platform::tests::pci_root_plan_fixture();
        drop(root);
        rebuild_product_platform(platform, 0, None, false)
    }

    fn rebuild_product_platform(
        platform: HvfSnapshotV2PlatformState,
        mmio_record_count: usize,
        exact_spi_count: Option<u32>,
        reline_time: bool,
    ) -> HvfSnapshotV2PlatformState {
        let (memory, machine, global, topology, vcpus, time) = platform.into_parts();
        let (compatibility, gic_device) = global.into_parts();
        let mut gic = compatibility.gic_metadata();
        if gic.msi.is_none() {
            let required = u32::try_from(mmio_record_count)
                .expect("record count should fit")
                .checked_add(3)
                .expect("SPI count should fit");
            gic.spi_interrupt_range.count =
                exact_spi_count.unwrap_or(gic.spi_interrupt_range.count.max(required));
        }
        let compatibility = HvfSnapshotV1CompatibilityState::new(
            compatibility.identification(),
            compatibility.optional_sve_sme_identification(),
            compatibility.cache_manifest(),
            compatibility.primary_mpidr(),
            gic,
            compatibility.rtc_mmio_layout(),
        );
        let global = HvfSnapshotV2GlobalState::try_new(compatibility, gic_device)
            .expect("rebuilt global state should validate");

        let (rtc, mut vmgenid, mut vmclock, vmclock_abi, pvtime) = time.into_parts();
        if reline_time {
            let mut allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
                .expect("fixture GIC allocator should validate");
            for _ in 0..mmio_record_count {
                allocator
                    .allocate()
                    .expect("block interrupt should allocate");
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
            vmgenid = SnapshotV1PlatformDeviceMetadata::new(
                vmgenid.range(),
                vmgenid.fdt_region(),
                vmgenid_interrupt,
            );
            vmclock = SnapshotV1PlatformDeviceMetadata::new(
                vmclock.range(),
                vmclock.fdt_region(),
                vmclock_interrupt,
            );
        }
        let time = HvfSnapshotV2TimeState::try_new(rtc, vmgenid, vmclock, vmclock_abi, pvtime)
            .expect("rebuilt time state should validate");

        let old_fdt = machine.fdt();
        let fdt = HvfSnapshotV2FdtState::try_new_product_process_profile(
            old_fdt.address(),
            usize::try_from(old_fdt.size()).expect("FDT size should fit"),
            old_fdt.checksum(),
        )
        .expect("product FDT profile should validate");
        let machine = HvfSnapshotV2MachineState::try_new(
            machine.machine(),
            machine.boot().clone(),
            fdt,
            machine.cpu_template().cloned(),
        )
        .expect("rebuilt machine state should validate");
        HvfSnapshotV2PlatformState::try_new(memory, machine, global, topology, vcpus, time)
            .expect("rebuilt platform should validate")
    }

    fn mmio_process() -> HvfSnapshotV2MultiBlockProcessConfig {
        HvfSnapshotV2MultiBlockProcessConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0xd000_0000), MmioRegionId::new(100)),
            false,
        )
    }

    fn pci_process() -> HvfSnapshotV2MultiBlockProcessConfig {
        HvfSnapshotV2MultiBlockProcessConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0xd000_0000), MmioRegionId::new(100)),
            true,
        )
    }

    #[test]
    fn rootless_and_rooted_mmio_vectors_project_exact_process_and_fdt_state() {
        let rootless = bundle_from_graph(fixture_graph(SnapshotV2DeviceTransportKind::Mmio, false));
        let platform = product_mmio_platform(rootless.bundle.records().len());
        let rootless_plan = prepare_hvf_snapshot_v2_multi_block_platform_plan(
            &platform,
            &rootless.bundle,
            mmio_process(),
        )
        .expect("rootless MMIO plan should validate");
        assert_eq!(rootless_plan.root_key(), None);
        assert!(rootless_plan.command_line().contains("pci=off"));
        assert!(!rootless_plan.command_line().contains("root="));
        assert_eq!(rootless_plan.metrics_ids(), ["data_0", "data_1"]);
        assert_eq!(
            rootless_plan
                .earliest_retry()
                .expect("fixture retry should project")
                .retry(),
            StorageRetryState::After {
                remaining_nanos: 99
            }
        );
        let mmio = rootless_plan
            .transport()
            .mmio_records()
            .expect("MMIO records should exist");
        assert_eq!(mmio.len(), rootless.bundle.records().len());
        for (resource, record) in mmio.iter().copied().zip(rootless.bundle.records()) {
            let SnapshotV2DeviceTransport::Mmio(captured) = record.transport() else {
                panic!("fixture record should use MMIO");
            };
            assert_eq!(resource.key(), record.key());
            assert_eq!(resource.region(), captured.region());
            assert_eq!(resource.interrupt_line(), captured.interrupt_line());
            assert_eq!(
                resource.fdt_device().region.base,
                captured.region().range().start().raw_value()
            );
        }

        let rooted = bundle_from_graph(fixture_graph(SnapshotV2DeviceTransportKind::Mmio, true));
        let platform = product_mmio_platform(rooted.bundle.records().len());
        let rooted_plan = prepare_hvf_snapshot_v2_multi_block_platform_plan(
            &platform,
            &rooted.bundle,
            mmio_process(),
        )
        .expect("rooted MMIO plan should validate");
        assert_eq!(
            rooted_plan.root_key(),
            Some(rooted.bundle.records()[0].key())
        );
        assert!(
            rooted_plan
                .command_line()
                .contains("root=PARTUUID=1111-2222 ro")
        );

        let writable = bundle_from_graph(captured_mmio_graph(1, Some(false), None, None, None));
        let platform = product_mmio_platform(1);
        let writable_plan = prepare_hvf_snapshot_v2_multi_block_platform_plan(
            &platform,
            &writable.bundle,
            mmio_process(),
        )
        .expect("writable root MMIO plan should validate");
        assert!(writable_plan.command_line().contains("root=/dev/vda rw"));
    }

    #[test]
    fn pci_vector_projects_one_host_and_exact_canonical_endpoints() {
        let rootless = bundle_from_graph(fixture_graph(SnapshotV2DeviceTransportKind::Pci, false));
        let platform = product_pci_platform();
        let rootless_plan = prepare_hvf_snapshot_v2_multi_block_platform_plan(
            &platform,
            &rootless.bundle,
            pci_process(),
        )
        .expect("rootless PCI plan should validate");
        assert_eq!(rootless_plan.root_key(), None);
        assert!(!rootless_plan.command_line().contains("root="));
        assert!(!rootless_plan.command_line().contains("pci=off"));
        assert_eq!(rootless_plan.metrics_ids(), ["data_0", "data_1"]);

        let fixture = bundle_from_graph(fixture_graph(SnapshotV2DeviceTransportKind::Pci, true));
        let plan = prepare_hvf_snapshot_v2_multi_block_platform_plan(
            &platform,
            &fixture.bundle,
            pci_process(),
        )
        .expect("PCI plan should validate");
        assert!(plan.command_line().contains("root=PARTUUID=1111-2222 ro"));
        assert!(!plan.command_line().contains("pci=off"));
        assert_eq!(plan.transport().mmio_records(), None);
        let pci = plan.transport().pci().expect("PCI plan should exist");
        assert_eq!(pci.records().len(), fixture.bundle.records().len());
        assert_eq!(pci.route_demand(), fixture.bundle.records().len() * 2);
        assert_eq!(pci.records()[1].origin(), StorageDeviceOrigin::Runtime);
        for (index, (resource, record)) in pci
            .records()
            .iter()
            .copied()
            .zip(fixture.bundle.records())
            .enumerate()
        {
            let SnapshotV2DeviceTransport::Pci(captured) = record.transport() else {
                panic!("fixture record should use PCI");
            };
            assert_eq!(resource.key(), record.key());
            assert_eq!(resource.sbdf(), captured.sbdf());
            assert_eq!(resource.bar_range(), captured.bar_range());
            assert_eq!(
                resource.bar_region_id(),
                pci_data_region_id(index).expect("region ID should project")
            );
            assert_eq!(resource.route_count(), 2);
        }
        assert_eq!(
            pci.host(),
            Arm64FdtPciHost::from_address_plan(
                Arm64PciAddressPlan::firecracker_v1_16().expect("PCI address plan should validate")
            )
        );
    }

    #[test]
    fn pci_rejects_out_of_range_msi_routes_and_noncanonical_bar_placement() {
        let platform = product_pci_platform();
        let msi = platform
            .global()
            .compatibility()
            .gic_metadata()
            .msi
            .expect("PCI fixture should carry MSI metadata");
        let range_end = msi
            .interrupt_range
            .base
            .checked_add(msi.interrupt_range.count)
            .expect("MSI range end should fit");
        let graph = fixture_graph(SnapshotV2DeviceTransportKind::Pci, true);
        let bad_msi = bundle_from_graph(mutate_first_pci_message_data(&graph, range_end));
        assert!(matches!(
            prepare_hvf_snapshot_v2_multi_block_platform_plan(
                &platform,
                &bad_msi.bundle,
                pci_process(),
            ),
            Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)
        ));

        let bad_bar = bundle_from_graph(mutate_second_pci_bar(&graph));
        assert!(matches!(
            prepare_hvf_snapshot_v2_multi_block_platform_plan(
                &platform,
                &bad_bar.bundle,
                pci_process(),
            ),
            Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)
        ));
    }

    #[test]
    fn mmio_accepts_minimum_and_profile_maximum_vectors() {
        for count in [
            1,
            usize::from(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_RECORDS),
        ] {
            let fixture = bundle_from_graph(captured_mmio_graph(count, None, None, None, None));
            let platform = product_mmio_platform(count);
            let plan = prepare_hvf_snapshot_v2_multi_block_platform_plan(
                &platform,
                &fixture.bundle,
                mmio_process(),
            )
            .expect("boundary MMIO plan should validate");
            assert_eq!(plan.transport().len(), count);
            assert_eq!(plan.metrics_ids().len(), count);
            assert_eq!(plan.retries().len(), count);
        }
    }

    #[test]
    fn pci_capacity_and_route_boundaries_are_checked_before_endpoint_planning() {
        assert_eq!(
            pci_route_demand(PCI_ENDPOINT_SLOT_COUNT, 93).expect("maximum PCI demand should fit"),
            (2, PCI_ENDPOINT_SLOT_COUNT * 2)
        );
        assert!(matches!(
            pci_route_demand(PCI_ENDPOINT_SLOT_COUNT + 1, 93),
            Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::PciCapacity {
                count,
                maximum,
            }) if count == PCI_ENDPOINT_SLOT_COUNT + 1 && maximum == PCI_ENDPOINT_SLOT_COUNT
        ));
        assert!(matches!(
            pci_route_demand(PCI_ENDPOINT_SLOT_COUNT, 61),
            Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)
        ));
    }

    #[test]
    fn mmio_rejects_noncanonical_placement_and_exhausted_shell_interrupts() {
        let mutated = bundle_from_graph(captured_mmio_graph(2, None, None, Some(1), None));
        let platform = product_mmio_platform(2);
        assert!(matches!(
            prepare_hvf_snapshot_v2_multi_block_platform_plan(
                &platform,
                &mutated.bundle,
                mmio_process(),
            ),
            Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)
        ));

        let bad_interrupt = bundle_from_graph(captured_mmio_graph(2, None, None, None, Some(1)));
        assert!(matches!(
            prepare_hvf_snapshot_v2_multi_block_platform_plan(
                &platform,
                &bad_interrupt.bundle,
                mmio_process(),
            ),
            Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)
        ));

        let canonical = bundle_from_graph(captured_mmio_graph(2, None, None, None, None));
        let shifted_process = HvfSnapshotV2MultiBlockProcessConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0xd000_1000), MmioRegionId::new(100)),
            false,
        );
        assert!(matches!(
            prepare_hvf_snapshot_v2_multi_block_platform_plan(
                &platform,
                &canonical.bundle,
                shifted_process,
            ),
            Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::ResourcePlan)
        ));

        let (base, root, _process) = crate::snapshot_v2_platform::tests::mmio_root_plan_fixture();
        drop(root);
        let exhausted = rebuild_product_platform(base, 0, Some(4), false);
        assert!(matches!(
            prepare_hvf_snapshot_v2_multi_block_platform_plan(
                &exhausted,
                &canonical.bundle,
                mmio_process(),
            ),
            Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::Interrupt(
                _
            ))
        ));
    }

    #[test]
    fn mmio_rejects_guest_memory_and_fixed_shell_overlaps() {
        let platform = product_mmio_platform(1);
        let gic = platform.global().compatibility().gic_metadata();
        let guest_memory = MmioRegion::new(
            MmioRegionId::new(500),
            platform.memory().extents()[0].range().start(),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("guest-memory-overlapping MMIO region should be structurally valid");
        assert!(
            mmio_region_conflicts_with_platform(&platform, guest_memory, &gic)
                .expect("guest memory conflict should be checked")
        );

        let serial = MmioRegion::new(
            MmioRegionId::new(501),
            PROCESS_SERIAL_MMIO_BASE,
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("serial-overlapping MMIO region should be structurally valid");
        assert!(
            mmio_region_conflicts_with_platform(&platform, serial, &gic)
                .expect("serial conflict should be checked")
        );
    }

    #[test]
    fn queue_conflict_validation_covers_fdt_identity_and_time_records() {
        let platform = product_mmio_platform(1);
        let fdt = platform.machine().fdt();
        let pvtime = platform
            .time()
            .pvtime_vcpus()
            .first()
            .expect("fixture should contain PVTime");
        for range in [
            GuestMemoryRange::new(fdt.address(), 1).expect("FDT range should validate"),
            GuestMemoryRange::new(platform.time().vmgenid().range().start(), 1)
                .expect("VMGenID range should validate"),
            GuestMemoryRange::new(platform.time().vmclock().range().start(), 1)
                .expect("VMClock range should validate"),
            GuestMemoryRange::new(pvtime.record_ipa(), 1).expect("PVTime range should validate"),
        ] {
            assert!(
                queue_ranges_conflict_with_platform(&platform, Some([range; 3]))
                    .expect("queue conflict check should complete")
            );
        }
        let ordinary =
            GuestMemoryRange::new(GuestAddress::new(0x10_0000), 1).expect("range should validate");
        assert!(
            !queue_ranges_conflict_with_platform(&platform, Some([ordinary; 3]))
                .expect("ordinary queue check should complete")
        );
    }

    struct FailingReserve {
        calls: usize,
        fail_at: usize,
    }

    impl PlatformPlanReserve for FailingReserve {
        fn reserve<T>(
            &mut self,
            values: &mut Vec<T>,
            additional: usize,
        ) -> Result<(), PrepareHvfSnapshotV2MultiBlockPlatformPlanError> {
            let call = self.calls;
            self.calls = self.calls.saturating_add(1);
            if call == self.fail_at {
                Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::Allocation)
            } else {
                values
                    .try_reserve_exact(additional)
                    .map_err(|_| PrepareHvfSnapshotV2MultiBlockPlatformPlanError::Allocation)
            }
        }
    }

    #[test]
    fn allocation_failure_is_explicit_and_debug_output_is_redacted() {
        let fixture = bundle_from_graph(fixture_graph(SnapshotV2DeviceTransportKind::Mmio, true));
        let platform = product_mmio_platform(fixture.bundle.records().len());
        for fail_at in 0..3 {
            assert!(matches!(
                prepare_hvf_snapshot_v2_multi_block_platform_plan_with(
                    &platform,
                    &fixture.bundle,
                    mmio_process(),
                    &mut FailingReserve { calls: 0, fail_at },
                ),
                Err(PrepareHvfSnapshotV2MultiBlockPlatformPlanError::Allocation)
            ));
        }

        let plan = prepare_hvf_snapshot_v2_multi_block_platform_plan(
            &platform,
            &fixture.bundle,
            mmio_process(),
        )
        .expect("redaction fixture should plan");
        let debug = format!("{plan:?}");
        for secret in ["rootfs", "1111-2222", "root=", "logical-selector"] {
            assert!(!debug.contains(secret));
        }
        let error = PrepareHvfSnapshotV2MultiBlockPlatformPlanError::PciCapacity {
            count: 32,
            maximum: 31,
        };
        let debug = format!("{error:?}");
        assert!(!debug.contains("32"));
        assert!(!debug.contains("31"));
        assert_eq!(
            format!("{:?}", mmio_process()),
            "HvfSnapshotV2MultiBlockProcessConfig { state: \"<redacted>\" }"
        );
    }

    #[test]
    fn retry_ranking_is_host_time_free_and_stable_under_ties() {
        let key = fixture_graph(SnapshotV2DeviceTransportKind::Mmio, false).records()[0].key();
        let current = HvfSnapshotV2MultiBlockRetryPlan {
            key,
            retry: StorageRetryState::After {
                remaining_nanos: 10,
            },
        };
        assert!(retry_precedes(StorageRetryState::Immediate, Some(current)));
        assert!(retry_precedes(
            StorageRetryState::After { remaining_nanos: 9 },
            Some(current)
        ));
        assert!(!retry_precedes(
            StorageRetryState::After {
                remaining_nanos: 10
            },
            Some(current)
        ));
        assert!(!retry_precedes(StorageRetryState::None, Some(current)));
    }
}
