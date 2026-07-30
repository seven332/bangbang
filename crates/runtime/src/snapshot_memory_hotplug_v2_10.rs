//! Canonical detached native-v2 2.10 virtio-mem state profile.
//!
//! This module owns only portable configuration, guest-visible virtio state,
//! transport placement, active queue cursors, and a bounded plugged-block
//! bitmap. Guest memory, host mappings, platform slots, interrupt authority,
//! dispatchers, metrics, and live device ownership remain destination-local.

use std::fmt;
use std::iter::FusedIterator;

use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestMemoryRange, aarch64};
use crate::memory_hotplug::{
    MemoryHotplugConfig, MemoryHotplugConfigInput, VIRTIO_FEATURE_VERSION_1,
    VIRTIO_MEM_DEFAULT_REGION_ADDRESS, VIRTIO_MEM_DEVICE_ID, VIRTIO_MEM_F_UNPLUGGED_INACCESSIBLE,
    VIRTIO_MEM_QUEUE_SIZE, VirtioMemConfigSpace, VirtioMemDeviceCaptureState,
    VirtioMemMmioCaptureState, VirtioMemPciCaptureState,
};
use crate::mmio::MmioRegion;
use crate::pci::PciSbdf;
use crate::snapshot_device_v2::{
    SnapshotV2DeviceGraphCaptureError, SnapshotV2DeviceTransport, SnapshotV2VirtioState,
    capture_mmio_common_for_device_with_config_status_gate, capture_mmio_transport,
    capture_pci_common_for_device, capture_pci_transport_parts,
};
use crate::snapshot_device_v2_5::{
    queue_ranges, validate_mmio, validate_pci, validate_virtio_with_queue_size,
};
use crate::snapshot_format::SnapshotFormatVersion;
use crate::snapshot_memory_v2::SnapshotV2MemoryBinding;
use crate::storage_capture::StorageDeviceOrigin;

mod codec;

#[cfg(test)]
mod tests;

const MIB: u64 = 1024 * 1024;
const MINIMUM_BLOCK_BYTES: u64 = 2 * MIB;
const REQUIRED_FEATURES: u64 =
    (1_u64 << VIRTIO_FEATURE_VERSION_1) | (1_u64 << VIRTIO_MEM_F_UNPLUGGED_INACCESSIBLE);
const REDACTED: &str = "<redacted>";

/// Exact compatibility context of the optional singleton virtio-mem component.
pub const NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION: SnapshotFormatVersion =
    SnapshotFormatVersion::new(2, 10, 0);

/// Maximum complete exact-2.10 virtio-mem component size.
pub const NATIVE_V2_MEMORY_HOTPLUG_STATE_MAX_BYTES: usize = 128 * 1024;

/// Fixed exact-2.10 virtio-mem component header size.
pub const NATIVE_V2_MEMORY_HOTPLUG_STATE_HEADER_BYTES: usize = 64;

/// Fixed encoded size of one virtio-mem section-directory entry.
pub const NATIVE_V2_MEMORY_HOTPLUG_STATE_SECTION_ENTRY_BYTES: usize = 32;

/// Fixed encoded size of the virtio-mem local section.
pub const NATIVE_V2_MEMORY_HOTPLUG_STATE_LOCAL_BYTES: usize = 96;

/// Architecture-derived maximum configured virtio-mem block count.
pub const NATIVE_V2_MEMORY_HOTPLUG_MAX_BLOCKS: usize =
    (aarch64::DRAM_MEM_MAX_SIZE / MINIMUM_BLOCK_BYTES) as usize;

/// Architecture-derived maximum raw plugged-block bitmap size.
pub const NATIVE_V2_MEMORY_HOTPLUG_MAX_BITMAP_BYTES: usize =
    NATIVE_V2_MEMORY_HOTPLUG_MAX_BLOCKS.div_ceil(8);

/// Exact largest complete profile-1 product admitted by this schema.
pub const NATIVE_V2_MEMORY_HOTPLUG_WORST_CASE_BYTES: usize = 64
    + 4 * 32
    + NATIVE_V2_MEMORY_HOTPLUG_STATE_LOCAL_BYTES
    + 80
    + NATIVE_V2_MEMORY_HOTPLUG_MAX_BITMAP_BYTES
    + 144;

