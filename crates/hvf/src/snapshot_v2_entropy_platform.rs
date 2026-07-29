//! Host-free exact native-v2 2.8 PCI entropy platform planning.

use std::fmt;

use bangbang_runtime::block::VIRTIO_BLOCK_QUEUE_SIZES;
use bangbang_runtime::entropy::VIRTIO_RNG_QUEUE_SIZES;
use bangbang_runtime::fdt::{ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET, Arm64FdtPciHost};
use bangbang_runtime::memory::GuestMemoryRange;
use bangbang_runtime::mmio::MmioRegionId;
use bangbang_runtime::pci::{Arm64PciAddressPlan, PciSbdf};
use bangbang_runtime::pmem::VIRTIO_PMEM_QUEUE_SIZES;
use bangbang_runtime::rtc::RtcMmioLayout;
use bangbang_runtime::snapshot_device_v2::{
    SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind,
};
use bangbang_runtime::snapshot_device_v2_6::PreparedSnapshotV2StorageBundle;
use bangbang_runtime::snapshot_entropy_v2_8::{
    PreparedSnapshotV2EntropyTransport, SnapshotV2EntropyRestorePlan,
};
use bangbang_runtime::storage_capture::StorageDeviceOrigin;
use bangbang_runtime::virtio_pci::{
    VIRTIO_PCI_NO_VECTOR, VirtioPciEndpointPhase, VirtioPciMsixState,
};

use crate::snapshot_v2::HvfSnapshotV2PlatformState;
use crate::snapshot_v2_multi_block_platform::{
    snapshot_v2_pci_endpoint_placement, snapshot_v2_pci_endpoint_route_count,
};
use crate::snapshot_v2_platform::{PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID};
use crate::snapshot_v2_storage_platform::{
    HvfSnapshotV2StoragePciPlatformPlan, PrepareHvfSnapshotV2StoragePciPlatformPlanError,
    prepare_hvf_snapshot_v2_storage_pci_platform_plan_for_entropy,
    queue_ranges_conflict_with_pci_platform, register_active_pci_routes,
};
use crate::startup::{
    PCI_ENDPOINT_SLOT_COUNT, pci_entropy_restore_gic_msi_configuration,
    pci_root_restore_gic_msi_configuration,
};

const REDACTED: &str = "<redacted>";

/// Exact destination PCI placement reserved for restored entropy.
#[doc(hidden)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2EntropyPciEndpointPlan {
    preceding_endpoint_count: usize,
    sbdf: PciSbdf,
    bar_region_id: MmioRegionId,
    bar_range: GuestMemoryRange,
    route_count: usize,
    msi_interrupt_count: u32,
}

impl HvfSnapshotV2EntropyPciEndpointPlan {
    /// Returns how many storage endpoints must already be published.
    pub const fn preceding_endpoint_count(self) -> usize {
        self.preceding_endpoint_count
    }

    /// Returns the exact retained PCI function.
    pub const fn sbdf(self) -> PciSbdf {
        self.sbdf
    }

    /// Returns the exact dispatcher identity for the capability BAR.
    pub const fn bar_region_id(self) -> MmioRegionId {
        self.bar_region_id
    }

    /// Returns the exact retained capability BAR.
    pub const fn bar_range(self) -> GuestMemoryRange {
        self.bar_range
    }

    /// Returns the configuration-plus-queue MSI-X route demand.
    pub const fn route_count(self) -> usize {
        self.route_count
    }

    pub(crate) const fn msi_interrupt_count(self) -> u32 {
        self.msi_interrupt_count
    }
}

impl fmt::Debug for HvfSnapshotV2EntropyPciEndpointPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2EntropyPciEndpointPlan")
            .field("preceding_endpoint_count", &self.preceding_endpoint_count)
            .field("state", &REDACTED)
            .finish()
    }
}

/// One combined exact-2.8 storage-then-entropy PCI proof.
#[doc(hidden)]
pub struct HvfSnapshotV2StorageEntropyPciPlatformPlan {
    storage: HvfSnapshotV2StoragePciPlatformPlan,
    entropy: HvfSnapshotV2EntropyPciEndpointPlan,
}

impl HvfSnapshotV2StorageEntropyPciPlatformPlan {
    /// Returns the checked storage platform plan.
    pub const fn storage(&self) -> &HvfSnapshotV2StoragePciPlatformPlan {
        &self.storage
    }

    /// Returns the checked entropy endpoint plan.
    pub const fn entropy(&self) -> HvfSnapshotV2EntropyPciEndpointPlan {
        self.entropy
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        HvfSnapshotV2StoragePciPlatformPlan,
        HvfSnapshotV2EntropyPciEndpointPlan,
    ) {
        (self.storage, self.entropy)
    }
}

