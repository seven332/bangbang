//! Canonical detached native-v2 2.10 virtio-mem state profile.
//!
//! This module owns only portable configuration, guest-visible virtio state,
//! transport placement, active queue cursors, and a bounded plugged-block
//! bitmap. Guest memory, host mappings, platform slots, interrupt authority,
//! dispatchers, metrics, and live device ownership remain destination-local.

use std::fmt;
use std::iter::FusedIterator;

use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestAddress, GuestMemory, GuestMemoryRange, aarch64};
use crate::memory_hotplug::{
    MemoryHotplugConfig, MemoryHotplugConfigInput, MemoryHotplugSizeUpdateInput,
    VIRTIO_FEATURE_VERSION_1, VIRTIO_MEM_DEFAULT_REGION_ADDRESS, VIRTIO_MEM_DEVICE_ID,
    VIRTIO_MEM_F_UNPLUGGED_INACCESSIBLE, VIRTIO_MEM_QUEUE_SIZE, VIRTIO_MEM_QUEUE_SIZES,
    VirtioMemConfigSpace, VirtioMemDevice, VirtioMemDeviceCaptureError,
    VirtioMemDeviceCaptureState, VirtioMemMmioCaptureState, VirtioMemMmioHandler,
    VirtioMemPciCaptureState, VirtioMemQueue, VirtioMemQueueBuildError, VirtioMemQueueCaptureError,
};
use crate::mmio::MmioRegion;
use crate::pci::PciSbdf;
use crate::snapshot_device_v2::{
    SnapshotV2DeviceGraphCaptureError, SnapshotV2DeviceTransport,
    SnapshotV2RootTransportRestoreError, SnapshotV2VirtioState,
    capture_mmio_common_for_device_with_config_status_gate, capture_mmio_transport,
    capture_pci_common_for_device, capture_pci_transport_parts,
    restore_mmio_transport_state_for_device_with_config_status_gate,
};
use crate::snapshot_device_v2_5::{
    queue_ranges, validate_mmio, validate_pci, validate_virtio_with_queue_size,
};
use crate::snapshot_format::SnapshotFormatVersion;
use crate::snapshot_memory_v2::{SnapshotV2MemoryBinding, SnapshotV2MemoryExtent};
use crate::storage_capture::StorageDeviceOrigin;
use crate::virtio_mmio::{VirtioMmioQueueState, VirtioMmioRegisterHandlerError};

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

/// Destination ownership class of one exact kind-1 memory extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2MemoryHotplugExtentClass {
    /// Memory outside the virtio-mem aperture, retained as base RAM.
    Base,
    /// Plugged memory inside the virtio-mem aperture.
    Dynamic,
}

/// Immutable classified view of one original ordered kind-1 extent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2MemoryHotplugClassifiedExtent {
    extent: SnapshotV2MemoryExtent,
    class: SnapshotV2MemoryHotplugExtentClass,
}

impl SnapshotV2MemoryHotplugClassifiedExtent {
    /// Returns the unchanged original GPA and file-offset record.
    pub const fn extent(self) -> SnapshotV2MemoryExtent {
        self.extent
    }

    /// Returns whether the original extent is base or dynamic memory.
    pub const fn class(self) -> SnapshotV2MemoryHotplugExtentClass {
        self.class
    }
}

impl fmt::Debug for SnapshotV2MemoryHotplugClassifiedExtent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2MemoryHotplugClassifiedExtent")
            .field("class", &self.class)
            .field("extent", &REDACTED)
            .finish()
    }
}

/// Checked controller values retained before destination owner construction.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2MemoryHotplugControllerProjection {
    config: MemoryHotplugConfig,
    requested_size_mib: u64,
}

impl SnapshotV2MemoryHotplugControllerProjection {
    /// Returns the validated external virtio-mem configuration.
    pub const fn config(self) -> MemoryHotplugConfig {
        self.config
    }

    /// Returns the validated requested size in MiB.
    pub const fn requested_size_mib(self) -> u64 {
        self.requested_size_mib
    }
}

impl fmt::Debug for SnapshotV2MemoryHotplugControllerProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2MemoryHotplugControllerProjection")
            .field("controller", &REDACTED)
            .finish()
    }
}

/// Original kind-1 binding and its cardinality-locked extent partition.
///
/// This value keeps the private classification vector attached to the exact
/// binding so callers cannot recombine tags with a different image.
#[derive(PartialEq, Eq)]
pub struct PreparedSnapshotV2MemoryHotplugMemory {
    binding: SnapshotV2MemoryBinding,
    extent_classes: Vec<SnapshotV2MemoryHotplugExtentClass>,
}