const _: () = assert!(NATIVE_V2_MEMORY_HOTPLUG_MAX_BLOCKS == 523_264);
const _: () = assert!(NATIVE_V2_MEMORY_HOTPLUG_MAX_BITMAP_BYTES == 65_408);
const _: () = assert!(NATIVE_V2_MEMORY_HOTPLUG_MAX_BITMAP_BYTES.is_multiple_of(8));
const _: () = assert!(NATIVE_V2_MEMORY_HOTPLUG_WORST_CASE_BYTES == 65_920);
const _: () =
    assert!(NATIVE_V2_MEMORY_HOTPLUG_WORST_CASE_BYTES <= NATIVE_V2_MEMORY_HOTPLUG_STATE_MAX_BYTES);
const _: () = assert!(
    NATIVE_V2_MEMORY_HOTPLUG_STATE_MAX_BYTES
        <= crate::snapshot_format_v2::NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES
);

/// One checked active virtio-mem queue cursor pair.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2MemoryHotplugQueueState {
    next_available: u16,
    next_used: u16,
}

impl SnapshotV2MemoryHotplugQueueState {
    /// Constructs the only source-admitted cursor relationship.
    pub fn try_new(
        next_available: u16,
        next_used: u16,
    ) -> Result<Self, SnapshotV2MemoryHotplugStateBuildError> {
        if next_available != next_used {
            return Err(SnapshotV2MemoryHotplugStateBuildError::Queue);
        }
        Ok(Self {
            next_available,
            next_used,
        })
    }

    pub(crate) const fn from_parts(next_available: u16, next_used: u16) -> Self {
        Self {
            next_available,
            next_used,
        }
    }

    /// Returns the next device-local available-ring cursor.
    pub const fn next_available(self) -> u16 {
        self.next_available
    }

    /// Returns the next device-local used-ring cursor.
    pub const fn next_used(self) -> u16 {
        self.next_used
    }
}

impl fmt::Debug for SnapshotV2MemoryHotplugQueueState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2MemoryHotplugQueueState")
            .field("cursors", &REDACTED)
            .finish()
    }
}

/// One maximal nonempty plugged-block range derived from the retained bitmap.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2MemoryHotplugPluggedRange {
    start_block: u64,
    block_count: u64,
}

impl SnapshotV2MemoryHotplugPluggedRange {
    /// Returns the first configured block in this range.
    pub const fn start_block(self) -> u64 {
        self.start_block
    }

    /// Returns the number of adjacent plugged blocks.
    pub const fn block_count(self) -> u64 {
        self.block_count
    }
}

impl fmt::Debug for SnapshotV2MemoryHotplugPluggedRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2MemoryHotplugPluggedRange")
            .field("range", &REDACTED)
            .finish()
    }
}

/// Zero-allocation iterator over maximal plugged-block ranges.
#[derive(Clone)]
pub struct SnapshotV2MemoryHotplugPluggedRanges<'bitmap> {
    bitmap: &'bitmap [u8],
    block_count: usize,
    next_block: usize,
    remaining_ranges: usize,
}

impl<'bitmap> SnapshotV2MemoryHotplugPluggedRanges<'bitmap> {
    fn new(bitmap: &'bitmap [u8], block_count: usize) -> Self {
        let remaining_ranges = count_ranges(bitmap, block_count);
        Self {
            bitmap,
            block_count,
            next_block: 0,
            remaining_ranges,
        }
    }
}

impl Iterator for SnapshotV2MemoryHotplugPluggedRanges<'_> {
    type Item = SnapshotV2MemoryHotplugPluggedRange;

    fn next(&mut self) -> Option<Self::Item> {
        while self.next_block < self.block_count && !bitmap_bit(self.bitmap, self.next_block) {
            self.next_block += 1;
        }
        if self.next_block == self.block_count {
            self.remaining_ranges = 0;
            return None;
        }
        let start = self.next_block;
        while self.next_block < self.block_count && bitmap_bit(self.bitmap, self.next_block) {
            self.next_block += 1;
        }
        self.remaining_ranges = self.remaining_ranges.saturating_sub(1);
        Some(SnapshotV2MemoryHotplugPluggedRange {
            start_block: start as u64,
            block_count: (self.next_block - start) as u64,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining_ranges, Some(self.remaining_ranges))
    }
}

impl ExactSizeIterator for SnapshotV2MemoryHotplugPluggedRanges<'_> {
    fn len(&self) -> usize {
        self.remaining_ranges
    }
}

impl FusedIterator for SnapshotV2MemoryHotplugPluggedRanges<'_> {}