impl fmt::Debug for HvfSnapshotV2StorageEntropyPciPlatformPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2StorageEntropyPciPlatformPlan")
            .field("storage_endpoint_count", &self.storage.pci().record_count())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Redacted rejection from exact-2.8 PCI entropy platform planning.
#[doc(hidden)]
pub enum PrepareHvfSnapshotV2EntropyPciPlatformPlanError {
    /// The existing storage graph could not form its own canonical PCI plan.
    Storage(PrepareHvfSnapshotV2StoragePciPlatformPlanError),
    /// The entropy continuation does not select PCI.
    TransportPolicy,
    /// The entropy queue overlaps platform- or storage-owned memory.
    QueueConflict,
    /// A destination-only vector could not reserve its fallible inventory.
    Allocation,
    /// The combined storage-plus-entropy endpoint capacity is exhausted.
    PciCapacity { count: usize, maximum: usize },
    /// Retained placement, route, or destination resource state diverged.
    ResourcePlan,
}

impl fmt::Debug for PrepareHvfSnapshotV2EntropyPciPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self {
            Self::Storage(_) => "storage",
            Self::TransportPolicy => "transport-policy",
            Self::QueueConflict => "queue-conflict",
            Self::Allocation => "allocation",
            Self::PciCapacity { .. } => "pci-capacity",
            Self::ResourcePlan => "resource-plan",
        };
        formatter
            .debug_struct("PrepareHvfSnapshotV2EntropyPciPlatformPlanError")
            .field("category", &category)
            .field("state", &REDACTED)
            .finish()
    }
}

impl fmt::Display for PrepareHvfSnapshotV2EntropyPciPlatformPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Storage(_) => "native-v2 storage PCI platform planning failed",
            Self::TransportPolicy => "native-v2 entropy PCI transport policy is inconsistent",
            Self::QueueConflict => {
                "native-v2 entropy queue overlaps platform- or storage-owned memory"
            }
            Self::Allocation => "native-v2 entropy PCI platform allocation failed",
            Self::PciCapacity { .. } => {
                "native-v2 storage-plus-entropy PCI endpoint capacity is exceeded"
            }
            Self::ResourcePlan => {
                "native-v2 storage-plus-entropy PCI platform resources are inconsistent"
            }
        })
    }
}

impl std::error::Error for PrepareHvfSnapshotV2EntropyPciPlatformPlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(source) => Some(source),
            Self::TransportPolicy
            | Self::QueueConflict
            | Self::Allocation
            | Self::PciCapacity { .. }
            | Self::ResourcePlan => None,
        }
    }
}

/// Proves one serial-plus-entropy PCI product before live HVF construction.
#[doc(hidden)]
pub fn prepare_hvf_snapshot_v2_serial_entropy_pci_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    entropy: &SnapshotV2EntropyRestorePlan,
) -> Result<HvfSnapshotV2EntropyPciEndpointPlan, PrepareHvfSnapshotV2EntropyPciPlatformPlanError> {
    prepare_entropy_pci_endpoint_plan(
        platform,
        None,
        entropy,
        0,
        0,
        None,
        &mut SystemEntropyPciPlatformPlanReserve,
    )
}

/// Proves one storage-then-entropy PCI product before live HVF construction.
#[doc(hidden)]
pub fn prepare_hvf_snapshot_v2_storage_entropy_pci_platform_plan(
    platform: &HvfSnapshotV2PlatformState,
    bundle: &PreparedSnapshotV2StorageBundle,
    entropy: &SnapshotV2EntropyRestorePlan,
) -> Result<
    HvfSnapshotV2StorageEntropyPciPlatformPlan,
    PrepareHvfSnapshotV2EntropyPciPlatformPlanError,
> {
    let storage = prepare_hvf_snapshot_v2_storage_pci_platform_plan_for_entropy(platform, bundle)
        .map_err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::Storage)?;
    let preceding_endpoint_count = storage.pci().record_count();
    let entropy_plan = prepare_entropy_pci_endpoint_plan(
        platform,
        Some((bundle, &storage)),
        entropy,
        0,
        preceding_endpoint_count,
        None,
        &mut SystemEntropyPciPlatformPlanReserve,
    )?;
    Ok(HvfSnapshotV2StorageEntropyPciPlatformPlan {
        storage,
        entropy: entropy_plan,
    })
}

