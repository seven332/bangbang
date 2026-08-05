//! Canonical detached native-v2 2.9 balloon state profile.
//!
//! This module contains only inert balloon configuration, guest-visible
//! state, queue cursors, statistics, hint history, exact host accounting,
//! common virtio registers, and transport placement. Guest-memory borrows,
//! notifier and interrupt authority, timers, metrics, reclaim advisers,
//! dispatchers, threads, and cleanup ownership remain destination-local.

use std::fmt;

use crate::balloon::{
    BalloonConfig, BalloonOptionalStats, VIRTIO_BALLOON_DEVICE_ID,
    VIRTIO_BALLOON_FREE_PAGE_HINT_DONE, VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
    VIRTIO_BALLOON_PAGE_SIZE, VIRTIO_BALLOON_QUEUE_SIZE, VIRTIO_BALLOON_S_ALLOC_STALL,
    VIRTIO_BALLOON_S_ASYNC_RECLAIM, VIRTIO_BALLOON_S_ASYNC_SCAN, VIRTIO_BALLOON_S_AVAIL,
    VIRTIO_BALLOON_S_CACHES, VIRTIO_BALLOON_S_DIRECT_RECLAIM, VIRTIO_BALLOON_S_DIRECT_SCAN,
    VIRTIO_BALLOON_S_HTLB_PGALLOC, VIRTIO_BALLOON_S_HTLB_PGFAIL, VIRTIO_BALLOON_S_MAJFLT,
    VIRTIO_BALLOON_S_MEMFREE, VIRTIO_BALLOON_S_MEMTOT, VIRTIO_BALLOON_S_MINFLT,
    VIRTIO_BALLOON_S_OOM_KILL, VIRTIO_BALLOON_S_SWAP_IN, VIRTIO_BALLOON_S_SWAP_OUT,
    VirtioBalloonActiveQueues, VirtioBalloonConfigSpace, VirtioBalloonDevice,
    VirtioBalloonDeviceCaptureState, VirtioBalloonMemoryAccounting, VirtioBalloonMmioCaptureState,
    VirtioBalloonMmioHandler, VirtioBalloonPciCaptureState, VirtioBalloonPfnRange,
    VirtioBalloonQueue, VirtioBalloonQueueCaptureState, VirtioBalloonQueueConfig,
    VirtioBalloonQueueLayout, VirtioBalloonStat, available_features, mib_to_4k_pages,
};
use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestAddress, GuestMemory, GuestMemoryRange};
use crate::message_interrupt::GuestMessageInterruptRegistry;
use crate::metrics::SharedBalloonDeviceMetrics;
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::pci::{
    PCI_BAR64_SIZE, PCI_BAR64_START, PCI_BUS_ZERO, PCI_FIRST_ENDPOINT_DEVICE, PCI_FUNCTION_ZERO,
    PCI_LAST_ENDPOINT_DEVICE, PCI_SEGMENT_ZERO, PciBarAddressSpace, PciBarPrefetchable, PciSbdf,
};
use crate::snapshot_device_v2::{
    SnapshotV2DeviceGraphCaptureError, SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind,
    SnapshotV2InterruptIntent, SnapshotV2PciDeviceState, SnapshotV2PciMsixState,
    SnapshotV2VirtioQueueState, SnapshotV2VirtioState,
    capture_mmio_common_for_device_with_queue_count_and_config_status_gate,
    capture_mmio_transport_parts, capture_pci_common_for_device_with_queue_count,
    capture_pci_transport_parts_with_queue_count, range_is_wholly_contained,
    restore_mmio_transport_state_for_device_with_config_status_gate,
};
use crate::snapshot_device_v2_5::queue_ranges;
use crate::snapshot_format::SnapshotFormatVersion;
use crate::storage_capture::StorageDeviceOrigin;
use crate::virtio::{
    VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DEVICE_NEEDS_RESET,
    VIRTIO_DEVICE_STATUS_DRIVER, VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FAILED,
    VIRTIO_DEVICE_STATUS_FEATURES_OK, VIRTIO_DEVICE_STATUS_INIT, VirtioDeviceType,
};
use crate::virtio_mmio::{
    VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VIRTIO_MMIO_VERSION_1_FEATURE, VirtioMmioQueueState,
    VirtioMmioTransportState,
};
use crate::virtio_pci::{
    PreparedVirtioPciEndpoint, VIRTIO_PCI_CAPABILITY_BAR_INDEX, VIRTIO_PCI_CAPABILITY_BAR_SIZE,
    VIRTIO_PCI_MAX_MSIX_VECTORS, VIRTIO_PCI_NO_VECTOR, VirtioPciEndpointError,
    VirtioPciEndpointPhase, VirtioPciIdentity, VirtioPciTransportState,
};

mod codec;

#[cfg(test)]
mod tests;

/// Exact compatibility context of the optional balloon component.
pub const NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION: SnapshotFormatVersion =
    SnapshotFormatVersion::new(2, 9, 0);

/// Maximum complete exact-2.9 balloon component size.
pub const NATIVE_V2_BALLOON_STATE_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Maximum number of canonical inflated-PFN ranges in one balloon component.
pub const NATIVE_V2_BALLOON_STATE_MAX_ACCOUNTING_RANGES: usize = 262_144;

/// Fixed exact-2.9 balloon component header size.
pub const NATIVE_V2_BALLOON_STATE_HEADER_BYTES: usize = 64;

/// Fixed encoded size of one balloon section-directory entry.
pub const NATIVE_V2_BALLOON_STATE_SECTION_ENTRY_BYTES: usize = 32;

/// Fixed encoded size of the balloon-local section.
pub const NATIVE_V2_BALLOON_STATE_LOCAL_BYTES: usize = 256;

/// Number of latest optional balloon-statistic values retained by profile 1.
pub const NATIVE_V2_BALLOON_STATISTIC_COUNT: usize = 16;

const REDACTED: &str = "<redacted>";

/// One checked active virtio-balloon queue cursor pair.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2BalloonQueueState {
    next_available: u16,
    next_used: u16,
}

impl SnapshotV2BalloonQueueState {
    /// Constructs cursors whose wrapping outstanding distance fits a queue.
    pub fn try_new(
        next_available: u16,
        next_used: u16,
        queue_size: u16,
    ) -> Result<Self, SnapshotV2BalloonStateBuildError> {
        let state = Self {
            next_available,
            next_used,
        };
        if queue_size == 0 || state.outstanding() > queue_size {
            return Err(SnapshotV2BalloonStateBuildError::Queue);
        }
        Ok(state)
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

    /// Returns the wrapping consumed-but-not-published descriptor count.
    pub const fn outstanding(self) -> u16 {
        self.next_available.wrapping_sub(self.next_used)
    }
}

impl fmt::Debug for SnapshotV2BalloonQueueState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2BalloonQueueState")
            .field("cursors", &REDACTED)
            .finish()
    }
}

/// Detached cursors for every active queue in the configured balloon layout.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2BalloonActiveQueuesState {
    inflate: SnapshotV2BalloonQueueState,
    deflate: SnapshotV2BalloonQueueState,
    statistics: Option<SnapshotV2BalloonQueueState>,
    free_page_hinting: Option<SnapshotV2BalloonQueueState>,
    free_page_reporting: Option<SnapshotV2BalloonQueueState>,
}

impl SnapshotV2BalloonActiveQueuesState {
    /// Constructs a complete active cursor set for one external configuration.
    pub fn try_new(
        config: BalloonConfig,
        inflate: SnapshotV2BalloonQueueState,
        deflate: SnapshotV2BalloonQueueState,
        statistics: Option<SnapshotV2BalloonQueueState>,
        free_page_hinting: Option<SnapshotV2BalloonQueueState>,
        free_page_reporting: Option<SnapshotV2BalloonQueueState>,
    ) -> Result<Self, SnapshotV2BalloonStateBuildError> {
        let state = Self {
            inflate,
            deflate,
            statistics,
            free_page_hinting,
            free_page_reporting,
        };
        validate_active_queue_shape(config, state)?;
        Ok(state)
    }

    pub(crate) const fn from_parts(
        inflate: SnapshotV2BalloonQueueState,
        deflate: SnapshotV2BalloonQueueState,
        statistics: Option<SnapshotV2BalloonQueueState>,
        free_page_hinting: Option<SnapshotV2BalloonQueueState>,
        free_page_reporting: Option<SnapshotV2BalloonQueueState>,
    ) -> Self {
        Self {
            inflate,
            deflate,
            statistics,
            free_page_hinting,
            free_page_reporting,
        }
    }

    /// Returns inflate-queue cursors.
    pub const fn inflate(self) -> SnapshotV2BalloonQueueState {
        self.inflate
    }

    /// Returns deflate-queue cursors.
    pub const fn deflate(self) -> SnapshotV2BalloonQueueState {
        self.deflate
    }

    /// Returns optional statistics-queue cursors.
    pub const fn statistics(self) -> Option<SnapshotV2BalloonQueueState> {
        self.statistics
    }

    /// Returns optional free-page-hinting queue cursors.
    pub const fn free_page_hinting(self) -> Option<SnapshotV2BalloonQueueState> {
        self.free_page_hinting
    }

    /// Returns optional free-page-reporting queue cursors.
    pub const fn free_page_reporting(self) -> Option<SnapshotV2BalloonQueueState> {
        self.free_page_reporting
    }

    fn cursor_for_layout(
        self,
        layout: VirtioBalloonQueueLayout,
        index: usize,
    ) -> Option<SnapshotV2BalloonQueueState> {
        [
            (Some(layout.inflate()), Some(self.inflate)),
            (Some(layout.deflate()), Some(self.deflate)),
            (layout.statistics(), self.statistics),
            (layout.free_page_hinting(), self.free_page_hinting),
            (layout.free_page_reporting(), self.free_page_reporting),
        ]
        .into_iter()
        .find_map(|(queue, cursor)| {
            queue
                .is_some_and(|queue| queue.index() == index)
                .then_some(cursor)
                .flatten()
        })
    }
}

impl fmt::Debug for SnapshotV2BalloonActiveQueuesState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2BalloonActiveQueuesState")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Exact latest optional balloon statistics in canonical API field order.
///
/// The order is swap-in, swap-out, major faults, minor faults, free memory,
/// total memory, available memory, disk caches, hugetlb allocations, hugetlb
/// failures, OOM kills, allocation stalls, asynchronous scans, direct scans,
/// asynchronous reclaims, and direct reclaims.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2BalloonStatistics {
    values: [Option<u64>; NATIVE_V2_BALLOON_STATISTIC_COUNT],
}