impl fmt::Debug for SnapshotV2MemoryHotplugPluggedRanges<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2MemoryHotplugPluggedRanges")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete bounded exact-2.10 virtio-mem component value.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2MemoryHotplugState {
    config: MemoryHotplugConfig,
    config_space: VirtioMemConfigSpace,
    active_queue: Option<SnapshotV2MemoryHotplugQueueState>,
    plugged_bitmap: Vec<u8>,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
}

impl SnapshotV2MemoryHotplugState {
    /// Converts one checked MMIO live capture without retaining source
    /// ownership.
    pub fn try_from_mmio_capture(
        config: MemoryHotplugConfig,
        region: MmioRegion,
        interrupt_line: GuestInterruptLine,
        captured: &VirtioMemMmioCaptureState,
    ) -> Result<Self, SnapshotV2MemoryHotplugStateCaptureError> {
        let expected_features = captured.device().config_space().available_features();
        let virtio = capture_mmio_common_for_device_with_config_status_gate(
            captured.transport(),
            VIRTIO_MEM_DEVICE_ID,
            expected_features,
            true,
        )
        .map_err(capture_common_error)?;
        let transport = capture_mmio_transport(region, interrupt_line, captured.transport())
            .map(SnapshotV2DeviceTransport::Mmio)
            .map_err(capture_common_error)?;
        capture_memory_hotplug_state(config, captured.device(), virtio, transport)
    }

    /// Converts one checked startup-origin PCI live capture without retaining
    /// source ownership.
    pub fn try_from_pci_capture(
        config: MemoryHotplugConfig,
        sbdf: PciSbdf,
        bar_range: GuestMemoryRange,
        captured: &VirtioMemPciCaptureState,
    ) -> Result<Self, SnapshotV2MemoryHotplugStateCaptureError> {
        let expected_features = captured.device().config_space().available_features();
        let virtio = capture_pci_common_for_device(
            captured.transport(),
            VIRTIO_MEM_DEVICE_ID,
            expected_features,
        )
        .map_err(capture_common_error)?;
        let transport = capture_pci_transport_parts(
            StorageDeviceOrigin::Startup,
            sbdf,
            bar_range,
            captured.transport(),
        )
        .map(SnapshotV2DeviceTransport::Pci)
        .map_err(capture_common_error)?;
        capture_memory_hotplug_state(config, captured.device(), virtio, transport)
    }

    /// Constructs one complete checked portable virtio-mem continuation.
    pub fn try_new(
        config: MemoryHotplugConfig,
        config_space: VirtioMemConfigSpace,
        active_queue: Option<SnapshotV2MemoryHotplugQueueState>,
        plugged_bitmap: Vec<u8>,
        virtio: SnapshotV2VirtioState,
        transport: SnapshotV2DeviceTransport,
    ) -> Result<Self, SnapshotV2MemoryHotplugStateBuildError> {
        let state = Self {
            config,
            config_space,
            active_queue,
            plugged_bitmap,
            virtio,
            transport,
        };
        validate_memory_hotplug_state(&state)?;
        Ok(state)
    }