pub(crate) fn prepare_hvf_snapshot_v2_entropy_pci_platform_plan_with_prefix(
    platform: &HvfSnapshotV2PlatformState,
    storage: Option<(
        &PreparedSnapshotV2StorageBundle,
        &HvfSnapshotV2StoragePciPlatformPlan,
    )>,
    entropy: &SnapshotV2EntropyRestorePlan,
    storage_start_slot: usize,
    preceding_endpoint_count: usize,
    exact_msi_interrupt_count: u32,
) -> Result<HvfSnapshotV2EntropyPciEndpointPlan, PrepareHvfSnapshotV2EntropyPciPlatformPlanError> {
    prepare_entropy_pci_endpoint_plan(
        platform,
        storage,
        entropy,
        storage_start_slot,
        preceding_endpoint_count,
        Some(exact_msi_interrupt_count),
        &mut SystemEntropyPciPlatformPlanReserve,
    )
}

fn prepare_entropy_pci_endpoint_plan(
    platform: &HvfSnapshotV2PlatformState,
    storage: Option<(
        &PreparedSnapshotV2StorageBundle,
        &HvfSnapshotV2StoragePciPlatformPlan,
    )>,
    entropy: &SnapshotV2EntropyRestorePlan,
    storage_start_slot: usize,
    preceding_endpoint_count: usize,
    exact_msi_interrupt_count: Option<u32>,
    reserve: &mut impl EntropyPciPlatformPlanReserve,
) -> Result<HvfSnapshotV2EntropyPciEndpointPlan, PrepareHvfSnapshotV2EntropyPciPlatformPlanError> {
    if !platform.machine().fdt().is_product_process_profile()
        || platform.time().rtc_layout()
            != RtcMmioLayout::new(PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID)
    {
        return Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan);
    }
    if entropy.transport_kind() != SnapshotV2DeviceTransportKind::Pci {
        return Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::TransportPolicy);
    }
    let PreparedSnapshotV2EntropyTransport::Pci(entropy_transport) = entropy.transport() else {
        return Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::TransportPolicy);
    };
    let endpoint_count = preceding_endpoint_count
        .checked_add(1)
        .ok_or(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan)?;
    if endpoint_count > PCI_ENDPOINT_SLOT_COUNT {
        return Err(
            PrepareHvfSnapshotV2EntropyPciPlatformPlanError::PciCapacity {
                count: endpoint_count,
                maximum: PCI_ENDPOINT_SLOT_COUNT,
            },
        );
    }

    let gic = platform.global().compatibility().gic_metadata();
    let msi = gic
        .msi
        .ok_or(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan)?;
    let expected_msi = pci_root_restore_gic_msi_configuration()
        .map_err(|_| PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan)?;
    let expected_entropy_msi = pci_entropy_restore_gic_msi_configuration()
        .map_err(|_| PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan)?;
    let msi_profile_matches = exact_msi_interrupt_count.map_or_else(
        || {
            msi.interrupt_range.count == expected_msi.interrupt_count().get()
                || msi.interrupt_range.count == expected_entropy_msi.interrupt_count().get()
        },
        |expected| msi.interrupt_range.count == expected,
    );
    if !msi_profile_matches {
        return Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan);
    }
    let address_plan = Arm64PciAddressPlan::firecracker_v1_16()
        .map_err(|_| PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan)?;
    let placement = snapshot_v2_pci_endpoint_placement(address_plan, preceding_endpoint_count)
        .ok_or(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan)?;
    let route_count = snapshot_v2_pci_endpoint_route_count(VIRTIO_RNG_QUEUE_SIZES.len())
        .ok_or(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan)?;

    if entropy_transport.origin() != StorageDeviceOrigin::Startup
        || entropy_transport.sbdf() != placement.sbdf
        || entropy_transport.bar_range() != placement.bar_range
        || entropy_transport.retained().phase() != VirtioPciEndpointPhase::Active
    {
        return Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan);
    }
    if queue_ranges_conflict_with_pci_platform(platform, entropy.queue_ranges(), &gic, address_plan)
        .map_err(|_| PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::QueueConflict);
    }

    let storage_route_demand = storage.map_or(0, |(_, plan)| plan.pci().route_demand());
    let combined_route_demand = storage_route_demand
        .checked_add(route_count)
        .ok_or(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan)?;
    if combined_route_demand
        > usize::try_from(msi.interrupt_range.count)
            .map_err(|_| PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan)?
    {
        return Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan);
    }

    let mut active_routes = Vec::new();
    reserve.reserve(&mut active_routes, combined_route_demand)?;
    if let Some((bundle, storage_plan)) = storage {
        if storage_plan.pci().host() != Arm64FdtPciHost::from_address_plan(address_plan)
            || storage_plan.pci().msi() != msi
            || !validate_storage_prefix(
                bundle,
                storage_plan,
                address_plan,
                storage_start_slot,
                &mut active_routes,
            )
        {
            return Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan);
        }
        if entropy_queue_conflicts_with_storage(entropy.queue_ranges(), bundle) {
            return Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::QueueConflict);
        }
    }
    if !register_active_retained_pci_routes(
        entropy_transport.retained().msix_state(),
        msi,
        VIRTIO_RNG_QUEUE_SIZES.len(),
        route_count,
        &mut active_routes,
    ) {
        return Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan);
    }

    Ok(HvfSnapshotV2EntropyPciEndpointPlan {
        preceding_endpoint_count,
        sbdf: placement.sbdf,
        bar_region_id: placement.bar_region_id,
        bar_range: placement.bar_range,
        route_count,
        msi_interrupt_count: msi.interrupt_range.count,
    })
}