impl SnapshotV2BalloonStatistics {
    /// Constructs an exact fixed-order optional-statistics value.
    pub const fn new(values: [Option<u64>; NATIVE_V2_BALLOON_STATISTIC_COUNT]) -> Self {
        Self { values }
    }

    /// Returns the canonical fixed-order values.
    pub const fn values(&self) -> &[Option<u64>; NATIVE_V2_BALLOON_STATISTIC_COUNT] {
        &self.values
    }

    /// Returns whether no latest statistic is present.
    pub fn is_empty(self) -> bool {
        self.values.iter().all(Option::is_none)
    }
}

impl Default for SnapshotV2BalloonStatistics {
    fn default() -> Self {
        Self::new([None; NATIVE_V2_BALLOON_STATISTIC_COUNT])
    }
}

impl fmt::Debug for SnapshotV2BalloonStatistics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2BalloonStatistics")
            .field("values", &REDACTED)
            .finish()
    }
}

/// Detached free-page-hinting continuation history.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2BalloonHintState {
    host_cmd: u32,
    guest_cmd: Option<u32>,
    last_cmd: u32,
    acknowledge_on_stop: bool,
}

impl SnapshotV2BalloonHintState {
    /// Retains one captured hint history for validation by the complete state.
    pub const fn new(
        host_cmd: u32,
        guest_cmd: Option<u32>,
        last_cmd: u32,
        acknowledge_on_stop: bool,
    ) -> Self {
        Self {
            host_cmd,
            guest_cmd,
            last_cmd,
            acknowledge_on_stop,
        }
    }

    /// Returns the captured host command.
    pub const fn host_cmd(self) -> u32 {
        self.host_cmd
    }

    /// Returns the latest observed guest command.
    pub const fn guest_cmd(self) -> Option<u32> {
        self.guest_cmd
    }

    /// Returns the last allocated host command identifier.
    pub const fn last_cmd(self) -> u32 {
        self.last_cmd
    }

    /// Returns the retained stop-acknowledgement policy.
    pub const fn acknowledge_on_stop(self) -> bool {
        self.acknowledge_on_stop
    }
}

impl fmt::Debug for SnapshotV2BalloonHintState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2BalloonHintState")
            .field("state", &REDACTED)
            .finish()
    }
}

/// One exact canonical host inflated-PFN range.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2BalloonPfnRange {
    start_pfn: u32,
    page_count: u32,
}

impl SnapshotV2BalloonPfnRange {
    /// Constructs one nonempty range that fits the complete u32 PFN domain.
    pub fn try_new(
        start_pfn: u32,
        page_count: u32,
    ) -> Result<Self, SnapshotV2BalloonStateBuildError> {
        let range = Self {
            start_pfn,
            page_count,
        };
        if page_count == 0 || range.end_pfn_exclusive() > u64::from(u32::MAX) + 1 {
            return Err(SnapshotV2BalloonStateBuildError::Accounting);
        }
        Ok(range)
    }

    pub(crate) const fn from_parts(start_pfn: u32, page_count: u32) -> Self {
        Self {
            start_pfn,
            page_count,
        }
    }

    /// Returns the first inflated PFN.
    pub const fn start_pfn(self) -> u32 {
        self.start_pfn
    }

    /// Returns the nonzero number of inflated pages.
    pub const fn page_count(self) -> u32 {
        self.page_count
    }

    const fn end_pfn_exclusive(self) -> u64 {
        self.start_pfn as u64 + self.page_count as u64
    }
}

impl fmt::Debug for SnapshotV2BalloonPfnRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2BalloonPfnRange")
            .field("range", &REDACTED)
            .finish()
    }
}

/// Exact canonical host inflated-page accounting.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2BalloonAccountingState {
    ranges: Vec<SnapshotV2BalloonPfnRange>,
    inflated_page_count: u64,
}

impl SnapshotV2BalloonAccountingState {
    /// Constructs canonical accounting and verifies its independently retained total.
    pub fn try_new(
        ranges: Vec<SnapshotV2BalloonPfnRange>,
        inflated_page_count: u64,
    ) -> Result<Self, SnapshotV2BalloonStateBuildError> {
        let state = Self {
            ranges,
            inflated_page_count,
        };
        validate_accounting(&state)?;
        Ok(state)
    }

    /// Returns an empty host-accounting value.
    pub const fn empty() -> Self {
        Self {
            ranges: Vec::new(),
            inflated_page_count: 0,
        }
    }

    /// Returns exact canonical inflated-PFN ranges.
    pub fn ranges(&self) -> &[SnapshotV2BalloonPfnRange] {
        &self.ranges
    }

    /// Returns the checked host inflated-page total.
    pub const fn inflated_page_count(&self) -> u64 {
        self.inflated_page_count
    }

    /// Returns whether no page is retained as inflated.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}

impl fmt::Debug for SnapshotV2BalloonAccountingState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2BalloonAccountingState")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Host-time-free balloon queue/statistics/hint continuation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2BalloonContinuationState {
    active_queues: Option<SnapshotV2BalloonActiveQueuesState>,
    stats_polling_interval_s: u16,
    statistics: SnapshotV2BalloonStatistics,
    statistics_pending_descriptor_head: Option<u16>,
    hinting: SnapshotV2BalloonHintState,
}

impl SnapshotV2BalloonContinuationState {
    /// Retains detached continuation fields for complete-state validation.
    pub const fn new(
        active_queues: Option<SnapshotV2BalloonActiveQueuesState>,
        stats_polling_interval_s: u16,
        statistics: SnapshotV2BalloonStatistics,
        statistics_pending_descriptor_head: Option<u16>,
        hinting: SnapshotV2BalloonHintState,
    ) -> Self {
        Self {
            active_queues,
            stats_polling_interval_s,
            statistics,
            statistics_pending_descriptor_head,
            hinting,
        }
    }

    /// Returns exact active queue cursors, if activated.
    pub const fn active_queues(&self) -> Option<SnapshotV2BalloonActiveQueuesState> {
        self.active_queues
    }

    /// Returns the retained polling interval without a timer or deadline.
    pub const fn stats_polling_interval_s(&self) -> u16 {
        self.stats_polling_interval_s
    }

    /// Returns latest optional statistics.
    pub const fn statistics(&self) -> SnapshotV2BalloonStatistics {
        self.statistics
    }

    /// Returns a consumed but not yet returned statistics descriptor head.
    pub const fn statistics_pending_descriptor_head(&self) -> Option<u16> {
        self.statistics_pending_descriptor_head
    }

    /// Returns complete captured hint history.
    pub const fn hinting(&self) -> SnapshotV2BalloonHintState {
        self.hinting
    }
}

impl fmt::Debug for SnapshotV2BalloonContinuationState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2BalloonContinuationState")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete inert exact-2.9 balloon component value.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2BalloonState {
    config: BalloonConfig,
    config_space: VirtioBalloonConfigSpace,
    continuation: SnapshotV2BalloonContinuationState,
    accounting: SnapshotV2BalloonAccountingState,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
}

impl SnapshotV2BalloonState {
    /// Converts one checked MMIO live capture without retaining source
    /// ownership.
    pub fn try_from_mmio_capture(
        config: BalloonConfig,
        region: MmioRegion,
        interrupt_line: GuestInterruptLine,
        captured: &VirtioBalloonMmioCaptureState,
    ) -> Result<Self, SnapshotV2BalloonStateCaptureError> {
        preflight_balloon_capture(captured.device())?;
        let queue_count = captured.device().queue_layout().queue_count();
        let virtio = capture_mmio_common_for_device_with_queue_count_and_config_status_gate(
            captured.transport(),
            VIRTIO_BALLOON_DEVICE_ID,
            available_features(config),
            queue_count,
            true,
        )
        .map_err(capture_common_error)?;
        let transport = SnapshotV2DeviceTransport::Mmio(capture_mmio_transport_parts(
            region,
            interrupt_line,
            captured.transport(),
        ));
        capture_balloon_state(
            config,
            captured.device(),
            virtio,
            transport,
            reserve_balloon_capture_ranges,
        )
    }

    /// Converts one checked startup-origin PCI live capture without retaining
    /// source ownership.
    pub fn try_from_pci_capture(
        config: BalloonConfig,
        sbdf: PciSbdf,
        bar_range: GuestMemoryRange,
        captured: &VirtioBalloonPciCaptureState,
    ) -> Result<Self, SnapshotV2BalloonStateCaptureError> {
        preflight_balloon_capture(captured.device())?;
        let queue_count = captured.device().queue_layout().queue_count();
        let virtio = capture_pci_common_for_device_with_queue_count(
            captured.transport(),
            VIRTIO_BALLOON_DEVICE_ID,
            available_features(config),
            queue_count,
        )
        .map_err(capture_common_error)?;
        let transport = capture_pci_transport_parts_with_queue_count(
            StorageDeviceOrigin::Startup,
            sbdf,
            bar_range,
            captured.transport(),
            queue_count,
        )
        .map(SnapshotV2DeviceTransport::Pci)
        .map_err(capture_common_error)?;
        capture_balloon_state(
            config,
            captured.device(),
            virtio,
            transport,
            reserve_balloon_capture_ranges,
        )
    }

    #[cfg(test)]
    pub(crate) fn try_from_mmio_capture_with_accounting_allocation_failure(
        config: BalloonConfig,
        region: MmioRegion,
        interrupt_line: GuestInterruptLine,
        captured: &VirtioBalloonMmioCaptureState,
    ) -> Result<Self, SnapshotV2BalloonStateCaptureError> {
        preflight_balloon_capture(captured.device())?;
        let queue_count = captured.device().queue_layout().queue_count();
        let virtio = capture_mmio_common_for_device_with_queue_count_and_config_status_gate(
            captured.transport(),
            VIRTIO_BALLOON_DEVICE_ID,
            available_features(config),
            queue_count,
            true,
        )
        .map_err(capture_common_error)?;
        let transport = SnapshotV2DeviceTransport::Mmio(capture_mmio_transport_parts(
            region,
            interrupt_line,
            captured.transport(),
        ));
        capture_balloon_state(config, captured.device(), virtio, transport, |_, _| {
            Err(SnapshotV2BalloonStateCaptureError::Allocation)
        })
    }