    /// Returns the exact compatibility context of this value.
    pub const fn compatibility_version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION
    }

    /// Returns the exact external virtio-mem configuration.
    pub const fn config(&self) -> MemoryHotplugConfig {
        self.config
    }

    /// Returns the exact guest-visible virtio-mem config space.
    pub const fn config_space(&self) -> VirtioMemConfigSpace {
        self.config_space
    }

    /// Returns active queue cursors when the device is activated.
    pub const fn active_queue(&self) -> Option<SnapshotV2MemoryHotplugQueueState> {
        self.active_queue
    }

    /// Returns the bounded raw LSB-first plugged-block bitmap.
    pub fn plugged_bitmap(&self) -> &[u8] {
        &self.plugged_bitmap
    }

    /// Iterates over maximal separated plugged-block ranges without allocating.
    pub fn plugged_ranges(&self) -> SnapshotV2MemoryHotplugPluggedRanges<'_> {
        SnapshotV2MemoryHotplugPluggedRanges::new(
            &self.plugged_bitmap,
            configured_block_count(self.config_space).unwrap_or(0),
        )
    }

    /// Closes this portable plugged topology against one exact-2.10 kind-1
    /// memory binding.
    ///
    /// Extents outside the aperture remain platform-owned. Every extent that
    /// touches the aperture must be wholly contained by it, and the contained
    /// extent union must exactly equal the canonical plugged-range union.
    pub fn validate_memory_binding(
        &self,
        binding: &SnapshotV2MemoryBinding,
    ) -> Result<(), SnapshotV2MemoryHotplugBindingError> {
        if binding.version() != NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION {
            return Err(SnapshotV2MemoryHotplugBindingError::Version);
        }

        let aperture_start = self.config_space.addr();
        let aperture_end = aperture_start
            .checked_add(self.config_space.region_size())
            .ok_or(SnapshotV2MemoryHotplugBindingError::Overflow)?;
        for extent in binding.extents() {
            let range = extent.range();
            let start = range.start().raw_value();
            let end = range.end_exclusive().raw_value();
            let outside = end <= aperture_start || start >= aperture_end;
            let inside = start >= aperture_start && end <= aperture_end;
            if !outside && !inside {
                return Err(SnapshotV2MemoryHotplugBindingError::BoundaryCrossing);
            }
        }

        let mut actual = binding.extents().iter().filter_map(|extent| {
            let range = extent.range();
            let start = range.start().raw_value();
            let end = range.end_exclusive().raw_value();
            (start >= aperture_start && end <= aperture_end).then_some((start, end))
        });
        let block_size = self.config_space.block_size();
        let mut plugged = self.plugged_ranges().map(|range| {
            let offset = range
                .start_block()
                .checked_mul(block_size)
                .ok_or(SnapshotV2MemoryHotplugBindingError::Overflow)?;
            let length = range
                .block_count()
                .checked_mul(block_size)
                .ok_or(SnapshotV2MemoryHotplugBindingError::Overflow)?;
            let start = aperture_start
                .checked_add(offset)
                .ok_or(SnapshotV2MemoryHotplugBindingError::Overflow)?;
            let end = start
                .checked_add(length)
                .ok_or(SnapshotV2MemoryHotplugBindingError::Overflow)?;
            Ok::<_, SnapshotV2MemoryHotplugBindingError>((start, end))
        });
        let mut actual_cursor = actual.next();
        let mut plugged_cursor = plugged.next().transpose()?;
        loop {
            match (actual_cursor, plugged_cursor) {
                (None, None) => return Ok(()),
                (Some((actual_start, actual_end)), Some((plugged_start, plugged_end)))
                    if actual_start == plugged_start =>
                {
                    let covered_end = actual_end.min(plugged_end);
                    actual_cursor = if covered_end == actual_end {
                        actual.next()
                    } else {
                        Some((covered_end, actual_end))
                    };
                    plugged_cursor = if covered_end == plugged_end {
                        plugged.next().transpose()?
                    } else {
                        Some((covered_end, plugged_end))
                    };
                }
                _ => return Err(SnapshotV2MemoryHotplugBindingError::Coverage),
            }
        }
    }

    /// Returns common virtio continuation state.
    pub const fn virtio(&self) -> &SnapshotV2VirtioState {
        &self.virtio
    }

    /// Returns exact MMIO or PCI transport state.
    pub const fn transport(&self) -> &SnapshotV2DeviceTransport {
        &self.transport
    }

    /// Consumes this value into its inert parts.
    pub fn into_parts(
        self,
    ) -> (
        MemoryHotplugConfig,
        VirtioMemConfigSpace,
        Option<SnapshotV2MemoryHotplugQueueState>,
        Vec<u8>,
        SnapshotV2VirtioState,
        SnapshotV2DeviceTransport,
    ) {
        (
            self.config,
            self.config_space,
            self.active_queue,
            self.plugged_bitmap,
            self.virtio,
            self.transport,
        )
    }

    /// Encodes the canonical virtio-mem payload for an exact outer context.
    pub fn encode(
        &self,
        outer_version: SnapshotFormatVersion,
    ) -> Result<Vec<u8>, SnapshotV2MemoryHotplugStateEncodeError> {
        codec::encode(outer_version, self)
    }

    /// Decodes and validates one canonical exact-2.10 virtio-mem payload.
    pub fn decode(
        outer_version: SnapshotFormatVersion,
        bytes: &[u8],
    ) -> Result<Self, SnapshotV2MemoryHotplugStateDecodeError> {
        codec::decode(outer_version, bytes)
    }
}

impl fmt::Debug for SnapshotV2MemoryHotplugState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2MemoryHotplugState")
            .field("version", &self.compatibility_version())
            .field("state", &REDACTED)
            .finish()
    }
}