fn validate_storage_prefix(
    bundle: &PreparedSnapshotV2StorageBundle,
    plan: &HvfSnapshotV2StoragePciPlatformPlan,
    address_plan: Arm64PciAddressPlan,
    storage_start_slot: usize,
    active_routes: &mut Vec<(u64, u32)>,
) -> bool {
    let block_records = bundle
        .block_bundle()
        .map_or(&[][..], |block| block.records());
    let pmem_records = bundle.pmem_records();
    let block_route_count = snapshot_v2_pci_endpoint_route_count(VIRTIO_BLOCK_QUEUE_SIZES.len());
    let pmem_route_count = snapshot_v2_pci_endpoint_route_count(VIRTIO_PMEM_QUEUE_SIZES.len());
    if block_records.len() != plan.pci().block_records().len()
        || pmem_records.len() != plan.pci().pmem_records().len()
        || block_route_count.is_none()
        || pmem_route_count.is_none()
    {
        return false;
    }

    block_records
        .iter()
        .map(|record| record.transport())
        .zip(plan.pci().block_records())
        .chain(
            pmem_records
                .iter()
                .map(|record| record.transport())
                .zip(plan.pci().pmem_records()),
        )
        .enumerate()
        .all(|(index, (transport, planned))| {
            let Some(slot) = storage_start_slot.checked_add(index) else {
                return false;
            };
            let Some(placement) = snapshot_v2_pci_endpoint_placement(address_plan, slot) else {
                return false;
            };
            let SnapshotV2DeviceTransport::Pci(captured) = transport else {
                return false;
            };
            let expected_route_count = if index < block_records.len() {
                block_route_count
            } else {
                pmem_route_count
            };
            planned.sbdf() == placement.sbdf
                && planned.bar_region_id() == placement.bar_region_id
                && planned.bar_range() == placement.bar_range
                && Some(planned.route_count()) == expected_route_count
                && captured.sbdf() == placement.sbdf
                && captured.bar_range() == placement.bar_range
                && register_active_pci_routes(captured, active_routes)
        })
}

fn entropy_queue_conflicts_with_storage(
    entropy_ranges: Option<[GuestMemoryRange; 3]>,
    bundle: &PreparedSnapshotV2StorageBundle,
) -> bool {
    let Some(entropy_ranges) = entropy_ranges else {
        return false;
    };
    let block_records = bundle
        .block_bundle()
        .map_or(&[][..], |block| block.records());
    entropy_ranges.iter().any(|entropy_range| {
        block_records
            .iter()
            .filter_map(|record| record.queue_ranges())
            .chain(
                bundle
                    .pmem_records()
                    .iter()
                    .filter_map(|record| record.queue_ranges()),
            )
            .flatten()
            .any(|storage_range| entropy_range.overlaps(storage_range))
            || bundle
                .pmem_records()
                .iter()
                .any(|record| entropy_range.overlaps(record.prepared_device().guest_range()))
    })
}

pub(crate) fn register_active_retained_pci_routes(
    state: &VirtioPciMsixState,
    msi: crate::gic::HvfGicMsiMetadata,
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
    if state.vector_count() != route_count
        || state.pending_words().len() != 1
        || state.queue_vectors().len() != queue_count
        || state
            .pending_words()
            .first()
            .is_none_or(|pending| pending & !pending_mask != 0)
        || !valid_vector(state.config_vector(), route_count)
        || !state
            .queue_vectors()
            .iter()
            .copied()
            .all(|vector| valid_vector(vector, route_count))
    {
        return false;
    }

    state.entries().iter().enumerate().all(|(index, entry)| {
        if entry.vector_control() & !1 != 0 {
            return false;
        }
        let Ok(vector) = u16::try_from(index) else {
            return false;
        };
        let referenced = state.config_vector() == vector || state.queue_vectors().contains(&vector);
        let pending = state
            .pending_words()
            .get(index / u64::BITS as usize)
            .is_some_and(|word| word & (1_u64 << (index % u64::BITS as usize)) != 0);
        if entry.is_masked() || (!referenced && !pending) {
            return true;
        }
        let address = (u64::from(entry.message_address_high()) << 32)
            | u64::from(entry.message_address_low());
        let data = entry.message_data();
        let route = (address, data);
        if address != expected_address
            || data < msi.interrupt_range.base
            || data >= interrupt_end
            || active_routes.contains(&route)
        {
            return false;
        }
        active_routes.push(route);
        true
    })
}