    /// Constructs and fully validates one detached balloon artifact value.
    pub fn try_new(
        config: BalloonConfig,
        config_space: VirtioBalloonConfigSpace,
        continuation: SnapshotV2BalloonContinuationState,
        accounting: SnapshotV2BalloonAccountingState,
        virtio: SnapshotV2VirtioState,
        transport: SnapshotV2DeviceTransport,
    ) -> Result<Self, SnapshotV2BalloonStateBuildError> {
        let state = Self {
            config,
            config_space,
            continuation,
            accounting,
            virtio,
            transport,
        };
        validate_balloon_state(&state)?;
        Ok(state)
    }

    /// Returns the exact compatibility context of this value.
    pub const fn compatibility_version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION
    }

    /// Returns the external balloon configuration.
    pub const fn config(&self) -> BalloonConfig {
        self.config
    }

    /// Returns exact guest-visible configuration state.
    pub const fn config_space(&self) -> VirtioBalloonConfigSpace {
        self.config_space
    }

    /// Returns host-time-free queue/statistics/hint continuation.
    pub const fn continuation(&self) -> &SnapshotV2BalloonContinuationState {
        &self.continuation
    }

    /// Returns exact canonical host inflated-page accounting.
    pub const fn accounting(&self) -> &SnapshotV2BalloonAccountingState {
        &self.accounting
    }

    /// Returns transport-neutral common virtio state.
    pub const fn virtio(&self) -> &SnapshotV2VirtioState {
        &self.virtio
    }

    /// Returns exact detached MMIO or PCI placement.
    pub const fn transport(&self) -> &SnapshotV2DeviceTransport {
        &self.transport
    }

    /// Encodes one canonical exact-2.9 profile-1 payload.
    pub fn encode(
        &self,
        version: SnapshotFormatVersion,
    ) -> Result<Vec<u8>, SnapshotV2BalloonStateEncodeError> {
        codec::encode(version, self)
    }

    /// Decodes and validates one canonical exact-2.9 profile-1 payload.
    pub fn decode(
        version: SnapshotFormatVersion,
        bytes: &[u8],
    ) -> Result<Self, SnapshotV2BalloonStateDecodeError> {
        codec::decode(version, bytes)
    }
}

impl fmt::Debug for SnapshotV2BalloonState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2BalloonState")
            .field("version", &NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION)
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete exact-2.9 balloon continuation validated against adopted memory.
///
/// The plan contains only detached runtime and transport values. It owns no
/// timer, notifier, interrupt route, dispatcher, PCI endpoint, metrics,
/// reclaim adviser, thread, platform VM, or publication authority.
pub struct SnapshotV2BalloonRestorePlan {
    config: BalloonConfig,
    config_space: VirtioBalloonConfigSpace,
    queue_ranges: Vec<[GuestMemoryRange; 3]>,
    transport: PreparedSnapshotV2BalloonTransport,
}

impl SnapshotV2BalloonRestorePlan {
    /// Validates and reconstructs one decoded balloon continuation.
    pub fn prepare(
        state: SnapshotV2BalloonState,
        memory: &GuestMemory,
    ) -> Result<Self, SnapshotV2BalloonRestorePlanError> {
        prepare_balloon_restore_plan(state, memory, BalloonRestoreReservePolicy::System)
    }

    #[cfg(test)]
    pub(crate) fn prepare_with_queue_range_allocation_failure(
        state: SnapshotV2BalloonState,
        memory: &GuestMemory,
    ) -> Result<Self, SnapshotV2BalloonRestorePlanError> {
        prepare_balloon_restore_plan(state, memory, BalloonRestoreReservePolicy::FailQueueRanges)
    }

    #[cfg(test)]
    pub(crate) fn prepare_with_accounting_allocation_failure(
        state: SnapshotV2BalloonState,
        memory: &GuestMemory,
    ) -> Result<Self, SnapshotV2BalloonRestorePlanError> {
        prepare_balloon_restore_plan(state, memory, BalloonRestoreReservePolicy::FailAccounting)
    }

    /// Returns the exact external balloon configuration.
    pub const fn config(&self) -> BalloonConfig {
        self.config
    }

    /// Returns guest-visible configuration after destination normalization.
    pub const fn config_space(&self) -> VirtioBalloonConfigSpace {
        self.config_space
    }

    /// Returns all loaded-memory ranges occupied by active queues.
    pub fn queue_ranges(&self) -> &[[GuestMemoryRange; 3]] {
        &self.queue_ranges
    }

    /// Returns the selected transport kind.
    pub const fn transport_kind(&self) -> SnapshotV2DeviceTransportKind {
        self.transport.kind()
    }

    /// Returns the checked detached transport.
    pub const fn transport(&self) -> &PreparedSnapshotV2BalloonTransport {
        &self.transport
    }

    /// Consumes the plan into configuration, ranges, and detached transport.
    pub fn into_parts(
        self,
    ) -> (
        BalloonConfig,
        VirtioBalloonConfigSpace,
        Vec<[GuestMemoryRange; 3]>,
        PreparedSnapshotV2BalloonTransport,
    ) {
        (
            self.config,
            self.config_space,
            self.queue_ranges,
            self.transport,
        )
    }

    /// Consumes a checked MMIO plan into one complete inert register handler.
    ///
    /// The returned value still owns no dispatcher registration, interrupt
    /// route, metrics, discard adviser, scheduler, notifier, or VM authority.
    #[doc(hidden)]
    pub fn into_mmio_handler(
        self,
    ) -> Result<PreparedSnapshotV2BalloonMmioHandler, SnapshotV2BalloonMmioHandlerError> {
        let Self {
            config,
            config_space,
            queue_ranges,
            transport,
        } = self;
        let PreparedSnapshotV2BalloonTransport::Mmio(mmio) = transport else {
            return Err(SnapshotV2BalloonMmioHandlerError::WrongTransport);
        };
        let (region, interrupt_line, device, retained) = mmio.into_parts();
        let activation_is_active = device.is_activated();
        let queue_sizes = device.queue_layout().queue_sizes();
        let registers = *retained.device_registers();
        let mut handler =
            VirtioBalloonMmioHandler::with_vendor_id_and_config_generation_and_device_config_and_activation(
                registers.device_id(),
                registers.vendor_id(),
                registers.device_features(),
                registers.config_generation(),
                queue_sizes.as_slice(),
                config_space,
                device,
            )
            .map_err(|_| SnapshotV2BalloonMmioHandlerError::Handler)?;
        handler
            .restore_transport_state(&retained, activation_is_active)
            .map_err(|_| SnapshotV2BalloonMmioHandlerError::Transport)?;

        Ok(PreparedSnapshotV2BalloonMmioHandler {
            config,
            queue_ranges,
            region,
            interrupt_line,
            handler,
        })
    }

    /// Consumes a checked PCI plan into one complete retained endpoint.
    ///
    /// The caller supplies one fresh destination message registry and the
    /// dispatcher region reserved by the destination platform plan. The
    /// returned value still owns no route resources, dispatcher registration,
    /// BAR/function lease, metrics, discard adviser, scheduler, or VM
    /// authority.
    #[doc(hidden)]
    pub fn into_pci_endpoint(
        self,
        region_id: MmioRegionId,
        messages: GuestMessageInterruptRegistry,
    ) -> Result<PreparedSnapshotV2BalloonPciEndpoint, SnapshotV2BalloonPciEndpointError> {
        let Self {
            config,
            config_space,
            queue_ranges,
            transport,
        } = self;
        let PreparedSnapshotV2BalloonTransport::Pci(pci) = transport else {
            return Err(SnapshotV2BalloonPciEndpointError::WrongTransport);
        };
        let (origin, sbdf, bar_range, identity, device, retained) = pci.into_parts();
        let activation_is_active = device.is_activated();
        let queue_sizes = device.queue_layout().queue_sizes();
        let endpoint = PreparedVirtioPciEndpoint::new(
            identity,
            queue_sizes.as_slice(),
            config_space,
            device,
            activation_is_active,
            false,
            &retained,
            sbdf,
            bar_range,
            region_id,
            messages,
        )
        .map_err(SnapshotV2BalloonPciEndpointError::Endpoint)?;

        Ok(PreparedSnapshotV2BalloonPciEndpoint {
            config,
            queue_ranges,
            origin,
            endpoint,
        })
    }
}

impl fmt::Debug for SnapshotV2BalloonRestorePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2BalloonRestorePlan")
            .field("transport", &self.transport_kind())
            .field("state", &REDACTED)
            .finish()
    }
}

/// One checked detached MMIO or PCI balloon transport.
pub enum PreparedSnapshotV2BalloonTransport {
    /// Value-only virtio-mmio continuation.
    Mmio(Box<PreparedSnapshotV2BalloonMmioTransport>),
    /// Value-only virtio-pci continuation.
    Pci(Box<PreparedSnapshotV2BalloonPciTransport>),
}

impl PreparedSnapshotV2BalloonTransport {
    /// Returns the selected transport kind.
    pub const fn kind(&self) -> SnapshotV2DeviceTransportKind {
        match self {
            Self::Mmio(_) => SnapshotV2DeviceTransportKind::Mmio,
            Self::Pci(_) => SnapshotV2DeviceTransportKind::Pci,
        }
    }
}

impl fmt::Debug for PreparedSnapshotV2BalloonTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2BalloonTransport")
            .field("kind", &self.kind())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Checked value-only MMIO balloon continuation.
pub struct PreparedSnapshotV2BalloonMmioTransport {
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    device: VirtioBalloonDevice,
    retained: VirtioMmioTransportState,
}

impl PreparedSnapshotV2BalloonMmioTransport {
    /// Returns the exact retained MMIO region.
    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    /// Returns the exact retained guest interrupt line.
    pub const fn interrupt_line(&self) -> GuestInterruptLine {
        self.interrupt_line
    }

    /// Returns the detached balloon device.
    pub const fn device(&self) -> &VirtioBalloonDevice {
        &self.device
    }

    /// Returns detached MMIO register, queue, and interrupt state.
    pub const fn retained(&self) -> &VirtioMmioTransportState {
        &self.retained
    }

    /// Consumes the value into placement, device, and retained transport.
    pub fn into_parts(
        self,
    ) -> (
        MmioRegion,
        GuestInterruptLine,
        VirtioBalloonDevice,
        VirtioMmioTransportState,
    ) {
        (self.region, self.interrupt_line, self.device, self.retained)
    }
}