impl PreparedSnapshotV2MemoryHotplugMemory {
    /// Returns the unchanged exact state/image binding.
    pub const fn binding(&self) -> &SnapshotV2MemoryBinding {
        &self.binding
    }

    /// Returns the number of original ordered extents.
    pub fn extent_count(&self) -> usize {
        self.extent_classes.len()
    }

    /// Iterates over original extents paired with their immutable class.
    pub fn classified_extents(
        &self,
    ) -> impl ExactSizeIterator<Item = SnapshotV2MemoryHotplugClassifiedExtent> + '_ {
        self.binding
            .extents()
            .iter()
            .copied()
            .zip(self.extent_classes.iter().copied())
            .map(|(extent, class)| SnapshotV2MemoryHotplugClassifiedExtent { extent, class })
    }
}

impl fmt::Debug for PreparedSnapshotV2MemoryHotplugMemory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2MemoryHotplugMemory")
            .field("version", &self.binding.version())
            .field("extent_count", &self.extent_classes.len())
            .field("binding", &REDACTED)
            .finish()
    }
}

/// Owned parts of one prepared exact-2.10 virtio-mem topology.
pub type PreparedSnapshotV2MemoryHotplugTopologyParts = (
    PreparedSnapshotV2MemoryHotplugMemory,
    Vec<GuestMemoryRange>,
    Option<[GuestMemoryRange; 3]>,
    SnapshotV2MemoryHotplugState,
    SnapshotV2MemoryHotplugControllerProjection,
);

/// Immutable owner-free exact-2.10 virtio-mem topology.
///
/// The value contains only checked snapshot facts. It owns no guest mapping,
/// memory descriptor, device, notifier, interrupt, platform slot, or VM
/// authority.
#[derive(PartialEq, Eq)]
pub struct PreparedSnapshotV2MemoryHotplugTopology {
    memory: PreparedSnapshotV2MemoryHotplugMemory,
    plugged_ranges: Vec<GuestMemoryRange>,
    queue_ranges: Option<[GuestMemoryRange; 3]>,
    state: SnapshotV2MemoryHotplugState,
    controller: SnapshotV2MemoryHotplugControllerProjection,
}

impl PreparedSnapshotV2MemoryHotplugTopology {
    /// Validates and prepares one closed kind-1/kind-11 state pair.
    pub fn prepare(
        state: SnapshotV2MemoryHotplugState,
        binding: SnapshotV2MemoryBinding,
    ) -> Result<Self, SnapshotV2MemoryHotplugPreparationError> {
        prepare_memory_hotplug_topology(state, binding, TopologyReservePolicy::System)
    }

    #[cfg(test)]
    pub(crate) fn prepare_with_extent_class_allocation_failure(
        state: SnapshotV2MemoryHotplugState,
        binding: SnapshotV2MemoryBinding,
    ) -> Result<Self, SnapshotV2MemoryHotplugPreparationError> {
        prepare_memory_hotplug_topology(state, binding, TopologyReservePolicy::FailExtentClasses)
    }

    #[cfg(test)]
    pub(crate) fn prepare_with_plugged_range_allocation_failure(
        state: SnapshotV2MemoryHotplugState,
        binding: SnapshotV2MemoryBinding,
    ) -> Result<Self, SnapshotV2MemoryHotplugPreparationError> {
        prepare_memory_hotplug_topology(state, binding, TopologyReservePolicy::FailPluggedRanges)
    }

    /// Returns the exact binding and attached extent classification.
    pub const fn memory(&self) -> &PreparedSnapshotV2MemoryHotplugMemory {
        &self.memory
    }

    /// Returns canonical maximal plugged GPA regions.
    pub fn plugged_ranges(&self) -> &[GuestMemoryRange] {
        &self.plugged_ranges
    }

    /// Returns descriptor, available-ring, and used-ring GPA ranges.
    pub const fn queue_ranges(&self) -> Option<[GuestMemoryRange; 3]> {
        self.queue_ranges
    }

    /// Returns the complete typed device and transport continuation.
    pub const fn state(&self) -> &SnapshotV2MemoryHotplugState {
        &self.state
    }

    /// Returns the checked controller configuration and requested size.
    pub const fn controller(&self) -> SnapshotV2MemoryHotplugControllerProjection {
        self.controller
    }