const fn valid_vector(vector: u16, count: usize) -> bool {
    vector == VIRTIO_PCI_NO_VECTOR || (vector as usize) < count
}

trait EntropyPciPlatformPlanReserve {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), PrepareHvfSnapshotV2EntropyPciPlatformPlanError>;
}

struct SystemEntropyPciPlatformPlanReserve;

impl EntropyPciPlatformPlanReserve for SystemEntropyPciPlatformPlanReserve {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), PrepareHvfSnapshotV2EntropyPciPlatformPlanError> {
        values
            .try_reserve_exact(additional)
            .map_err(|_| PrepareHvfSnapshotV2EntropyPciPlatformPlanError::Allocation)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Instant;

    use bangbang_runtime::interrupt::GuestInterruptLine;
    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange,
    };
    use bangbang_runtime::mmio::MmioRegion;
    use bangbang_runtime::pci::Arm64PciAddressPlan;
    use bangbang_runtime::snapshot_device_v2::SnapshotV2MmioDeviceState;
    use bangbang_runtime::snapshot_entropy_v2_8::{
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION, NATIVE_V2_ENTROPY_STATE_HEADER_BYTES,
        NATIVE_V2_ENTROPY_STATE_SECTION_ENTRY_BYTES, SnapshotV2EntropyState,
    };

    use super::*;

    const ACTIVE_PCI_HEX: &str =
        include_str!("../../runtime/src/snapshot_entropy_v2_8/fixtures/active-pci.hex");
    const INACTIVE_MMIO_HEX: &str =
        include_str!("../../runtime/src/snapshot_entropy_v2_8/fixtures/inactive-mmio.hex");
    const RESTORE_MEMORY_SIZE: u64 = 0x20_0000;
    const DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x2_0000);
    const AVAILABLE_RING: GuestAddress = GuestAddress::new(0x4_0000);
    const USED_RING: GuestAddress = GuestAddress::new(0x6_0000);
    const DATA_BUFFER: GuestAddress = GuestAddress::new(0x8_0000);

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

    fn restore_memory(active: bool) -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), RESTORE_MEMORY_SIZE)
                .expect("entropy restore range should validate"),
        ])
        .expect("entropy restore layout should validate");
        let mut memory =
            GuestMemory::allocate(&layout).expect("entropy restore memory should allocate");
        if active {
            memory
                .write_slice(&DATA_BUFFER.raw_value().to_le_bytes(), DESCRIPTOR_TABLE)
                .expect("entropy descriptor address should write");
            memory
                .write_slice(
                    &8_u32.to_le_bytes(),
                    DESCRIPTOR_TABLE
                        .checked_add(8)
                        .expect("entropy descriptor length address should fit"),
                )
                .expect("entropy descriptor length should write");
            memory
                .write_slice(
                    &2_u16.to_le_bytes(),
                    DESCRIPTOR_TABLE
                        .checked_add(12)
                        .expect("entropy descriptor flags address should fit"),
                )
                .expect("entropy descriptor flags should write");
            memory
                .write_slice(
                    &0_u16.to_le_bytes(),
                    DESCRIPTOR_TABLE
                        .checked_add(14)
                        .expect("entropy descriptor next address should fit"),
                )
                .expect("entropy descriptor next should write");
            memory
                .write_slice(
                    &0_u16.to_le_bytes(),
                    AVAILABLE_RING
                        .checked_add(16)
                        .expect("entropy available entry address should fit"),
                )
                .expect("entropy available entry should write");
            memory
                .write_slice(
                    &7_u16.to_le_bytes(),
                    AVAILABLE_RING
                        .checked_add(2)
                        .expect("entropy available index address should fit"),
                )
                .expect("entropy available index should write");
            memory
                .write_slice(
                    &6_u16.to_le_bytes(),
                    USED_RING
                        .checked_add(2)
                        .expect("entropy used index address should fit"),
                )
                .expect("entropy used index should write");
        }
        memory
    }

    fn entropy_state_at(
        index: usize,
        first_message_data: u32,
        active_routes: bool,
        referenced_routes: bool,
    ) -> SnapshotV2EntropyState {
        let mut bytes = fixture_bytes(ACTIVE_PCI_HEX);
        let common_directory_entry =
            NATIVE_V2_ENTROPY_STATE_HEADER_BYTES + NATIVE_V2_ENTROPY_STATE_SECTION_ENTRY_BYTES;
        let common_offset_start = common_directory_entry + 8;
        let common_offset = usize::try_from(u64::from_le_bytes(
            bytes[common_offset_start..common_offset_start + 8]
                .try_into()
                .expect("common offset should be present"),
        ))
        .expect("common offset should fit usize");
        for (offset, address) in [
            (40, DESCRIPTOR_TABLE),
            (48, AVAILABLE_RING),
            (56, USED_RING),
        ] {
            bytes[common_offset + offset..common_offset + offset + 8]
                .copy_from_slice(&address.raw_value().to_le_bytes());
        }

        let transport_directory_entry =
            NATIVE_V2_ENTROPY_STATE_HEADER_BYTES + 2 * NATIVE_V2_ENTROPY_STATE_SECTION_ENTRY_BYTES;
        let offset_start = transport_directory_entry + 8;
        let transport_offset = usize::try_from(u64::from_le_bytes(
            bytes[offset_start..offset_start + 8]
                .try_into()
                .expect("transport offset should be present"),
        ))
        .expect("transport offset should fit usize");
        let placement = snapshot_v2_pci_endpoint_placement(
            Arm64PciAddressPlan::firecracker_v1_16().expect("PCI address plan should validate"),
            index,
        )
        .expect("entropy placement should exist");
        bytes[transport_offset + 11] = placement.sbdf.device();
        bytes[transport_offset + 16..transport_offset + 24]
            .copy_from_slice(&placement.bar_range.start().raw_value().to_le_bytes());
        bytes[transport_offset + 104..transport_offset + 108]
            .copy_from_slice(&first_message_data.to_le_bytes());
        if !active_routes {
            bytes[transport_offset + 108..transport_offset + 112]
                .copy_from_slice(&1_u32.to_le_bytes());
            bytes[transport_offset + 124..transport_offset + 128]
                .copy_from_slice(&1_u32.to_le_bytes());
        }
        if !referenced_routes {
            bytes[transport_offset + 68..transport_offset + 70]
                .copy_from_slice(&u16::MAX.to_le_bytes());
            bytes[transport_offset + 128..transport_offset + 136].fill(0);
            bytes[transport_offset + 136..transport_offset + 138]
                .copy_from_slice(&u16::MAX.to_le_bytes());
        }
        SnapshotV2EntropyState::decode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION, &bytes)
            .expect("relocated entropy fixture should decode")
    }

    pub(crate) fn entropy_plan_at(
        index: usize,
        first_message_data: u32,
    ) -> SnapshotV2EntropyRestorePlan {
        entropy_plan_at_with_routes(index, first_message_data, true)
    }

    pub(crate) fn entropy_plan_at_with_routes(
        index: usize,
        first_message_data: u32,
        active_routes: bool,
    ) -> SnapshotV2EntropyRestorePlan {
        SnapshotV2EntropyRestorePlan::prepare(
            entropy_state_at(index, first_message_data, active_routes, true),
            &restore_memory(true),
            Instant::now(),
        )
        .expect("entropy restore plan should prepare")
    }

    pub(crate) fn entropy_plan_at_with_unreferenced_routes(
        index: usize,
        first_message_data: u32,
    ) -> SnapshotV2EntropyRestorePlan {
        SnapshotV2EntropyRestorePlan::prepare(
            entropy_state_at(index, first_message_data, true, false),
            &restore_memory(true),
            Instant::now(),
        )
        .expect("unreferenced entropy restore plan should prepare")
    }

    pub(crate) fn entropy_mmio_plan_at(
        region: MmioRegion,
        interrupt_line: GuestInterruptLine,
    ) -> SnapshotV2EntropyRestorePlan {
        let state = SnapshotV2EntropyState::decode(
            NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
            &fixture_bytes(INACTIVE_MMIO_HEX),
        )
        .expect("MMIO entropy fixture should decode");
        let (config, active_queue, limiter, retry, pending, virtio, transport) = state.into_parts();
        let SnapshotV2DeviceTransport::Mmio(captured) = transport else {
            panic!("MMIO entropy fixture should retain MMIO transport");
        };
        let transport = SnapshotV2DeviceTransport::Mmio(SnapshotV2MmioDeviceState::from_parts(
            captured.device_feature_select(),
            captured.driver_feature_select(),
            captured.queue_select(),
            region,
            interrupt_line,
        ));
        let state = SnapshotV2EntropyState::try_new(
            config,
            active_queue,
            limiter,
            retry,
            pending,
            virtio,
            transport,
        )
        .expect("relined MMIO entropy state should validate");
        SnapshotV2EntropyRestorePlan::prepare(state, &restore_memory(false), Instant::now())
            .expect("relined MMIO entropy plan should prepare")
    }

    fn with_msi_interrupt_count(
        platform: HvfSnapshotV2PlatformState,
        interrupt_count: u32,
    ) -> HvfSnapshotV2PlatformState {
        let (memory, machine, global, topology, vcpus, time) = platform.into_parts();
        let (compatibility, gic_device) = global.into_parts();
        let mut gic = compatibility.gic_metadata();
        let mut msi = gic.msi.expect("PCI platform should contain MSI metadata");
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
            .expect("mutated PCI platform should cross-validate")
    }

    #[test]
    fn serial_entropy_plan_selects_the_exact_first_pci_endpoint() {
        let platform = crate::snapshot_v2_multi_block_platform::tests::product_pci_platform();
        let msi = platform
            .global()
            .compatibility()
            .gic_metadata()
            .msi
            .expect("product PCI platform should retain MSI metadata");
        let entropy = entropy_plan_at(0, msi.interrupt_range.base);
        let plan = prepare_hvf_snapshot_v2_serial_entropy_pci_platform_plan(&platform, &entropy)
            .expect("serial PCI entropy plan should validate");
        let placement = snapshot_v2_pci_endpoint_placement(
            Arm64PciAddressPlan::firecracker_v1_16().expect("PCI address plan should validate"),
            0,
        )
        .expect("first PCI placement should exist");
        assert_eq!(plan.preceding_endpoint_count(), 0);
        assert_eq!(plan.sbdf(), placement.sbdf);
        assert_eq!(plan.bar_region_id(), placement.bar_region_id);
        assert_eq!(plan.bar_range(), placement.bar_range);
        assert_eq!(plan.route_count(), VIRTIO_RNG_QUEUE_SIZES.len() + 1);
        let debug = format!("{plan:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(&placement.bar_range.start().raw_value().to_string()));
    }

    #[test]
    fn combined_storage_entropy_plan_proves_storage_prefix_then_entropy() {
        let fixture = crate::snapshot_v2_storage_platform::tests::rootless_block_pci_fixture();
        let msi = fixture
            .platform
            .global()
            .compatibility()
            .gic_metadata()
            .msi
            .expect("product PCI platform should retain MSI metadata");
        let message_data = msi
            .interrupt_range
            .base
            .checked_add(msi.interrupt_range.count - 1)
            .expect("last MSI INTID should fit");
        let entropy = entropy_plan_at(1, message_data);
        let plan = prepare_hvf_snapshot_v2_storage_entropy_pci_platform_plan(
            &fixture.platform,
            &fixture.bundle,
            &entropy,
        )
        .expect("storage-plus-entropy PCI plan should validate");
        assert_eq!(plan.storage().pci().record_count(), 1);
        assert_eq!(plan.entropy().preceding_endpoint_count(), 1);
        let placement = snapshot_v2_pci_endpoint_placement(
            Arm64PciAddressPlan::firecracker_v1_16().expect("PCI address plan should validate"),
            1,
        )
        .expect("second PCI placement should exist");
        assert_eq!(plan.entropy().sbdf(), placement.sbdf);

        let SnapshotV2DeviceTransport::Pci(storage_transport) = fixture
            .bundle
            .block_bundle()
            .expect("combined fixture should contain block storage")
            .records()[0]
            .transport()
        else {
            panic!("combined fixture storage should use PCI");
        };
        let storage_message_data = storage_transport
            .msix()
            .entries()
            .iter()
            .enumerate()
            .find_map(|(index, entry)| {
                let vector = u16::try_from(index).ok()?;
                let referenced = storage_transport.msix().config_vector() == vector
                    || storage_transport.msix().queue_vectors().contains(&vector);
                (entry.vector_control() & 1 == 0 && referenced).then_some(entry.message_data())
            })
            .expect("combined fixture should retain one active storage route");
        let colliding_entropy = entropy_plan_at(1, storage_message_data);
        assert!(matches!(
            prepare_hvf_snapshot_v2_storage_entropy_pci_platform_plan(
                &fixture.platform,
                &fixture.bundle,
                &colliding_entropy,
            ),
            Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan)
        ));

        fixture
            .bundle
            .abort()
            .expect("planned storage bundle should abort cleanly");
    }

    #[test]
    fn wrong_transport_capacity_and_reservation_fail_before_construction() {
        let platform = crate::snapshot_v2_multi_block_platform::tests::product_pci_platform();
        let mmio = SnapshotV2EntropyState::decode(
            NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
            &fixture_bytes(INACTIVE_MMIO_HEX),
        )
        .expect("MMIO entropy fixture should decode");
        let mmio =
            SnapshotV2EntropyRestorePlan::prepare(mmio, &restore_memory(false), Instant::now())
                .expect("MMIO entropy restore plan should prepare");
        assert!(matches!(
            prepare_hvf_snapshot_v2_serial_entropy_pci_platform_plan(&platform, &mmio),
            Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::TransportPolicy)
        ));

        let msi = platform
            .global()
            .compatibility()
            .gic_metadata()
            .msi
            .expect("product PCI platform should retain MSI metadata");
        let last_entropy = entropy_plan_at(PCI_ENDPOINT_SLOT_COUNT - 1, msi.interrupt_range.base);
        let last_plan = prepare_entropy_pci_endpoint_plan(
            &platform,
            None,
            &last_entropy,
            0,
            PCI_ENDPOINT_SLOT_COUNT - 1,
            None,
            &mut SystemEntropyPciPlatformPlanReserve,
        )
        .expect("30 preceding storage endpoints should leave one entropy slot");
        assert_eq!(
            last_plan.preceding_endpoint_count(),
            PCI_ENDPOINT_SLOT_COUNT - 1
        );

        let entropy = entropy_plan_at(0, msi.interrupt_range.base);
        assert!(matches!(
            prepare_entropy_pci_endpoint_plan(
                &platform,
                None,
                &entropy,
                0,
                PCI_ENDPOINT_SLOT_COUNT,
                None,
                &mut SystemEntropyPciPlatformPlanReserve,
            ),
            Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::PciCapacity {
                count,
                maximum,
            }) if count == PCI_ENDPOINT_SLOT_COUNT + 1 && maximum == PCI_ENDPOINT_SLOT_COUNT
        ));

        struct FailingReserve;
        impl EntropyPciPlatformPlanReserve for FailingReserve {
            fn reserve<T>(
                &mut self,
                _values: &mut Vec<T>,
                _additional: usize,
            ) -> Result<(), PrepareHvfSnapshotV2EntropyPciPlatformPlanError> {
                Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::Allocation)
            }
        }
        assert!(matches!(
            prepare_entropy_pci_endpoint_plan(
                &platform,
                None,
                &entropy,
                0,
                0,
                None,
                &mut FailingReserve,
            ),
            Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::Allocation)
        ));
    }

    #[test]
    fn placement_and_active_route_divergence_are_rejected() {
        let platform = crate::snapshot_v2_multi_block_platform::tests::product_pci_platform();
        let msi = platform
            .global()
            .compatibility()
            .gic_metadata()
            .msi
            .expect("product PCI platform should retain MSI metadata");
        let wrong_placement = entropy_plan_at(1, msi.interrupt_range.base);
        assert!(matches!(
            prepare_hvf_snapshot_v2_serial_entropy_pci_platform_plan(&platform, &wrong_placement,),
            Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan)
        ));

        let entropy_pool_count = pci_entropy_restore_gic_msi_configuration()
            .expect("entropy PCI MSI profile should validate")
            .interrupt_count()
            .get();
        let entropy_profile = with_msi_interrupt_count(platform.clone(), entropy_pool_count);
        let canonical_entropy = entropy_plan_at(0, msi.interrupt_range.base);
        prepare_hvf_snapshot_v2_serial_entropy_pci_platform_plan(
            &entropy_profile,
            &canonical_entropy,
        )
        .expect("fixed-entropy MSI profile should remain canonical");

        let wrong_profile = with_msi_interrupt_count(
            platform.clone(),
            entropy_pool_count
                .checked_sub(1)
                .expect("wrong MSI profile count should remain nonzero"),
        );
        assert!(matches!(
            prepare_hvf_snapshot_v2_serial_entropy_pci_platform_plan(
                &wrong_profile,
                &canonical_entropy,
            ),
            Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan)
        ));

        let bad_message = msi
            .interrupt_range
            .base
            .checked_add(msi.interrupt_range.count)
            .expect("first out-of-range INTID should fit");
        let wrong_route = entropy_plan_at(0, bad_message);
        assert!(matches!(
            prepare_hvf_snapshot_v2_serial_entropy_pci_platform_plan(&platform, &wrong_route),
            Err(PrepareHvfSnapshotV2EntropyPciPlatformPlanError::ResourcePlan)
        ));
    }
}
