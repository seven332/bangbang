//! Canonical detached native-v2 2.9 balloon state profile.
//!
//! This module contains only inert balloon configuration, guest-visible
//! state, queue cursors, statistics, hint history, exact host accounting,
//! common virtio registers, and transport placement. Guest-memory borrows,
//! notifier and interrupt authority, timers, metrics, reclaim advisers,
//! dispatchers, threads, and cleanup ownership remain destination-local.

use std::fmt;

use crate::balloon::{
    BalloonConfig, VIRTIO_BALLOON_FREE_PAGE_HINT_DONE, VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
    VIRTIO_BALLOON_QUEUE_SIZE, VirtioBalloonConfigSpace, VirtioBalloonQueueLayout,
    available_features, mib_to_4k_pages,
};
use crate::pci::{
    PCI_BAR64_SIZE, PCI_BAR64_START, PCI_BUS_ZERO, PCI_FIRST_ENDPOINT_DEVICE, PCI_FUNCTION_ZERO,
    PCI_LAST_ENDPOINT_DEVICE, PCI_SEGMENT_ZERO, PciBarAddressSpace, PciBarPrefetchable,
};
use crate::snapshot_device_v2::{
    SnapshotV2DeviceTransport, SnapshotV2InterruptIntent, SnapshotV2PciDeviceState,
    SnapshotV2PciMsixState, SnapshotV2VirtioQueueState, SnapshotV2VirtioState,
};
use crate::snapshot_device_v2_5::queue_ranges;
use crate::snapshot_format::SnapshotFormatVersion;
use crate::storage_capture::StorageDeviceOrigin;
use crate::virtio::{
    VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DEVICE_NEEDS_RESET,
    VIRTIO_DEVICE_STATUS_DRIVER, VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FAILED,
    VIRTIO_DEVICE_STATUS_FEATURES_OK, VIRTIO_DEVICE_STATUS_INIT,
};
use crate::virtio_mmio::{VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VIRTIO_MMIO_VERSION_1_FEATURE};
use crate::virtio_pci::{
    VIRTIO_PCI_CAPABILITY_BAR_INDEX, VIRTIO_PCI_CAPABILITY_BAR_SIZE, VIRTIO_PCI_MAX_MSIX_VECTORS,
    VIRTIO_PCI_NO_VECTOR, VirtioPciEndpointPhase,
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