fn capture_memory_hotplug_state(
    config: MemoryHotplugConfig,
    device: &VirtioMemDeviceCaptureState,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
) -> Result<SnapshotV2MemoryHotplugState, SnapshotV2MemoryHotplugStateCaptureError> {
    if device.config() != config
        || device.available_features() != virtio.available_features()
        || device.negotiated_features() != virtio.driver_features()
        || device.active_queue().is_some() != virtio.is_activated()
    {
        return Err(SnapshotV2MemoryHotplugStateCaptureError::Device);
    }
    let active_queue = device
        .active_queue()
        .map(|queue| {
            SnapshotV2MemoryHotplugQueueState::try_new(queue.next_available(), queue.next_used())
        })
        .transpose()
        .map_err(|_| SnapshotV2MemoryHotplugStateCaptureError::Queue)?;
    let plugged_bitmap = bitmap_from_capture(device)?;
    SnapshotV2MemoryHotplugState::try_new(
        config,
        device.config_space(),
        active_queue,
        plugged_bitmap,
        virtio,
        transport,
    )
    .map_err(|source| SnapshotV2MemoryHotplugStateCaptureError::Build { source })
}

fn bitmap_from_capture(
    device: &VirtioMemDeviceCaptureState,
) -> Result<Vec<u8>, SnapshotV2MemoryHotplugStateCaptureError> {
    let block_count = configured_block_count(device.config_space())
        .map_err(|_| SnapshotV2MemoryHotplugStateCaptureError::Bitmap)?;
    let bitmap_length = block_count.div_ceil(8);
    if bitmap_length > NATIVE_V2_MEMORY_HOTPLUG_MAX_BITMAP_BYTES {
        return Err(SnapshotV2MemoryHotplugStateCaptureError::Bitmap);
    }
    let mut bitmap = Vec::new();
    bitmap
        .try_reserve_exact(bitmap_length)
        .map_err(|_| SnapshotV2MemoryHotplugStateCaptureError::Allocation)?;
    bitmap.resize(bitmap_length, 0);
    let mut previous_end = None;
    for range in device.plugged_ranges() {
        let start = usize::try_from(range.start_block())
            .map_err(|_| SnapshotV2MemoryHotplugStateCaptureError::Bitmap)?;
        let count = usize::try_from(range.block_count())
            .map_err(|_| SnapshotV2MemoryHotplugStateCaptureError::Bitmap)?;
        let end = start
            .checked_add(count)
            .ok_or(SnapshotV2MemoryHotplugStateCaptureError::Bitmap)?;
        if count == 0 || end > block_count || previous_end.is_some_and(|previous| start <= previous)
        {
            return Err(SnapshotV2MemoryHotplugStateCaptureError::Bitmap);
        }
        for block in start..end {
            set_bitmap_bit(&mut bitmap, block);
        }
        previous_end = Some(end);
    }
    Ok(bitmap)
}

fn capture_common_error(
    source: SnapshotV2DeviceGraphCaptureError,
) -> SnapshotV2MemoryHotplugStateCaptureError {
    if source == SnapshotV2DeviceGraphCaptureError::Allocation {
        SnapshotV2MemoryHotplugStateCaptureError::Allocation
    } else {
        SnapshotV2MemoryHotplugStateCaptureError::Common { source }
    }
}

pub(crate) fn validate_memory_hotplug_state(
    state: &SnapshotV2MemoryHotplugState,
) -> Result<(), SnapshotV2MemoryHotplugStateBuildError> {
    let block_count = validate_local_relationship(state.config, state.config_space)?;
    validate_bitmap(
        &state.plugged_bitmap,
        block_count,
        state.config_space.usable_region_size(),
        state.config_space.block_size(),
        state.config_space.plugged_size(),
    )?;

    validate_virtio_with_queue_size(&state.virtio, REQUIRED_FEATURES, VIRTIO_MEM_QUEUE_SIZE)
        .map_err(|_| SnapshotV2MemoryHotplugStateBuildError::Virtio)?;
    if state.active_queue.is_some() != state.virtio.is_activated() {
        return Err(SnapshotV2MemoryHotplugStateBuildError::Virtio);
    }
    if state
        .active_queue
        .is_some_and(|cursor| cursor.next_available != cursor.next_used)
    {
        return Err(SnapshotV2MemoryHotplugStateBuildError::Queue);
    }
    if state.virtio.is_activated()
        && state.virtio.driver_features() & REQUIRED_FEATURES != REQUIRED_FEATURES
    {
        return Err(SnapshotV2MemoryHotplugStateBuildError::Virtio);
    }

    let placement = match &state.transport {
        SnapshotV2DeviceTransport::Mmio(mmio) => {
            validate_mmio(mmio).map_err(|_| SnapshotV2MemoryHotplugStateBuildError::Transport)?;
            mmio.region().range()
        }
        SnapshotV2DeviceTransport::Pci(pci) => {
            validate_pci(pci).map_err(|_| SnapshotV2MemoryHotplugStateBuildError::Transport)?;
            if pci.origin() != StorageDeviceOrigin::Startup {
                return Err(SnapshotV2MemoryHotplugStateBuildError::Transport);
            }
            pci.bar_range()
        }
    };
    let queue = state
        .virtio
        .queues()
        .first()
        .ok_or(SnapshotV2MemoryHotplugStateBuildError::Virtio)?;
    if queue_ranges(queue)
        .map_err(|_| SnapshotV2MemoryHotplugStateBuildError::Queue)?
        .is_some_and(|ranges| ranges.into_iter().any(|range| range.overlaps(placement)))
    {
        return Err(SnapshotV2MemoryHotplugStateBuildError::Placement);
    }
    Ok(())
}