impl fmt::Debug for PreparedSnapshotV2BalloonMmioTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2BalloonMmioTransport")
            .field("state", &REDACTED)
            .finish()
    }
}

/// One checked, complete, and still unpublished MMIO balloon handler.
///
/// Destination-local dispatcher, interrupt, metrics, discard, process
/// scheduler, VM, and cleanup owners are intentionally absent.
#[doc(hidden)]
pub struct PreparedSnapshotV2BalloonMmioHandler {
    config: BalloonConfig,
    queue_ranges: Vec<[GuestMemoryRange; 3]>,
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    handler: VirtioBalloonMmioHandler,
}

impl PreparedSnapshotV2BalloonMmioHandler {
    /// Returns the exact public balloon configuration.
    pub const fn config(&self) -> BalloonConfig {
        self.config
    }

    /// Returns every loaded-memory range occupied by an active queue.
    pub fn queue_ranges(&self) -> &[[GuestMemoryRange; 3]] {
        &self.queue_ranges
    }

    /// Returns the exact retained MMIO region.
    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    /// Returns the exact retained guest interrupt line.
    pub const fn interrupt_line(&self) -> GuestInterruptLine {
        self.interrupt_line
    }

    /// Returns the fully restored, still-unpublished handler.
    pub const fn handler(&self) -> &VirtioBalloonMmioHandler {
        &self.handler
    }

    /// Attaches fresh destination-local metrics before publication.
    pub fn attach_metrics(&mut self, metrics: SharedBalloonDeviceMetrics) {
        self.handler.attach_balloon_metrics(metrics);
    }

    /// Consumes the value into configuration, queue ranges, placement, and
    /// inert handler.
    pub fn into_parts(
        self,
    ) -> (
        BalloonConfig,
        Vec<[GuestMemoryRange; 3]>,
        MmioRegion,
        GuestInterruptLine,
        VirtioBalloonMmioHandler,
    ) {
        (
            self.config,
            self.queue_ranges,
            self.region,
            self.interrupt_line,
            self.handler,
        )
    }
}

impl fmt::Debug for PreparedSnapshotV2BalloonMmioHandler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2BalloonMmioHandler")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Failure while materializing a checked balloon plan as an MMIO handler.
#[derive(Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum SnapshotV2BalloonMmioHandlerError {
    /// The checked plan selects PCI rather than MMIO.
    WrongTransport,
    /// The exact variable-queue handler could not be built.
    Handler,
    /// The retained common MMIO state could not be applied.
    Transport,
}

impl fmt::Debug for SnapshotV2BalloonMmioHandlerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2BalloonMmioHandlerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::WrongTransport => "native-v2 balloon restore plan is not MMIO",
            Self::Handler => "native-v2 balloon MMIO handler construction failed",
            Self::Transport => "native-v2 balloon MMIO handler state is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2BalloonMmioHandlerError {}

/// Checked value-only PCI balloon continuation.
pub struct PreparedSnapshotV2BalloonPciTransport {
    origin: StorageDeviceOrigin,
    sbdf: PciSbdf,
    bar_range: GuestMemoryRange,
    identity: VirtioPciIdentity,
    device: VirtioBalloonDevice,
    retained: VirtioPciTransportState,
}

impl PreparedSnapshotV2BalloonPciTransport {
    /// Returns the retained startup/runtime origin.
    pub const fn origin(&self) -> StorageDeviceOrigin {
        self.origin
    }

    /// Returns the exact retained PCI function.
    pub const fn sbdf(&self) -> PciSbdf {
        self.sbdf
    }

    /// Returns the exact retained capability BAR.
    pub const fn bar_range(&self) -> GuestMemoryRange {
        self.bar_range
    }

    /// Returns the fixed balloon PCI identity.
    pub const fn identity(&self) -> VirtioPciIdentity {
        self.identity
    }

    /// Returns the detached balloon device.
    pub const fn device(&self) -> &VirtioBalloonDevice {
        &self.device
    }

    /// Returns detached PCI configuration, queue, and MSI-X state.
    pub const fn retained(&self) -> &VirtioPciTransportState {
        &self.retained
    }

    /// Consumes placement, identity, device, and retained transport.
    pub fn into_parts(
        self,
    ) -> (
        StorageDeviceOrigin,
        PciSbdf,
        GuestMemoryRange,
        VirtioPciIdentity,
        VirtioBalloonDevice,
        VirtioPciTransportState,
    ) {
        (
            self.origin,
            self.sbdf,
            self.bar_range,
            self.identity,
            self.device,
            self.retained,
        )
    }
}

impl fmt::Debug for PreparedSnapshotV2BalloonPciTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2BalloonPciTransport")
            .field("state", &REDACTED)
            .finish()
    }
}

/// One checked exact-2.9 balloon endpoint awaiting destination PCI
/// publication.
#[doc(hidden)]
pub struct PreparedSnapshotV2BalloonPciEndpoint {
    config: BalloonConfig,
    queue_ranges: Vec<[GuestMemoryRange; 3]>,
    origin: StorageDeviceOrigin,
    endpoint: PreparedVirtioPciEndpoint<VirtioBalloonConfigSpace, VirtioBalloonDevice>,
}

/// Consumed checked balloon continuation and retained PCI endpoint.
#[doc(hidden)]
pub type PreparedSnapshotV2BalloonPciEndpointParts = (
    BalloonConfig,
    Vec<[GuestMemoryRange; 3]>,
    StorageDeviceOrigin,
    PreparedVirtioPciEndpoint<VirtioBalloonConfigSpace, VirtioBalloonDevice>,
);

impl PreparedSnapshotV2BalloonPciEndpoint {
    /// Returns the exact public balloon configuration.
    pub const fn config(&self) -> BalloonConfig {
        self.config
    }

    /// Returns every loaded-memory range occupied by an active queue.
    pub fn queue_ranges(&self) -> &[[GuestMemoryRange; 3]] {
        &self.queue_ranges
    }

    /// Returns the retained startup/runtime origin.
    pub const fn origin(&self) -> StorageDeviceOrigin {
        self.origin
    }

    /// Returns the complete retained endpoint before publication.
    pub const fn endpoint(
        &self,
    ) -> &PreparedVirtioPciEndpoint<VirtioBalloonConfigSpace, VirtioBalloonDevice> {
        &self.endpoint
    }

    /// Attaches fresh destination-local metrics before publication.
    pub fn attach_metrics(
        &self,
        metrics: SharedBalloonDeviceMetrics,
    ) -> Result<(), VirtioPciEndpointError> {
        self.endpoint.endpoint().attach_balloon_metrics(metrics)
    }

    /// Consumes the checked continuation and retained endpoint.
    pub fn into_parts(self) -> PreparedSnapshotV2BalloonPciEndpointParts {
        (self.config, self.queue_ranges, self.origin, self.endpoint)
    }
}

impl fmt::Debug for PreparedSnapshotV2BalloonPciEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2BalloonPciEndpoint")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Failure while binding a checked balloon plan to a destination PCI
/// registry.
#[doc(hidden)]
pub enum SnapshotV2BalloonPciEndpointError {
    /// The checked plan selects MMIO rather than PCI.
    WrongTransport,
    /// The retained endpoint could not be reconstructed exactly.
    Endpoint(VirtioPciEndpointError),
}

impl fmt::Debug for SnapshotV2BalloonPciEndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::WrongTransport => "wrong-transport",
            Self::Endpoint(_) => "endpoint",
        };
        formatter
            .debug_struct("SnapshotV2BalloonPciEndpointError")
            .field("kind", &kind)
            .field("source", &REDACTED)
            .finish()
    }
}

impl fmt::Display for SnapshotV2BalloonPciEndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::WrongTransport => "native-v2 balloon restore plan is not PCI",
            Self::Endpoint(_) => "native-v2 balloon PCI endpoint reconstruction failed",
        })
    }
}

impl std::error::Error for SnapshotV2BalloonPciEndpointError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::WrongTransport => None,
            Self::Endpoint(source) => Some(source),
        }
    }
}

/// Failure while proving decoded balloon state against destination memory.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2BalloonRestorePlanError {
    /// The decoded typed state no longer satisfies the exact profile.
    InvalidState,
    /// A bounded restore inventory could not be allocated.
    Allocation,
    /// A queue range is not wholly contained by adopted guest memory.
    QueueMemory,
    /// Queue cursors, rings, or retained statistics work are inconsistent.
    QueueContinuation,
    /// A retained inflated-PFN range is not fully mapped.
    AccountingMemory,
    /// Exact host inflated-PFN accounting could not be reconstructed.
    Accounting,
    /// Detached balloon device continuation could not be reconstructed.
    Device,
    /// The fixed balloon virtio identity cannot be represented.
    DeviceType,
    /// Detached MMIO retained state reconstruction failed.
    MmioTransport,
    /// Detached PCI retained state reconstruction failed.
    PciTransport,
}

impl fmt::Debug for SnapshotV2BalloonRestorePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2BalloonRestorePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidState => "native-v2 balloon restore state is invalid",
            Self::Allocation => "native-v2 balloon restore allocation failed",
            Self::QueueMemory => "native-v2 balloon restore queue memory is invalid",
            Self::QueueContinuation => "native-v2 balloon restore queue continuation is invalid",
            Self::AccountingMemory => "native-v2 balloon restore accounting memory is invalid",
            Self::Accounting => "native-v2 balloon restore accounting state is invalid",
            Self::Device => "native-v2 balloon restore device state is invalid",
            Self::DeviceType => "native-v2 balloon restore device identity is invalid",
            Self::MmioTransport => "native-v2 balloon MMIO retained state is invalid",
            Self::PciTransport => "native-v2 balloon PCI retained state is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2BalloonRestorePlanError {}

#[derive(Clone, Copy)]
enum BalloonRestoreReservePolicy {
    System,
    #[cfg(test)]
    FailQueueRanges,
    #[cfg(test)]
    FailAccounting,
}

impl BalloonRestoreReservePolicy {
    fn reserve_queue_ranges(
        self,
        ranges: &mut Vec<[GuestMemoryRange; 3]>,
        count: usize,
    ) -> Result<(), SnapshotV2BalloonRestorePlanError> {
        #[cfg(test)]
        if matches!(self, Self::FailQueueRanges) {
            return Err(SnapshotV2BalloonRestorePlanError::Allocation);
        }
        ranges
            .try_reserve_exact(count)
            .map_err(|_| SnapshotV2BalloonRestorePlanError::Allocation)
    }