    /// Consumes the topology into its still-detached prepared parts.
    pub fn into_parts(self) -> PreparedSnapshotV2MemoryHotplugTopologyParts {
        (
            self.memory,
            self.plugged_ranges,
            self.queue_ranges,
            self.state,
            self.controller,
        )
    }

    /// Consumes one checked MMIO topology into a complete inert register
    /// handler against the destination mixed-memory owner.
    ///
    /// The returned value owns no dispatcher registration, notifier,
    /// interrupt route, platform slot, mapper, metrics registry, or VM
    /// authority.
    #[doc(hidden)]
    pub fn into_mmio_handler(
        self,
        destination_memory: &GuestMemory,
    ) -> Result<PreparedSnapshotV2MemoryHotplugMmioHandler, SnapshotV2MemoryHotplugMmioHandlerError>
    {
        let Self {
            memory: _,
            plugged_ranges,
            queue_ranges,
            state,
            controller,
        } = self;
        let SnapshotV2DeviceTransport::Mmio(mmio) = state.transport() else {
            return Err(SnapshotV2MemoryHotplugMmioHandlerError::WrongTransport);
        };
        let queue_state = state.virtio().queues().first().ok_or(
            SnapshotV2MemoryHotplugMmioHandlerError::QueueContinuation(
                VirtioMemQueueBuildError::QueueNotReady,
            ),
        )?;
        let transport_queue = VirtioMmioQueueState::from_parts(
            queue_state.max_size(),
            queue_state.size(),
            queue_state.ready(),
            queue_state.descriptor_table(),
            queue_state.driver_ring(),
            queue_state.device_ring(),
        );
        let active_queue = state
            .active_queue()
            .map(|cursor| {
                let queue = VirtioMemQueue::from_snapshot_state(
                    &transport_queue,
                    cursor.next_available(),
                    cursor.next_used(),
                )
                .map_err(SnapshotV2MemoryHotplugMmioHandlerError::QueueContinuation)?;
                queue
                    .validate_snapshot_state(&transport_queue, destination_memory)
                    .map_err(SnapshotV2MemoryHotplugMmioHandlerError::QueueMemory)?;
                Ok(queue)
            })
            .transpose()?;
        let activation_is_active = active_queue.is_some();
        let device = VirtioMemDevice::from_snapshot_parts(
            active_queue,
            state
                .plugged_ranges()
                .map(|range| (range.start_block(), range.block_count())),
        )
        .map_err(|_| SnapshotV2MemoryHotplugMmioHandlerError::Device)?;
        let retained = restore_mmio_transport_state_for_device_with_config_status_gate(
            VIRTIO_MEM_DEVICE_ID,
            state.virtio(),
            mmio,
            true,
        )
        .map_err(SnapshotV2MemoryHotplugMmioHandlerError::RetainedTransport)?;
        let registers = *retained.device_registers();
        let mut handler =
            VirtioMemMmioHandler::with_vendor_id_and_config_generation_and_device_config_and_activation(
                registers.device_id(),
                registers.vendor_id(),
                registers.device_features(),
                registers.config_generation(),
                &VIRTIO_MEM_QUEUE_SIZES,
                state.config_space(),
                device,
            )
            .map_err(SnapshotV2MemoryHotplugMmioHandlerError::Handler)?;
        handler
            .restore_transport_state(&retained, activation_is_active)
            .map_err(|_| SnapshotV2MemoryHotplugMmioHandlerError::Transport)?;

        let region = mmio.region();
        let interrupt_line = mmio.interrupt_line();
        let captured = handler
            .capture_memory_hotplug_state(state.config(), destination_memory)
            .map_err(SnapshotV2MemoryHotplugMmioHandlerError::Capture)?;
        let normalized = SnapshotV2MemoryHotplugState::try_from_mmio_capture(
            state.config(),
            region,
            interrupt_line,
            &captured,
        )
        .map_err(SnapshotV2MemoryHotplugMmioHandlerError::Normalize)?;
        if normalized != state {
            return Err(SnapshotV2MemoryHotplugMmioHandlerError::StateMismatch);
        }

        Ok(PreparedSnapshotV2MemoryHotplugMmioHandler {
            expected_state: state,
            controller,
            plugged_ranges,
            queue_ranges,
            region,
            interrupt_line,
            handler,
        })
    }
}