pub(crate) fn validate_local_relationship(
    config: MemoryHotplugConfig,
    config_space: VirtioMemConfigSpace,
) -> Result<usize, SnapshotV2MemoryHotplugStateBuildError> {
    MemoryHotplugConfigInput::new(
        config.total_size_mib(),
        config.block_size_mib(),
        config.slot_size_mib(),
    )
    .validate()
    .map_err(|_| SnapshotV2MemoryHotplugStateBuildError::Configuration)?;

    let total_size = config
        .total_size_mib()
        .checked_mul(MIB)
        .ok_or(SnapshotV2MemoryHotplugStateBuildError::Configuration)?;
    let block_size = config
        .block_size_mib()
        .checked_mul(MIB)
        .ok_or(SnapshotV2MemoryHotplugStateBuildError::Configuration)?;
    let slot_size = config
        .slot_size_mib()
        .checked_mul(MIB)
        .ok_or(SnapshotV2MemoryHotplugStateBuildError::Configuration)?;
    if config_space.block_size() != block_size
        || config_space.region_size() != total_size
        || block_size < MINIMUM_BLOCK_BYTES
        || slot_size == 0
    {
        return Err(SnapshotV2MemoryHotplugStateBuildError::Configuration);
    }

    let address_space_end = aarch64::DRAM_MEM_START
        .checked_add(aarch64::DRAM_MEM_MAX_SIZE)
        .ok_or(SnapshotV2MemoryHotplugStateBuildError::Geometry)?;
    let aperture_end = config_space
        .addr()
        .checked_add(config_space.region_size())
        .ok_or(SnapshotV2MemoryHotplugStateBuildError::Geometry)?;
    if config_space.addr() < VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value()
        || !config_space.addr().is_multiple_of(slot_size)
        || aperture_end > address_space_end
        || config_space.usable_region_size() > total_size
        || !config_space.usable_region_size().is_multiple_of(slot_size)
        || config_space.requested_size() > total_size
        || !config_space.requested_size().is_multiple_of(block_size)
        || config_space.plugged_size() > total_size
        || !config_space.plugged_size().is_multiple_of(block_size)
    {
        return Err(SnapshotV2MemoryHotplugStateBuildError::Geometry);
    }
    configured_block_count(config_space)
}

fn configured_block_count(
    config_space: VirtioMemConfigSpace,
) -> Result<usize, SnapshotV2MemoryHotplugStateBuildError> {
    if config_space.block_size() == 0
        || !config_space
            .region_size()
            .is_multiple_of(config_space.block_size())
    {
        return Err(SnapshotV2MemoryHotplugStateBuildError::Geometry);
    }
    let count = usize::try_from(config_space.region_size() / config_space.block_size())
        .map_err(|_| SnapshotV2MemoryHotplugStateBuildError::Geometry)?;
    if count == 0 || count > NATIVE_V2_MEMORY_HOTPLUG_MAX_BLOCKS {
        return Err(SnapshotV2MemoryHotplugStateBuildError::Geometry);
    }
    Ok(count)
}