    fn reserve_accounting(
        self,
        ranges: &mut Vec<VirtioBalloonPfnRange>,
        count: usize,
    ) -> Result<(), SnapshotV2BalloonRestorePlanError> {
        #[cfg(test)]
        if matches!(self, Self::FailAccounting) {
            return Err(SnapshotV2BalloonRestorePlanError::Allocation);
        }
        ranges
            .try_reserve_exact(count)
            .map_err(|_| SnapshotV2BalloonRestorePlanError::Allocation)
    }
}

fn prepare_balloon_restore_plan(
    state: SnapshotV2BalloonState,
    memory: &GuestMemory,
    reserve_policy: BalloonRestoreReservePolicy,
) -> Result<SnapshotV2BalloonRestorePlan, SnapshotV2BalloonRestorePlanError> {
    validate_balloon_state(&state).map_err(|_| SnapshotV2BalloonRestorePlanError::InvalidState)?;

    let queue_layout = VirtioBalloonQueueLayout::from_config(state.config);
    let mut restored_queue_ranges = Vec::new();
    reserve_policy.reserve_queue_ranges(&mut restored_queue_ranges, state.virtio.queues().len())?;
    for queue in state.virtio.queues() {
        let Some(ranges) =
            queue_ranges(queue).map_err(|_| SnapshotV2BalloonRestorePlanError::InvalidState)?
        else {
            continue;
        };
        if ranges
            .iter()
            .copied()
            .any(|range| !range_is_wholly_contained(memory, range))
        {
            return Err(SnapshotV2BalloonRestorePlanError::QueueMemory);
        }
        restored_queue_ranges.push(ranges);
    }

    for range in state.accounting.ranges().iter().copied() {
        let range = balloon_accounting_guest_range(range)?;
        memory
            .validate_mapped_range(range)
            .map_err(|_| SnapshotV2BalloonRestorePlanError::AccountingMemory)?;
    }

    let SnapshotV2BalloonState {
        config,
        mut config_space,
        continuation,
        accounting,
        virtio,
        transport,
    } = state;
    let SnapshotV2BalloonContinuationState {
        active_queues,
        stats_polling_interval_s,
        statistics,
        statistics_pending_descriptor_head,
        hinting,
    } = continuation;

    let mut accounting_ranges = Vec::new();
    reserve_policy.reserve_accounting(&mut accounting_ranges, accounting.ranges.len())?;
    for range in accounting.ranges {
        accounting_ranges.push(
            VirtioBalloonPfnRange::from_snapshot_parts(range.start_pfn(), range.page_count())
                .map_err(|_| SnapshotV2BalloonRestorePlanError::Accounting)?,
        );
    }
    let accounting = VirtioBalloonMemoryAccounting::from_snapshot_ranges(
        accounting_ranges,
        accounting.inflated_page_count,
    )
    .map_err(|_| SnapshotV2BalloonRestorePlanError::Accounting)?;

    let active_queues = active_queues
        .map(|active| {
            restore_balloon_active_queues(
                queue_layout,
                active,
                statistics_pending_descriptor_head,
                &virtio,
                memory,
            )
        })
        .transpose()?;
    let statistics = restore_balloon_statistics(statistics);

    let hinting_host_cmd = if config.free_page_hinting() {
        config_space = config_space.with_free_page_hint_cmd_id(VIRTIO_BALLOON_FREE_PAGE_HINT_DONE);
        VIRTIO_BALLOON_FREE_PAGE_HINT_DONE
    } else {
        VIRTIO_BALLOON_FREE_PAGE_HINT_STOP
    };
    let device = VirtioBalloonDevice::from_snapshot_parts(
        queue_layout,
        active_queues,
        accounting,
        stats_polling_interval_s,
        statistics,
        statistics_pending_descriptor_head,
        hinting_host_cmd,
        hinting.guest_cmd(),
        hinting.last_cmd(),
        hinting.acknowledge_on_stop(),
    )
    .map_err(|_| SnapshotV2BalloonRestorePlanError::Device)?;

    let transport = match transport {
        SnapshotV2DeviceTransport::Mmio(mmio) => {
            let retained = restore_mmio_transport_state_for_device_with_config_status_gate(
                VIRTIO_BALLOON_DEVICE_ID,
                &virtio,
                &mmio,
                true,
            )
            .map_err(|_| SnapshotV2BalloonRestorePlanError::MmioTransport)?;
            PreparedSnapshotV2BalloonTransport::Mmio(Box::new(
                PreparedSnapshotV2BalloonMmioTransport {
                    region: mmio.region(),
                    interrupt_line: mmio.interrupt_line(),
                    device,
                    retained,
                },
            ))
        }
        SnapshotV2DeviceTransport::Pci(pci) => {
            let device_type = VirtioDeviceType::new(VIRTIO_BALLOON_DEVICE_ID)
                .map_err(|_| SnapshotV2BalloonRestorePlanError::DeviceType)?;
            let identity = VirtioPciIdentity::new(device_type, virtio.available_features())
                .with_config_generation(virtio.config_generation());
            let retained =
                VirtioPciTransportState::from_snapshot_v2_parts(identity, &virtio, &pci, false)
                    .map_err(|_| SnapshotV2BalloonRestorePlanError::PciTransport)?;
            PreparedSnapshotV2BalloonTransport::Pci(Box::new(
                PreparedSnapshotV2BalloonPciTransport {
                    origin: pci.origin(),
                    sbdf: pci.sbdf(),
                    bar_range: pci.bar_range(),
                    identity,
                    device,
                    retained,
                },
            ))
        }
    };

    Ok(SnapshotV2BalloonRestorePlan {
        config,
        config_space,
        queue_ranges: restored_queue_ranges,
        transport,
    })
}

fn balloon_accounting_guest_range(
    range: SnapshotV2BalloonPfnRange,
) -> Result<GuestMemoryRange, SnapshotV2BalloonRestorePlanError> {
    let start = u64::from(range.start_pfn())
        .checked_mul(VIRTIO_BALLOON_PAGE_SIZE)
        .ok_or(SnapshotV2BalloonRestorePlanError::Accounting)?;
    let size = u64::from(range.page_count())
        .checked_mul(VIRTIO_BALLOON_PAGE_SIZE)
        .ok_or(SnapshotV2BalloonRestorePlanError::Accounting)?;
    GuestMemoryRange::new(GuestAddress::new(start), size)
        .map_err(|_| SnapshotV2BalloonRestorePlanError::Accounting)
}

fn restore_balloon_active_queues(
    layout: VirtioBalloonQueueLayout,
    active: SnapshotV2BalloonActiveQueuesState,
    statistics_pending_descriptor_head: Option<u16>,
    virtio: &SnapshotV2VirtioState,
    memory: &GuestMemory,
) -> Result<VirtioBalloonActiveQueues, SnapshotV2BalloonRestorePlanError> {
    let inflate = restore_balloon_queue(layout.inflate(), active.inflate(), None, virtio, memory)?;
    let deflate = restore_balloon_queue(layout.deflate(), active.deflate(), None, virtio, memory)?;
    let statistics = restore_optional_balloon_queue(
        layout.statistics(),
        active.statistics(),
        statistics_pending_descriptor_head,
        virtio,
        memory,
    )?;
    let free_page_hinting = restore_optional_balloon_queue(
        layout.free_page_hinting(),
        active.free_page_hinting(),
        None,
        virtio,
        memory,
    )?;
    let free_page_reporting = restore_optional_balloon_queue(
        layout.free_page_reporting(),
        active.free_page_reporting(),
        None,
        virtio,
        memory,
    )?;

    VirtioBalloonActiveQueues::from_snapshot_parts(
        layout,
        inflate,
        deflate,
        statistics,
        free_page_hinting,
        free_page_reporting,
    )
    .map_err(|_| SnapshotV2BalloonRestorePlanError::QueueContinuation)
}

fn restore_optional_balloon_queue(
    config: Option<VirtioBalloonQueueConfig>,
    cursor: Option<SnapshotV2BalloonQueueState>,
    statistics_pending_descriptor_head: Option<u16>,
    virtio: &SnapshotV2VirtioState,
    memory: &GuestMemory,
) -> Result<Option<VirtioBalloonQueue>, SnapshotV2BalloonRestorePlanError> {
    match (config, cursor) {
        (Some(config), Some(cursor)) => restore_balloon_queue(
            config,
            cursor,
            statistics_pending_descriptor_head,
            virtio,
            memory,
        )
        .map(Some),
        (None, None) => Ok(None),
        _ => Err(SnapshotV2BalloonRestorePlanError::QueueContinuation),
    }
}

fn restore_balloon_queue(
    config: VirtioBalloonQueueConfig,
    cursor: SnapshotV2BalloonQueueState,
    statistics_pending_descriptor_head: Option<u16>,
    virtio: &SnapshotV2VirtioState,
    memory: &GuestMemory,
) -> Result<VirtioBalloonQueue, SnapshotV2BalloonRestorePlanError> {
    let queue = virtio
        .queues()
        .get(config.index())
        .ok_or(SnapshotV2BalloonRestorePlanError::QueueContinuation)?;
    let queue = VirtioMmioQueueState::from_parts(
        queue.max_size(),
        queue.size(),
        queue.ready(),
        queue.descriptor_table(),
        queue.driver_ring(),
        queue.device_ring(),
    );
    let queue = VirtioBalloonQueue::from_snapshot_state(
        &queue,
        cursor.next_available(),
        cursor.next_used(),
    )
    .map_err(|_| SnapshotV2BalloonRestorePlanError::QueueContinuation)?;
    queue
        .validate_snapshot_state(memory, statistics_pending_descriptor_head)
        .map_err(|_| SnapshotV2BalloonRestorePlanError::QueueContinuation)?;
    Ok(queue)
}