impl fmt::Debug for PreparedSnapshotV2MemoryHotplugTopology {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2MemoryHotplugTopology")
            .field("version", &self.state.compatibility_version())
            .field("extent_count", &self.memory.extent_count())
            .field("plugged_range_count", &self.plugged_ranges.len())
            .field("has_queue_ranges", &self.queue_ranges.is_some())
            .field("topology", &REDACTED)
            .finish()
    }
}

/// One checked, complete, and still-unpublished MMIO virtio-mem handler.
#[doc(hidden)]
pub struct PreparedSnapshotV2MemoryHotplugMmioHandler {
    expected_state: SnapshotV2MemoryHotplugState,
    controller: SnapshotV2MemoryHotplugControllerProjection,
    plugged_ranges: Vec<GuestMemoryRange>,
    queue_ranges: Option<[GuestMemoryRange; 3]>,
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    handler: VirtioMemMmioHandler,
}

impl PreparedSnapshotV2MemoryHotplugMmioHandler {
    pub const fn expected_state(&self) -> &SnapshotV2MemoryHotplugState {
        &self.expected_state
    }

    pub const fn controller(&self) -> SnapshotV2MemoryHotplugControllerProjection {
        self.controller
    }

    pub fn plugged_ranges(&self) -> &[GuestMemoryRange] {
        &self.plugged_ranges
    }

    pub const fn queue_ranges(&self) -> Option<[GuestMemoryRange; 3]> {
        self.queue_ranges
    }

    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    pub const fn interrupt_line(&self) -> GuestInterruptLine {
        self.interrupt_line
    }

    pub const fn handler(&self) -> &VirtioMemMmioHandler {
        &self.handler
    }

    pub fn into_parts(
        self,
    ) -> (
        SnapshotV2MemoryHotplugState,
        SnapshotV2MemoryHotplugControllerProjection,
        Vec<GuestMemoryRange>,
        Option<[GuestMemoryRange; 3]>,
        MmioRegion,
        GuestInterruptLine,
        VirtioMemMmioHandler,
    ) {
        (
            self.expected_state,
            self.controller,
            self.plugged_ranges,
            self.queue_ranges,
            self.region,
            self.interrupt_line,
            self.handler,
        )
    }
}

impl fmt::Debug for PreparedSnapshotV2MemoryHotplugMmioHandler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2MemoryHotplugMmioHandler")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Failure while materializing a checked topology as an inert MMIO handler.
#[doc(hidden)]
pub enum SnapshotV2MemoryHotplugMmioHandlerError {
    WrongTransport,
    QueueContinuation(VirtioMemQueueBuildError),
    QueueMemory(VirtioMemQueueCaptureError),
    Device,
    RetainedTransport(SnapshotV2RootTransportRestoreError),
    Handler(VirtioMmioRegisterHandlerError),
    Transport,
    Capture(VirtioMemDeviceCaptureError),
    Normalize(SnapshotV2MemoryHotplugStateCaptureError),
    StateMismatch,
}

impl fmt::Debug for SnapshotV2MemoryHotplugMmioHandlerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2MemoryHotplugMmioHandlerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::WrongTransport => "native-v2 virtio-mem plan does not select MMIO",
            Self::QueueContinuation(_) => "native-v2 virtio-mem queue continuation is invalid",
            Self::QueueMemory(_) => "native-v2 virtio-mem queue memory is invalid",
            Self::Device => "native-v2 virtio-mem device reconstruction failed",
            Self::RetainedTransport(_) => "native-v2 virtio-mem retained MMIO transport is invalid",
            Self::Handler(_) => "native-v2 virtio-mem MMIO handler construction failed",
            Self::Transport => "native-v2 virtio-mem MMIO transport restoration failed",
            Self::Capture(_) => "native-v2 virtio-mem restored handler capture failed",
            Self::Normalize(_) => "native-v2 virtio-mem restored handler normalization failed",
            Self::StateMismatch => "native-v2 virtio-mem restored handler state diverged",
        })
    }
}

impl std::error::Error for SnapshotV2MemoryHotplugMmioHandlerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueContinuation(source) => Some(source),
            Self::QueueMemory(source) => Some(source),
            Self::RetainedTransport(source) => Some(source),
            Self::Handler(source) => Some(source),
            Self::Capture(source) => Some(source),
            Self::Normalize(source) => Some(source),
            Self::WrongTransport | Self::Device | Self::Transport | Self::StateMismatch => None,
        }
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