fn validate_bitmap(
    bitmap: &[u8],
    block_count: usize,
    usable_region_size: u64,
    block_size: u64,
    plugged_size: u64,
) -> Result<(), SnapshotV2MemoryHotplugStateBuildError> {
    let expected_length = block_count.div_ceil(8);
    if bitmap.len() != expected_length
        || expected_length > NATIVE_V2_MEMORY_HOTPLUG_MAX_BITMAP_BYTES
    {
        return Err(SnapshotV2MemoryHotplugStateBuildError::Bitmap);
    }
    let usable_blocks = usize::try_from(usable_region_size / block_size)
        .map_err(|_| SnapshotV2MemoryHotplugStateBuildError::Bitmap)?;
    if (usable_blocks..block_count).any(|index| bitmap_bit(bitmap, index)) {
        return Err(SnapshotV2MemoryHotplugStateBuildError::Bitmap);
    }
    if let Some(last) = bitmap.last().copied() {
        let used_bits = block_count % 8;
        if used_bits != 0 && last & !((1_u8 << used_bits) - 1) != 0 {
            return Err(SnapshotV2MemoryHotplugStateBuildError::Bitmap);
        }
    }
    let plugged_blocks = bitmap
        .iter()
        .try_fold(0_u64, |count, byte| {
            count.checked_add(u64::from(byte.count_ones()))
        })
        .ok_or(SnapshotV2MemoryHotplugStateBuildError::Bitmap)?;
    if plugged_blocks.checked_mul(block_size) != Some(plugged_size) {
        return Err(SnapshotV2MemoryHotplugStateBuildError::Bitmap);
    }
    Ok(())
}

fn bitmap_bit(bitmap: &[u8], index: usize) -> bool {
    bitmap
        .get(index / 8)
        .is_some_and(|byte| byte & (1_u8 << (index % 8)) != 0)
}

fn set_bitmap_bit(bitmap: &mut [u8], index: usize) {
    if let Some(byte) = bitmap.get_mut(index / 8) {
        *byte |= 1_u8 << (index % 8);
    }
}

fn count_ranges(bitmap: &[u8], block_count: usize) -> usize {
    (0..block_count)
        .filter(|index| {
            bitmap_bit(bitmap, *index)
                && (*index == 0 || !bitmap_bit(bitmap, index.saturating_sub(1)))
        })
        .count()
}

/// Failure while closing exact-2.10 kind-1 memory extents against kind 11.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2MemoryHotplugBindingError {
    /// Kind 1 is not in the exact-2.10 compatibility context.
    Version,
    /// Aperture or plugged-range arithmetic overflowed.
    Overflow,
    /// One extent crosses an aperture boundary.
    BoundaryCrossing,
    /// In-aperture extents and plugged ranges have different GPA unions.
    Coverage,
}

impl fmt::Display for SnapshotV2MemoryHotplugBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Version => "native-v2 virtio-mem binding version is invalid",
            Self::Overflow => "native-v2 virtio-mem binding arithmetic overflowed",
            Self::BoundaryCrossing => "native-v2 virtio-mem binding crosses the memory aperture",
            Self::Coverage => "native-v2 virtio-mem binding coverage is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2MemoryHotplugBindingError {}

/// Failure while converting one trusted live capture into exact-2.10 state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2MemoryHotplugStateCaptureError {
    /// A bounded bitmap or common-state collection could not be allocated.
    Allocation,
    /// Repeated device and transport state disagree.
    Device,
    /// Active queue cursors are inconsistent.
    Queue,
    /// Captured plugged ranges cannot form the canonical bounded bitmap.
    Bitmap,
    /// Common virtio or transport capture failed.
    Common {
        /// Redacted common capture category.
        source: SnapshotV2DeviceGraphCaptureError,
    },
    /// Complete converted state failed its final semantic gate.
    Build {
        /// Redacted exact-2.10 build category.
        source: SnapshotV2MemoryHotplugStateBuildError,
    },
}

impl fmt::Debug for SnapshotV2MemoryHotplugStateCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2MemoryHotplugStateCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Allocation => "native-v2 captured virtio-mem state allocation failed",
            Self::Device => "native-v2 captured virtio-mem device state is inconsistent",
            Self::Queue => "native-v2 captured virtio-mem queue state is invalid",
            Self::Bitmap => "native-v2 captured virtio-mem bitmap is invalid",
            Self::Common { .. } => "native-v2 captured virtio-mem transport state is invalid",
            Self::Build { .. } => "native-v2 captured virtio-mem state is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2MemoryHotplugStateCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Common { source } => Some(source),
            Self::Build { source } => Some(source),
            Self::Allocation | Self::Device | Self::Queue | Self::Bitmap => None,
        }
    }
}

/// Failure while constructing trusted exact-2.10 virtio-mem state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2MemoryHotplugStateBuildError {
    /// External configuration is noncanonical or disagrees with config space.
    Configuration,
    /// Aperture and size geometry are invalid.
    Geometry,
    /// Plugged topology or plugged-size accounting is invalid.
    Bitmap,
    /// Active queue cursors are inconsistent.
    Queue,
    /// Common virtio state is not canonical for virtio-mem.
    Virtio,
    /// MMIO or PCI transport state is not canonical for virtio-mem.
    Transport,
    /// Queue ranges overlap the selected transport placement.
    Placement,
}