fn restore_balloon_statistics(statistics: SnapshotV2BalloonStatistics) -> BalloonOptionalStats {
    let tags = [
        VIRTIO_BALLOON_S_SWAP_IN,
        VIRTIO_BALLOON_S_SWAP_OUT,
        VIRTIO_BALLOON_S_MAJFLT,
        VIRTIO_BALLOON_S_MINFLT,
        VIRTIO_BALLOON_S_MEMFREE,
        VIRTIO_BALLOON_S_MEMTOT,
        VIRTIO_BALLOON_S_AVAIL,
        VIRTIO_BALLOON_S_CACHES,
        VIRTIO_BALLOON_S_HTLB_PGALLOC,
        VIRTIO_BALLOON_S_HTLB_PGFAIL,
        VIRTIO_BALLOON_S_OOM_KILL,
        VIRTIO_BALLOON_S_ALLOC_STALL,
        VIRTIO_BALLOON_S_ASYNC_SCAN,
        VIRTIO_BALLOON_S_DIRECT_SCAN,
        VIRTIO_BALLOON_S_ASYNC_RECLAIM,
        VIRTIO_BALLOON_S_DIRECT_RECLAIM,
    ];
    let mut restored = BalloonOptionalStats::new();
    for (tag, value) in tags.into_iter().zip(*statistics.values()) {
        if let Some(value) = value {
            let recorded = restored.record_stat(VirtioBalloonStat::new(tag, value));
            debug_assert!(recorded);
        }
    }
    restored
}

fn preflight_balloon_capture(
    device: &VirtioBalloonDeviceCaptureState,
) -> Result<(), SnapshotV2BalloonStateCaptureError> {
    if device.memory_accounting().inflated_page_ranges().len()
        > NATIVE_V2_BALLOON_STATE_MAX_ACCOUNTING_RANGES
    {
        return Err(SnapshotV2BalloonStateCaptureError::AccountingLimit);
    }
    Ok(())
}

fn capture_balloon_state(
    config: BalloonConfig,
    device: &VirtioBalloonDeviceCaptureState,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
    reserve_ranges: impl FnOnce(
        &mut Vec<SnapshotV2BalloonPfnRange>,
        usize,
    ) -> Result<(), SnapshotV2BalloonStateCaptureError>,
) -> Result<SnapshotV2BalloonState, SnapshotV2BalloonStateCaptureError> {
    if device.queue_layout() != VirtioBalloonQueueLayout::from_config(config)
        || device.available_features() != virtio.available_features()
        || device.negotiated_features() != virtio.driver_features()
        || device.active_queues().is_some() != virtio.is_activated()
    {
        return Err(SnapshotV2BalloonStateCaptureError::Device);
    }

    let active_queues = device
        .active_queues()
        .map(|active| {
            SnapshotV2BalloonActiveQueuesState::try_new(
                config,
                capture_balloon_queue(active.inflate())?,
                capture_balloon_queue(active.deflate())?,
                active.statistics().map(capture_balloon_queue).transpose()?,
                active
                    .free_page_hinting()
                    .map(capture_balloon_queue)
                    .transpose()?,
                active
                    .free_page_reporting()
                    .map(capture_balloon_queue)
                    .transpose()?,
            )
            .map_err(|_| SnapshotV2BalloonStateCaptureError::Queue)
        })
        .transpose()?;
    let hinting = device.hinting();
    let continuation = SnapshotV2BalloonContinuationState::new(
        active_queues,
        device.stats_polling_interval_s(),
        capture_balloon_statistics(device.statistics()),
        device.statistics_pending_descriptor_head(),
        SnapshotV2BalloonHintState::new(
            hinting.host_cmd(),
            hinting.guest_cmd(),
            hinting.last_cmd(),
            hinting.acknowledge_on_stop(),
        ),
    );

    let captured_ranges = device.memory_accounting().inflated_page_ranges();
    let mut ranges = Vec::new();
    reserve_ranges(&mut ranges, captured_ranges.len())?;
    for captured in captured_ranges {
        ranges.push(
            SnapshotV2BalloonPfnRange::try_new(captured.start_pfn(), captured.page_count())
                .map_err(|_| SnapshotV2BalloonStateCaptureError::Accounting)?,
        );
    }
    let accounting = SnapshotV2BalloonAccountingState::try_new(
        ranges,
        device.memory_accounting().inflated_page_count(),
    )
    .map_err(|_| SnapshotV2BalloonStateCaptureError::Accounting)?;

    SnapshotV2BalloonState::try_new(
        config,
        device.config_space(),
        continuation,
        accounting,
        virtio,
        transport,
    )
    .map_err(capture_build_error)
}

fn reserve_balloon_capture_ranges(
    ranges: &mut Vec<SnapshotV2BalloonPfnRange>,
    count: usize,
) -> Result<(), SnapshotV2BalloonStateCaptureError> {
    ranges
        .try_reserve_exact(count)
        .map_err(|_| SnapshotV2BalloonStateCaptureError::Allocation)
}

fn capture_balloon_queue(
    captured: VirtioBalloonQueueCaptureState,
) -> Result<SnapshotV2BalloonQueueState, SnapshotV2BalloonStateCaptureError> {
    SnapshotV2BalloonQueueState::try_new(
        captured.next_available(),
        captured.next_used(),
        VIRTIO_BALLOON_QUEUE_SIZE,
    )
    .map_err(|_| SnapshotV2BalloonStateCaptureError::Queue)
}

fn capture_balloon_statistics(captured: BalloonOptionalStats) -> SnapshotV2BalloonStatistics {
    SnapshotV2BalloonStatistics::new([
        captured.swap_in(),
        captured.swap_out(),
        captured.major_faults(),
        captured.minor_faults(),
        captured.free_memory(),
        captured.total_memory(),
        captured.available_memory(),
        captured.disk_caches(),
        captured.hugetlb_allocations(),
        captured.hugetlb_failures(),
        captured.oom_kill(),
        captured.alloc_stall(),
        captured.async_scan(),
        captured.direct_scan(),
        captured.async_reclaim(),
        captured.direct_reclaim(),
    ])
}

fn capture_common_error(
    source: SnapshotV2DeviceGraphCaptureError,
) -> SnapshotV2BalloonStateCaptureError {
    if source == SnapshotV2DeviceGraphCaptureError::Allocation {
        SnapshotV2BalloonStateCaptureError::Allocation
    } else {
        SnapshotV2BalloonStateCaptureError::Common { source }
    }
}

fn capture_build_error(
    source: SnapshotV2BalloonStateBuildError,
) -> SnapshotV2BalloonStateCaptureError {
    match source {
        SnapshotV2BalloonStateBuildError::Configuration
        | SnapshotV2BalloonStateBuildError::Virtio => SnapshotV2BalloonStateCaptureError::Device,
        SnapshotV2BalloonStateBuildError::Queue => SnapshotV2BalloonStateCaptureError::Queue,
        SnapshotV2BalloonStateBuildError::Statistics => {
            SnapshotV2BalloonStateCaptureError::Statistics
        }
        SnapshotV2BalloonStateBuildError::Hinting => SnapshotV2BalloonStateCaptureError::Hinting,
        SnapshotV2BalloonStateBuildError::Accounting => {
            SnapshotV2BalloonStateCaptureError::Accounting
        }
        source => SnapshotV2BalloonStateCaptureError::Build { source },
    }
}

/// Failure while converting one trusted live capture into exact-2.9 state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2BalloonStateCaptureError {
    /// The canonical live accounting inventory exceeds the exact profile.
    AccountingLimit,
    /// A bounded detached artifact collection could not be allocated.
    Allocation,
    /// Repeated device, configuration, and transport state disagree.
    Device,
    /// Active queue cursors or shape are inconsistent.
    Queue,
    /// Captured latest statistics or pending work are inconsistent.
    Statistics,
    /// Captured free-page-hint continuation is inconsistent.
    Hinting,
    /// Canonical live PFN accounting is inconsistent.
    Accounting,
    /// Common virtio or transport capture failed.
    Common {
        /// Redacted common capture category.
        source: SnapshotV2DeviceGraphCaptureError,
    },
    /// Complete converted state failed its final semantic gate.
    Build {
        /// Redacted exact-2.9 build category.
        source: SnapshotV2BalloonStateBuildError,
    },
}

impl fmt::Debug for SnapshotV2BalloonStateCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2BalloonStateCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AccountingLimit => {
                "native-v2 captured balloon accounting exceeds the range limit"
            }
            Self::Allocation => "native-v2 captured balloon state allocation failed",
            Self::Device => "native-v2 captured balloon device state is inconsistent",
            Self::Queue => "native-v2 captured balloon queue state is invalid",
            Self::Statistics => "native-v2 captured balloon statistics state is invalid",
            Self::Hinting => "native-v2 captured balloon hint state is invalid",
            Self::Accounting => "native-v2 captured balloon accounting state is invalid",
            Self::Common { .. } => "native-v2 captured balloon transport state is invalid",
            Self::Build { .. } => "native-v2 captured balloon state is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2BalloonStateCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Common { source } => Some(source),
            Self::Build { source } => Some(source),
            Self::AccountingLimit
            | Self::Allocation
            | Self::Device
            | Self::Queue
            | Self::Statistics
            | Self::Hinting
            | Self::Accounting => None,
        }
    }
}

/// Invalid relationship in a detached exact-2.9 balloon value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2BalloonStateBuildError {
    /// External configuration and guest-visible state disagree.
    Configuration,
    /// Common virtio features, status, activation, or inventory are invalid.
    Virtio,
    /// Queue registers, cursors, or ring geometry are invalid.
    Queue,
    /// Latest statistics or retained descriptor state is invalid.
    Statistics,
    /// Captured free-page-hint history is invalid.
    Hinting,
    /// Exact host inflated-page accounting is invalid.
    Accounting,
    /// MMIO or PCI transport state is invalid.
    Transport,
    /// Queue rings overlap their transport placement.
    Placement,
}

impl fmt::Display for SnapshotV2BalloonStateBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration => formatter.write_str("balloon configuration state is invalid"),
            Self::Virtio => formatter.write_str("balloon common virtio state is invalid"),
            Self::Queue => formatter.write_str("balloon queue state is invalid"),
            Self::Statistics => formatter.write_str("balloon statistics state is invalid"),
            Self::Hinting => formatter.write_str("balloon hinting state is invalid"),
            Self::Accounting => formatter.write_str("balloon accounting state is invalid"),
            Self::Transport => formatter.write_str("balloon transport state is invalid"),
            Self::Placement => formatter.write_str("balloon placement state is invalid"),
        }
    }
}

impl std::error::Error for SnapshotV2BalloonStateBuildError {}