#[derive(Clone, Copy)]
enum TopologyReservePolicy {
    System,
    #[cfg(test)]
    FailExtentClasses,
    #[cfg(test)]
    FailPluggedRanges,
}

impl TopologyReservePolicy {
    fn reserve_extent_classes(
        self,
        classes: &mut Vec<SnapshotV2MemoryHotplugExtentClass>,
        count: usize,
    ) -> Result<(), SnapshotV2MemoryHotplugPreparationError> {
        #[cfg(test)]
        if matches!(self, Self::FailExtentClasses) {
            return Err(SnapshotV2MemoryHotplugPreparationError::Allocation);
        }
        classes
            .try_reserve_exact(count)
            .map_err(|_| SnapshotV2MemoryHotplugPreparationError::Allocation)
    }

    fn reserve_plugged_ranges(
        self,
        ranges: &mut Vec<GuestMemoryRange>,
        count: usize,
    ) -> Result<(), SnapshotV2MemoryHotplugPreparationError> {
        #[cfg(test)]
        if matches!(self, Self::FailPluggedRanges) {
            return Err(SnapshotV2MemoryHotplugPreparationError::Allocation);
        }
        ranges
            .try_reserve_exact(count)
            .map_err(|_| SnapshotV2MemoryHotplugPreparationError::Allocation)
    }
}

fn prepare_memory_hotplug_topology(
    state: SnapshotV2MemoryHotplugState,
    binding: SnapshotV2MemoryBinding,
    reserve_policy: TopologyReservePolicy,
) -> Result<PreparedSnapshotV2MemoryHotplugTopology, SnapshotV2MemoryHotplugPreparationError> {
    validate_memory_hotplug_state(&state)
        .map_err(|_| SnapshotV2MemoryHotplugPreparationError::InvalidState)?;
    state
        .validate_memory_binding(&binding)
        .map_err(SnapshotV2MemoryHotplugPreparationError::Binding)?;

    let config_space = state.config_space();
    let aperture = GuestMemoryRange::new(
        GuestAddress::new(config_space.addr()),
        config_space.region_size(),
    )
    .map_err(|_| SnapshotV2MemoryHotplugPreparationError::Aperture)?;
    let queue = state
        .virtio()
        .queues()
        .first()
        .ok_or(SnapshotV2MemoryHotplugPreparationError::InvalidState)?;
    let restored_queue_ranges =
        queue_ranges(queue).map_err(|_| SnapshotV2MemoryHotplugPreparationError::Queue)?;
    if let Some(ranges) = restored_queue_ranges {
        for range in ranges {
            validate_prepared_queue_range(&state, &binding, aperture, range)?;
        }
    }

    let requested_size = config_space.requested_size();
    if !requested_size.is_multiple_of(MIB) {
        return Err(SnapshotV2MemoryHotplugPreparationError::Controller);
    }
    let requested_size_mib = requested_size / MIB;
    if requested_size_mib.checked_mul(MIB) != Some(requested_size) {
        return Err(SnapshotV2MemoryHotplugPreparationError::Controller);
    }
    let update = state
        .config()
        .validate_size_update(MemoryHotplugSizeUpdateInput::new(requested_size_mib))
        .map_err(|_| SnapshotV2MemoryHotplugPreparationError::Controller)?;
    if update.requested_size() != requested_size {
        return Err(SnapshotV2MemoryHotplugPreparationError::Controller);
    }
    let controller = SnapshotV2MemoryHotplugControllerProjection {
        config: state.config(),
        requested_size_mib,
    };

    let mut extent_classes = Vec::new();
    reserve_policy.reserve_extent_classes(&mut extent_classes, binding.extents().len())?;
    for extent in binding.extents() {
        let class = if range_is_wholly_contained(aperture, extent.range()) {
            SnapshotV2MemoryHotplugExtentClass::Dynamic
        } else {
            SnapshotV2MemoryHotplugExtentClass::Base
        };
        extent_classes.push(class);
    }

    let plugged_range_count = state.plugged_ranges().len();
    let mut plugged_ranges = Vec::new();
    reserve_policy.reserve_plugged_ranges(&mut plugged_ranges, plugged_range_count)?;
    for plugged in state.plugged_ranges() {
        plugged_ranges.push(plugged_guest_range(config_space, plugged)?);
    }

    debug_assert_eq!(binding.extents().len(), extent_classes.len());
    debug_assert_eq!(plugged_range_count, plugged_ranges.len());

    Ok(PreparedSnapshotV2MemoryHotplugTopology {
        memory: PreparedSnapshotV2MemoryHotplugMemory {
            binding,
            extent_classes,
        },
        plugged_ranges,
        queue_ranges: restored_queue_ranges,
        state,
        controller,
    })
}