impl fmt::Display for SnapshotV2MemoryHotplugStateBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Configuration => "native-v2 virtio-mem configuration is invalid",
            Self::Geometry => "native-v2 virtio-mem geometry is invalid",
            Self::Bitmap => "native-v2 virtio-mem bitmap is invalid",
            Self::Queue => "native-v2 virtio-mem queue state is invalid",
            Self::Virtio => "native-v2 virtio-mem virtio state is invalid",
            Self::Transport => "native-v2 virtio-mem transport state is invalid",
            Self::Placement => "native-v2 virtio-mem placement is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2MemoryHotplugStateBuildError {}

/// Failure while encoding trusted exact-2.10 virtio-mem state.
#[derive(Debug)]
pub enum SnapshotV2MemoryHotplugStateEncodeError {
    /// The supplied outer semantic version is not exact 2.10.
    UnsupportedVersion,
    /// Trusted state no longer satisfies the canonical profile.
    InvalidState(SnapshotV2MemoryHotplugStateBuildError),
    /// Encoded length arithmetic overflowed.
    LengthOverflow,
    /// The encoded payload exceeds the fixed profile limit.
    TooLarge,
    /// The exact output buffer could not be reserved.
    Allocation,
}

impl fmt::Display for SnapshotV2MemoryHotplugStateEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => "native-v2 virtio-mem encoding version is unsupported",
            Self::InvalidState(_) => "native-v2 virtio-mem state is invalid",
            Self::LengthOverflow => "native-v2 virtio-mem state length arithmetic overflowed",
            Self::TooLarge => "native-v2 virtio-mem state exceeds its size limit",
            Self::Allocation => "native-v2 virtio-mem output allocation failed",
        })
    }
}

impl std::error::Error for SnapshotV2MemoryHotplugStateEncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidState(source) => Some(source),
            Self::UnsupportedVersion | Self::LengthOverflow | Self::TooLarge | Self::Allocation => {
                None
            }
        }
    }
}

/// Failure while decoding untrusted exact-2.10 virtio-mem state.
#[derive(Debug)]
pub enum SnapshotV2MemoryHotplugStateDecodeError {
    /// The supplied outer semantic version is not exact 2.10.
    UnsupportedVersion,
    /// Input ends before a required bounded field.
    Truncated,
    /// The payload exceeds the fixed complete limit.
    TooLarge,
    /// Header magic is invalid.
    InvalidMagic,
    /// Header profile or transport tag is unsupported.
    UnsupportedProfile,
    /// Header or section layout is noncanonical.
    InvalidStructure,
    /// Flags, booleans, tags, or scalar relationships are invalid.
    InvalidValue,
    /// Reserved bytes or canonical padding are nonzero.
    NonzeroReserved,
    /// A bounded decoded collection could not be reserved.
    Allocation,
    /// Complete decoded semantics fail the final typed gate.
    InvalidState(SnapshotV2MemoryHotplugStateBuildError),
}

impl fmt::Display for SnapshotV2MemoryHotplugStateDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => "native-v2 virtio-mem decoding version is unsupported",
            Self::Truncated => "native-v2 virtio-mem state is truncated",
            Self::TooLarge => "native-v2 virtio-mem state exceeds its bounds",
            Self::InvalidMagic => "native-v2 virtio-mem state magic is invalid",
            Self::UnsupportedProfile => "native-v2 virtio-mem state profile is unsupported",
            Self::InvalidStructure => "native-v2 virtio-mem state structure is noncanonical",
            Self::InvalidValue => "native-v2 virtio-mem state scalar value is invalid",
            Self::NonzeroReserved => "native-v2 virtio-mem reserved bytes are nonzero",
            Self::Allocation => "native-v2 virtio-mem state allocation failed",
            Self::InvalidState(_) => "native-v2 virtio-mem state semantics are invalid",
        })
    }
}

impl std::error::Error for SnapshotV2MemoryHotplugStateDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidState(source) => Some(source),
            Self::UnsupportedVersion
            | Self::Truncated
            | Self::TooLarge
            | Self::InvalidMagic
            | Self::UnsupportedProfile
            | Self::InvalidStructure
            | Self::InvalidValue
            | Self::NonzeroReserved
            | Self::Allocation => None,
        }
    }
}