/// Exact-2.9 balloon payload encoding failure.
#[derive(Debug)]
pub enum SnapshotV2BalloonStateEncodeError {
    /// The requested outer compatibility context is not exact 2.9.
    UnsupportedVersion,
    /// The detached state has an invalid relationship.
    InvalidState(SnapshotV2BalloonStateBuildError),
    /// Encoded length arithmetic overflowed.
    LengthOverflow,
    /// The complete component exceeds its fixed profile limit.
    TooLarge,
    /// Output allocation failed.
    Allocation,
}

impl fmt::Display for SnapshotV2BalloonStateEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion => {
                formatter.write_str("native-v2 balloon state version is unsupported")
            }
            Self::InvalidState(source) => write!(formatter, "invalid balloon state: {source}"),
            Self::LengthOverflow => {
                formatter.write_str("native-v2 balloon state length arithmetic overflowed")
            }
            Self::TooLarge => formatter.write_str("native-v2 balloon state exceeds its size limit"),
            Self::Allocation => {
                formatter.write_str("native-v2 balloon state output allocation failed")
            }
        }
    }
}

impl std::error::Error for SnapshotV2BalloonStateEncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidState(source) => Some(source),
            _ => None,
        }
    }
}

/// Exact-2.9 balloon payload decoding failure.
#[derive(Debug)]
pub enum SnapshotV2BalloonStateDecodeError {
    /// The requested outer compatibility context is not exact 2.9.
    UnsupportedVersion,
    /// The payload is shorter than a required fixed field.
    Truncated,
    /// The private component magic is invalid.
    InvalidMagic,
    /// Header, directory, section, or exact-consumption structure is invalid.
    InvalidStructure,
    /// The private payload profile is unsupported.
    InvalidProfile,
    /// The encoded transport tag is unsupported or inconsistent.
    InvalidTransport,
    /// A reserved field or padding byte is nonzero.
    NonzeroReserved,
    /// One encoded field value is not canonical.
    InvalidValue,
    /// The complete component exceeds its fixed profile limit.
    TooLarge,
    /// A bounded decoded value could not be allocated.
    Allocation,
    /// Decoded fields do not form a valid detached balloon value.
    InvalidState(SnapshotV2BalloonStateBuildError),
}

impl fmt::Display for SnapshotV2BalloonStateDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion => {
                formatter.write_str("native-v2 balloon state version is unsupported")
            }
            Self::Truncated => formatter.write_str("native-v2 balloon state is truncated"),
            Self::InvalidMagic => formatter.write_str("native-v2 balloon state magic is invalid"),
            Self::InvalidStructure => {
                formatter.write_str("native-v2 balloon state structure is invalid")
            }
            Self::InvalidProfile => {
                formatter.write_str("native-v2 balloon state profile is unsupported")
            }
            Self::InvalidTransport => {
                formatter.write_str("native-v2 balloon state transport is invalid")
            }
            Self::NonzeroReserved => {
                formatter.write_str("native-v2 balloon state reserved bytes are nonzero")
            }
            Self::InvalidValue => formatter.write_str("native-v2 balloon state value is invalid"),
            Self::TooLarge => formatter.write_str("native-v2 balloon state exceeds its size limit"),
            Self::Allocation => formatter.write_str("native-v2 balloon state allocation failed"),
            Self::InvalidState(source) => {
                write!(
                    formatter,
                    "decoded native-v2 balloon state is invalid: {source}"
                )
            }
        }
    }
}

impl std::error::Error for SnapshotV2BalloonStateDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidState(source) => Some(source),
            _ => None,
        }
    }
}

pub(crate) fn validate_balloon_state(
    state: &SnapshotV2BalloonState,
) -> Result<(), SnapshotV2BalloonStateBuildError> {
    if mib_to_4k_pages(state.config.amount_mib()).ok() != Some(state.config_space.num_pages())
        || state.config.stats_polling_interval_s() != state.continuation.stats_polling_interval_s
    {
        return Err(SnapshotV2BalloonStateBuildError::Configuration);
    }

    validate_accounting(&state.accounting)?;
    validate_hinting(state)?;
    validate_virtio(state)?;
    validate_transport(state)?;
    validate_queue_placement(state)?;
    Ok(())
}

fn validate_active_queue_shape(
    config: BalloonConfig,
    active: SnapshotV2BalloonActiveQueuesState,
) -> Result<(), SnapshotV2BalloonStateBuildError> {
    let layout = VirtioBalloonQueueLayout::from_config(config);
    if active.statistics.is_some() != layout.statistics().is_some()
        || active.free_page_hinting.is_some() != layout.free_page_hinting().is_some()
        || active.free_page_reporting.is_some() != layout.free_page_reporting().is_some()
    {
        Err(SnapshotV2BalloonStateBuildError::Queue)
    } else {
        Ok(())
    }
}

fn validate_accounting(
    accounting: &SnapshotV2BalloonAccountingState,
) -> Result<(), SnapshotV2BalloonStateBuildError> {
    if accounting.ranges.len() > NATIVE_V2_BALLOON_STATE_MAX_ACCOUNTING_RANGES {
        return Err(SnapshotV2BalloonStateBuildError::Accounting);
    }
    let mut previous_end = None;
    let mut total = 0_u64;
    for range in accounting.ranges.iter().copied() {
        let start = u64::from(range.start_pfn);
        let end = range.end_pfn_exclusive();
        if range.page_count == 0
            || end <= start
            || end > u64::from(u32::MAX) + 1
            || previous_end.is_some_and(|previous| start <= previous)
        {
            return Err(SnapshotV2BalloonStateBuildError::Accounting);
        }
        total = total
            .checked_add(u64::from(range.page_count))
            .ok_or(SnapshotV2BalloonStateBuildError::Accounting)?;
        previous_end = Some(end);
    }
    if total != accounting.inflated_page_count {
        return Err(SnapshotV2BalloonStateBuildError::Accounting);
    }
    Ok(())
}

fn validate_hinting(
    state: &SnapshotV2BalloonState,
) -> Result<(), SnapshotV2BalloonStateBuildError> {
    let hinting = state.continuation.hinting;
    if state.config_space.free_page_hint_cmd_id() != hinting.host_cmd {
        return Err(SnapshotV2BalloonStateBuildError::Hinting);
    }
    if !state.config.free_page_hinting() {
        if hinting.host_cmd != VIRTIO_BALLOON_FREE_PAGE_HINT_STOP
            || hinting.guest_cmd.is_some()
            || hinting.last_cmd != VIRTIO_BALLOON_FREE_PAGE_HINT_STOP
            || !hinting.acknowledge_on_stop
        {
            return Err(SnapshotV2BalloonStateBuildError::Hinting);
        }
        return Ok(());
    }
    if hinting.last_cmd == VIRTIO_BALLOON_FREE_PAGE_HINT_DONE
        || (hinting.host_cmd > VIRTIO_BALLOON_FREE_PAGE_HINT_DONE
            && hinting.host_cmd != hinting.last_cmd)
    {
        return Err(SnapshotV2BalloonStateBuildError::Hinting);
    }
    Ok(())
}

fn validate_virtio(state: &SnapshotV2BalloonState) -> Result<(), SnapshotV2BalloonStateBuildError> {
    let common = &state.virtio;
    let expected_features = available_features(state.config);
    let layout = VirtioBalloonQueueLayout::from_config(state.config);
    let queue_count = layout.queue_count();
    if common.available_features() != expected_features
        || common.driver_features() & !common.available_features() != 0
        || common.queues().len() != queue_count
        || common.pending_notifications().len() > queue_count
        || common.interrupt_intents().len() > queue_count + 1
    {
        return Err(SnapshotV2BalloonStateBuildError::Virtio);
    }

    let healthy_driver_ok = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK
        | VIRTIO_DEVICE_STATUS_DRIVER_OK;
    let healthy_statuses = [
        VIRTIO_DEVICE_STATUS_INIT,
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
            | VIRTIO_DEVICE_STATUS_DRIVER
            | VIRTIO_DEVICE_STATUS_FEATURES_OK,
        healthy_driver_ok,
    ];
    if !healthy_statuses.contains(&common.status())
        || common.status() & (VIRTIO_DEVICE_STATUS_FAILED | VIRTIO_DEVICE_STATUS_DEVICE_NEEDS_RESET)
            != 0
        || (common.status() & VIRTIO_DEVICE_STATUS_DRIVER == 0 && common.driver_features() != 0)
        || (common.status() & VIRTIO_DEVICE_STATUS_FEATURES_OK != 0
            && common.driver_features() & VIRTIO_MMIO_VERSION_1_FEATURE == 0)
        || common.is_activated() != (common.status() == healthy_driver_ok)
        || common.is_activated() != state.continuation.active_queues.is_some()
    {
        return Err(SnapshotV2BalloonStateBuildError::Virtio);
    }

    if let Some(active) = state.continuation.active_queues {
        validate_active_queue_shape(state.config, active)?;
    }

    for queue in common.queues() {
        validate_queue(queue, common.status(), common.is_activated())?;
    }
    validate_cross_queue_ranges(common.queues())?;

    if !common.is_activated()
        && (!common.pending_notifications().is_empty()
            || state
                .continuation
                .statistics_pending_descriptor_head
                .is_some()
            || !state.continuation.statistics.is_empty()
            || !state.accounting.is_empty())
    {
        return Err(SnapshotV2BalloonStateBuildError::Virtio);
    }

    if !common
        .pending_notifications()
        .windows(2)
        .all(|window| matches!(window, [first, second] if first < second))
        || common
            .pending_notifications()
            .iter()
            .copied()
            .any(|index| usize::from(index) >= queue_count)
        || !common
            .interrupt_intents()
            .windows(2)
            .all(|window| matches!(window, [first, second] if first < second))
    {
        return Err(SnapshotV2BalloonStateBuildError::Virtio);
    }

    validate_cursor_and_statistics_relationships(state, layout)?;
    validate_interrupt_intents(state, queue_count)?;
    Ok(())
}