fn validate_prepared_queue_range(
    state: &SnapshotV2MemoryHotplugState,
    binding: &SnapshotV2MemoryBinding,
    aperture: GuestMemoryRange,
    queue_range: GuestMemoryRange,
) -> Result<(), SnapshotV2MemoryHotplugPreparationError> {
    if queue_range.overlaps(aperture) {
        if !range_is_wholly_contained(aperture, queue_range) {
            return Err(SnapshotV2MemoryHotplugPreparationError::QueueBoundary);
        }
        for plugged in state.plugged_ranges() {
            let plugged_range = plugged_guest_range(state.config_space(), plugged)?;
            if range_is_wholly_contained(plugged_range, queue_range) {
                return Ok(());
            }
        }
        return Err(SnapshotV2MemoryHotplugPreparationError::QueueMemory);
    }

    if binding.extents().iter().any(|extent| {
        !extent.range().overlaps(aperture) && range_is_wholly_contained(extent.range(), queue_range)
    }) {
        Ok(())
    } else {
        Err(SnapshotV2MemoryHotplugPreparationError::QueueMemory)
    }
}

fn plugged_guest_range(
    config_space: VirtioMemConfigSpace,
    plugged: SnapshotV2MemoryHotplugPluggedRange,
) -> Result<GuestMemoryRange, SnapshotV2MemoryHotplugPreparationError> {
    let offset = plugged
        .start_block()
        .checked_mul(config_space.block_size())
        .ok_or(SnapshotV2MemoryHotplugPreparationError::Aperture)?;
    let length = plugged
        .block_count()
        .checked_mul(config_space.block_size())
        .ok_or(SnapshotV2MemoryHotplugPreparationError::Aperture)?;
    let start = config_space
        .addr()
        .checked_add(offset)
        .ok_or(SnapshotV2MemoryHotplugPreparationError::Aperture)?;
    GuestMemoryRange::new(GuestAddress::new(start), length)
        .map_err(|_| SnapshotV2MemoryHotplugPreparationError::Aperture)
}

const fn range_is_wholly_contained(outer: GuestMemoryRange, inner: GuestMemoryRange) -> bool {
    outer.start().raw_value() <= inner.start().raw_value()
        && inner.end_exclusive().raw_value() <= outer.end_exclusive().raw_value()
}

/// Failure while preparing owner-free exact-2.10 virtio-mem topology.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2MemoryHotplugPreparationError {
    /// The retained typed state no longer satisfies its exact profile.
    InvalidState,
    /// Kind-1 memory and kind 11 do not form one closed topology.
    Binding(SnapshotV2MemoryHotplugBindingError),
    /// The checked aperture or plugged GPA projection is invalid.
    Aperture,
    /// Queue range derivation failed.
    Queue,
    /// A queue range crosses the virtio-mem aperture boundary.
    QueueBoundary,
    /// A queue range is not contained by one planned destination region.
    QueueMemory,
    /// Controller requested-size projection is invalid.
    Controller,
    /// Bounded topology metadata could not be reserved.
    Allocation,
}

impl fmt::Debug for SnapshotV2MemoryHotplugPreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2MemoryHotplugPreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidState => "native-v2 virtio-mem preparation state is invalid",
            Self::Binding(_) => "native-v2 virtio-mem preparation binding is invalid",
            Self::Aperture => "native-v2 virtio-mem preparation aperture is invalid",
            Self::Queue => "native-v2 virtio-mem preparation queue is invalid",
            Self::QueueBoundary => "native-v2 virtio-mem preparation queue crosses the aperture",
            Self::QueueMemory => "native-v2 virtio-mem preparation queue memory is invalid",
            Self::Controller => "native-v2 virtio-mem preparation controller state is invalid",
            Self::Allocation => "native-v2 virtio-mem preparation allocation failed",
        })
    }
}

impl std::error::Error for SnapshotV2MemoryHotplugPreparationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Binding(source) => Some(source),
            Self::InvalidState
            | Self::Aperture
            | Self::Queue
            | Self::QueueBoundary
            | Self::QueueMemory
            | Self::Controller
            | Self::Allocation => None,
        }
    }
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