fn validate_queue(
    queue: &SnapshotV2VirtioQueueState,
    status: u32,
    activated: bool,
) -> Result<(), SnapshotV2BalloonStateBuildError> {
    if queue.max_size() != VIRTIO_BALLOON_QUEUE_SIZE
        || (queue.size() != 0
            && (!queue.size().is_power_of_two() || queue.size() > queue.max_size()))
        || (queue.ready() && queue.size() == 0)
        || !queue
            .descriptor_table()
            .raw_value()
            .is_multiple_of(crate::virtio_queue::VIRTQUEUE_DESCRIPTOR_ALIGNMENT)
        || !queue
            .driver_ring()
            .raw_value()
            .is_multiple_of(crate::virtio_queue::VIRTQUEUE_AVAILABLE_RING_ALIGNMENT)
        || !queue
            .device_ring()
            .raw_value()
            .is_multiple_of(crate::virtio_queue::VIRTQUEUE_USED_RING_ALIGNMENT)
    {
        return Err(SnapshotV2BalloonStateBuildError::Queue);
    }
    if activated && !queue.ready() {
        return Err(SnapshotV2BalloonStateBuildError::Queue);
    }
    if status & VIRTIO_DEVICE_STATUS_FEATURES_OK == 0
        && (queue.size() != 0
            || queue.ready()
            || queue.descriptor_table().raw_value() != 0
            || queue.driver_ring().raw_value() != 0
            || queue.device_ring().raw_value() != 0)
    {
        return Err(SnapshotV2BalloonStateBuildError::Queue);
    }
    let ranges = queue_ranges(queue).map_err(|_| SnapshotV2BalloonStateBuildError::Queue)?;
    if ranges.is_some_and(|ranges| {
        ranges[0].overlaps(ranges[1])
            || ranges[0].overlaps(ranges[2])
            || ranges[1].overlaps(ranges[2])
    }) {
        return Err(SnapshotV2BalloonStateBuildError::Queue);
    }
    Ok(())
}

fn validate_cross_queue_ranges(
    queues: &[SnapshotV2VirtioQueueState],
) -> Result<(), SnapshotV2BalloonStateBuildError> {
    for (index, queue) in queues.iter().enumerate() {
        let Some(current) =
            queue_ranges(queue).map_err(|_| SnapshotV2BalloonStateBuildError::Queue)?
        else {
            continue;
        };
        for previous_queue in queues
            .get(..index)
            .ok_or(SnapshotV2BalloonStateBuildError::Queue)?
        {
            let Some(previous) = queue_ranges(previous_queue)
                .map_err(|_| SnapshotV2BalloonStateBuildError::Queue)?
            else {
                continue;
            };
            if current
                .iter()
                .any(|current| previous.iter().any(|previous| current.overlaps(*previous)))
            {
                return Err(SnapshotV2BalloonStateBuildError::Queue);
            }
        }
    }
    Ok(())
}

fn validate_cursor_and_statistics_relationships(
    state: &SnapshotV2BalloonState,
    layout: VirtioBalloonQueueLayout,
) -> Result<(), SnapshotV2BalloonStateBuildError> {
    let statistics_index = layout.statistics().map(|queue| queue.index());
    let active = state.continuation.active_queues;
    for (index, queue) in state.virtio.queues().iter().enumerate() {
        let cursor = active.and_then(|active| active.cursor_for_layout(layout, index));
        if state.virtio.is_activated() != cursor.is_some() {
            return Err(SnapshotV2BalloonStateBuildError::Queue);
        }
        if let Some(cursor) = cursor {
            let expected = u16::from(
                statistics_index == Some(index)
                    && state
                        .continuation
                        .statistics_pending_descriptor_head
                        .is_some(),
            );
            if cursor.outstanding() != expected || cursor.outstanding() > queue.size() {
                return Err(SnapshotV2BalloonStateBuildError::Queue);
            }
        }
    }

    if layout.statistics().is_none()
        && (!state.continuation.statistics.is_empty()
            || state
                .continuation
                .statistics_pending_descriptor_head
                .is_some())
    {
        return Err(SnapshotV2BalloonStateBuildError::Statistics);
    }
    if let Some(head) = state.continuation.statistics_pending_descriptor_head {
        let index = statistics_index.ok_or(SnapshotV2BalloonStateBuildError::Statistics)?;
        let queue = state
            .virtio
            .queues()
            .get(index)
            .ok_or(SnapshotV2BalloonStateBuildError::Statistics)?;
        if !state.virtio.is_activated() || head >= queue.size() {
            return Err(SnapshotV2BalloonStateBuildError::Statistics);
        }
    }
    Ok(())
}

fn validate_interrupt_intents(
    state: &SnapshotV2BalloonState,
    queue_count: usize,
) -> Result<(), SnapshotV2BalloonStateBuildError> {
    match &state.transport {
        SnapshotV2DeviceTransport::Mmio(_) => {
            if state.virtio.interrupt_intents().len() > 2
                || state.virtio.interrupt_intents().iter().any(|intent| {
                    matches!(
                        intent,
                        SnapshotV2InterruptIntent::Queue { queue_index } if *queue_index != 0
                    )
                })
            {
                return Err(SnapshotV2BalloonStateBuildError::Virtio);
            }
        }
        SnapshotV2DeviceTransport::Pci(_) => {
            if state.virtio.interrupt_intents().iter().any(|intent| {
                matches!(
                    intent,
                    SnapshotV2InterruptIntent::Queue { queue_index }
                        if usize::from(*queue_index) >= queue_count
                )
            }) {
                return Err(SnapshotV2BalloonStateBuildError::Virtio);
            }
        }
    }
    if !state.virtio.is_activated()
        && state
            .virtio
            .interrupt_intents()
            .iter()
            .any(|intent| matches!(intent, SnapshotV2InterruptIntent::Queue { .. }))
    {
        return Err(SnapshotV2BalloonStateBuildError::Virtio);
    }
    Ok(())
}

fn validate_transport(
    state: &SnapshotV2BalloonState,
) -> Result<(), SnapshotV2BalloonStateBuildError> {
    let queue_count = state.virtio.queues().len();
    match &state.transport {
        SnapshotV2DeviceTransport::Mmio(mmio) => {
            if mmio.device_feature_select() > 1
                || mmio.driver_feature_select() > 1
                || usize::try_from(mmio.queue_select())
                    .ok()
                    .is_none_or(|index| index >= queue_count)
                || mmio.region().id().raw_value() == 0
                || mmio.region().range().size() != VIRTIO_MMIO_DEVICE_WINDOW_SIZE
                || mmio
                    .region()
                    .range()
                    .validate_alignment(VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
                    .is_err()
                || mmio.interrupt_line().raw_value() < 32
            {
                return Err(SnapshotV2BalloonStateBuildError::Transport);
            }
        }
        SnapshotV2DeviceTransport::Pci(pci) => validate_pci(pci, queue_count)?,
    }
    Ok(())
}

fn validate_pci(
    pci: &SnapshotV2PciDeviceState,
    queue_count: usize,
) -> Result<(), SnapshotV2BalloonStateBuildError> {
    const WRITABLE_OFFSETS: [u16; 4] = [0x04, 0x05, 0x0c, 0x3c];
    let aperture_end = PCI_BAR64_START
        .checked_add(PCI_BAR64_SIZE)
        .ok_or(SnapshotV2BalloonStateBuildError::Transport)?;
    if pci.phase() != VirtioPciEndpointPhase::Active
        || pci.origin() != StorageDeviceOrigin::Startup
        || pci.sbdf().segment() != PCI_SEGMENT_ZERO
        || pci.sbdf().bus() != PCI_BUS_ZERO
        || !(PCI_FIRST_ENDPOINT_DEVICE..=PCI_LAST_ENDPOINT_DEVICE).contains(&pci.sbdf().device())
        || pci.sbdf().function() != PCI_FUNCTION_ZERO
        || pci.bar_index() != VIRTIO_PCI_CAPABILITY_BAR_INDEX
        || pci.bar_address_space() != PciBarAddressSpace::Memory64
        || pci.bar_prefetchable() != PciBarPrefetchable::No
        || pci.bar_range().size() != VIRTIO_PCI_CAPABILITY_BAR_SIZE
        || pci
            .bar_range()
            .validate_alignment(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .is_err()
        || pci.bar_range().start().raw_value() < PCI_BAR64_START
        || pci.bar_range().end_exclusive().raw_value() > aperture_end
        || pci.device_feature_select() > 1
        || pci.driver_feature_select() > 1
        || usize::from(pci.queue_select()) >= queue_count
        || pci.writable_bytes().len() != WRITABLE_OFFSETS.len()
        || pci.bar_probes().len() != 2
        || pci
            .writable_bytes()
            .iter()
            .map(|byte| byte.offset())
            .ne(WRITABLE_OFFSETS)
        || pci
            .bar_probes()
            .iter()
            .map(|probe| probe.index())
            .ne([0, 1])
    {
        return Err(SnapshotV2BalloonStateBuildError::Transport);
    }
    validate_msix(pci.msix(), queue_count)
}

fn validate_msix(
    msix: &SnapshotV2PciMsixState,
    queue_count: usize,
) -> Result<(), SnapshotV2BalloonStateBuildError> {
    let entry_count = queue_count
        .checked_add(1)
        .ok_or(SnapshotV2BalloonStateBuildError::Transport)?;
    let pending_mask = (1_u64 << entry_count) - 1;
    if msix.entries().len() != entry_count
        || entry_count > VIRTIO_PCI_MAX_MSIX_VECTORS
        || msix.pending_words().len() != 1
        || msix.queue_vectors().len() != queue_count
        || msix
            .pending_words()
            .first()
            .copied()
            .is_none_or(|pending| pending & !pending_mask != 0)
        || !valid_msix_vector(msix.config_vector(), entry_count)
        || msix
            .queue_vectors()
            .iter()
            .copied()
            .any(|vector| !valid_msix_vector(vector, entry_count))
        || msix
            .entries()
            .iter()
            .any(|entry| entry.vector_control() & !1 != 0)
    {
        return Err(SnapshotV2BalloonStateBuildError::Transport);
    }
    Ok(())
}

fn valid_msix_vector(vector: u16, count: usize) -> bool {
    vector == VIRTIO_PCI_NO_VECTOR || usize::from(vector) < count
}

fn validate_queue_placement(
    state: &SnapshotV2BalloonState,
) -> Result<(), SnapshotV2BalloonStateBuildError> {
    let placement = match &state.transport {
        SnapshotV2DeviceTransport::Mmio(mmio) => mmio.region().range(),
        SnapshotV2DeviceTransport::Pci(pci) => pci.bar_range(),
    };
    for queue in state.virtio.queues() {
        if queue_ranges(queue)
            .map_err(|_| SnapshotV2BalloonStateBuildError::Queue)?
            .is_some_and(|ranges| ranges.into_iter().any(|range| range.overlaps(placement)))
        {
            return Err(SnapshotV2BalloonStateBuildError::Placement);
        }
    }
    Ok(())
}
