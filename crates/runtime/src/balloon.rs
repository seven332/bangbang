//! Backend-neutral virtio-balloon configuration model.

use std::collections::TryReserveError;
use std::fmt;

use crate::memory::{
    GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryDiscardAdviser,
    GuestMemoryDiscardOutcome, GuestMemoryError, GuestMemoryRange, SystemGuestMemoryDiscardAdviser,
};
use crate::mmio::{
    MmioAccessBytes, MmioAccessBytesError, MmioBusError, MmioDispatchError, MmioDispatcher,
    MmioHandlerError, MmioHandlerLookupError, MmioRegion, MmioRegionId,
};
use crate::virtio::VirtioInterruptIntent;
use crate::virtio_mmio::{
    VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioDeviceActivation, VirtioMmioDeviceActivationError,
    VirtioMmioDeviceActivationHandler, VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError,
    VirtioMmioDeviceConfigHandler, VirtioMmioQueueRegisterError, VirtioMmioQueueState,
    VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError,
};
use crate::virtio_pci::{VirtioPciDeviceOperationError, VirtioPciEndpoint};
use crate::virtio_queue::{
    VirtqueueAvailableRing, VirtqueueAvailableRingError, VirtqueueDescriptor,
    VirtqueueDescriptorChain, VirtqueueNotificationSuppression, VirtqueueUsedRing,
    VirtqueueUsedRingError, VirtqueueUsedRingPublication,
};

pub const VIRTIO_BALLOON_DEVICE_ID: u32 = 5;
pub const VIRTIO_BALLOON_QUEUE_SIZE: u16 = 256;
pub const VIRTIO_BALLOON_MIN_QUEUE_COUNT: usize = 2;
pub const VIRTIO_BALLOON_MAX_QUEUE_COUNT: usize = 5;
pub const VIRTIO_BALLOON_INFLATE_QUEUE_INDEX: usize = 0;
pub const VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX: usize = 1;
pub const VIRTIO_BALLOON_STATS_QUEUE_INDEX: usize = 2;
pub const VIRTIO_BALLOON_PFN_SIZE: usize = 4;
pub const VIRTIO_BALLOON_HINTING_COMMAND_SIZE: usize = 4;
const VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32: u32 = 4;
pub const VIRTIO_BALLOON_STAT_SIZE: usize = 10;
pub const VIRTIO_BALLOON_MAX_STATS_PER_DESCRIPTOR: usize = 256;
pub const VIRTIO_BALLOON_MAX_STATS_PAYLOAD_SIZE: usize =
    VIRTIO_BALLOON_MAX_STATS_PER_DESCRIPTOR * VIRTIO_BALLOON_STAT_SIZE;
pub const VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR: usize = 256;
pub const VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE: usize =
    VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR * VIRTIO_BALLOON_PFN_SIZE;
pub const VIRTIO_BALLOON_PAGE_SIZE: u64 = 4096;
pub const VIRTIO_BALLOON_MIB_TO_4K_PAGES: u32 = 256;
pub const VIRTIO_BALLOON_MAX_AMOUNT_MIB: u32 = u32::MAX / VIRTIO_BALLOON_MIB_TO_4K_PAGES;
pub const VIRTIO_BALLOON_CONFIG_SPACE_SIZE: usize = 12;
pub const VIRTIO_BALLOON_FREE_PAGE_HINT_STOP: u32 = 0;
pub const VIRTIO_BALLOON_FREE_PAGE_HINT_DONE: u32 = 1;
pub const VIRTIO_BALLOON_F_STATS_VQ: u32 = 1;
pub const VIRTIO_BALLOON_F_DEFLATE_ON_OOM: u32 = 2;
pub const VIRTIO_BALLOON_F_FREE_PAGE_HINTING: u32 = 3;
pub const VIRTIO_BALLOON_F_FREE_PAGE_REPORTING: u32 = 5;
pub const VIRTIO_FEATURE_VERSION_1: u32 = 32;
pub const VIRTIO_BALLOON_S_SWAP_IN: u16 = 0;
pub const VIRTIO_BALLOON_S_SWAP_OUT: u16 = 1;
pub const VIRTIO_BALLOON_S_MAJFLT: u16 = 2;
pub const VIRTIO_BALLOON_S_MINFLT: u16 = 3;
pub const VIRTIO_BALLOON_S_MEMFREE: u16 = 4;
pub const VIRTIO_BALLOON_S_MEMTOT: u16 = 5;
pub const VIRTIO_BALLOON_S_AVAIL: u16 = 6;
pub const VIRTIO_BALLOON_S_CACHES: u16 = 7;
pub const VIRTIO_BALLOON_S_HTLB_PGALLOC: u16 = 8;
pub const VIRTIO_BALLOON_S_HTLB_PGFAIL: u16 = 9;
pub const VIRTIO_BALLOON_S_OOM_KILL: u16 = 10;
pub const VIRTIO_BALLOON_S_ALLOC_STALL: u16 = 11;
pub const VIRTIO_BALLOON_S_ASYNC_SCAN: u16 = 12;
pub const VIRTIO_BALLOON_S_DIRECT_SCAN: u16 = 13;
pub const VIRTIO_BALLOON_S_ASYNC_RECLAIM: u16 = 14;
pub const VIRTIO_BALLOON_S_DIRECT_RECLAIM: u16 = 15;

pub type VirtioBalloonMmioHandler =
    VirtioMmioRegisterHandler<VirtioBalloonConfigSpace, VirtioBalloonDevice>;

const fn virtio_feature_bit(feature: u32) -> u64 {
    1_u64 << feature
}

pub fn mib_to_4k_pages(amount_mib: u32) -> Result<u32, BalloonPageCountOverflow> {
    amount_mib
        .checked_mul(VIRTIO_BALLOON_MIB_TO_4K_PAGES)
        .ok_or(BalloonPageCountOverflow { amount_mib })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonPageCountOverflow {
    amount_mib: u32,
}

impl BalloonPageCountOverflow {
    pub const fn amount_mib(self) -> u32 {
        self.amount_mib
    }

    pub const fn max_amount_mib(self) -> u32 {
        VIRTIO_BALLOON_MAX_AMOUNT_MIB
    }
}

impl fmt::Display for BalloonPageCountOverflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "balloon amount_mib {} exceeds maximum {} MiB representable as 4 KiB pages",
            self.amount_mib,
            self.max_amount_mib()
        )
    }
}

impl std::error::Error for BalloonPageCountOverflow {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BalloonConfigError {
    TargetExceedsGuestMemory { amount_mib: u32, mem_size_mib: u64 },
}

impl fmt::Display for BalloonConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetExceedsGuestMemory {
                amount_mib,
                mem_size_mib,
            } => write!(
                f,
                "balloon amount_mib {amount_mib} exceeds configured guest memory {mem_size_mib} MiB"
            ),
        }
    }
}

impl std::error::Error for BalloonConfigError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BalloonUpdateError {
    PageCountOverflow(BalloonPageCountOverflow),
    TargetExceedsGuestMemory { amount_mib: u32, mem_size_mib: u64 },
    StatisticsStateChange,
    ActiveSessionUnavailable,
    ActiveSessionCommand { message: String },
    MmioDispatcherUnavailable,
    HandlerLookup(MmioHandlerLookupError),
}

impl fmt::Display for BalloonUpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PageCountOverflow(err) => write!(f, "{err}"),
            Self::TargetExceedsGuestMemory {
                amount_mib,
                mem_size_mib,
            } => write!(
                f,
                "balloon amount_mib {amount_mib} exceeds configured guest memory {mem_size_mib} MiB"
            ),
            Self::StatisticsStateChange => f.write_str(
                "balloon statistics cannot be enabled or disabled after device activation",
            ),
            Self::ActiveSessionUnavailable => {
                f.write_str("active balloon device session is unavailable")
            }
            Self::ActiveSessionCommand { message } => {
                write!(f, "active balloon device update failed: {message}")
            }
            Self::MmioDispatcherUnavailable => f.write_str("active MMIO dispatcher is unavailable"),
            Self::HandlerLookup(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for BalloonUpdateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PageCountOverflow(err) => Some(err),
            Self::HandlerLookup(err) => Some(err),
            Self::TargetExceedsGuestMemory { .. }
            | Self::StatisticsStateChange
            | Self::ActiveSessionUnavailable
            | Self::ActiveSessionCommand { .. }
            | Self::MmioDispatcherUnavailable => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BalloonStatsError {
    PageCountOverflow(BalloonPageCountOverflow),
    ActualPageCountTooLarge { actual_pages: u64 },
    ActiveSessionUnavailable,
    ActiveSessionCommand { message: String },
    MmioDispatcherUnavailable,
    HandlerLookup(MmioHandlerLookupError),
}

impl fmt::Display for BalloonStatsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PageCountOverflow(err) => write!(f, "{err}"),
            Self::ActualPageCountTooLarge { actual_pages } => write!(
                f,
                "balloon actual_pages {actual_pages} exceeds maximum {} representable in the API response",
                u32::MAX
            ),
            Self::ActiveSessionUnavailable => {
                f.write_str("active balloon device session is unavailable")
            }
            Self::ActiveSessionCommand { message } => {
                write!(
                    f,
                    "active balloon device statistics query failed: {message}"
                )
            }
            Self::MmioDispatcherUnavailable => f.write_str("active MMIO dispatcher is unavailable"),
            Self::HandlerLookup(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for BalloonStatsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PageCountOverflow(err) => Some(err),
            Self::HandlerLookup(err) => Some(err),
            Self::ActualPageCountTooLarge { .. }
            | Self::ActiveSessionUnavailable
            | Self::ActiveSessionCommand { .. }
            | Self::MmioDispatcherUnavailable => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BalloonHintingCommandError {
    HintingNotEnabled,
    ActiveSessionUnavailable,
    ActiveSessionCommand { message: String },
    MmioDispatcherUnavailable,
    HandlerLookup(MmioHandlerLookupError),
}

impl fmt::Display for BalloonHintingCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HintingNotEnabled => f.write_str("balloon free-page hinting is not enabled"),
            Self::ActiveSessionUnavailable => {
                f.write_str("active balloon device session is unavailable")
            }
            Self::ActiveSessionCommand { message } => {
                write!(f, "active balloon device hinting command failed: {message}")
            }
            Self::MmioDispatcherUnavailable => f.write_str("active MMIO dispatcher is unavailable"),
            Self::HandlerLookup(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for BalloonHintingCommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HandlerLookup(err) => Some(err),
            Self::HintingNotEnabled
            | Self::ActiveSessionUnavailable
            | Self::ActiveSessionCommand { .. }
            | Self::MmioDispatcherUnavailable => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BalloonHintingStatusError {
    HintingNotEnabled,
    ActiveSessionUnavailable,
    ActiveSessionCommand { message: String },
    MmioDispatcherUnavailable,
    HandlerLookup(MmioHandlerLookupError),
}

impl fmt::Display for BalloonHintingStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HintingNotEnabled => f.write_str("balloon free-page hinting is not enabled"),
            Self::ActiveSessionUnavailable => {
                f.write_str("active balloon device session is unavailable")
            }
            Self::ActiveSessionCommand { message } => {
                write!(
                    f,
                    "active balloon device hinting status query failed: {message}"
                )
            }
            Self::MmioDispatcherUnavailable => f.write_str("active MMIO dispatcher is unavailable"),
            Self::HandlerLookup(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for BalloonHintingStatusError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HandlerLookup(err) => Some(err),
            Self::HintingNotEnabled
            | Self::ActiveSessionUnavailable
            | Self::ActiveSessionCommand { .. }
            | Self::MmioDispatcherUnavailable => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonConfigInput {
    amount_mib: u32,
    deflate_on_oom: bool,
    stats_polling_interval_s: u16,
    free_page_hinting: bool,
    free_page_reporting: bool,
}

impl BalloonConfigInput {
    pub const fn new(amount_mib: u32, deflate_on_oom: bool) -> Self {
        Self {
            amount_mib,
            deflate_on_oom,
            stats_polling_interval_s: 0,
            free_page_hinting: false,
            free_page_reporting: false,
        }
    }

    pub const fn with_stats_polling_interval_s(mut self, stats_polling_interval_s: u16) -> Self {
        self.stats_polling_interval_s = stats_polling_interval_s;
        self
    }

    pub const fn with_free_page_hinting(mut self, free_page_hinting: bool) -> Self {
        self.free_page_hinting = free_page_hinting;
        self
    }

    pub const fn with_free_page_reporting(mut self, free_page_reporting: bool) -> Self {
        self.free_page_reporting = free_page_reporting;
        self
    }

    pub const fn amount_mib(self) -> u32 {
        self.amount_mib
    }

    pub const fn deflate_on_oom(self) -> bool {
        self.deflate_on_oom
    }

    pub const fn stats_polling_interval_s(self) -> u16 {
        self.stats_polling_interval_s
    }

    pub const fn free_page_hinting(self) -> bool {
        self.free_page_hinting
    }

    pub const fn free_page_reporting(self) -> bool {
        self.free_page_reporting
    }

    pub fn validate(self) -> Result<BalloonConfig, BalloonConfigError> {
        Ok(self.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonUpdateInput {
    amount_mib: u32,
}

impl BalloonUpdateInput {
    pub const fn new(amount_mib: u32) -> Self {
        Self { amount_mib }
    }

    pub const fn amount_mib(self) -> u32 {
        self.amount_mib
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonStatsUpdateInput {
    stats_polling_interval_s: u16,
}

impl BalloonStatsUpdateInput {
    pub const fn new(stats_polling_interval_s: u16) -> Self {
        Self {
            stats_polling_interval_s,
        }
    }

    pub const fn stats_polling_interval_s(self) -> u16 {
        self.stats_polling_interval_s
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonConfig {
    amount_mib: u32,
    deflate_on_oom: bool,
    stats_polling_interval_s: u16,
    free_page_hinting: bool,
    free_page_reporting: bool,
}

impl BalloonConfig {
    pub const fn amount_mib(self) -> u32 {
        self.amount_mib
    }

    pub const fn deflate_on_oom(self) -> bool {
        self.deflate_on_oom
    }

    pub const fn stats_polling_interval_s(self) -> u16 {
        self.stats_polling_interval_s
    }

    pub const fn free_page_hinting(self) -> bool {
        self.free_page_hinting
    }

    pub const fn free_page_reporting(self) -> bool {
        self.free_page_reporting
    }

    pub fn updated(self, input: BalloonUpdateInput) -> Result<Self, BalloonUpdateError> {
        let amount_mib = input.amount_mib();
        mib_to_4k_pages(amount_mib).map_err(BalloonUpdateError::PageCountOverflow)?;

        Ok(Self {
            amount_mib,
            deflate_on_oom: self.deflate_on_oom,
            stats_polling_interval_s: self.stats_polling_interval_s,
            free_page_hinting: self.free_page_hinting,
            free_page_reporting: self.free_page_reporting,
        })
    }

    pub fn updated_stats(self, input: BalloonStatsUpdateInput) -> Result<Self, BalloonUpdateError> {
        let stats_polling_interval_s = input.stats_polling_interval_s();
        if self.stats_polling_interval_s == stats_polling_interval_s {
            return Ok(self);
        }
        if self.stats_polling_interval_s == 0 || stats_polling_interval_s == 0 {
            return Err(BalloonUpdateError::StatisticsStateChange);
        }

        Ok(Self {
            amount_mib: self.amount_mib,
            deflate_on_oom: self.deflate_on_oom,
            stats_polling_interval_s,
            free_page_hinting: self.free_page_hinting,
            free_page_reporting: self.free_page_reporting,
        })
    }
}

impl From<BalloonConfigInput> for BalloonConfig {
    fn from(input: BalloonConfigInput) -> Self {
        Self {
            amount_mib: input.amount_mib,
            deflate_on_oom: input.deflate_on_oom,
            stats_polling_interval_s: input.stats_polling_interval_s,
            free_page_hinting: input.free_page_hinting,
            free_page_reporting: input.free_page_reporting,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BalloonOptionalStats {
    swap_in: Option<u64>,
    swap_out: Option<u64>,
    major_faults: Option<u64>,
    minor_faults: Option<u64>,
    free_memory: Option<u64>,
    total_memory: Option<u64>,
    available_memory: Option<u64>,
    disk_caches: Option<u64>,
    hugetlb_allocations: Option<u64>,
    hugetlb_failures: Option<u64>,
    oom_kill: Option<u64>,
    alloc_stall: Option<u64>,
    async_scan: Option<u64>,
    direct_scan: Option<u64>,
    async_reclaim: Option<u64>,
    direct_reclaim: Option<u64>,
}

impl BalloonOptionalStats {
    pub const fn new() -> Self {
        Self {
            swap_in: None,
            swap_out: None,
            major_faults: None,
            minor_faults: None,
            free_memory: None,
            total_memory: None,
            available_memory: None,
            disk_caches: None,
            hugetlb_allocations: None,
            hugetlb_failures: None,
            oom_kill: None,
            alloc_stall: None,
            async_scan: None,
            direct_scan: None,
            async_reclaim: None,
            direct_reclaim: None,
        }
    }

    pub const fn swap_in(self) -> Option<u64> {
        self.swap_in
    }

    pub const fn swap_out(self) -> Option<u64> {
        self.swap_out
    }

    pub const fn major_faults(self) -> Option<u64> {
        self.major_faults
    }

    pub const fn minor_faults(self) -> Option<u64> {
        self.minor_faults
    }

    pub const fn free_memory(self) -> Option<u64> {
        self.free_memory
    }

    pub const fn total_memory(self) -> Option<u64> {
        self.total_memory
    }

    pub const fn available_memory(self) -> Option<u64> {
        self.available_memory
    }

    pub const fn disk_caches(self) -> Option<u64> {
        self.disk_caches
    }

    pub const fn hugetlb_allocations(self) -> Option<u64> {
        self.hugetlb_allocations
    }

    pub const fn hugetlb_failures(self) -> Option<u64> {
        self.hugetlb_failures
    }

    pub const fn oom_kill(self) -> Option<u64> {
        self.oom_kill
    }

    pub const fn alloc_stall(self) -> Option<u64> {
        self.alloc_stall
    }

    pub const fn async_scan(self) -> Option<u64> {
        self.async_scan
    }

    pub const fn direct_scan(self) -> Option<u64> {
        self.direct_scan
    }

    pub const fn async_reclaim(self) -> Option<u64> {
        self.async_reclaim
    }

    pub const fn direct_reclaim(self) -> Option<u64> {
        self.direct_reclaim
    }

    pub const fn is_empty(self) -> bool {
        self.swap_in.is_none()
            && self.swap_out.is_none()
            && self.major_faults.is_none()
            && self.minor_faults.is_none()
            && self.free_memory.is_none()
            && self.total_memory.is_none()
            && self.available_memory.is_none()
            && self.disk_caches.is_none()
            && self.hugetlb_allocations.is_none()
            && self.hugetlb_failures.is_none()
            && self.oom_kill.is_none()
            && self.alloc_stall.is_none()
            && self.async_scan.is_none()
            && self.direct_scan.is_none()
            && self.async_reclaim.is_none()
            && self.direct_reclaim.is_none()
    }

    pub fn record_stat(&mut self, stat: VirtioBalloonStat) -> bool {
        let value = Some(stat.value());
        match stat.tag() {
            VIRTIO_BALLOON_S_SWAP_IN => self.swap_in = value,
            VIRTIO_BALLOON_S_SWAP_OUT => self.swap_out = value,
            VIRTIO_BALLOON_S_MAJFLT => self.major_faults = value,
            VIRTIO_BALLOON_S_MINFLT => self.minor_faults = value,
            VIRTIO_BALLOON_S_MEMFREE => self.free_memory = value,
            VIRTIO_BALLOON_S_MEMTOT => self.total_memory = value,
            VIRTIO_BALLOON_S_AVAIL => self.available_memory = value,
            VIRTIO_BALLOON_S_CACHES => self.disk_caches = value,
            VIRTIO_BALLOON_S_HTLB_PGALLOC => self.hugetlb_allocations = value,
            VIRTIO_BALLOON_S_HTLB_PGFAIL => self.hugetlb_failures = value,
            VIRTIO_BALLOON_S_OOM_KILL => self.oom_kill = value,
            VIRTIO_BALLOON_S_ALLOC_STALL => self.alloc_stall = value,
            VIRTIO_BALLOON_S_ASYNC_SCAN => self.async_scan = value,
            VIRTIO_BALLOON_S_DIRECT_SCAN => self.direct_scan = value,
            VIRTIO_BALLOON_S_ASYNC_RECLAIM => self.async_reclaim = value,
            VIRTIO_BALLOON_S_DIRECT_RECLAIM => self.direct_reclaim = value,
            _ => return false,
        }

        true
    }

    fn merge_from(&mut self, other: Self) {
        if other.swap_in.is_some() {
            self.swap_in = other.swap_in;
        }
        if other.swap_out.is_some() {
            self.swap_out = other.swap_out;
        }
        if other.major_faults.is_some() {
            self.major_faults = other.major_faults;
        }
        if other.minor_faults.is_some() {
            self.minor_faults = other.minor_faults;
        }
        if other.free_memory.is_some() {
            self.free_memory = other.free_memory;
        }
        if other.total_memory.is_some() {
            self.total_memory = other.total_memory;
        }
        if other.available_memory.is_some() {
            self.available_memory = other.available_memory;
        }
        if other.disk_caches.is_some() {
            self.disk_caches = other.disk_caches;
        }
        if other.hugetlb_allocations.is_some() {
            self.hugetlb_allocations = other.hugetlb_allocations;
        }
        if other.hugetlb_failures.is_some() {
            self.hugetlb_failures = other.hugetlb_failures;
        }
        if other.oom_kill.is_some() {
            self.oom_kill = other.oom_kill;
        }
        if other.alloc_stall.is_some() {
            self.alloc_stall = other.alloc_stall;
        }
        if other.async_scan.is_some() {
            self.async_scan = other.async_scan;
        }
        if other.direct_scan.is_some() {
            self.direct_scan = other.direct_scan;
        }
        if other.async_reclaim.is_some() {
            self.async_reclaim = other.async_reclaim;
        }
        if other.direct_reclaim.is_some() {
            self.direct_reclaim = other.direct_reclaim;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonStats {
    target_pages: u32,
    actual_pages: u32,
    target_mib: u32,
    actual_mib: u32,
    optional: BalloonOptionalStats,
}

impl BalloonStats {
    pub fn from_config_and_actual_pages(
        config: BalloonConfig,
        actual_pages: u64,
    ) -> Result<Self, BalloonStatsError> {
        Self::from_config_actual_pages_and_optional_stats(
            config,
            actual_pages,
            BalloonOptionalStats::default(),
        )
    }

    pub fn from_config_actual_pages_and_optional_stats(
        config: BalloonConfig,
        actual_pages: u64,
        optional: BalloonOptionalStats,
    ) -> Result<Self, BalloonStatsError> {
        let target_pages =
            mib_to_4k_pages(config.amount_mib()).map_err(BalloonStatsError::PageCountOverflow)?;
        let actual_pages = u32::try_from(actual_pages)
            .map_err(|_| BalloonStatsError::ActualPageCountTooLarge { actual_pages })?;

        Ok(Self {
            target_pages,
            actual_pages,
            target_mib: config.amount_mib(),
            actual_mib: actual_pages / VIRTIO_BALLOON_MIB_TO_4K_PAGES,
            optional,
        })
    }

    pub const fn target_pages(self) -> u32 {
        self.target_pages
    }

    pub const fn actual_pages(self) -> u32 {
        self.actual_pages
    }

    pub const fn target_mib(self) -> u32 {
        self.target_mib
    }

    pub const fn actual_mib(self) -> u32 {
        self.actual_mib
    }

    pub const fn optional(self) -> BalloonOptionalStats {
        self.optional
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonHintingStatus {
    host_cmd: u32,
    guest_cmd: Option<u32>,
}

impl BalloonHintingStatus {
    pub const fn new(host_cmd: u32, guest_cmd: Option<u32>) -> Self {
        Self {
            host_cmd,
            guest_cmd,
        }
    }

    pub const fn host_cmd(self) -> u32 {
        self.host_cmd
    }

    pub const fn guest_cmd(self) -> Option<u32> {
        self.guest_cmd
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonHintingStartInput {
    acknowledge_on_stop: bool,
}

impl BalloonHintingStartInput {
    pub const fn new(acknowledge_on_stop: bool) -> Self {
        Self {
            acknowledge_on_stop,
        }
    }

    pub const fn acknowledge_on_stop(self) -> bool {
        self.acknowledge_on_stop
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioBalloonConfigSpace {
    num_pages: u32,
    actual_pages: u32,
    free_page_hint_cmd_id: u32,
}

impl VirtioBalloonConfigSpace {
    pub const fn new(num_pages: u32, actual_pages: u32, free_page_hint_cmd_id: u32) -> Self {
        Self {
            num_pages,
            actual_pages,
            free_page_hint_cmd_id,
        }
    }

    pub fn from_config(config: BalloonConfig) -> Result<Self, BalloonPageCountOverflow> {
        Ok(Self::new(
            mib_to_4k_pages(config.amount_mib())?,
            0,
            VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
        ))
    }

    pub const fn num_pages(self) -> u32 {
        self.num_pages
    }

    pub const fn actual_pages(self) -> u32 {
        self.actual_pages
    }

    pub const fn free_page_hint_cmd_id(self) -> u32 {
        self.free_page_hint_cmd_id
    }

    pub const fn with_num_pages(mut self, num_pages: u32) -> Self {
        self.num_pages = num_pages;
        self
    }

    pub const fn with_free_page_hint_cmd_id(mut self, cmd_id: u32) -> Self {
        self.free_page_hint_cmd_id = cmd_id;
        self
    }

    pub const fn from_le_bytes(bytes: [u8; VIRTIO_BALLOON_CONFIG_SPACE_SIZE]) -> Self {
        let [
            num_pages0,
            num_pages1,
            num_pages2,
            num_pages3,
            actual_pages0,
            actual_pages1,
            actual_pages2,
            actual_pages3,
            cmd0,
            cmd1,
            cmd2,
            cmd3,
        ] = bytes;
        Self {
            num_pages: u32::from_le_bytes([num_pages0, num_pages1, num_pages2, num_pages3]),
            actual_pages: u32::from_le_bytes([
                actual_pages0,
                actual_pages1,
                actual_pages2,
                actual_pages3,
            ]),
            free_page_hint_cmd_id: u32::from_le_bytes([cmd0, cmd1, cmd2, cmd3]),
        }
    }

    pub fn to_le_bytes(self) -> [u8; VIRTIO_BALLOON_CONFIG_SPACE_SIZE] {
        let [num_pages0, num_pages1, num_pages2, num_pages3] = self.num_pages.to_le_bytes();
        let [actual_pages0, actual_pages1, actual_pages2, actual_pages3] =
            self.actual_pages.to_le_bytes();
        let [cmd0, cmd1, cmd2, cmd3] = self.free_page_hint_cmd_id.to_le_bytes();

        [
            num_pages0,
            num_pages1,
            num_pages2,
            num_pages3,
            actual_pages0,
            actual_pages1,
            actual_pages2,
            actual_pages3,
            cmd0,
            cmd1,
            cmd2,
            cmd3,
        ]
    }
}

impl VirtioMmioDeviceConfigHandler for VirtioBalloonConfigSpace {
    fn read_device_config(
        &self,
        access: VirtioMmioDeviceConfigAccess,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
        let bytes = self.to_le_bytes();
        let bytes = balloon_config_access_bytes(&bytes, access)?;
        MmioAccessBytes::new(bytes).map_err(balloon_config_bytes_error)
    }

    fn write_device_config(
        &mut self,
        access: VirtioMmioDeviceConfigAccess,
        data: MmioAccessBytes,
    ) -> Result<(), VirtioMmioDeviceConfigError> {
        let mut bytes = self.to_le_bytes();
        let destination = balloon_config_access_bytes_mut(&mut bytes, access)?;
        destination.copy_from_slice(data.as_slice());
        *self = Self::from_le_bytes(bytes);
        Ok(())
    }
}

impl TryFrom<BalloonConfig> for VirtioBalloonConfigSpace {
    type Error = BalloonPageCountOverflow;

    fn try_from(config: BalloonConfig) -> Result<Self, Self::Error> {
        Self::from_config(config)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioBalloonPfnPayload {
    pfns: Vec<u32>,
}

impl VirtioBalloonPfnPayload {
    pub fn parse(bytes: &[u8]) -> Result<Self, VirtioBalloonPfnPayloadParseError> {
        if bytes.is_empty() {
            return Err(VirtioBalloonPfnPayloadParseError::EmptyPayload);
        }
        if !bytes.len().is_multiple_of(VIRTIO_BALLOON_PFN_SIZE) {
            return Err(VirtioBalloonPfnPayloadParseError::UnalignedLength { len: bytes.len() });
        }

        let count = bytes.len() / VIRTIO_BALLOON_PFN_SIZE;
        if count > VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR {
            return Err(VirtioBalloonPfnPayloadParseError::TooManyPfns {
                count,
                max: VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR,
            });
        }

        let mut pfns = Vec::new();
        pfns.try_reserve_exact(count)
            .map_err(|source| VirtioBalloonPfnPayloadParseError::PfnAllocation { count, source })?;
        for chunk in bytes.chunks_exact(VIRTIO_BALLOON_PFN_SIZE) {
            let mut pfn = [0; VIRTIO_BALLOON_PFN_SIZE];
            pfn.copy_from_slice(chunk);
            pfns.push(u32::from_le_bytes(pfn));
        }

        Ok(Self { pfns })
    }

    pub fn pfns(&self) -> &[u32] {
        &self.pfns
    }

    pub fn into_vec(self) -> Vec<u32> {
        self.pfns
    }

    pub fn len(&self) -> usize {
        self.pfns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pfns.is_empty()
    }

    pub fn into_page_ranges(
        self,
    ) -> Result<VirtioBalloonPfnRanges, VirtioBalloonPfnRangeCompactError> {
        VirtioBalloonPfnRanges::from_pfns(self.pfns)
    }
}

#[derive(Debug)]
pub enum VirtioBalloonPfnPayloadParseError {
    EmptyPayload,
    UnalignedLength {
        len: usize,
    },
    TooManyPfns {
        count: usize,
        max: usize,
    },
    PfnAllocation {
        count: usize,
        source: TryReserveError,
    },
}

impl fmt::Display for VirtioBalloonPfnPayloadParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPayload => f.write_str("virtio-balloon PFN payload cannot be empty"),
            Self::UnalignedLength { len } => write!(
                f,
                "virtio-balloon PFN payload length {len} is not a multiple of {VIRTIO_BALLOON_PFN_SIZE}"
            ),
            Self::TooManyPfns { count, max } => write!(
                f,
                "virtio-balloon PFN payload contains {count} PFNs, exceeding maximum {max}"
            ),
            Self::PfnAllocation { count, source } => write!(
                f,
                "failed to allocate virtio-balloon PFN payload with {count} PFNs: {source}"
            ),
        }
    }
}

impl std::error::Error for VirtioBalloonPfnPayloadParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PfnAllocation { source, .. } => Some(source),
            Self::EmptyPayload | Self::UnalignedLength { .. } | Self::TooManyPfns { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioBalloonPfnDescriptorPayload {
    bytes: Vec<u8>,
}

impl VirtioBalloonPfnDescriptorPayload {
    pub fn read(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
    ) -> Result<Self, VirtioBalloonPfnDescriptorPayloadReadError> {
        let payload_len = validate_balloon_pfn_descriptor_chain(memory, chain)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(payload_len).map_err(|source| {
            VirtioBalloonPfnDescriptorPayloadReadError::PayloadAllocation {
                len: payload_len,
                source,
            }
        })?;
        bytes.resize(payload_len, 0);

        let mut offset = 0;
        for descriptor in chain.descriptors().iter().copied() {
            offset = read_balloon_pfn_descriptor_segment(memory, descriptor, &mut bytes, offset)?;
        }

        Ok(Self { bytes })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn parse_pfn_payload(
        &self,
    ) -> Result<VirtioBalloonPfnPayload, VirtioBalloonPfnPayloadParseError> {
        VirtioBalloonPfnPayload::parse(&self.bytes)
    }

    pub fn into_pfn_payload(
        self,
    ) -> Result<VirtioBalloonPfnPayload, VirtioBalloonPfnPayloadParseError> {
        VirtioBalloonPfnPayload::parse(&self.bytes)
    }
}

#[derive(Debug)]
pub enum VirtioBalloonPfnDescriptorPayloadReadError {
    EmptyDescriptorChain,
    DescriptorWriteOnly {
        index: u16,
    },
    DescriptorEmpty {
        index: u16,
    },
    DescriptorLengthTooLarge {
        index: u16,
        len: u32,
    },
    DescriptorRange {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryError,
    },
    DescriptorAccess {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryAccessError,
    },
    PayloadLengthOverflow {
        current: usize,
        len: u32,
    },
    PayloadLengthTooLarge {
        len: usize,
        max: usize,
    },
    PayloadAllocation {
        len: usize,
        source: TryReserveError,
    },
    PayloadBufferRange {
        offset: usize,
        len: usize,
        buffer_len: usize,
    },
    DescriptorRead {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryAccessError,
    },
}

impl fmt::Display for VirtioBalloonPfnDescriptorPayloadReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDescriptorChain => {
                f.write_str("virtio-balloon PFN descriptor chain cannot be empty")
            }
            Self::DescriptorWriteOnly { index } => {
                write!(f, "virtio-balloon PFN descriptor {index} is write-only")
            }
            Self::DescriptorEmpty { index } => {
                write!(f, "virtio-balloon PFN descriptor {index} is empty")
            }
            Self::DescriptorLengthTooLarge { index, len } => write!(
                f,
                "virtio-balloon PFN descriptor {index} length {len} is too large to represent"
            ),
            Self::DescriptorRange {
                index,
                address,
                len,
                source,
            } => write!(
                f,
                "virtio-balloon PFN descriptor {index} range address={address}, len={len} is invalid: {source}"
            ),
            Self::DescriptorAccess {
                index,
                address,
                len,
                source,
            } => write!(
                f,
                "virtio-balloon PFN descriptor {index} range address={address}, len={len} is not readable: {source}"
            ),
            Self::PayloadLengthOverflow { current, len } => write!(
                f,
                "virtio-balloon PFN descriptor payload length overflows: current={current}, len={len}"
            ),
            Self::PayloadLengthTooLarge { len, max } => write!(
                f,
                "virtio-balloon PFN descriptor payload length {len} exceeds maximum {max}"
            ),
            Self::PayloadAllocation { len, source } => write!(
                f,
                "failed to allocate virtio-balloon PFN descriptor payload with {len} bytes: {source}"
            ),
            Self::PayloadBufferRange {
                offset,
                len,
                buffer_len,
            } => write!(
                f,
                "internal virtio-balloon PFN payload buffer range offset={offset}, len={len}, buffer_len={buffer_len} is invalid"
            ),
            Self::DescriptorRead {
                index,
                address,
                len,
                source,
            } => write!(
                f,
                "failed to read virtio-balloon PFN descriptor {index} at address={address}, len={len}: {source}"
            ),
        }
    }
}

impl std::error::Error for VirtioBalloonPfnDescriptorPayloadReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DescriptorRange { source, .. } => Some(source),
            Self::DescriptorAccess { source, .. } => Some(source),
            Self::PayloadAllocation { source, .. } => Some(source),
            Self::DescriptorRead { source, .. } => Some(source),
            Self::EmptyDescriptorChain
            | Self::DescriptorWriteOnly { .. }
            | Self::DescriptorEmpty { .. }
            | Self::DescriptorLengthTooLarge { .. }
            | Self::PayloadLengthOverflow { .. }
            | Self::PayloadLengthTooLarge { .. }
            | Self::PayloadBufferRange { .. } => None,
        }
    }
}

fn validate_balloon_pfn_descriptor_chain(
    memory: &GuestMemory,
    chain: &VirtqueueDescriptorChain,
) -> Result<usize, VirtioBalloonPfnDescriptorPayloadReadError> {
    if chain.is_empty() {
        return Err(VirtioBalloonPfnDescriptorPayloadReadError::EmptyDescriptorChain);
    }

    let mut payload_len: usize = 0;
    for descriptor in chain.descriptors().iter().copied() {
        validate_balloon_pfn_descriptor_header(descriptor)?;
        let segment_len = balloon_pfn_descriptor_len(descriptor)?;
        payload_len = payload_len.checked_add(segment_len).ok_or(
            VirtioBalloonPfnDescriptorPayloadReadError::PayloadLengthOverflow {
                current: payload_len,
                len: descriptor.len(),
            },
        )?;
        if payload_len > VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE {
            return Err(
                VirtioBalloonPfnDescriptorPayloadReadError::PayloadLengthTooLarge {
                    len: payload_len,
                    max: VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE,
                },
            );
        }
        validate_balloon_pfn_descriptor_range(memory, descriptor)?;
    }

    Ok(payload_len)
}

fn validate_balloon_pfn_descriptor_header(
    descriptor: VirtqueueDescriptor,
) -> Result<(), VirtioBalloonPfnDescriptorPayloadReadError> {
    if descriptor.is_write_only() {
        return Err(
            VirtioBalloonPfnDescriptorPayloadReadError::DescriptorWriteOnly {
                index: descriptor.index(),
            },
        );
    }
    if descriptor.is_empty() {
        return Err(
            VirtioBalloonPfnDescriptorPayloadReadError::DescriptorEmpty {
                index: descriptor.index(),
            },
        );
    }

    Ok(())
}

fn validate_balloon_pfn_descriptor_range(
    memory: &GuestMemory,
    descriptor: VirtqueueDescriptor,
) -> Result<(), VirtioBalloonPfnDescriptorPayloadReadError> {
    let range = GuestMemoryRange::new(descriptor.address(), u64::from(descriptor.len())).map_err(
        |source| VirtioBalloonPfnDescriptorPayloadReadError::DescriptorRange {
            index: descriptor.index(),
            address: descriptor.address(),
            len: descriptor.len(),
            source,
        },
    )?;
    memory.validate_mapped_range(range).map_err(|source| {
        VirtioBalloonPfnDescriptorPayloadReadError::DescriptorAccess {
            index: descriptor.index(),
            address: descriptor.address(),
            len: descriptor.len(),
            source,
        }
    })
}

fn balloon_pfn_descriptor_len(
    descriptor: VirtqueueDescriptor,
) -> Result<usize, VirtioBalloonPfnDescriptorPayloadReadError> {
    usize::try_from(descriptor.len()).map_err(|_| {
        VirtioBalloonPfnDescriptorPayloadReadError::DescriptorLengthTooLarge {
            index: descriptor.index(),
            len: descriptor.len(),
        }
    })
}

fn read_balloon_pfn_descriptor_segment(
    memory: &GuestMemory,
    descriptor: VirtqueueDescriptor,
    bytes: &mut [u8],
    offset: usize,
) -> Result<usize, VirtioBalloonPfnDescriptorPayloadReadError> {
    let segment_len = balloon_pfn_descriptor_len(descriptor)?;
    let end = offset.checked_add(segment_len).ok_or(
        VirtioBalloonPfnDescriptorPayloadReadError::PayloadLengthOverflow {
            current: offset,
            len: descriptor.len(),
        },
    )?;
    let buffer_len = bytes.len();
    let destination = bytes.get_mut(offset..end).ok_or(
        VirtioBalloonPfnDescriptorPayloadReadError::PayloadBufferRange {
            offset,
            len: segment_len,
            buffer_len,
        },
    )?;

    memory
        .read_slice(destination, descriptor.address())
        .map_err(
            |source| VirtioBalloonPfnDescriptorPayloadReadError::DescriptorRead {
                index: descriptor.index(),
                address: descriptor.address(),
                len: descriptor.len(),
                source,
            },
        )?;

    Ok(end)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBalloonPfnRange {
    start_pfn: u32,
    page_count: u32,
}

impl VirtioBalloonPfnRange {
    const fn new(start_pfn: u32, page_count: u32) -> Self {
        Self {
            start_pfn,
            page_count,
        }
    }

    pub const fn start_pfn(self) -> u32 {
        self.start_pfn
    }

    pub const fn page_count(self) -> u32 {
        self.page_count
    }

    fn end_pfn_exclusive(self) -> u64 {
        u64::from(self.start_pfn) + u64::from(self.page_count)
    }

    fn guest_memory_range(self) -> Result<Option<GuestMemoryRange>, GuestMemoryError> {
        if self.page_count == 0 {
            return Ok(None);
        }

        let start = u64::from(self.start_pfn) * VIRTIO_BALLOON_PAGE_SIZE;
        let size = u64::from(self.page_count) * VIRTIO_BALLOON_PAGE_SIZE;
        GuestMemoryRange::new(GuestAddress::new(start), size).map(Some)
    }
}

impl fmt::Display for VirtioBalloonPfnRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "start_pfn={}, page_count={}",
            self.start_pfn, self.page_count
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioBalloonPfnRanges {
    ranges: Vec<VirtioBalloonPfnRange>,
}

impl VirtioBalloonPfnRanges {
    pub fn from_pfns(mut pfns: Vec<u32>) -> Result<Self, VirtioBalloonPfnRangeCompactError> {
        let pfn_count = pfns.len();
        if pfn_count > VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR {
            return Err(VirtioBalloonPfnRangeCompactError::TooManyPfns {
                count: pfn_count,
                max: VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR,
            });
        }
        if pfns.is_empty() {
            return Ok(Self { ranges: Vec::new() });
        }

        pfns.sort_unstable();

        let mut ranges = Vec::new();
        ranges.try_reserve_exact(pfn_count).map_err(|source| {
            VirtioBalloonPfnRangeCompactError::RangeAllocation { pfn_count, source }
        })?;

        let mut iter = pfns.into_iter();
        let Some(mut start_pfn) = iter.next() else {
            return Ok(Self { ranges });
        };
        let mut previous_pfn = start_pfn;
        let mut page_count = 1;

        for pfn in iter {
            if pfn == previous_pfn {
                continue;
            }

            if previous_pfn.checked_add(1) == Some(pfn) {
                page_count += 1;
            } else {
                ranges.push(VirtioBalloonPfnRange::new(start_pfn, page_count));
                start_pfn = pfn;
                page_count = 1;
            }
            previous_pfn = pfn;
        }

        ranges.push(VirtioBalloonPfnRange::new(start_pfn, page_count));

        Ok(Self { ranges })
    }

    pub fn ranges(&self) -> &[VirtioBalloonPfnRange] {
        &self.ranges
    }

    pub fn into_vec(self) -> Vec<VirtioBalloonPfnRange> {
        self.ranges
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VirtioBalloonMemoryAccounting {
    inflated_page_ranges: Vec<VirtioBalloonPfnRange>,
}

impl VirtioBalloonMemoryAccounting {
    pub const fn new() -> Self {
        Self {
            inflated_page_ranges: Vec::new(),
        }
    }

    pub fn inflated_page_ranges(&self) -> &[VirtioBalloonPfnRange] {
        &self.inflated_page_ranges
    }

    pub fn inflated_page_count(&self) -> u64 {
        self.inflated_page_ranges
            .iter()
            .map(|range| u64::from(range.page_count()))
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.inflated_page_ranges.is_empty()
    }

    fn add_inflated_ranges(
        &mut self,
        ranges: &[VirtioBalloonPfnRange],
    ) -> Result<(), TryReserveError> {
        if ranges.is_empty() {
            return Ok(());
        }

        let mut combined = Vec::new();
        combined.try_reserve_exact(self.inflated_page_ranges.len())?;
        combined.extend_from_slice(&self.inflated_page_ranges);
        combined.try_reserve_exact(ranges.len())?;
        combined.extend_from_slice(ranges);

        self.inflated_page_ranges = compact_accounting_ranges(combined)?;
        Ok(())
    }

    fn remove_inflated_ranges(
        &mut self,
        ranges: &[VirtioBalloonPfnRange],
    ) -> Result<(), TryReserveError> {
        if self.inflated_page_ranges.is_empty() || ranges.is_empty() {
            return Ok(());
        }

        let mut removals = Vec::new();
        removals.try_reserve_exact(ranges.len())?;
        removals.extend_from_slice(ranges);
        let removals = compact_accounting_ranges(removals)?;

        let max_retained_ranges = self
            .inflated_page_ranges
            .len()
            .saturating_add(removals.len());
        let mut retained = Vec::new();
        retained.try_reserve_exact(max_retained_ranges)?;
        let mut removal_iter = removals.iter().peekable();

        for existing in &self.inflated_page_ranges {
            let existing_start = u64::from(existing.start_pfn());
            let existing_end = existing.end_pfn_exclusive();
            let mut retained_start = existing_start;

            while let Some(removal) = removal_iter.peek().copied() {
                let remove_start = u64::from(removal.start_pfn());
                let remove_end = removal.end_pfn_exclusive();

                if remove_end <= retained_start {
                    removal_iter.next();
                    continue;
                }
                if existing_end <= remove_start {
                    break;
                }
                if retained_start < remove_start {
                    push_pfn_bounds(
                        &mut retained,
                        retained_start,
                        remove_start.min(existing_end),
                    );
                }

                retained_start = retained_start.max(remove_end.min(existing_end));
                if remove_end <= existing_end {
                    removal_iter.next();
                } else {
                    break;
                }
            }

            if retained_start < existing_end {
                push_pfn_bounds(&mut retained, retained_start, existing_end);
            }
        }

        self.inflated_page_ranges = retained;
        Ok(())
    }
}

fn compact_accounting_ranges(
    mut ranges: Vec<VirtioBalloonPfnRange>,
) -> Result<Vec<VirtioBalloonPfnRange>, TryReserveError> {
    if ranges.is_empty() {
        return Ok(ranges);
    }

    ranges.sort_unstable_by_key(|range| (range.start_pfn(), range.end_pfn_exclusive()));

    let mut merged = Vec::new();
    merged.try_reserve_exact(ranges.len())?;
    let mut iter = ranges.into_iter();
    let Some(first) = iter.next() else {
        return Ok(merged);
    };
    let mut pending_start = u64::from(first.start_pfn());
    let mut pending_end = first.end_pfn_exclusive();

    for range in iter {
        let range_start = u64::from(range.start_pfn());
        let range_end = range.end_pfn_exclusive();

        if pending_end < range_start {
            push_pfn_bounds(&mut merged, pending_start, pending_end);
            pending_start = range_start;
            pending_end = range_end;
        } else {
            pending_end = pending_end.max(range_end);
        }
    }

    push_pfn_bounds(&mut merged, pending_start, pending_end);

    Ok(merged)
}

fn push_pfn_bounds(ranges: &mut Vec<VirtioBalloonPfnRange>, start: u64, end_exclusive: u64) {
    let mut next_start = start;
    while next_start < end_exclusive {
        let Ok(start_pfn) = u32::try_from(next_start) else {
            break;
        };
        let remaining = end_exclusive - next_start;
        let page_count = u32::try_from(remaining).unwrap_or(u32::MAX);
        ranges.push(VirtioBalloonPfnRange::new(start_pfn, page_count));
        next_start += u64::from(page_count);
    }
}

#[derive(Debug)]
pub enum VirtioBalloonPfnRangeCompactError {
    TooManyPfns {
        count: usize,
        max: usize,
    },
    RangeAllocation {
        pfn_count: usize,
        source: TryReserveError,
    },
}

impl fmt::Display for VirtioBalloonPfnRangeCompactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyPfns { count, max } => write!(
                f,
                "virtio-balloon PFN compaction input contains {count} PFNs, exceeding maximum {max}"
            ),
            Self::RangeAllocation { pfn_count, source } => write!(
                f,
                "failed to allocate virtio-balloon PFN ranges for {pfn_count} PFNs: {source}"
            ),
        }
    }
}

impl std::error::Error for VirtioBalloonPfnRangeCompactError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RangeAllocation { source, .. } => Some(source),
            Self::TooManyPfns { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum VirtioBalloonPfnRangeAccessError {
    GuestRange {
        pfn_range: VirtioBalloonPfnRange,
        source: GuestMemoryError,
    },
    GuestMemory {
        pfn_range: VirtioBalloonPfnRange,
        guest_range: GuestMemoryRange,
        source: GuestMemoryAccessError,
    },
}

impl fmt::Display for VirtioBalloonPfnRangeAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GuestRange { pfn_range, source } => {
                write!(
                    f,
                    "virtio-balloon PFN range {pfn_range} does not map to a valid guest memory byte range: {source}"
                )
            }
            Self::GuestMemory {
                pfn_range,
                guest_range,
                source,
            } => {
                write!(
                    f,
                    "virtio-balloon PFN range {pfn_range} maps to unmapped guest memory range {guest_range}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioBalloonPfnRangeAccessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::GuestRange { source, .. } => Some(source),
            Self::GuestMemory { source, .. } => Some(source),
        }
    }
}

fn validate_pfn_ranges_mapped(
    memory: &GuestMemory,
    ranges: &VirtioBalloonPfnRanges,
) -> Result<(), VirtioBalloonPfnRangeAccessError> {
    for pfn_range in ranges.ranges().iter().copied() {
        validate_pfn_range_mapped(memory, pfn_range)?;
    }

    Ok(())
}

fn validate_pfn_range_mapped(
    memory: &GuestMemory,
    pfn_range: VirtioBalloonPfnRange,
) -> Result<(), VirtioBalloonPfnRangeAccessError> {
    let Some(guest_range) = pfn_range
        .guest_memory_range()
        .map_err(|source| VirtioBalloonPfnRangeAccessError::GuestRange { pfn_range, source })?
    else {
        return Ok(());
    };

    memory.validate_mapped_range(guest_range).map_err(|source| {
        VirtioBalloonPfnRangeAccessError::GuestMemory {
            pfn_range,
            guest_range,
            source,
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBalloonStat {
    tag: u16,
    value: u64,
}

impl VirtioBalloonStat {
    pub const fn new(tag: u16, value: u64) -> Self {
        Self { tag, value }
    }

    pub const fn tag(self) -> u16 {
        self.tag
    }

    pub const fn value(self) -> u64 {
        self.value
    }

    pub const fn from_le_bytes(bytes: [u8; VIRTIO_BALLOON_STAT_SIZE]) -> Self {
        let [
            tag0,
            tag1,
            value0,
            value1,
            value2,
            value3,
            value4,
            value5,
            value6,
            value7,
        ] = bytes;
        Self {
            tag: u16::from_le_bytes([tag0, tag1]),
            value: u64::from_le_bytes([
                value0, value1, value2, value3, value4, value5, value6, value7,
            ]),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBalloonStatisticsDescriptorPayload {
    stats: Option<BalloonOptionalStats>,
    stat_count: usize,
    recognized_stat_count: usize,
    oversized_len: Option<usize>,
    max_len: usize,
}

impl VirtioBalloonStatisticsDescriptorPayload {
    const fn report(
        stats: BalloonOptionalStats,
        stat_count: usize,
        recognized_stat_count: usize,
    ) -> Self {
        Self {
            stats: Some(stats),
            stat_count,
            recognized_stat_count,
            oversized_len: None,
            max_len: VIRTIO_BALLOON_MAX_STATS_PAYLOAD_SIZE,
        }
    }

    const fn oversized(len: usize, max: usize) -> Self {
        Self {
            stats: None,
            stat_count: 0,
            recognized_stat_count: 0,
            oversized_len: Some(len),
            max_len: max,
        }
    }

    pub const fn report_stats(self) -> Option<BalloonOptionalStats> {
        self.stats
    }

    pub const fn stat_count(self) -> usize {
        self.stat_count
    }

    pub const fn recognized_stat_count(self) -> usize {
        self.recognized_stat_count
    }

    pub const fn is_oversized(self) -> bool {
        self.oversized_len.is_some()
    }

    pub const fn oversized_len(self) -> Option<usize> {
        self.oversized_len
    }

    pub const fn max_len(self) -> usize {
        self.max_len
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioBalloonStatisticsDescriptor {
    payload: VirtioBalloonStatisticsDescriptorPayload,
}

impl VirtioBalloonStatisticsDescriptor {
    pub fn read(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
    ) -> Result<Self, VirtioBalloonStatisticsDescriptorReadError> {
        let payload_len = validate_balloon_statistics_descriptor_chain(memory, chain)?;
        if payload_len > VIRTIO_BALLOON_MAX_STATS_PAYLOAD_SIZE {
            return Ok(Self {
                payload: VirtioBalloonStatisticsDescriptorPayload::oversized(
                    payload_len,
                    VIRTIO_BALLOON_MAX_STATS_PAYLOAD_SIZE,
                ),
            });
        }

        let mut bytes = Vec::new();
        bytes.try_reserve_exact(payload_len).map_err(|source| {
            VirtioBalloonStatisticsDescriptorReadError::PayloadAllocation {
                len: payload_len,
                source,
            }
        })?;
        bytes.resize(payload_len, 0);

        let mut offset = 0;
        for descriptor in chain.descriptors().iter().copied() {
            offset =
                read_balloon_statistics_descriptor_segment(memory, descriptor, &mut bytes, offset)?;
        }

        let mut stats = BalloonOptionalStats::default();
        let mut stat_count = 0;
        let mut recognized_stat_count = 0;
        for chunk in bytes.chunks_exact(VIRTIO_BALLOON_STAT_SIZE) {
            let mut stat_bytes = [0; VIRTIO_BALLOON_STAT_SIZE];
            stat_bytes.copy_from_slice(chunk);
            stat_count += 1;
            if stats.record_stat(VirtioBalloonStat::from_le_bytes(stat_bytes)) {
                recognized_stat_count += 1;
            }
        }

        Ok(Self {
            payload: VirtioBalloonStatisticsDescriptorPayload::report(
                stats,
                stat_count,
                recognized_stat_count,
            ),
        })
    }

    pub const fn payload(&self) -> VirtioBalloonStatisticsDescriptorPayload {
        self.payload
    }
}

#[derive(Debug)]
pub enum VirtioBalloonStatisticsDescriptorReadError {
    EmptyDescriptorChain,
    DescriptorWriteOnly {
        index: u16,
    },
    DescriptorEmpty {
        index: u16,
    },
    DescriptorLengthTooLarge {
        index: u16,
        len: u32,
    },
    PayloadLengthOverflow {
        current: usize,
        len: u32,
    },
    PayloadLengthUnaligned {
        len: usize,
    },
    DescriptorRange {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryError,
    },
    DescriptorAccess {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryAccessError,
    },
    PayloadAllocation {
        len: usize,
        source: TryReserveError,
    },
    PayloadBufferRange {
        offset: usize,
        len: usize,
        buffer_len: usize,
    },
    DescriptorRead {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryAccessError,
    },
}

impl fmt::Display for VirtioBalloonStatisticsDescriptorReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDescriptorChain => {
                f.write_str("virtio-balloon statistics descriptor chain cannot be empty")
            }
            Self::DescriptorWriteOnly { index } => {
                write!(
                    f,
                    "virtio-balloon statistics descriptor {index} is write-only"
                )
            }
            Self::DescriptorEmpty { index } => {
                write!(f, "virtio-balloon statistics descriptor {index} is empty")
            }
            Self::DescriptorLengthTooLarge { index, len } => write!(
                f,
                "virtio-balloon statistics descriptor {index} length {len} is too large to represent"
            ),
            Self::PayloadLengthOverflow { current, len } => write!(
                f,
                "virtio-balloon statistics descriptor payload length overflows: current={current}, len={len}"
            ),
            Self::PayloadLengthUnaligned { len } => write!(
                f,
                "virtio-balloon statistics descriptor payload length {len} is not a multiple of {VIRTIO_BALLOON_STAT_SIZE}"
            ),
            Self::DescriptorRange {
                index,
                address,
                len,
                source,
            } => write!(
                f,
                "virtio-balloon statistics descriptor {index} range address={address}, len={len} is invalid: {source}"
            ),
            Self::DescriptorAccess {
                index,
                address,
                len,
                source,
            } => write!(
                f,
                "virtio-balloon statistics descriptor {index} range address={address}, len={len} is not readable: {source}"
            ),
            Self::PayloadAllocation { len, source } => write!(
                f,
                "failed to allocate virtio-balloon statistics payload with {len} bytes: {source}"
            ),
            Self::PayloadBufferRange {
                offset,
                len,
                buffer_len,
            } => write!(
                f,
                "internal virtio-balloon statistics payload buffer range offset={offset}, len={len}, buffer_len={buffer_len} is invalid"
            ),
            Self::DescriptorRead {
                index,
                address,
                len,
                source,
            } => write!(
                f,
                "failed to read virtio-balloon statistics descriptor {index} at address={address}, len={len}: {source}"
            ),
        }
    }
}

impl std::error::Error for VirtioBalloonStatisticsDescriptorReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DescriptorRange { source, .. } => Some(source),
            Self::DescriptorAccess { source, .. } => Some(source),
            Self::PayloadAllocation { source, .. } => Some(source),
            Self::DescriptorRead { source, .. } => Some(source),
            Self::EmptyDescriptorChain
            | Self::DescriptorWriteOnly { .. }
            | Self::DescriptorEmpty { .. }
            | Self::DescriptorLengthTooLarge { .. }
            | Self::PayloadLengthOverflow { .. }
            | Self::PayloadLengthUnaligned { .. }
            | Self::PayloadBufferRange { .. } => None,
        }
    }
}

fn validate_balloon_statistics_descriptor_chain(
    memory: &GuestMemory,
    chain: &VirtqueueDescriptorChain,
) -> Result<usize, VirtioBalloonStatisticsDescriptorReadError> {
    if chain.is_empty() {
        return Err(VirtioBalloonStatisticsDescriptorReadError::EmptyDescriptorChain);
    }

    let mut payload_len: usize = 0;
    for descriptor in chain.descriptors().iter().copied() {
        validate_balloon_statistics_descriptor_header(descriptor)?;
        let segment_len = balloon_statistics_descriptor_len(descriptor)?;
        payload_len = payload_len.checked_add(segment_len).ok_or(
            VirtioBalloonStatisticsDescriptorReadError::PayloadLengthOverflow {
                current: payload_len,
                len: descriptor.len(),
            },
        )?;
        if payload_len > VIRTIO_BALLOON_MAX_STATS_PAYLOAD_SIZE {
            return Ok(payload_len);
        }
        validate_balloon_statistics_descriptor_range(memory, descriptor)?;
    }

    if !payload_len.is_multiple_of(VIRTIO_BALLOON_STAT_SIZE) {
        return Err(
            VirtioBalloonStatisticsDescriptorReadError::PayloadLengthUnaligned { len: payload_len },
        );
    }

    Ok(payload_len)
}

fn validate_balloon_statistics_descriptor_header(
    descriptor: VirtqueueDescriptor,
) -> Result<(), VirtioBalloonStatisticsDescriptorReadError> {
    if descriptor.is_write_only() {
        return Err(
            VirtioBalloonStatisticsDescriptorReadError::DescriptorWriteOnly {
                index: descriptor.index(),
            },
        );
    }
    if descriptor.is_empty() {
        return Err(
            VirtioBalloonStatisticsDescriptorReadError::DescriptorEmpty {
                index: descriptor.index(),
            },
        );
    }

    Ok(())
}

fn validate_balloon_statistics_descriptor_range(
    memory: &GuestMemory,
    descriptor: VirtqueueDescriptor,
) -> Result<(), VirtioBalloonStatisticsDescriptorReadError> {
    let range = GuestMemoryRange::new(descriptor.address(), u64::from(descriptor.len())).map_err(
        |source| VirtioBalloonStatisticsDescriptorReadError::DescriptorRange {
            index: descriptor.index(),
            address: descriptor.address(),
            len: descriptor.len(),
            source,
        },
    )?;
    memory.validate_mapped_range(range).map_err(|source| {
        VirtioBalloonStatisticsDescriptorReadError::DescriptorAccess {
            index: descriptor.index(),
            address: descriptor.address(),
            len: descriptor.len(),
            source,
        }
    })
}

fn balloon_statistics_descriptor_len(
    descriptor: VirtqueueDescriptor,
) -> Result<usize, VirtioBalloonStatisticsDescriptorReadError> {
    usize::try_from(descriptor.len()).map_err(|_| {
        VirtioBalloonStatisticsDescriptorReadError::DescriptorLengthTooLarge {
            index: descriptor.index(),
            len: descriptor.len(),
        }
    })
}

fn read_balloon_statistics_descriptor_segment(
    memory: &GuestMemory,
    descriptor: VirtqueueDescriptor,
    bytes: &mut [u8],
    offset: usize,
) -> Result<usize, VirtioBalloonStatisticsDescriptorReadError> {
    let segment_len = balloon_statistics_descriptor_len(descriptor)?;
    let end = offset.checked_add(segment_len).ok_or(
        VirtioBalloonStatisticsDescriptorReadError::PayloadLengthOverflow {
            current: offset,
            len: descriptor.len(),
        },
    )?;
    let buffer_len = bytes.len();
    let destination = bytes.get_mut(offset..end).ok_or(
        VirtioBalloonStatisticsDescriptorReadError::PayloadBufferRange {
            offset,
            len: segment_len,
            buffer_len,
        },
    )?;

    memory
        .read_slice(destination, descriptor.address())
        .map_err(
            |source| VirtioBalloonStatisticsDescriptorReadError::DescriptorRead {
                index: descriptor.index(),
                address: descriptor.address(),
                len: descriptor.len(),
                source,
            },
        )?;

    Ok(end)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioBalloonQueueKind {
    Inflate,
    Deflate,
    Statistics,
    FreePageHinting,
    FreePageReporting,
}

impl fmt::Display for VirtioBalloonQueueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inflate => f.write_str("inflate"),
            Self::Deflate => f.write_str("deflate"),
            Self::Statistics => f.write_str("statistics"),
            Self::FreePageHinting => f.write_str("free-page-hinting"),
            Self::FreePageReporting => f.write_str("free-page-reporting"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBalloonQueueConfig {
    kind: VirtioBalloonQueueKind,
    index: usize,
    size: u16,
}

impl VirtioBalloonQueueConfig {
    pub const fn new(kind: VirtioBalloonQueueKind, index: usize, size: u16) -> Self {
        Self { kind, index, size }
    }

    pub const fn kind(self) -> VirtioBalloonQueueKind {
        self.kind
    }

    pub const fn index(self) -> usize {
        self.index
    }

    pub const fn size(self) -> u16 {
        self.size
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBalloonQueueLayout {
    inflate: VirtioBalloonQueueConfig,
    deflate: VirtioBalloonQueueConfig,
    statistics: Option<VirtioBalloonQueueConfig>,
    free_page_hinting: Option<VirtioBalloonQueueConfig>,
    free_page_reporting: Option<VirtioBalloonQueueConfig>,
}

impl VirtioBalloonQueueLayout {
    pub fn from_config(config: BalloonConfig) -> Self {
        let inflate = VirtioBalloonQueueConfig::new(
            VirtioBalloonQueueKind::Inflate,
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
            VIRTIO_BALLOON_QUEUE_SIZE,
        );
        let deflate = VirtioBalloonQueueConfig::new(
            VirtioBalloonQueueKind::Deflate,
            VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
            VIRTIO_BALLOON_QUEUE_SIZE,
        );
        let mut next_queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;

        let statistics = if config.stats_polling_interval_s() > 0 {
            let queue = VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::Statistics,
                next_queue_index,
                VIRTIO_BALLOON_QUEUE_SIZE,
            );
            next_queue_index += 1;
            Some(queue)
        } else {
            None
        };

        let free_page_hinting = if config.free_page_hinting() {
            let queue = VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::FreePageHinting,
                next_queue_index,
                VIRTIO_BALLOON_QUEUE_SIZE,
            );
            next_queue_index += 1;
            Some(queue)
        } else {
            None
        };

        let free_page_reporting = if config.free_page_reporting() {
            Some(VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::FreePageReporting,
                next_queue_index,
                VIRTIO_BALLOON_QUEUE_SIZE,
            ))
        } else {
            None
        };

        Self {
            inflate,
            deflate,
            statistics,
            free_page_hinting,
            free_page_reporting,
        }
    }

    pub const fn inflate(self) -> VirtioBalloonQueueConfig {
        self.inflate
    }

    pub const fn deflate(self) -> VirtioBalloonQueueConfig {
        self.deflate
    }

    pub const fn statistics(self) -> Option<VirtioBalloonQueueConfig> {
        self.statistics
    }

    pub const fn free_page_hinting(self) -> Option<VirtioBalloonQueueConfig> {
        self.free_page_hinting
    }

    pub const fn free_page_reporting(self) -> Option<VirtioBalloonQueueConfig> {
        self.free_page_reporting
    }

    pub fn queue_count(self) -> usize {
        self.iter().count()
    }

    pub fn queue_sizes(self) -> VirtioBalloonQueueSizes {
        VirtioBalloonQueueSizes::from_layout(self)
    }

    pub fn iter(self) -> impl Iterator<Item = VirtioBalloonQueueConfig> {
        [
            Some(self.inflate),
            Some(self.deflate),
            self.statistics,
            self.free_page_hinting,
            self.free_page_reporting,
        ]
        .into_iter()
        .flatten()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBalloonQueueSizes {
    sizes: [u16; VIRTIO_BALLOON_MAX_QUEUE_COUNT],
    len: usize,
}

impl VirtioBalloonQueueSizes {
    pub fn from_layout(layout: VirtioBalloonQueueLayout) -> Self {
        let mut sizes = [0; VIRTIO_BALLOON_MAX_QUEUE_COUNT];
        let mut len = 0;
        for queue in layout.iter() {
            if let Some(size) = sizes.get_mut(len) {
                *size = queue.size();
                len += 1;
            }
        }

        Self { sizes, len }
    }

    pub const fn len(self) -> usize {
        self.len
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u16] {
        let (used, _) = self.sizes.split_at(self.len);
        used
    }
}

#[derive(Debug)]
pub enum VirtioBalloonQueueBuildError {
    QueueNotReady,
    AvailableRing { source: VirtqueueAvailableRingError },
    UsedRing { source: VirtqueueUsedRingError },
}

impl fmt::Display for VirtioBalloonQueueBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueNotReady => f.write_str("virtio-balloon queue is not ready"),
            Self::AvailableRing { source } => {
                write!(
                    f,
                    "failed to build virtio-balloon available ring from queue state: {source}"
                )
            }
            Self::UsedRing { source } => {
                write!(
                    f,
                    "failed to build virtio-balloon used ring from queue state: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioBalloonQueueBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRing { source } => Some(source),
            Self::UsedRing { source } => Some(source),
            Self::QueueNotReady => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioBalloonQueue {
    available: VirtqueueAvailableRing,
    used: VirtqueueUsedRing,
}

impl VirtioBalloonQueue {
    pub const fn new(available: VirtqueueAvailableRing, used: VirtqueueUsedRing) -> Self {
        Self { available, used }
    }

    pub fn from_mmio_queue_state(
        queue: &VirtioMmioQueueState,
    ) -> Result<Self, VirtioBalloonQueueBuildError> {
        if !queue.ready() {
            return Err(VirtioBalloonQueueBuildError::QueueNotReady);
        }

        let available = VirtqueueAvailableRing::new(
            queue.descriptor_table(),
            queue.driver_ring(),
            queue.size(),
        )
        .map_err(|source| VirtioBalloonQueueBuildError::AvailableRing { source })?;
        let used = VirtqueueUsedRing::new(queue.device_ring(), queue.size())
            .map_err(|source| VirtioBalloonQueueBuildError::UsedRing { source })?;

        Ok(Self { available, used })
    }

    pub const fn available_ring(&self) -> &VirtqueueAvailableRing {
        &self.available
    }

    pub const fn used_ring(&self) -> &VirtqueueUsedRing {
        &self.used
    }

    pub fn dispatch_deflate(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioBalloonQueueDispatch, VirtioBalloonQueueDispatchError> {
        let mut dispatch = VirtioBalloonQueueDispatch::default();

        while let Some(chain) = self
            .available
            .pop_descriptor_chain(memory)
            .map_err(|source| VirtioBalloonQueueDispatchError::AvailableRing {
                queue: VirtioBalloonQueueKind::Deflate,
                completed_dispatch: Box::new(dispatch.clone()),
                source,
            })?
        {
            let descriptor_head = descriptor_chain_head(&chain).ok_or_else(|| {
                VirtioBalloonQueueDispatchError::EmptyDescriptorChain {
                    queue: VirtioBalloonQueueKind::Deflate,
                    completed_dispatch: Box::new(dispatch.clone()),
                }
            })?;
            let pfn_ranges = read_balloon_queue_pfn_ranges(
                memory,
                &chain,
                VirtioBalloonQueueKind::Deflate,
                descriptor_head,
                &dispatch,
            )?;
            let range_count = pfn_ranges.len();
            dispatch
                .reserve_deflated_page_ranges(range_count)
                .map_err(
                    |source| VirtioBalloonQueueDispatchError::DeflatedRangeAllocation {
                        completed_dispatch: Box::new(dispatch.clone()),
                        descriptor_head,
                        range_count,
                        source,
                    },
                )?;
            let publication = self
                .used
                .publish_used_element_with_notification(
                    memory,
                    descriptor_head,
                    0,
                    VirtqueueNotificationSuppression::Disabled,
                )
                .map_err(|source| VirtioBalloonQueueDispatchError::UsedRing {
                    queue: VirtioBalloonQueueKind::Deflate,
                    completed_dispatch: Box::new(dispatch.clone()),
                    descriptor_head,
                    source,
                })?;
            dispatch.record_deflate_descriptor(pfn_ranges.ranges(), publication);
        }

        Ok(dispatch)
    }

    pub fn dispatch_inflate(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioBalloonQueueDispatch, VirtioBalloonQueueDispatchError> {
        let mut adviser = SystemGuestMemoryDiscardAdviser;
        self.dispatch_inflate_with_adviser(memory, &mut adviser)
    }

    fn dispatch_inflate_with_adviser(
        &mut self,
        memory: &mut GuestMemory,
        adviser: &mut impl GuestMemoryDiscardAdviser,
    ) -> Result<VirtioBalloonQueueDispatch, VirtioBalloonQueueDispatchError> {
        let mut dispatch = VirtioBalloonQueueDispatch::default();

        while let Some(chain) = self
            .available
            .pop_descriptor_chain(memory)
            .map_err(|source| VirtioBalloonQueueDispatchError::AvailableRing {
                queue: VirtioBalloonQueueKind::Inflate,
                completed_dispatch: Box::new(dispatch.clone()),
                source,
            })?
        {
            let descriptor_head = descriptor_chain_head(&chain).ok_or_else(|| {
                VirtioBalloonQueueDispatchError::EmptyDescriptorChain {
                    queue: VirtioBalloonQueueKind::Inflate,
                    completed_dispatch: Box::new(dispatch.clone()),
                }
            })?;
            let pfn_ranges = read_balloon_queue_pfn_ranges(
                memory,
                &chain,
                VirtioBalloonQueueKind::Inflate,
                descriptor_head,
                &dispatch,
            )?;
            let range_count = pfn_ranges.len();
            dispatch
                .reserve_inflated_page_ranges(range_count)
                .map_err(
                    |source| VirtioBalloonQueueDispatchError::InflatedRangeAllocation {
                        completed_dispatch: Box::new(dispatch.clone()),
                        descriptor_head,
                        range_count,
                        source,
                    },
                )?;
            let publication = self
                .used
                .publish_used_element_with_notification(
                    memory,
                    descriptor_head,
                    0,
                    VirtqueueNotificationSuppression::Disabled,
                )
                .map_err(|source| VirtioBalloonQueueDispatchError::UsedRing {
                    queue: VirtioBalloonQueueKind::Inflate,
                    completed_dispatch: Box::new(dispatch.clone()),
                    descriptor_head,
                    source,
                })?;
            let discard = discard_balloon_pfn_ranges(memory, pfn_ranges.ranges(), adviser);
            dispatch.record_inflate_descriptor(pfn_ranges.ranges(), discard, publication);
        }

        Ok(dispatch)
    }

    pub fn dispatch_statistics(
        &mut self,
        memory: &mut GuestMemory,
        context: VirtioBalloonStatisticsDispatchContext,
    ) -> Result<VirtioBalloonQueueDispatch, VirtioBalloonQueueDispatchError> {
        let mut dispatch = VirtioBalloonQueueDispatch {
            statistics_pending_descriptor_head: context.pending_descriptor_head(),
            statistics: context.statistics(),
            ..Default::default()
        };

        while let Some(chain) = self
            .available
            .pop_descriptor_chain(memory)
            .map_err(|source| VirtioBalloonQueueDispatchError::AvailableRing {
                queue: VirtioBalloonQueueKind::Statistics,
                completed_dispatch: Box::new(dispatch.clone()),
                source,
            })?
        {
            let descriptor_head = descriptor_chain_head(&chain).ok_or_else(|| {
                VirtioBalloonQueueDispatchError::EmptyDescriptorChain {
                    queue: VirtioBalloonQueueKind::Statistics,
                    completed_dispatch: Box::new(dispatch.clone()),
                }
            })?;

            if let Some(pending_descriptor_head) = dispatch.statistics_pending_descriptor_head {
                let publication = self
                    .used
                    .publish_used_element_with_notification(
                        memory,
                        pending_descriptor_head,
                        0,
                        VirtqueueNotificationSuppression::Disabled,
                    )
                    .map_err(|source| VirtioBalloonQueueDispatchError::UsedRing {
                        queue: VirtioBalloonQueueKind::Statistics,
                        completed_dispatch: Box::new(dispatch.clone()),
                        descriptor_head: pending_descriptor_head,
                        source,
                    })?;
                dispatch.statistics_pending_descriptor_head = None;
                dispatch.record_statistics_completed_descriptor(publication);
            }

            let descriptor =
                VirtioBalloonStatisticsDescriptor::read(memory, &chain).map_err(|source| {
                    VirtioBalloonQueueDispatchError::StatisticsDescriptorRead {
                        completed_dispatch: Box::new(dispatch.clone()),
                        descriptor_head,
                        source,
                    }
                })?;
            dispatch.record_statistics_descriptor(descriptor_head, descriptor.payload());
        }

        Ok(dispatch)
    }

    pub fn complete_pending_statistics(
        &mut self,
        memory: &mut GuestMemory,
        context: VirtioBalloonStatisticsDispatchContext,
    ) -> Result<VirtioBalloonQueueDispatch, VirtioBalloonQueueDispatchError> {
        let mut dispatch = VirtioBalloonQueueDispatch {
            statistics_pending_descriptor_head: context.pending_descriptor_head(),
            statistics: context.statistics(),
            ..Default::default()
        };
        let Some(pending_descriptor_head) = dispatch.statistics_pending_descriptor_head else {
            return Ok(dispatch);
        };

        let publication = self
            .used
            .publish_used_element_with_notification(
                memory,
                pending_descriptor_head,
                0,
                VirtqueueNotificationSuppression::Disabled,
            )
            .map_err(|source| VirtioBalloonQueueDispatchError::UsedRing {
                queue: VirtioBalloonQueueKind::Statistics,
                completed_dispatch: Box::new(dispatch.clone()),
                descriptor_head: pending_descriptor_head,
                source,
            })?;
        dispatch.statistics_pending_descriptor_head = None;
        dispatch.record_statistics_completed_descriptor(publication);

        Ok(dispatch)
    }

    pub fn dispatch_hinting_commands(
        &mut self,
        memory: &mut GuestMemory,
        context: VirtioBalloonHintingDispatchContext,
    ) -> Result<VirtioBalloonQueueDispatch, VirtioBalloonQueueDispatchError> {
        let mut adviser = SystemGuestMemoryDiscardAdviser;
        self.dispatch_hinting_commands_with_adviser(memory, context, &mut adviser)
    }

    fn dispatch_hinting_commands_with_adviser(
        &mut self,
        memory: &mut GuestMemory,
        context: VirtioBalloonHintingDispatchContext,
        adviser: &mut impl GuestMemoryDiscardAdviser,
    ) -> Result<VirtioBalloonQueueDispatch, VirtioBalloonQueueDispatchError> {
        let mut dispatch = VirtioBalloonQueueDispatch::default();
        let mut current_guest_cmd = context.guest_cmd();

        while let Some(chain) = self
            .available
            .pop_descriptor_chain(memory)
            .map_err(|source| VirtioBalloonQueueDispatchError::AvailableRing {
                queue: VirtioBalloonQueueKind::FreePageHinting,
                completed_dispatch: Box::new(dispatch.clone()),
                source,
            })?
        {
            let descriptor_head = descriptor_chain_head(&chain).ok_or_else(|| {
                VirtioBalloonQueueDispatchError::EmptyDescriptorChain {
                    queue: VirtioBalloonQueueKind::FreePageHinting,
                    completed_dispatch: Box::new(dispatch.clone()),
                }
            })?;
            let descriptor_dispatch = read_balloon_hinting_descriptor(
                memory,
                &chain,
                descriptor_head,
                context.host_cmd(),
                current_guest_cmd,
                &dispatch,
            )?;
            dispatch
                .reserve_hinting_page_ranges(descriptor_dispatch.hinting_page_ranges().len())
                .map_err(
                    |source| VirtioBalloonQueueDispatchError::HintingRangeAllocation {
                        completed_dispatch: Box::new(dispatch.clone()),
                        descriptor_head,
                        range_count: descriptor_dispatch.hinting_page_ranges().len(),
                        source,
                    },
                )?;
            let publication = self
                .used
                .publish_used_element_with_notification(
                    memory,
                    descriptor_head,
                    0,
                    VirtqueueNotificationSuppression::Disabled,
                )
                .map_err(|source| VirtioBalloonQueueDispatchError::UsedRing {
                    queue: VirtioBalloonQueueKind::FreePageHinting,
                    completed_dispatch: Box::new(dispatch.clone()),
                    descriptor_head,
                    source,
                })?;
            if let Some(guest_cmd) = descriptor_dispatch.guest_cmd() {
                current_guest_cmd = Some(guest_cmd);
            }
            let discard = discard_balloon_guest_ranges(
                memory,
                descriptor_dispatch.hinting_page_ranges(),
                adviser,
            );
            dispatch.record_hinting_descriptor(descriptor_dispatch, discard, publication);
        }

        Ok(dispatch)
    }

    pub fn dispatch_free_page_reporting(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioBalloonQueueDispatch, VirtioBalloonQueueDispatchError> {
        let mut adviser = SystemGuestMemoryDiscardAdviser;
        self.dispatch_free_page_reporting_with_adviser(memory, &mut adviser)
    }

    fn dispatch_free_page_reporting_with_adviser(
        &mut self,
        memory: &mut GuestMemory,
        adviser: &mut impl GuestMemoryDiscardAdviser,
    ) -> Result<VirtioBalloonQueueDispatch, VirtioBalloonQueueDispatchError> {
        let mut dispatch = VirtioBalloonQueueDispatch::default();

        while let Some(chain) = self
            .available
            .pop_descriptor_chain(memory)
            .map_err(|source| VirtioBalloonQueueDispatchError::AvailableRing {
                queue: VirtioBalloonQueueKind::FreePageReporting,
                completed_dispatch: Box::new(dispatch.clone()),
                source,
            })?
        {
            let descriptor_head = descriptor_chain_head(&chain).ok_or_else(|| {
                VirtioBalloonQueueDispatchError::EmptyDescriptorChain {
                    queue: VirtioBalloonQueueKind::FreePageReporting,
                    completed_dispatch: Box::new(dispatch.clone()),
                }
            })?;
            let discard = discard_balloon_reporting_descriptors(memory, &chain, adviser);
            dispatch.record_reporting_discard(discard);
            let publication = self
                .used
                .publish_used_element_with_notification(
                    memory,
                    descriptor_head,
                    0,
                    VirtqueueNotificationSuppression::Disabled,
                )
                .map_err(|source| VirtioBalloonQueueDispatchError::UsedRing {
                    queue: VirtioBalloonQueueKind::FreePageReporting,
                    completed_dispatch: Box::new(dispatch.clone()),
                    descriptor_head,
                    source,
                })?;
            dispatch.record_reporting_completion(publication);
        }

        Ok(dispatch)
    }
}

fn discard_balloon_reporting_descriptors(
    memory: &GuestMemory,
    chain: &VirtqueueDescriptorChain,
    adviser: &mut impl GuestMemoryDiscardAdviser,
) -> VirtioBalloonDiscardOutcome {
    let mut discard = VirtioBalloonDiscardOutcome::default();
    for descriptor in chain.descriptors().iter().copied() {
        match balloon_reporting_range(descriptor) {
            Ok(range) => discard
                .record_guest_memory_outcome(memory.discard_range_with_adviser(range, adviser)),
            Err(_) => discard.record_failed_conversion(u64::from(descriptor.len())),
        }
    }
    discard
}

fn balloon_reporting_range(
    descriptor: VirtqueueDescriptor,
) -> Result<GuestMemoryRange, VirtioBalloonReportingRangeError> {
    if !descriptor.is_write_only() {
        return Err(VirtioBalloonReportingRangeError::DescriptorReadable {
            index: descriptor.index(),
        });
    }
    if descriptor.is_empty() {
        return Err(VirtioBalloonReportingRangeError::DescriptorEmpty {
            index: descriptor.index(),
        });
    }

    GuestMemoryRange::new(descriptor.address(), u64::from(descriptor.len())).map_err(|source| {
        VirtioBalloonReportingRangeError::Range {
            index: descriptor.index(),
            address: descriptor.address(),
            len: descriptor.len(),
            source,
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VirtioBalloonReportingRangeError {
    DescriptorReadable {
        index: u16,
    },
    DescriptorEmpty {
        index: u16,
    },
    Range {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryError,
    },
}

impl fmt::Display for VirtioBalloonReportingRangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DescriptorReadable { index } => write!(
                f,
                "virtio-balloon free-page reporting descriptor {index} is device-readable"
            ),
            Self::DescriptorEmpty { index } => write!(
                f,
                "virtio-balloon free-page reporting descriptor {index} is empty"
            ),
            Self::Range {
                index,
                address,
                len,
                source,
            } => write!(
                f,
                "virtio-balloon free-page reporting descriptor {index} range address={address}, len={len} is invalid: {source}"
            ),
        }
    }
}

impl std::error::Error for VirtioBalloonReportingRangeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Range { source, .. } => Some(source),
            Self::DescriptorReadable { .. } | Self::DescriptorEmpty { .. } => None,
        }
    }
}

fn discard_balloon_pfn_ranges(
    memory: &GuestMemory,
    ranges: &[VirtioBalloonPfnRange],
    adviser: &mut impl GuestMemoryDiscardAdviser,
) -> VirtioBalloonDiscardOutcome {
    let mut discard = VirtioBalloonDiscardOutcome::default();
    for pfn_range in ranges.iter().copied() {
        match pfn_range.guest_memory_range() {
            Ok(Some(guest_range)) => discard.record_guest_memory_outcome(
                memory.discard_range_with_adviser(guest_range, adviser),
            ),
            Ok(None) => {}
            Err(_) => discard.record_failed_conversion(
                u64::from(pfn_range.page_count()).saturating_mul(VIRTIO_BALLOON_PAGE_SIZE),
            ),
        }
    }
    discard
}

fn discard_balloon_guest_ranges(
    memory: &GuestMemory,
    ranges: &[GuestMemoryRange],
    adviser: &mut impl GuestMemoryDiscardAdviser,
) -> VirtioBalloonDiscardOutcome {
    let mut discard = VirtioBalloonDiscardOutcome::default();
    for range in ranges.iter().copied() {
        discard.record_guest_memory_outcome(memory.discard_range_with_adviser(range, adviser));
    }
    discard
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBalloonStatisticsDispatchContext {
    pending_descriptor_head: Option<u16>,
    statistics: BalloonOptionalStats,
}

impl VirtioBalloonStatisticsDispatchContext {
    pub const fn new(
        pending_descriptor_head: Option<u16>,
        statistics: BalloonOptionalStats,
    ) -> Self {
        Self {
            pending_descriptor_head,
            statistics,
        }
    }

    pub const fn pending_descriptor_head(self) -> Option<u16> {
        self.pending_descriptor_head
    }

    pub const fn statistics(self) -> BalloonOptionalStats {
        self.statistics
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBalloonHintingDispatchContext {
    host_cmd: u32,
    guest_cmd: Option<u32>,
}

impl VirtioBalloonHintingDispatchContext {
    pub const fn new(host_cmd: u32, guest_cmd: Option<u32>) -> Self {
        Self {
            host_cmd,
            guest_cmd,
        }
    }

    pub const fn host_cmd(self) -> u32 {
        self.host_cmd
    }

    pub const fn guest_cmd(self) -> Option<u32> {
        self.guest_cmd
    }
}

/// Aggregates best-effort host-discard work for one balloon queue dispatch.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBalloonDiscardOutcome {
    attempts: u64,
    requested_bytes: u64,
    advised_bytes: u64,
    skipped_bytes: u64,
    failed_bytes: u64,
    failures: u64,
}

impl VirtioBalloonDiscardOutcome {
    /// Returns the number of accepted guest ranges passed to discard.
    pub const fn attempts(self) -> u64 {
        self.attempts
    }

    /// Returns the total bytes requested by accepted guest ranges.
    pub const fn requested_bytes(self) -> u64 {
        self.requested_bytes
    }

    /// Returns bytes whose aligned host interiors completed zero and free advice.
    pub const fn advised_bytes(self) -> u64 {
        self.advised_bytes
    }

    /// Returns partial host-page edge bytes skipped by discard.
    pub const fn skipped_bytes(self) -> u64 {
        self.skipped_bytes
    }

    /// Returns bytes that did not complete zero and free advice.
    pub const fn failed_bytes(self) -> u64 {
        self.failed_bytes
    }

    /// Returns accepted guest-range attempts that had any discard failure.
    pub const fn failures(self) -> u64 {
        self.failures
    }

    const fn merged_with(self, other: Self) -> Self {
        Self {
            attempts: self.attempts.saturating_add(other.attempts),
            requested_bytes: self.requested_bytes.saturating_add(other.requested_bytes),
            advised_bytes: self.advised_bytes.saturating_add(other.advised_bytes),
            skipped_bytes: self.skipped_bytes.saturating_add(other.skipped_bytes),
            failed_bytes: self.failed_bytes.saturating_add(other.failed_bytes),
            failures: self.failures.saturating_add(other.failures),
        }
    }

    fn record_guest_memory_outcome(&mut self, outcome: GuestMemoryDiscardOutcome) {
        self.attempts = self.attempts.saturating_add(1);
        self.requested_bytes = self
            .requested_bytes
            .saturating_add(outcome.requested_bytes());
        self.advised_bytes = self.advised_bytes.saturating_add(outcome.advised_bytes());
        self.skipped_bytes = self.skipped_bytes.saturating_add(outcome.skipped_bytes());
        self.failed_bytes = self.failed_bytes.saturating_add(outcome.failed_bytes());
        if !outcome.is_complete() {
            self.failures = self.failures.saturating_add(1);
        }
    }

    fn record_failed_conversion(&mut self, requested_bytes: u64) {
        self.attempts = self.attempts.saturating_add(1);
        self.requested_bytes = self.requested_bytes.saturating_add(requested_bytes);
        self.failed_bytes = self.failed_bytes.saturating_add(requested_bytes);
        self.failures = self.failures.saturating_add(1);
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VirtioBalloonQueueDispatch {
    completed_descriptors: usize,
    needs_queue_interrupt: bool,
    inflated_page_ranges: Vec<VirtioBalloonPfnRange>,
    deflated_page_ranges: Vec<VirtioBalloonPfnRange>,
    statistics: BalloonOptionalStats,
    statistics_reports: usize,
    statistics_oversized_reports: usize,
    statistics_pending_descriptor_head: Option<u16>,
    hinting_page_ranges: Vec<GuestMemoryRange>,
    inflate_discard: VirtioBalloonDiscardOutcome,
    hinting_discard: VirtioBalloonDiscardOutcome,
    reporting_discard: VirtioBalloonDiscardOutcome,
    hinting_guest_cmd: Option<u32>,
    hinting_completed_run: bool,
}

impl VirtioBalloonQueueDispatch {
    pub const fn completed_descriptors(&self) -> usize {
        self.completed_descriptors
    }

    pub const fn needs_queue_interrupt(&self) -> bool {
        self.needs_queue_interrupt
    }

    pub fn inflated_page_ranges(&self) -> &[VirtioBalloonPfnRange] {
        &self.inflated_page_ranges
    }

    pub fn deflated_page_ranges(&self) -> &[VirtioBalloonPfnRange] {
        &self.deflated_page_ranges
    }

    pub const fn statistics(&self) -> BalloonOptionalStats {
        self.statistics
    }

    pub const fn statistics_reports(&self) -> usize {
        self.statistics_reports
    }

    pub const fn statistics_oversized_reports(&self) -> usize {
        self.statistics_oversized_reports
    }

    pub const fn statistics_pending_descriptor_head(&self) -> Option<u16> {
        self.statistics_pending_descriptor_head
    }

    pub fn hinting_page_ranges(&self) -> &[GuestMemoryRange] {
        &self.hinting_page_ranges
    }

    pub const fn inflate_discard(&self) -> VirtioBalloonDiscardOutcome {
        self.inflate_discard
    }

    pub const fn hinting_discard(&self) -> VirtioBalloonDiscardOutcome {
        self.hinting_discard
    }

    pub const fn reporting_discard(&self) -> VirtioBalloonDiscardOutcome {
        self.reporting_discard
    }

    pub const fn hinting_guest_cmd(&self) -> Option<u32> {
        self.hinting_guest_cmd
    }

    pub const fn hinting_completed_run(&self) -> bool {
        self.hinting_completed_run
    }

    fn reserve_inflated_page_ranges(&mut self, range_count: usize) -> Result<(), TryReserveError> {
        self.inflated_page_ranges.try_reserve(range_count)
    }

    fn reserve_deflated_page_ranges(&mut self, range_count: usize) -> Result<(), TryReserveError> {
        self.deflated_page_ranges.try_reserve(range_count)
    }

    fn reserve_hinting_page_ranges(&mut self, range_count: usize) -> Result<(), TryReserveError> {
        self.hinting_page_ranges.try_reserve(range_count)
    }

    fn record_inflate_descriptor(
        &mut self,
        ranges: &[VirtioBalloonPfnRange],
        discard: VirtioBalloonDiscardOutcome,
        publication: VirtqueueUsedRingPublication,
    ) {
        self.completed_descriptors += 1;
        self.needs_queue_interrupt |= publication.needs_queue_interrupt();
        self.inflated_page_ranges.extend_from_slice(ranges);
        self.inflate_discard = self.inflate_discard.merged_with(discard);
    }

    fn record_deflate_descriptor(
        &mut self,
        ranges: &[VirtioBalloonPfnRange],
        publication: VirtqueueUsedRingPublication,
    ) {
        self.completed_descriptors += 1;
        self.needs_queue_interrupt |= publication.needs_queue_interrupt();
        self.deflated_page_ranges.extend_from_slice(ranges);
    }

    fn record_statistics_completed_descriptor(
        &mut self,
        publication: VirtqueueUsedRingPublication,
    ) {
        self.completed_descriptors += 1;
        self.needs_queue_interrupt |= publication.needs_queue_interrupt();
    }

    fn record_statistics_descriptor(
        &mut self,
        descriptor_head: u16,
        payload: VirtioBalloonStatisticsDescriptorPayload,
    ) {
        self.statistics_pending_descriptor_head = Some(descriptor_head);
        if let Some(stats) = payload.report_stats() {
            self.statistics_reports += 1;
            self.statistics.merge_from(stats);
        }
        if payload.is_oversized() {
            self.statistics_oversized_reports += 1;
        }
    }

    fn record_hinting_descriptor(
        &mut self,
        mut descriptor_dispatch: VirtioBalloonHintingDescriptorDispatch,
        discard: VirtioBalloonDiscardOutcome,
        publication: VirtqueueUsedRingPublication,
    ) {
        self.completed_descriptors += 1;
        self.needs_queue_interrupt |= publication.needs_queue_interrupt();
        self.hinting_page_ranges
            .append(&mut descriptor_dispatch.hinting_page_ranges);
        self.hinting_discard = self.hinting_discard.merged_with(discard);
        if let Some(guest_cmd) = descriptor_dispatch.guest_cmd {
            self.hinting_guest_cmd = Some(guest_cmd);
            self.hinting_completed_run = is_completed_balloon_hinting_guest_cmd(guest_cmd);
        }
    }

    fn record_reporting_discard(&mut self, discard: VirtioBalloonDiscardOutcome) {
        self.reporting_discard = self.reporting_discard.merged_with(discard);
    }

    fn record_reporting_completion(&mut self, publication: VirtqueueUsedRingPublication) {
        self.completed_descriptors += 1;
        self.needs_queue_interrupt |= publication.needs_queue_interrupt();
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct VirtioBalloonHintingDescriptorDispatch {
    guest_cmd: Option<u32>,
    hinting_page_ranges: Vec<GuestMemoryRange>,
}

impl VirtioBalloonHintingDescriptorDispatch {
    const fn guest_cmd(&self) -> Option<u32> {
        self.guest_cmd
    }

    fn hinting_page_ranges(&self) -> &[GuestMemoryRange] {
        &self.hinting_page_ranges
    }

    fn reserve_hinting_page_range(&mut self) -> Result<(), TryReserveError> {
        self.hinting_page_ranges.try_reserve(1)
    }
}

fn read_balloon_hinting_descriptor(
    memory: &GuestMemory,
    chain: &VirtqueueDescriptorChain,
    descriptor_head: u16,
    host_cmd: u32,
    mut current_guest_cmd: Option<u32>,
    completed_dispatch: &VirtioBalloonQueueDispatch,
) -> Result<VirtioBalloonHintingDescriptorDispatch, VirtioBalloonQueueDispatchError> {
    let mut descriptor_dispatch = VirtioBalloonHintingDescriptorDispatch::default();
    for descriptor in chain.descriptors().iter().copied() {
        // Firecracker identifies free-page hinting command descriptors by length.
        if descriptor.len() == VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32 {
            let mut bytes = [0; VIRTIO_BALLOON_HINTING_COMMAND_SIZE];
            memory
                .read_slice(&mut bytes, descriptor.address())
                .map_err(
                    |source| VirtioBalloonQueueDispatchError::HintingCommandRead {
                        completed_dispatch: Box::new(completed_dispatch.clone()),
                        descriptor_head,
                        descriptor_index: descriptor.index(),
                        address: descriptor.address(),
                        source,
                    },
                )?;
            let guest_cmd = u32::from_le_bytes(bytes);
            descriptor_dispatch.guest_cmd = Some(guest_cmd);
            current_guest_cmd = Some(guest_cmd);
            continue;
        }

        if !should_record_balloon_hinting_range(host_cmd, current_guest_cmd) {
            continue;
        }

        let range = read_balloon_hinting_range(memory, descriptor).map_err(|source| {
            VirtioBalloonQueueDispatchError::HintingRange {
                completed_dispatch: Box::new(completed_dispatch.clone()),
                descriptor_head,
                descriptor_index: descriptor.index(),
                address: descriptor.address(),
                len: descriptor.len(),
                source,
            }
        })?;
        descriptor_dispatch
            .reserve_hinting_page_range()
            .map_err(
                |source| VirtioBalloonQueueDispatchError::HintingRangeAllocation {
                    completed_dispatch: Box::new(completed_dispatch.clone()),
                    descriptor_head,
                    range_count: descriptor_dispatch.hinting_page_ranges.len() + 1,
                    source,
                },
            )?;
        descriptor_dispatch.hinting_page_ranges.push(range);
    }

    Ok(descriptor_dispatch)
}

const fn is_completed_balloon_hinting_guest_cmd(guest_cmd: u32) -> bool {
    matches!(
        guest_cmd,
        VIRTIO_BALLOON_FREE_PAGE_HINT_STOP | VIRTIO_BALLOON_FREE_PAGE_HINT_DONE
    )
}

const fn is_active_balloon_hinting_host_cmd(host_cmd: u32) -> bool {
    !matches!(
        host_cmd,
        VIRTIO_BALLOON_FREE_PAGE_HINT_STOP | VIRTIO_BALLOON_FREE_PAGE_HINT_DONE
    )
}

fn should_record_balloon_hinting_range(host_cmd: u32, guest_cmd: Option<u32>) -> bool {
    is_active_balloon_hinting_host_cmd(host_cmd) && guest_cmd == Some(host_cmd)
}

fn read_balloon_hinting_range(
    memory: &GuestMemory,
    descriptor: VirtqueueDescriptor,
) -> Result<GuestMemoryRange, VirtioBalloonHintingRangeError> {
    let range = GuestMemoryRange::new(descriptor.address(), u64::from(descriptor.len()))
        .map_err(VirtioBalloonHintingRangeError::Range)?;
    memory
        .validate_mapped_range(range)
        .map_err(VirtioBalloonHintingRangeError::Access)?;

    Ok(range)
}

#[derive(Debug)]
pub enum VirtioBalloonHintingRangeError {
    Range(GuestMemoryError),
    Access(GuestMemoryAccessError),
}

impl fmt::Display for VirtioBalloonHintingRangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Range(source) => write!(f, "{source}"),
            Self::Access(source) => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for VirtioBalloonHintingRangeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Range(source) => Some(source),
            Self::Access(source) => Some(source),
        }
    }
}

fn read_balloon_queue_pfn_ranges(
    memory: &GuestMemory,
    chain: &VirtqueueDescriptorChain,
    queue: VirtioBalloonQueueKind,
    descriptor_head: u16,
    completed_dispatch: &VirtioBalloonQueueDispatch,
) -> Result<VirtioBalloonPfnRanges, VirtioBalloonQueueDispatchError> {
    let payload = VirtioBalloonPfnDescriptorPayload::read(memory, chain).map_err(|source| {
        VirtioBalloonQueueDispatchError::PfnDescriptorRead {
            queue,
            completed_dispatch: Box::new(completed_dispatch.clone()),
            descriptor_head,
            source,
        }
    })?;
    let pfn_payload = payload.into_pfn_payload().map_err(|source| {
        VirtioBalloonQueueDispatchError::PfnPayloadParse {
            queue,
            completed_dispatch: Box::new(completed_dispatch.clone()),
            descriptor_head,
            source,
        }
    })?;
    let pfn_ranges = pfn_payload.into_page_ranges().map_err(|source| {
        VirtioBalloonQueueDispatchError::PfnRangeCompact {
            queue,
            completed_dispatch: Box::new(completed_dispatch.clone()),
            descriptor_head,
            source,
        }
    })?;

    validate_pfn_ranges_mapped(memory, &pfn_ranges).map_err(|source| {
        VirtioBalloonQueueDispatchError::PfnRangeAccess {
            queue,
            completed_dispatch: Box::new(completed_dispatch.clone()),
            descriptor_head,
            source,
        }
    })?;

    Ok(pfn_ranges)
}

#[derive(Debug)]
pub enum VirtioBalloonQueueDispatchError {
    AvailableRing {
        queue: VirtioBalloonQueueKind,
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        source: VirtqueueAvailableRingError,
    },
    EmptyDescriptorChain {
        queue: VirtioBalloonQueueKind,
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
    },
    UsedRing {
        queue: VirtioBalloonQueueKind,
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        source: VirtqueueUsedRingError,
    },
    PfnDescriptorRead {
        queue: VirtioBalloonQueueKind,
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        source: VirtioBalloonPfnDescriptorPayloadReadError,
    },
    PfnPayloadParse {
        queue: VirtioBalloonQueueKind,
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        source: VirtioBalloonPfnPayloadParseError,
    },
    PfnRangeCompact {
        queue: VirtioBalloonQueueKind,
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        source: VirtioBalloonPfnRangeCompactError,
    },
    PfnRangeAccess {
        queue: VirtioBalloonQueueKind,
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        source: VirtioBalloonPfnRangeAccessError,
    },
    StatisticsDescriptorRead {
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        source: VirtioBalloonStatisticsDescriptorReadError,
    },
    InflatedRangeAllocation {
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        range_count: usize,
        source: TryReserveError,
    },
    DeflatedRangeAllocation {
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        range_count: usize,
        source: TryReserveError,
    },
    HintingRange {
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        descriptor_index: u16,
        address: GuestAddress,
        len: u32,
        source: VirtioBalloonHintingRangeError,
    },
    HintingRangeAllocation {
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        range_count: usize,
        source: TryReserveError,
    },
    HintingCommandRead {
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        descriptor_index: u16,
        address: GuestAddress,
        source: GuestMemoryAccessError,
    },
}

impl VirtioBalloonQueueDispatchError {
    pub const fn completed_dispatch(&self) -> &VirtioBalloonQueueDispatch {
        match self {
            Self::AvailableRing {
                completed_dispatch, ..
            }
            | Self::EmptyDescriptorChain {
                completed_dispatch, ..
            }
            | Self::UsedRing {
                completed_dispatch, ..
            }
            | Self::PfnDescriptorRead {
                completed_dispatch, ..
            }
            | Self::PfnPayloadParse {
                completed_dispatch, ..
            }
            | Self::PfnRangeCompact {
                completed_dispatch, ..
            }
            | Self::PfnRangeAccess {
                completed_dispatch, ..
            }
            | Self::StatisticsDescriptorRead {
                completed_dispatch, ..
            }
            | Self::InflatedRangeAllocation {
                completed_dispatch, ..
            }
            | Self::DeflatedRangeAllocation {
                completed_dispatch, ..
            }
            | Self::HintingRange {
                completed_dispatch, ..
            }
            | Self::HintingRangeAllocation {
                completed_dispatch, ..
            }
            | Self::HintingCommandRead {
                completed_dispatch, ..
            } => completed_dispatch,
        }
    }
}

impl fmt::Display for VirtioBalloonQueueDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AvailableRing { queue, source, .. } => {
                write!(
                    f,
                    "failed to pop virtio-balloon {queue} descriptor chain: {source}"
                )
            }
            Self::EmptyDescriptorChain { queue, .. } => {
                write!(f, "virtio-balloon {queue} descriptor chain cannot be empty")
            }
            Self::UsedRing {
                queue,
                descriptor_head,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to publish virtio-balloon {queue} descriptor {descriptor_head}: {source}"
                )
            }
            Self::PfnDescriptorRead {
                queue,
                descriptor_head,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to read virtio-balloon {queue} descriptor {descriptor_head}: {source}"
                )
            }
            Self::PfnPayloadParse {
                queue,
                descriptor_head,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to parse virtio-balloon {queue} descriptor {descriptor_head} PFNs: {source}"
                )
            }
            Self::PfnRangeCompact {
                queue,
                descriptor_head,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to compact virtio-balloon {queue} descriptor {descriptor_head} PFNs: {source}"
                )
            }
            Self::PfnRangeAccess {
                queue,
                descriptor_head,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to validate virtio-balloon {queue} descriptor {descriptor_head} PFN ranges: {source}"
                )
            }
            Self::StatisticsDescriptorRead {
                descriptor_head,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to read virtio-balloon statistics descriptor {descriptor_head}: {source}"
                )
            }
            Self::InflatedRangeAllocation {
                descriptor_head,
                range_count,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to reserve {range_count} inflated page range(s) for virtio-balloon inflate descriptor {descriptor_head}: {source}"
                )
            }
            Self::DeflatedRangeAllocation {
                descriptor_head,
                range_count,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to reserve {range_count} deflated page range(s) for virtio-balloon deflate descriptor {descriptor_head}: {source}"
                )
            }
            Self::HintingRange {
                descriptor_head,
                descriptor_index,
                address,
                len,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to validate virtio-balloon free-page-hinting descriptor {descriptor_head} range from descriptor {descriptor_index} at {address} with length {len}: {source}"
                )
            }
            Self::HintingRangeAllocation {
                descriptor_head,
                range_count,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to reserve {range_count} free-page-hinting range(s) for virtio-balloon descriptor {descriptor_head}: {source}"
                )
            }
            Self::HintingCommandRead {
                descriptor_head,
                descriptor_index,
                address,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to read virtio-balloon free-page-hinting descriptor {descriptor_head} command from descriptor {descriptor_index} at {address}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioBalloonQueueDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRing { source, .. } => Some(source),
            Self::UsedRing { source, .. } => Some(source),
            Self::PfnDescriptorRead { source, .. } => Some(source),
            Self::PfnPayloadParse { source, .. } => Some(source),
            Self::PfnRangeCompact { source, .. } => Some(source),
            Self::PfnRangeAccess { source, .. } => Some(source),
            Self::StatisticsDescriptorRead { source, .. } => Some(source),
            Self::InflatedRangeAllocation { source, .. } => Some(source),
            Self::DeflatedRangeAllocation { source, .. } => Some(source),
            Self::HintingRange { source, .. } => Some(source),
            Self::HintingRangeAllocation { source, .. } => Some(source),
            Self::HintingCommandRead { source, .. } => Some(source),
            Self::EmptyDescriptorChain { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioBalloonActiveQueues {
    inflate: VirtioBalloonQueue,
    deflate: VirtioBalloonQueue,
    statistics: Option<VirtioBalloonQueue>,
    free_page_hinting: Option<VirtioBalloonQueue>,
    free_page_reporting: Option<VirtioBalloonQueue>,
}

impl VirtioBalloonActiveQueues {
    fn from_activation(
        layout: VirtioBalloonQueueLayout,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<Self, VirtioBalloonDeviceActivationError> {
        Ok(Self {
            inflate: active_queue_from_activation(layout.inflate(), activation)?,
            deflate: active_queue_from_activation(layout.deflate(), activation)?,
            statistics: active_optional_queue_from_activation(layout.statistics(), activation)?,
            free_page_hinting: active_optional_queue_from_activation(
                layout.free_page_hinting(),
                activation,
            )?,
            free_page_reporting: active_optional_queue_from_activation(
                layout.free_page_reporting(),
                activation,
            )?,
        })
    }

    pub const fn inflate(&self) -> &VirtioBalloonQueue {
        &self.inflate
    }

    pub fn inflate_mut(&mut self) -> &mut VirtioBalloonQueue {
        &mut self.inflate
    }

    pub const fn deflate(&self) -> &VirtioBalloonQueue {
        &self.deflate
    }

    pub fn deflate_mut(&mut self) -> &mut VirtioBalloonQueue {
        &mut self.deflate
    }

    pub const fn statistics(&self) -> Option<&VirtioBalloonQueue> {
        self.statistics.as_ref()
    }

    pub fn statistics_mut(&mut self) -> Option<&mut VirtioBalloonQueue> {
        self.statistics.as_mut()
    }

    pub const fn free_page_hinting(&self) -> Option<&VirtioBalloonQueue> {
        self.free_page_hinting.as_ref()
    }

    pub fn free_page_hinting_mut(&mut self) -> Option<&mut VirtioBalloonQueue> {
        self.free_page_hinting.as_mut()
    }

    pub const fn free_page_reporting(&self) -> Option<&VirtioBalloonQueue> {
        self.free_page_reporting.as_ref()
    }

    pub fn free_page_reporting_mut(&mut self) -> Option<&mut VirtioBalloonQueue> {
        self.free_page_reporting.as_mut()
    }

    pub fn queue_count(&self) -> usize {
        [
            true,
            true,
            self.statistics.is_some(),
            self.free_page_hinting.is_some(),
            self.free_page_reporting.is_some(),
        ]
        .into_iter()
        .filter(|included| *included)
        .count()
    }
}

#[derive(Debug)]
pub struct VirtioBalloonDevice {
    queue_layout: VirtioBalloonQueueLayout,
    active_queues: Option<VirtioBalloonActiveQueues>,
    memory_accounting: VirtioBalloonMemoryAccounting,
    stats_polling_interval_s: u16,
    statistics: BalloonOptionalStats,
    statistics_pending_descriptor_head: Option<u16>,
    hinting_host_cmd: u32,
    hinting_guest_cmd: Option<u32>,
    hinting_last_cmd: u32,
    hinting_acknowledge_on_stop: bool,
}

impl VirtioBalloonDevice {
    pub const fn new(queue_layout: VirtioBalloonQueueLayout) -> Self {
        let stats_polling_interval_s = if queue_layout.statistics().is_some() {
            1
        } else {
            0
        };
        Self::with_stats_polling_interval_s(queue_layout, stats_polling_interval_s)
    }

    pub const fn with_stats_polling_interval_s(
        queue_layout: VirtioBalloonQueueLayout,
        stats_polling_interval_s: u16,
    ) -> Self {
        Self {
            queue_layout,
            active_queues: None,
            memory_accounting: VirtioBalloonMemoryAccounting::new(),
            stats_polling_interval_s,
            statistics: BalloonOptionalStats::new(),
            statistics_pending_descriptor_head: None,
            hinting_host_cmd: VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
            hinting_guest_cmd: None,
            hinting_last_cmd: VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
            hinting_acknowledge_on_stop: true,
        }
    }

    pub const fn queue_layout(&self) -> VirtioBalloonQueueLayout {
        self.queue_layout
    }

    pub fn is_activated(&self) -> bool {
        self.active_queues.is_some()
    }

    pub const fn active_queues(&self) -> Option<&VirtioBalloonActiveQueues> {
        self.active_queues.as_ref()
    }

    pub fn active_queues_mut(&mut self) -> Option<&mut VirtioBalloonActiveQueues> {
        self.active_queues.as_mut()
    }

    pub const fn memory_accounting(&self) -> &VirtioBalloonMemoryAccounting {
        &self.memory_accounting
    }

    pub const fn statistics(&self) -> BalloonOptionalStats {
        self.statistics
    }

    pub const fn stats_polling_interval_s(&self) -> u16 {
        self.stats_polling_interval_s
    }

    pub fn update_stats_polling_interval_s(
        &mut self,
        input: BalloonStatsUpdateInput,
    ) -> Result<(), BalloonUpdateError> {
        let stats_polling_interval_s = input.stats_polling_interval_s();
        if self.stats_polling_interval_s == stats_polling_interval_s {
            return Ok(());
        }
        if self.stats_polling_interval_s == 0 || stats_polling_interval_s == 0 {
            return Err(BalloonUpdateError::StatisticsStateChange);
        }

        self.stats_polling_interval_s = stats_polling_interval_s;
        Ok(())
    }

    pub fn hinting_status(&self) -> Result<BalloonHintingStatus, BalloonHintingStatusError> {
        if self.queue_layout.free_page_hinting().is_none() {
            return Err(BalloonHintingStatusError::HintingNotEnabled);
        }

        Ok(BalloonHintingStatus::new(
            self.hinting_host_cmd,
            self.hinting_guest_cmd,
        ))
    }

    pub fn hinting_acknowledge_on_stop(&self) -> Result<bool, BalloonHintingCommandError> {
        if self.queue_layout.free_page_hinting().is_none() {
            return Err(BalloonHintingCommandError::HintingNotEnabled);
        }

        Ok(self.hinting_acknowledge_on_stop)
    }

    pub fn start_hinting(
        &mut self,
        input: BalloonHintingStartInput,
    ) -> Result<u32, BalloonHintingCommandError> {
        if self.queue_layout.free_page_hinting().is_none() {
            return Err(BalloonHintingCommandError::HintingNotEnabled);
        }

        let mut cmd_id = self.hinting_last_cmd.wrapping_add(1);
        if cmd_id <= VIRTIO_BALLOON_FREE_PAGE_HINT_DONE {
            cmd_id = VIRTIO_BALLOON_FREE_PAGE_HINT_DONE + 1;
        }

        self.hinting_last_cmd = cmd_id;
        self.hinting_acknowledge_on_stop = input.acknowledge_on_stop();
        self.hinting_host_cmd = cmd_id;

        Ok(cmd_id)
    }

    pub fn stop_hinting(&mut self) -> Result<u32, BalloonHintingCommandError> {
        if self.queue_layout.free_page_hinting().is_none() {
            return Err(BalloonHintingCommandError::HintingNotEnabled);
        }

        self.hinting_host_cmd = VIRTIO_BALLOON_FREE_PAGE_HINT_DONE;

        Ok(VIRTIO_BALLOON_FREE_PAGE_HINT_DONE)
    }

    fn acknowledge_completed_hinting_run(&mut self) -> Option<u32> {
        if !self.hinting_acknowledge_on_stop {
            return None;
        }

        self.hinting_host_cmd = VIRTIO_BALLOON_FREE_PAGE_HINT_DONE;
        Some(VIRTIO_BALLOON_FREE_PAGE_HINT_DONE)
    }

    pub fn activate_balloon(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioBalloonDeviceActivationError> {
        if self.active_queues.is_some() {
            return Err(VirtioBalloonDeviceActivationError::AlreadyActive);
        }

        let expected = self.queue_layout.queue_count();
        let actual = activation.queue_count();
        if actual != expected {
            return Err(VirtioBalloonDeviceActivationError::QueueCountMismatch {
                expected,
                actual,
            });
        }

        self.active_queues = Some(VirtioBalloonActiveQueues::from_activation(
            self.queue_layout,
            activation,
        )?);

        Ok(())
    }

    fn dispatch_drained_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        drained_notifications: Vec<usize>,
    ) -> Result<VirtioBalloonDeviceNotificationDispatch, VirtioBalloonDeviceNotificationError> {
        if drained_notifications.is_empty() {
            return Ok(VirtioBalloonDeviceNotificationDispatch {
                drained_notifications,
                inflate_notifications: 0,
                deflate_notifications: 0,
                statistics_notifications: 0,
                hinting_notifications: 0,
                reporting_notifications: 0,
                inflate_queue_dispatch: None,
                deflate_queue_dispatch: None,
                statistics_queue_dispatch: None,
                hinting_queue_dispatch: None,
                reporting_queue_dispatch: None,
            });
        }

        let statistics_queue_index = self
            .queue_layout
            .statistics()
            .map(VirtioBalloonQueueConfig::index);
        let hinting_queue_index = self
            .queue_layout
            .free_page_hinting()
            .map(VirtioBalloonQueueConfig::index);
        let reporting_queue_index = self
            .queue_layout
            .free_page_reporting()
            .map(VirtioBalloonQueueConfig::index);

        if let Some(queue_index) = drained_notifications.iter().copied().find(|queue_index| {
            !is_supported_notification_queue(
                *queue_index,
                statistics_queue_index,
                hinting_queue_index,
                reporting_queue_index,
            )
        }) {
            return Err(VirtioBalloonDeviceNotificationError::UnsupportedQueue {
                drained_notifications,
                queue_index,
            });
        }

        let statistics_context = VirtioBalloonStatisticsDispatchContext::new(
            self.statistics_pending_descriptor_head,
            self.statistics,
        );
        let hinting_context =
            VirtioBalloonHintingDispatchContext::new(self.hinting_host_cmd, self.hinting_guest_cmd);
        let Some(active_queues) = self.active_queues.as_mut() else {
            return Err(VirtioBalloonDeviceNotificationError::Inactive {
                drained_notifications,
            });
        };

        let mut inflate_notifications = 0;
        let mut deflate_notifications = 0;
        let mut statistics_notifications = 0;
        let mut hinting_notifications = 0;
        let mut reporting_notifications = 0;
        for queue_index in &drained_notifications {
            match *queue_index {
                VIRTIO_BALLOON_INFLATE_QUEUE_INDEX => inflate_notifications += 1,
                VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX => deflate_notifications += 1,
                queue_index if Some(queue_index) == statistics_queue_index => {
                    statistics_notifications += 1;
                }
                queue_index if Some(queue_index) == hinting_queue_index => {
                    hinting_notifications += 1;
                }
                queue_index if Some(queue_index) == reporting_queue_index => {
                    reporting_notifications += 1;
                }
                _ => {}
            }
        }
        let mut dispatch = VirtioBalloonDeviceNotificationDispatch {
            drained_notifications,
            inflate_notifications,
            deflate_notifications,
            statistics_notifications,
            hinting_notifications,
            reporting_notifications,
            inflate_queue_dispatch: None,
            deflate_queue_dispatch: None,
            statistics_queue_dispatch: None,
            hinting_queue_dispatch: None,
            reporting_queue_dispatch: None,
        };

        if inflate_notifications > 0 {
            match active_queues.inflate_mut().dispatch_inflate(memory) {
                Ok(inflate_dispatch) => {
                    dispatch.inflate_queue_dispatch = Some(inflate_dispatch);
                    if let Err(source) = apply_completed_balloon_queue_accounting(
                        &mut self.memory_accounting,
                        VirtioBalloonQueueKind::Inflate,
                        &dispatch,
                    ) {
                        return Err(VirtioBalloonDeviceNotificationError::Accounting {
                            completed_dispatch: Box::new(dispatch),
                            source,
                        });
                    }
                }
                Err(source) => {
                    dispatch.inflate_queue_dispatch = Some(source.completed_dispatch().clone());
                    if let Err(accounting_source) = apply_completed_balloon_queue_accounting(
                        &mut self.memory_accounting,
                        VirtioBalloonQueueKind::Inflate,
                        &dispatch,
                    ) {
                        return Err(VirtioBalloonDeviceNotificationError::Accounting {
                            completed_dispatch: Box::new(dispatch),
                            source: accounting_source,
                        });
                    }
                    return Err(VirtioBalloonDeviceNotificationError::QueueDispatch {
                        completed_dispatch: Box::new(dispatch),
                        source,
                    });
                }
            }
        }

        if deflate_notifications > 0 {
            match active_queues.deflate_mut().dispatch_deflate(memory) {
                Ok(deflate_dispatch) => {
                    dispatch.deflate_queue_dispatch = Some(deflate_dispatch);
                    if let Err(source) = apply_completed_balloon_queue_accounting(
                        &mut self.memory_accounting,
                        VirtioBalloonQueueKind::Deflate,
                        &dispatch,
                    ) {
                        return Err(VirtioBalloonDeviceNotificationError::Accounting {
                            completed_dispatch: Box::new(dispatch),
                            source,
                        });
                    }
                }
                Err(source) => {
                    dispatch.deflate_queue_dispatch = Some(source.completed_dispatch().clone());
                    if let Err(accounting_source) = apply_completed_balloon_queue_accounting(
                        &mut self.memory_accounting,
                        VirtioBalloonQueueKind::Deflate,
                        &dispatch,
                    ) {
                        return Err(VirtioBalloonDeviceNotificationError::Accounting {
                            completed_dispatch: Box::new(dispatch),
                            source: accounting_source,
                        });
                    }
                    return Err(VirtioBalloonDeviceNotificationError::QueueDispatch {
                        completed_dispatch: Box::new(dispatch),
                        source,
                    });
                }
            }
        }

        if statistics_notifications > 0 {
            let Some(statistics_queue) = active_queues.statistics_mut() else {
                return Err(VirtioBalloonDeviceNotificationError::UnsupportedQueue {
                    drained_notifications: dispatch.drained_notifications().to_vec(),
                    queue_index: statistics_queue_index.unwrap_or(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
                });
            };
            match statistics_queue.dispatch_statistics(memory, statistics_context) {
                Ok(statistics_dispatch) => {
                    self.statistics = statistics_dispatch.statistics();
                    self.statistics_pending_descriptor_head =
                        statistics_dispatch.statistics_pending_descriptor_head();
                    dispatch.statistics_queue_dispatch = Some(statistics_dispatch);
                }
                Err(source) => {
                    let completed = source.completed_dispatch().clone();
                    self.statistics = completed.statistics();
                    self.statistics_pending_descriptor_head =
                        completed.statistics_pending_descriptor_head();
                    dispatch.statistics_queue_dispatch = Some(completed);
                    return Err(VirtioBalloonDeviceNotificationError::QueueDispatch {
                        completed_dispatch: Box::new(dispatch),
                        source,
                    });
                }
            }
        }

        if hinting_notifications > 0 {
            let Some(hinting_queue) = active_queues.free_page_hinting_mut() else {
                return Err(VirtioBalloonDeviceNotificationError::UnsupportedQueue {
                    drained_notifications: dispatch.drained_notifications().to_vec(),
                    queue_index: hinting_queue_index.unwrap_or(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
                });
            };
            match hinting_queue.dispatch_hinting_commands(memory, hinting_context) {
                Ok(hinting_dispatch) => {
                    dispatch.hinting_queue_dispatch = Some(hinting_dispatch);
                    apply_completed_balloon_hinting_guest_cmd(
                        &mut self.hinting_guest_cmd,
                        &dispatch,
                    );
                }
                Err(source) => {
                    dispatch.hinting_queue_dispatch = Some(source.completed_dispatch().clone());
                    apply_completed_balloon_hinting_guest_cmd(
                        &mut self.hinting_guest_cmd,
                        &dispatch,
                    );
                    return Err(VirtioBalloonDeviceNotificationError::QueueDispatch {
                        completed_dispatch: Box::new(dispatch),
                        source,
                    });
                }
            }
        }

        if reporting_notifications > 0 {
            let Some(reporting_queue) = active_queues.free_page_reporting_mut() else {
                return Err(VirtioBalloonDeviceNotificationError::UnsupportedQueue {
                    drained_notifications: dispatch.drained_notifications().to_vec(),
                    queue_index: reporting_queue_index.unwrap_or(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
                });
            };
            match reporting_queue.dispatch_free_page_reporting(memory) {
                Ok(reporting_dispatch) => {
                    dispatch.reporting_queue_dispatch = Some(reporting_dispatch);
                }
                Err(source) => {
                    dispatch.reporting_queue_dispatch = Some(source.completed_dispatch().clone());
                    return Err(VirtioBalloonDeviceNotificationError::QueueDispatch {
                        completed_dispatch: Box::new(dispatch),
                        source,
                    });
                }
            }
        }

        Ok(dispatch)
    }

    pub fn trigger_statistics_update(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioBalloonDeviceNotificationDispatch, VirtioBalloonDeviceNotificationError> {
        let mut dispatch = VirtioBalloonDeviceNotificationDispatch {
            drained_notifications: Vec::new(),
            inflate_notifications: 0,
            deflate_notifications: 0,
            statistics_notifications: 0,
            hinting_notifications: 0,
            reporting_notifications: 0,
            inflate_queue_dispatch: None,
            deflate_queue_dispatch: None,
            statistics_queue_dispatch: None,
            hinting_queue_dispatch: None,
            reporting_queue_dispatch: None,
        };
        let statistics_context = VirtioBalloonStatisticsDispatchContext::new(
            self.statistics_pending_descriptor_head,
            self.statistics,
        );
        let Some(active_queues) = self.active_queues.as_mut() else {
            return Err(VirtioBalloonDeviceNotificationError::Inactive {
                drained_notifications: Vec::new(),
            });
        };
        let Some(statistics_queue) = active_queues.statistics_mut() else {
            return Ok(dispatch);
        };

        match statistics_queue.complete_pending_statistics(memory, statistics_context) {
            Ok(statistics_dispatch) => {
                self.statistics = statistics_dispatch.statistics();
                self.statistics_pending_descriptor_head =
                    statistics_dispatch.statistics_pending_descriptor_head();
                dispatch.statistics_queue_dispatch = Some(statistics_dispatch);
                Ok(dispatch)
            }
            Err(source) => {
                let completed = source.completed_dispatch().clone();
                self.statistics = completed.statistics();
                self.statistics_pending_descriptor_head =
                    completed.statistics_pending_descriptor_head();
                dispatch.statistics_queue_dispatch = Some(completed);
                Err(VirtioBalloonDeviceNotificationError::QueueDispatch {
                    completed_dispatch: Box::new(dispatch),
                    source,
                })
            }
        }
    }

    pub fn reset(&mut self) {
        self.active_queues = None;
        self.memory_accounting = VirtioBalloonMemoryAccounting::new();
        self.statistics = BalloonOptionalStats::default();
        self.statistics_pending_descriptor_head = None;
        self.hinting_host_cmd = VIRTIO_BALLOON_FREE_PAGE_HINT_STOP;
        self.hinting_guest_cmd = None;
        self.hinting_last_cmd = VIRTIO_BALLOON_FREE_PAGE_HINT_STOP;
        self.hinting_acknowledge_on_stop = true;
    }
}

impl VirtioMmioRegisterHandler<VirtioBalloonConfigSpace, VirtioBalloonDevice> {
    pub fn start_balloon_hinting(
        &mut self,
        input: BalloonHintingStartInput,
    ) -> Result<(), BalloonHintingCommandError> {
        let cmd_id = self.activation_handler_mut().start_hinting(input)?;
        self.update_balloon_hinting_host_cmd(cmd_id);

        Ok(())
    }

    pub fn stop_balloon_hinting(&mut self) -> Result<(), BalloonHintingCommandError> {
        let cmd_id = self.activation_handler_mut().stop_hinting()?;
        self.update_balloon_hinting_host_cmd(cmd_id);

        Ok(())
    }

    pub fn balloon_hinting_status(
        &self,
    ) -> Result<BalloonHintingStatus, BalloonHintingStatusError> {
        self.activation_handler().hinting_status()
    }

    pub fn dispatch_balloon_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioBalloonDeviceNotificationDispatch, VirtioBalloonDeviceNotificationError> {
        let drained_notifications = self.take_pending_queue_notifications();
        let dispatch = self
            .activation_handler_mut()
            .dispatch_drained_queue_notifications(memory, drained_notifications);
        let queue_layout = self.activation_handler().queue_layout();
        let queue_interrupts = balloon_queue_interrupts(&dispatch, queue_layout);
        let hinting_completed_run = match &dispatch {
            Ok(dispatch) => dispatch.hinting_completed_run(),
            Err(error) => error
                .completed_notification_dispatch()
                .is_some_and(VirtioBalloonDeviceNotificationDispatch::hinting_completed_run),
        };
        for queue_index in queue_interrupts {
            self.mark_queue_interrupt_pending(queue_index);
        }
        if hinting_completed_run
            && let Some(cmd_id) = self
                .activation_handler_mut()
                .acknowledge_completed_hinting_run()
        {
            self.update_balloon_hinting_host_cmd(cmd_id);
        }

        dispatch
    }

    pub fn trigger_balloon_statistics_update(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioBalloonDeviceNotificationDispatch, VirtioBalloonDeviceNotificationError> {
        let dispatch = self
            .activation_handler_mut()
            .trigger_statistics_update(memory);
        let needs_queue_interrupt = match &dispatch {
            Ok(dispatch) => dispatch.needs_queue_interrupt(),
            Err(error) => error
                .completed_notification_dispatch()
                .is_some_and(VirtioBalloonDeviceNotificationDispatch::needs_queue_interrupt),
        };
        if needs_queue_interrupt
            && let Some(queue_index) = self
                .activation_handler()
                .queue_layout()
                .statistics()
                .and_then(|queue| u16::try_from(queue.index()).ok())
        {
            self.mark_queue_interrupt_pending(queue_index);
        }

        dispatch
    }

    pub fn update_balloon_config(
        &mut self,
        config: BalloonConfig,
    ) -> Result<(), BalloonUpdateError> {
        let num_pages =
            mib_to_4k_pages(config.amount_mib()).map_err(BalloonUpdateError::PageCountOverflow)?;

        let config_space = self.device_config_handler().with_num_pages(num_pages);
        *self.device_config_handler_mut() = config_space;
        self.increment_config_generation();
        self.mark_config_interrupt_pending();

        Ok(())
    }

    pub fn update_balloon_statistics(
        &mut self,
        input: BalloonStatsUpdateInput,
    ) -> Result<(), BalloonUpdateError> {
        self.activation_handler_mut()
            .update_stats_polling_interval_s(input)
    }

    fn update_balloon_hinting_host_cmd(&mut self, cmd_id: u32) {
        let config_space = self
            .device_config_handler()
            .with_free_page_hint_cmd_id(cmd_id);
        *self.device_config_handler_mut() = config_space;
        self.increment_config_generation();
        self.mark_config_interrupt_pending();
    }
}

impl VirtioPciEndpoint<VirtioBalloonConfigSpace, VirtioBalloonDevice> {
    pub fn dispatch_balloon_queue_notifications(
        &self,
        memory: &mut GuestMemory,
    ) -> Result<
        VirtioBalloonDeviceNotificationDispatch,
        VirtioPciDeviceOperationError<
            VirtioBalloonDeviceNotificationError,
            VirtioBalloonDeviceNotificationDispatch,
        >,
    > {
        let work = self
            .admit_device_work()
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let dispatch = work
            .with_core_mut(|core| {
                let drained_notifications =
                    core.queue_notifications.take_pending_queue_notifications();
                let dispatch = core
                    .activation
                    .dispatch_drained_queue_notifications(memory, drained_notifications);
                let queue_layout = core.activation.queue_layout();
                for queue_index in balloon_queue_interrupts(&dispatch, queue_layout) {
                    core.record_interrupt_intent(VirtioInterruptIntent::Queue { queue_index });
                }
                let hinting_completed_run = match &dispatch {
                    Ok(dispatch) => dispatch.hinting_completed_run(),
                    Err(error) => error.completed_notification_dispatch().is_some_and(
                        VirtioBalloonDeviceNotificationDispatch::hinting_completed_run,
                    ),
                };
                if hinting_completed_run
                    && let Some(cmd_id) = core.activation.acknowledge_completed_hinting_run()
                {
                    core.device_config = core.device_config.with_free_page_hint_cmd_id(cmd_id);
                    core.device.increment_config_generation();
                    core.record_interrupt_intent(VirtioInterruptIntent::Configuration);
                }
                dispatch
            })
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let endpoint = work.drain_interrupt_intents();
        VirtioPciDeviceOperationError::combine(dispatch, endpoint)
    }

    pub fn trigger_balloon_statistics_update(
        &self,
        memory: &mut GuestMemory,
    ) -> Result<
        VirtioBalloonDeviceNotificationDispatch,
        VirtioPciDeviceOperationError<
            VirtioBalloonDeviceNotificationError,
            VirtioBalloonDeviceNotificationDispatch,
        >,
    > {
        let work = self
            .admit_device_work()
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let dispatch = work
            .with_core_mut(|core| {
                let dispatch = core.activation.trigger_statistics_update(memory);
                let needs_queue_interrupt = match &dispatch {
                    Ok(dispatch) => dispatch.needs_queue_interrupt(),
                    Err(error) => error.completed_notification_dispatch().is_some_and(
                        VirtioBalloonDeviceNotificationDispatch::needs_queue_interrupt,
                    ),
                };
                if needs_queue_interrupt
                    && let Some(queue_index) = core
                        .activation
                        .queue_layout()
                        .statistics()
                        .and_then(|queue| u16::try_from(queue.index()).ok())
                {
                    core.record_interrupt_intent(VirtioInterruptIntent::Queue { queue_index });
                }
                dispatch
            })
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let endpoint = work.drain_interrupt_intents();
        VirtioPciDeviceOperationError::combine(dispatch, endpoint)
    }

    pub fn update_balloon_config(
        &self,
        config: BalloonConfig,
    ) -> Result<(), VirtioPciDeviceOperationError<BalloonUpdateError, ()>> {
        let num_pages = mib_to_4k_pages(config.amount_mib()).map_err(|source| {
            VirtioPciDeviceOperationError::Device(Box::new(BalloonUpdateError::PageCountOverflow(
                source,
            )))
        })?;
        let work = self
            .admit_device_work()
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        work.with_core_mut(|core| {
            core.device_config = core.device_config.with_num_pages(num_pages);
            core.device.increment_config_generation();
            core.record_interrupt_intent(VirtioInterruptIntent::Configuration);
        })
        .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        VirtioPciDeviceOperationError::combine(Ok(()), work.drain_interrupt_intents())
    }

    pub fn update_balloon_statistics(
        &self,
        input: BalloonStatsUpdateInput,
    ) -> Result<(), VirtioPciDeviceOperationError<BalloonUpdateError, ()>> {
        let work = self
            .admit_device_work()
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let update = work
            .with_core_mut(|core| core.activation.update_stats_polling_interval_s(input))
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        VirtioPciDeviceOperationError::combine(update, Ok(()))
    }

    pub fn start_balloon_hinting(
        &self,
        input: BalloonHintingStartInput,
    ) -> Result<(), VirtioPciDeviceOperationError<BalloonHintingCommandError, ()>> {
        self.update_balloon_hinting_command(|device| device.start_hinting(input))
    }

    pub fn stop_balloon_hinting(
        &self,
    ) -> Result<(), VirtioPciDeviceOperationError<BalloonHintingCommandError, ()>> {
        self.update_balloon_hinting_command(VirtioBalloonDevice::stop_hinting)
    }

    fn update_balloon_hinting_command(
        &self,
        command: impl FnOnce(&mut VirtioBalloonDevice) -> Result<u32, BalloonHintingCommandError>,
    ) -> Result<(), VirtioPciDeviceOperationError<BalloonHintingCommandError, ()>> {
        let work = self
            .admit_device_work()
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let result = work
            .with_core_mut(|core| {
                let cmd_id = command(&mut core.activation)?;
                core.device_config = core.device_config.with_free_page_hint_cmd_id(cmd_id);
                core.device.increment_config_generation();
                core.record_interrupt_intent(VirtioInterruptIntent::Configuration);
                Ok(())
            })
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        VirtioPciDeviceOperationError::combine(result, work.drain_interrupt_intents())
    }

    pub fn balloon_stats(
        &self,
        config: BalloonConfig,
    ) -> Result<BalloonStats, VirtioPciDeviceOperationError<BalloonStatsError, BalloonStats>> {
        let work = self
            .admit_device_work()
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let result = work
            .with_core_mut(|core| {
                BalloonStats::from_config_actual_pages_and_optional_stats(
                    config,
                    core.activation.memory_accounting().inflated_page_count(),
                    core.activation.statistics(),
                )
            })
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        VirtioPciDeviceOperationError::combine(result, Ok(()))
    }

    pub fn balloon_hinting_status(
        &self,
    ) -> Result<
        BalloonHintingStatus,
        VirtioPciDeviceOperationError<BalloonHintingStatusError, BalloonHintingStatus>,
    > {
        let work = self
            .admit_device_work()
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let result = work
            .with_core_mut(|core| core.activation.hinting_status())
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        VirtioPciDeviceOperationError::combine(result, Ok(()))
    }
}

fn balloon_queue_interrupts(
    dispatch: &Result<
        VirtioBalloonDeviceNotificationDispatch,
        VirtioBalloonDeviceNotificationError,
    >,
    layout: VirtioBalloonQueueLayout,
) -> Vec<u16> {
    let completed = match dispatch {
        Ok(dispatch) => Some(dispatch),
        Err(error) => error.completed_notification_dispatch(),
    };
    let Some(completed) = completed else {
        return Vec::new();
    };
    let candidates = [
        (Some(layout.inflate()), completed.inflate_queue_dispatch()),
        (Some(layout.deflate()), completed.deflate_queue_dispatch()),
        (layout.statistics(), completed.statistics_queue_dispatch()),
        (
            layout.free_page_hinting(),
            completed.hinting_queue_dispatch(),
        ),
        (
            layout.free_page_reporting(),
            completed.reporting_queue_dispatch(),
        ),
    ];
    candidates
        .into_iter()
        .filter_map(|(queue, dispatch)| {
            dispatch
                .is_some_and(VirtioBalloonQueueDispatch::needs_queue_interrupt)
                .then_some(queue)
                .flatten()
        })
        .filter_map(|queue| u16::try_from(queue.index()).ok())
        .collect()
}

impl VirtioMmioDeviceActivationHandler for VirtioBalloonDevice {
    fn activate(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioMmioDeviceActivationError> {
        self.activate_balloon(activation).map_err(Into::into)
    }

    fn reset(&mut self) {
        VirtioBalloonDevice::reset(self);
    }
}

#[derive(Debug)]
pub enum VirtioBalloonDeviceActivationError {
    AlreadyActive,
    QueueCountMismatch {
        expected: usize,
        actual: usize,
    },
    QueueIndexTooLarge {
        queue_index: usize,
    },
    QueueMetadata {
        queue_index: u32,
        source: VirtioMmioQueueRegisterError,
    },
    QueueBuild {
        queue_index: u32,
        kind: VirtioBalloonQueueKind,
        source: VirtioBalloonQueueBuildError,
    },
}

impl fmt::Display for VirtioBalloonDeviceActivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyActive => f.write_str("virtio-balloon device is already active"),
            Self::QueueCountMismatch { expected, actual } => {
                write!(
                    f,
                    "virtio-balloon device requires {expected} queue(s), got {actual}"
                )
            }
            Self::QueueIndexTooLarge { queue_index } => {
                write!(
                    f,
                    "virtio-balloon queue index {queue_index} does not fit a virtio-mmio queue selector"
                )
            }
            Self::QueueMetadata {
                queue_index,
                source,
            } => {
                write!(
                    f,
                    "failed to read virtio-balloon queue {queue_index} activation metadata: {source}"
                )
            }
            Self::QueueBuild {
                queue_index,
                kind,
                source,
            } => {
                write!(
                    f,
                    "failed to activate virtio-balloon {kind} queue {queue_index}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioBalloonDeviceActivationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueMetadata { source, .. } => Some(source),
            Self::QueueBuild { source, .. } => Some(source),
            Self::AlreadyActive
            | Self::QueueCountMismatch { .. }
            | Self::QueueIndexTooLarge { .. } => None,
        }
    }
}

impl From<VirtioBalloonDeviceActivationError> for VirtioMmioDeviceActivationError {
    fn from(source: VirtioBalloonDeviceActivationError) -> Self {
        MmioHandlerError::new(source.to_string()).into()
    }
}

#[derive(Debug)]
pub struct VirtioBalloonDeviceNotificationDispatch {
    drained_notifications: Vec<usize>,
    inflate_notifications: usize,
    deflate_notifications: usize,
    statistics_notifications: usize,
    hinting_notifications: usize,
    reporting_notifications: usize,
    inflate_queue_dispatch: Option<VirtioBalloonQueueDispatch>,
    deflate_queue_dispatch: Option<VirtioBalloonQueueDispatch>,
    statistics_queue_dispatch: Option<VirtioBalloonQueueDispatch>,
    hinting_queue_dispatch: Option<VirtioBalloonQueueDispatch>,
    reporting_queue_dispatch: Option<VirtioBalloonQueueDispatch>,
}

impl VirtioBalloonDeviceNotificationDispatch {
    pub fn drained_notifications(&self) -> &[usize] {
        &self.drained_notifications
    }

    pub const fn inflate_notifications(&self) -> usize {
        self.inflate_notifications
    }

    pub const fn deflate_notifications(&self) -> usize {
        self.deflate_notifications
    }

    pub const fn statistics_notifications(&self) -> usize {
        self.statistics_notifications
    }

    pub const fn hinting_notifications(&self) -> usize {
        self.hinting_notifications
    }

    pub const fn reporting_notifications(&self) -> usize {
        self.reporting_notifications
    }

    pub const fn inflate_queue_dispatch(&self) -> Option<&VirtioBalloonQueueDispatch> {
        self.inflate_queue_dispatch.as_ref()
    }

    pub const fn deflate_queue_dispatch(&self) -> Option<&VirtioBalloonQueueDispatch> {
        self.deflate_queue_dispatch.as_ref()
    }

    pub const fn statistics_queue_dispatch(&self) -> Option<&VirtioBalloonQueueDispatch> {
        self.statistics_queue_dispatch.as_ref()
    }

    pub const fn hinting_queue_dispatch(&self) -> Option<&VirtioBalloonQueueDispatch> {
        self.hinting_queue_dispatch.as_ref()
    }

    pub const fn reporting_queue_dispatch(&self) -> Option<&VirtioBalloonQueueDispatch> {
        self.reporting_queue_dispatch.as_ref()
    }

    pub fn hinting_completed_run(&self) -> bool {
        self.hinting_queue_dispatch
            .as_ref()
            .is_some_and(VirtioBalloonQueueDispatch::hinting_completed_run)
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.inflate_queue_dispatch
            .as_ref()
            .is_some_and(VirtioBalloonQueueDispatch::needs_queue_interrupt)
            || self
                .deflate_queue_dispatch
                .as_ref()
                .is_some_and(VirtioBalloonQueueDispatch::needs_queue_interrupt)
            || self
                .statistics_queue_dispatch
                .as_ref()
                .is_some_and(VirtioBalloonQueueDispatch::needs_queue_interrupt)
            || self
                .hinting_queue_dispatch
                .as_ref()
                .is_some_and(VirtioBalloonQueueDispatch::needs_queue_interrupt)
            || self
                .reporting_queue_dispatch
                .as_ref()
                .is_some_and(VirtioBalloonQueueDispatch::needs_queue_interrupt)
    }
}

#[derive(Debug)]
pub enum VirtioBalloonDeviceNotificationError {
    Inactive {
        drained_notifications: Vec<usize>,
    },
    UnsupportedQueue {
        drained_notifications: Vec<usize>,
        queue_index: usize,
    },
    QueueDispatch {
        completed_dispatch: Box<VirtioBalloonDeviceNotificationDispatch>,
        source: VirtioBalloonQueueDispatchError,
    },
    Accounting {
        completed_dispatch: Box<VirtioBalloonDeviceNotificationDispatch>,
        source: VirtioBalloonAccountingError,
    },
}

impl VirtioBalloonDeviceNotificationError {
    pub fn drained_notifications(&self) -> &[usize] {
        match self {
            Self::Inactive {
                drained_notifications,
            }
            | Self::UnsupportedQueue {
                drained_notifications,
                ..
            } => drained_notifications,
            Self::QueueDispatch {
                completed_dispatch, ..
            }
            | Self::Accounting {
                completed_dispatch, ..
            } => completed_dispatch.drained_notifications(),
        }
    }

    pub const fn completed_dispatch(&self) -> Option<&VirtioBalloonQueueDispatch> {
        match self {
            Self::QueueDispatch { source, .. } => Some(source.completed_dispatch()),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } | Self::Accounting { .. } => None,
        }
    }

    pub const fn completed_notification_dispatch(
        &self,
    ) -> Option<&VirtioBalloonDeviceNotificationDispatch> {
        match self {
            Self::QueueDispatch {
                completed_dispatch, ..
            }
            | Self::Accounting {
                completed_dispatch, ..
            } => Some(completed_dispatch),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }
}

impl fmt::Display for VirtioBalloonDeviceNotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inactive { .. } => f.write_str(
                "virtio-balloon queue notification cannot be dispatched before activation",
            ),
            Self::UnsupportedQueue { queue_index, .. } => {
                write!(
                    f,
                    "virtio-balloon queue notification for unsupported queue {queue_index}"
                )
            }
            Self::QueueDispatch { source, .. } => {
                write!(
                    f,
                    "failed to dispatch virtio-balloon queue notification: {source}"
                )
            }
            Self::Accounting { source, .. } => {
                write!(
                    f,
                    "failed to update virtio-balloon memory accounting: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioBalloonDeviceNotificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueDispatch { source, .. } => Some(source),
            Self::Accounting { source, .. } => Some(source),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum VirtioBalloonAccountingError {
    RangeUpdate {
        queue: VirtioBalloonQueueKind,
        source: TryReserveError,
    },
}

impl fmt::Display for VirtioBalloonAccountingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RangeUpdate { queue, source } => {
                write!(
                    f,
                    "failed to update virtio-balloon {queue} page ranges: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioBalloonAccountingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RangeUpdate { source, .. } => Some(source),
        }
    }
}

fn apply_balloon_queue_accounting(
    accounting: &mut VirtioBalloonMemoryAccounting,
    queue: VirtioBalloonQueueKind,
    dispatch: &VirtioBalloonQueueDispatch,
) -> Result<(), VirtioBalloonAccountingError> {
    match queue {
        VirtioBalloonQueueKind::Inflate => accounting
            .add_inflated_ranges(dispatch.inflated_page_ranges())
            .map_err(|source| VirtioBalloonAccountingError::RangeUpdate { queue, source }),
        VirtioBalloonQueueKind::Deflate => accounting
            .remove_inflated_ranges(dispatch.deflated_page_ranges())
            .map_err(|source| VirtioBalloonAccountingError::RangeUpdate { queue, source }),
        VirtioBalloonQueueKind::Statistics
        | VirtioBalloonQueueKind::FreePageHinting
        | VirtioBalloonQueueKind::FreePageReporting => Ok(()),
    }
}

fn apply_completed_balloon_queue_accounting(
    accounting: &mut VirtioBalloonMemoryAccounting,
    queue: VirtioBalloonQueueKind,
    dispatch: &VirtioBalloonDeviceNotificationDispatch,
) -> Result<(), VirtioBalloonAccountingError> {
    let Some(queue_dispatch) = (match queue {
        VirtioBalloonQueueKind::Inflate => dispatch.inflate_queue_dispatch(),
        VirtioBalloonQueueKind::Deflate => dispatch.deflate_queue_dispatch(),
        VirtioBalloonQueueKind::Statistics
        | VirtioBalloonQueueKind::FreePageHinting
        | VirtioBalloonQueueKind::FreePageReporting => None,
    }) else {
        return Ok(());
    };

    apply_balloon_queue_accounting(accounting, queue, queue_dispatch)
}

fn apply_completed_balloon_hinting_guest_cmd(
    hinting_guest_cmd: &mut Option<u32>,
    dispatch: &VirtioBalloonDeviceNotificationDispatch,
) {
    if let Some(guest_cmd) = dispatch
        .hinting_queue_dispatch()
        .and_then(VirtioBalloonQueueDispatch::hinting_guest_cmd)
    {
        *hinting_guest_cmd = Some(guest_cmd);
    }
}

fn active_optional_queue_from_activation(
    config: Option<VirtioBalloonQueueConfig>,
    activation: VirtioMmioDeviceActivation<'_>,
) -> Result<Option<VirtioBalloonQueue>, VirtioBalloonDeviceActivationError> {
    config
        .map(|config| active_queue_from_activation(config, activation))
        .transpose()
}

fn active_queue_from_activation(
    config: VirtioBalloonQueueConfig,
    activation: VirtioMmioDeviceActivation<'_>,
) -> Result<VirtioBalloonQueue, VirtioBalloonDeviceActivationError> {
    let queue_index = u32::try_from(config.index()).map_err(|_| {
        VirtioBalloonDeviceActivationError::QueueIndexTooLarge {
            queue_index: config.index(),
        }
    })?;
    let queue = activation.queue(queue_index).map_err(|source| {
        VirtioBalloonDeviceActivationError::QueueMetadata {
            queue_index,
            source,
        }
    })?;
    VirtioBalloonQueue::from_mmio_queue_state(queue).map_err(|source| {
        VirtioBalloonDeviceActivationError::QueueBuild {
            queue_index,
            kind: config.kind(),
            source,
        }
    })
}

fn descriptor_chain_head(chain: &VirtqueueDescriptorChain) -> Option<u16> {
    chain
        .descriptors()
        .first()
        .copied()
        .map(VirtqueueDescriptor::index)
}

const fn is_inflate_or_deflate_queue(queue_index: usize) -> bool {
    queue_index == VIRTIO_BALLOON_INFLATE_QUEUE_INDEX
        || queue_index == VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX
}

fn is_supported_notification_queue(
    queue_index: usize,
    statistics_queue_index: Option<usize>,
    hinting_queue_index: Option<usize>,
    reporting_queue_index: Option<usize>,
) -> bool {
    is_inflate_or_deflate_queue(queue_index)
        || Some(queue_index) == statistics_queue_index
        || Some(queue_index) == hinting_queue_index
        || Some(queue_index) == reporting_queue_index
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedBalloonDevice {
    config_space: VirtioBalloonConfigSpace,
    available_features: u64,
    queue_layout: VirtioBalloonQueueLayout,
    stats_polling_interval_s: u16,
}

impl PreparedBalloonDevice {
    pub fn from_config(config: BalloonConfig) -> Result<Self, BalloonPageCountOverflow> {
        Ok(Self {
            config_space: VirtioBalloonConfigSpace::from_config(config)?,
            available_features: available_features(config),
            queue_layout: VirtioBalloonQueueLayout::from_config(config),
            stats_polling_interval_s: config.stats_polling_interval_s(),
        })
    }

    pub const fn config_space(self) -> VirtioBalloonConfigSpace {
        self.config_space
    }

    pub const fn available_features(self) -> u64 {
        self.available_features
    }

    pub const fn queue_layout(self) -> VirtioBalloonQueueLayout {
        self.queue_layout
    }

    pub const fn stats_polling_interval_s(self) -> u16 {
        self.stats_polling_interval_s
    }

    pub fn queue_sizes(self) -> VirtioBalloonQueueSizes {
        self.queue_layout.queue_sizes()
    }

    #[doc(hidden)]
    pub fn into_parts(
        self,
    ) -> (
        VirtioBalloonConfigSpace,
        u64,
        VirtioBalloonQueueSizes,
        VirtioBalloonDevice,
    ) {
        (
            self.config_space,
            self.available_features,
            self.queue_sizes(),
            VirtioBalloonDevice::with_stats_polling_interval_s(
                self.queue_layout,
                self.stats_polling_interval_s,
            ),
        )
    }

    pub fn register_mmio(
        self,
        layout: BalloonMmioLayout,
    ) -> Result<BalloonMmioDevice, BalloonMmioRegistrationError> {
        BalloonMmioDevice::from_prepared(self, layout)
    }

    pub fn register_mmio_with_dispatcher(
        self,
        layout: BalloonMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<BalloonMmioDevice, BalloonMmioRegistrationError> {
        BalloonMmioDevice::from_prepared_with_dispatcher(self, layout, dispatcher)
    }
}

impl TryFrom<BalloonConfig> for PreparedBalloonDevice {
    type Error = BalloonPageCountOverflow;

    fn try_from(config: BalloonConfig) -> Result<Self, Self::Error> {
        Self::from_config(config)
    }
}

pub const fn available_features(config: BalloonConfig) -> u64 {
    let mut features = virtio_feature_bit(VIRTIO_FEATURE_VERSION_1);
    if config.deflate_on_oom() {
        features |= virtio_feature_bit(VIRTIO_BALLOON_F_DEFLATE_ON_OOM);
    }
    if config.stats_polling_interval_s() > 0 {
        features |= virtio_feature_bit(VIRTIO_BALLOON_F_STATS_VQ);
    }
    if config.free_page_hinting() {
        features |= virtio_feature_bit(VIRTIO_BALLOON_F_FREE_PAGE_HINTING);
    }
    if config.free_page_reporting() {
        features |= virtio_feature_bit(VIRTIO_BALLOON_F_FREE_PAGE_REPORTING);
    }

    features
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonMmioLayout {
    address: GuestAddress,
    region_id: MmioRegionId,
}

impl BalloonMmioLayout {
    pub const fn new(address: GuestAddress, region_id: MmioRegionId) -> Self {
        Self { address, region_id }
    }

    pub const fn address(self) -> GuestAddress {
        self.address
    }

    pub const fn region_id(self) -> MmioRegionId {
        self.region_id
    }

    fn region(self) -> Result<MmioRegion, BalloonMmioRegistrationError> {
        MmioRegion::new(self.region_id, self.address, VIRTIO_MMIO_DEVICE_WINDOW_SIZE).map_err(
            |source| BalloonMmioRegistrationError::InvalidRegion {
                region_id: self.region_id,
                address: self.address,
                source,
            },
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonMmioDeviceRegistration {
    region: MmioRegion,
}

impl BalloonMmioDeviceRegistration {
    pub const fn region(self) -> MmioRegion {
        self.region
    }

    pub const fn region_id(self) -> MmioRegionId {
        self.region.id()
    }

    pub const fn address(self) -> GuestAddress {
        self.region.range().start()
    }
}

#[derive(Debug)]
pub struct BalloonMmioDevice {
    dispatcher: MmioDispatcher,
    registration: BalloonMmioDeviceRegistration,
}

impl BalloonMmioDevice {
    pub fn from_prepared(
        prepared: PreparedBalloonDevice,
        layout: BalloonMmioLayout,
    ) -> Result<Self, BalloonMmioRegistrationError> {
        Self::from_prepared_with_dispatcher(prepared, layout, MmioDispatcher::new())
    }

    pub fn from_prepared_with_dispatcher(
        prepared: PreparedBalloonDevice,
        layout: BalloonMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<Self, BalloonMmioRegistrationError> {
        let region = layout.region()?;
        let queue_sizes = prepared.queue_sizes();
        let handler = VirtioBalloonMmioHandler::with_device_config_and_activation(
            VIRTIO_BALLOON_DEVICE_ID,
            prepared.available_features(),
            queue_sizes.as_slice(),
            prepared.config_space(),
            VirtioBalloonDevice::with_stats_polling_interval_s(
                prepared.queue_layout(),
                prepared.stats_polling_interval_s(),
            ),
        )
        .map_err(|source| BalloonMmioRegistrationError::BuildHandler {
            region_id: layout.region_id(),
            source,
        })?;
        let mut dispatcher = dispatcher;
        let inserted_region = dispatcher
            .insert_region(
                layout.region_id(),
                layout.address(),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .map_err(|source| BalloonMmioRegistrationError::InsertRegion {
                region_id: layout.region_id(),
                address: layout.address(),
                source,
            })?;
        dispatcher
            .register_handler(layout.region_id(), handler)
            .map_err(|source| BalloonMmioRegistrationError::RegisterHandler {
                region_id: layout.region_id(),
                source,
            })?;
        debug_assert_eq!(inserted_region, region);

        Ok(Self {
            dispatcher,
            registration: BalloonMmioDeviceRegistration { region },
        })
    }

    pub fn dispatcher(&self) -> &MmioDispatcher {
        &self.dispatcher
    }

    pub fn dispatcher_mut(&mut self) -> &mut MmioDispatcher {
        &mut self.dispatcher
    }

    pub const fn registration(&self) -> BalloonMmioDeviceRegistration {
        self.registration
    }

    pub fn into_parts(self) -> (MmioDispatcher, BalloonMmioDeviceRegistration) {
        (self.dispatcher, self.registration)
    }
}

#[derive(Debug)]
pub enum BalloonMmioRegistrationError {
    InvalidRegion {
        region_id: MmioRegionId,
        address: GuestAddress,
        source: GuestMemoryError,
    },
    BuildHandler {
        region_id: MmioRegionId,
        source: VirtioMmioRegisterHandlerError,
    },
    InsertRegion {
        region_id: MmioRegionId,
        address: GuestAddress,
        source: MmioBusError,
    },
    RegisterHandler {
        region_id: MmioRegionId,
        source: MmioDispatchError,
    },
}

impl fmt::Display for BalloonMmioRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRegion {
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "invalid balloon MMIO region id={region_id} at {address}: {source}"
                )
            }
            Self::BuildHandler { region_id, source } => {
                write!(
                    f,
                    "failed to build balloon MMIO handler for region id={region_id}: {source}"
                )
            }
            Self::InsertRegion {
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "failed to insert balloon MMIO region id={region_id} at {address}: {source}"
                )
            }
            Self::RegisterHandler { region_id, source } => {
                write!(
                    f,
                    "failed to register balloon MMIO handler for region id={region_id}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for BalloonMmioRegistrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRegion { source, .. } => Some(source),
            Self::BuildHandler { source, .. } => Some(source),
            Self::InsertRegion { source, .. } => Some(source),
            Self::RegisterHandler { source, .. } => Some(source),
        }
    }
}

fn balloon_config_access_bytes(
    bytes: &[u8; VIRTIO_BALLOON_CONFIG_SPACE_SIZE],
    access: VirtioMmioDeviceConfigAccess,
) -> Result<&[u8], VirtioMmioDeviceConfigError> {
    let offset = usize::try_from(access.offset()).map_err(|_| {
        VirtioMmioDeviceConfigError::UnsupportedRead {
            offset: access.offset(),
            len: access.len(),
        }
    })?;
    let Some(end) = offset.checked_add(access.len()) else {
        return Err(VirtioMmioDeviceConfigError::UnsupportedRead {
            offset: access.offset(),
            len: access.len(),
        });
    };

    bytes
        .get(offset..end)
        .ok_or(VirtioMmioDeviceConfigError::UnsupportedRead {
            offset: access.offset(),
            len: access.len(),
        })
}

fn balloon_config_access_bytes_mut(
    bytes: &mut [u8; VIRTIO_BALLOON_CONFIG_SPACE_SIZE],
    access: VirtioMmioDeviceConfigAccess,
) -> Result<&mut [u8], VirtioMmioDeviceConfigError> {
    let offset = usize::try_from(access.offset()).map_err(|_| {
        VirtioMmioDeviceConfigError::UnsupportedWrite {
            offset: access.offset(),
            len: access.len(),
        }
    })?;
    let Some(end) = offset.checked_add(access.len()) else {
        return Err(VirtioMmioDeviceConfigError::UnsupportedWrite {
            offset: access.offset(),
            len: access.len(),
        });
    };

    bytes
        .get_mut(offset..end)
        .ok_or(VirtioMmioDeviceConfigError::UnsupportedWrite {
            offset: access.offset(),
            len: access.len(),
        })
}

fn balloon_config_bytes_error(source: MmioAccessBytesError) -> VirtioMmioDeviceConfigError {
    VirtioMmioDeviceConfigError::Handler {
        source: MmioHandlerError::new(format!(
            "virtio-balloon config access bytes failed: {source}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::ffi::c_void;
    use std::io;
    use std::ptr::NonNull;

    use crate::interrupt::DeviceInterruptKind;
    use crate::memory::{
        GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryDiscardFailureKind,
        GuestMemoryError, GuestMemoryLayout, GuestMemoryRange,
    };
    use crate::metrics::{
        BalloonDeviceMetrics, BalloonDiscardMetrics, BalloonFreePageReportMetrics,
        SharedBalloonDeviceMetrics,
    };
    use crate::mmio::{MmioAccessBytes, MmioDispatchOutcome, MmioOperation, MmioRegionId};
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
        VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, VirtioMmioDeviceActivation, VirtioMmioDeviceRegisters,
        VirtioMmioQueueRegisters, VirtioMmioRegister, VirtioMmioRegisterHandlerError,
    };
    use crate::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE,
        VirtqueueAvailableRing, VirtqueueUsedRing, read_descriptor_chain,
    };

    const TEST_BALLOON_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_8000);
    const TEST_BALLOON_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(4000);
    const TEST_QUEUE_SIZE: u16 = 8;
    const TEST_MEMORY_SIZE: u64 = 0x80000;
    const TEST_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x2000);
    const TEST_PFN_DATA: GuestAddress = GuestAddress::new(0x4000);
    const TEST_PFN_DATA_SPLIT: GuestAddress = GuestAddress::new(0x5000);
    const TEST_DESCRIPTOR_BASE: u64 = 0x1000;
    const TEST_DRIVER_BASE: u64 = 0x8000;
    const TEST_DEVICE_BASE: u64 = 0x10000;
    const TEST_QUEUE_STRIDE: u64 = 0x1000;
    const QUEUE_CONFIG_STATUS: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    const DRIVER_OK_STATUS: u32 = QUEUE_CONFIG_STATUS | VIRTIO_DEVICE_STATUS_DRIVER_OK;

    #[derive(Debug)]
    struct TestBalloonDiscardAdviser {
        page_size: u64,
        zero_calls: usize,
        free_calls: usize,
        zero_failures_remaining: usize,
        free_failures_remaining: usize,
    }

    impl TestBalloonDiscardAdviser {
        const fn new(page_size: u64) -> Self {
            Self {
                page_size,
                zero_calls: 0,
                free_calls: 0,
                zero_failures_remaining: 0,
                free_failures_remaining: 0,
            }
        }

        const fn with_zero_failure(mut self) -> Self {
            self.zero_failures_remaining = 1;
            self
        }

        const fn with_free_failure(mut self) -> Self {
            self.free_failures_remaining = 1;
            self
        }
    }

    impl GuestMemoryDiscardAdviser for TestBalloonDiscardAdviser {
        fn host_page_size(&mut self) -> Result<u64, GuestMemoryDiscardFailureKind> {
            Ok(self.page_size)
        }

        fn zero(&mut self, _address: NonNull<c_void>, _size: usize) -> io::Result<()> {
            self.zero_calls += 1;
            if self.zero_failures_remaining > 0 {
                self.zero_failures_remaining -= 1;
                Err(io::Error::other("injected balloon zero failure"))
            } else {
                Ok(())
            }
        }

        fn free(&mut self, _address: NonNull<c_void>, _size: usize) -> io::Result<()> {
            self.free_calls += 1;
            if self.free_failures_remaining > 0 {
                self.free_failures_remaining -= 1;
                Err(io::Error::other("injected balloon free failure"))
            } else {
                Ok(())
            }
        }
    }

    fn balloon_config(
        amount_mib: u32,
        deflate_on_oom: bool,
        stats_polling_interval_s: u16,
        free_page_hinting: bool,
        free_page_reporting: bool,
    ) -> BalloonConfig {
        BalloonConfigInput::new(amount_mib, deflate_on_oom)
            .with_stats_polling_interval_s(stats_polling_interval_s)
            .with_free_page_hinting(free_page_hinting)
            .with_free_page_reporting(free_page_reporting)
            .into()
    }

    fn prepared(config: BalloonConfig) -> PreparedBalloonDevice {
        PreparedBalloonDevice::from_config(config).expect("balloon config should prepare")
    }

    #[test]
    fn config_input_accepts_free_page_reporting() {
        assert_eq!(
            BalloonConfigInput::new(64, true)
                .with_free_page_reporting(true)
                .validate()
                .expect("free-page reporting should be accepted"),
            balloon_config(64, true, 0, false, true)
        );
    }

    #[test]
    fn config_input_accepts_missing_or_false_free_page_reporting() {
        assert_eq!(
            BalloonConfigInput::new(64, true)
                .validate()
                .expect("omitted free-page reporting should be accepted"),
            balloon_config(64, true, 0, false, false)
        );
        assert_eq!(
            BalloonConfigInput::new(64, true)
                .with_free_page_reporting(false)
                .validate()
                .expect("false free-page reporting should be accepted"),
            balloon_config(64, true, 0, false, false)
        );
    }

    fn pfn_payload_bytes(pfns: &[u32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for pfn in pfns {
            bytes.extend_from_slice(&pfn.to_le_bytes());
        }
        bytes
    }

    fn stat_payload_bytes(stats: &[(u16, u64)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (tag, value) in stats {
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    #[derive(Debug, Clone, Copy)]
    struct TestDescriptor {
        address: GuestAddress,
        len: u32,
        flags: u16,
        next: u16,
    }

    impl TestDescriptor {
        const fn readable(address: GuestAddress, len: u32, next: Option<u16>) -> Self {
            let (flags, next_index) = match next {
                Some(index) => (VIRTQUEUE_DESC_F_NEXT, index),
                None => (0, 0),
            };
            Self {
                address,
                len,
                flags,
                next: next_index,
            }
        }

        const fn writable(address: GuestAddress, len: u32, next: Option<u16>) -> Self {
            let (flags, next_index) = match next {
                Some(index) => (VIRTQUEUE_DESC_F_WRITE | VIRTQUEUE_DESC_F_NEXT, index),
                None => (VIRTQUEUE_DESC_F_WRITE, 0),
            };
            Self {
                address,
                len,
                flags,
                next: next_index,
            }
        }
    }

    fn pfn_descriptor_memory() -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), TEST_MEMORY_SIZE)
                .expect("test memory range should be valid"),
        ])
        .expect("test memory layout should be valid");
        GuestMemory::allocate(&layout).expect("test guest memory should allocate")
    }

    fn write_guest_bytes(memory: &mut GuestMemory, address: GuestAddress, bytes: &[u8]) {
        memory
            .write_slice(bytes, address)
            .expect("guest bytes should write");
    }

    fn descriptor_table_for_queue(queue_index: usize) -> GuestAddress {
        queue_address(TEST_DESCRIPTOR_BASE, queue_index_u32(queue_index))
    }

    fn write_descriptor_at(
        memory: &mut GuestMemory,
        descriptor_table: GuestAddress,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        let mut bytes = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
        let (address_bytes, tail) = bytes.split_at_mut(8);
        let (len_bytes, tail) = tail.split_at_mut(4);
        let (flags_bytes, next_bytes) = tail.split_at_mut(2);
        address_bytes.copy_from_slice(&descriptor.address.raw_value().to_le_bytes());
        len_bytes.copy_from_slice(&descriptor.len.to_le_bytes());
        flags_bytes.copy_from_slice(&descriptor.flags.to_le_bytes());
        next_bytes.copy_from_slice(&descriptor.next.to_le_bytes());

        let descriptor_address = descriptor_table
            .checked_add(
                u64::from(index)
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE).expect("descriptor size should fit"),
            )
            .expect("descriptor address should not overflow");
        memory
            .write_slice(&bytes, descriptor_address)
            .expect("descriptor should write");
    }

    fn write_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        write_descriptor_at(memory, TEST_DESCRIPTOR_TABLE, index, descriptor);
    }

    fn write_inflate_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        write_descriptor_at(
            memory,
            descriptor_table_for_queue(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX),
            index,
            descriptor,
        );
    }

    fn write_queue_descriptor(
        memory: &mut GuestMemory,
        queue_index: usize,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        write_descriptor_at(
            memory,
            descriptor_table_for_queue(queue_index),
            index,
            descriptor,
        );
    }

    fn write_statistics_descriptor(
        memory: &mut GuestMemory,
        queue_index: usize,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        write_queue_descriptor(memory, queue_index, index, descriptor);
    }

    fn write_hinting_descriptor(
        memory: &mut GuestMemory,
        queue_index: usize,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        write_queue_descriptor(memory, queue_index, index, descriptor);
    }

    fn write_reporting_descriptor(
        memory: &mut GuestMemory,
        queue_index: usize,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        write_queue_descriptor(memory, queue_index, index, descriptor);
    }

    fn descriptor_chain(memory: &GuestMemory, head_index: u16) -> VirtqueueDescriptorChain {
        read_descriptor_chain(memory, TEST_DESCRIPTOR_TABLE, TEST_QUEUE_SIZE, head_index)
            .expect("descriptor chain should read")
    }

    fn descriptor_len(bytes: &[u8]) -> u32 {
        u32::try_from(bytes.len()).expect("test descriptor length should fit u32")
    }

    fn first_unmapped_test_pfn() -> u32 {
        u32::try_from(TEST_MEMORY_SIZE / VIRTIO_BALLOON_PAGE_SIZE)
            .expect("test memory size should fit a PFN")
    }

    fn has_feature(features: u64, feature: u32) -> bool {
        features & virtio_feature_bit(feature) != 0
    }

    fn read_mmio_config(device: &mut BalloonMmioDevice, offset: u64, len: u64) -> MmioAccessBytes {
        let address = TEST_BALLOON_MMIO_BASE
            .checked_add(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + offset)
            .expect("test MMIO address should not overflow");
        let access = device
            .dispatcher()
            .lookup(address, len)
            .expect("balloon config access should resolve");
        let outcome = device
            .dispatcher_mut()
            .dispatch(MmioOperation::read(access).expect("read operation should build"))
            .expect("balloon config read should dispatch");

        match outcome {
            MmioDispatchOutcome::Read { data } => data,
            MmioDispatchOutcome::Write => panic!("read should return read data"),
        }
    }

    fn write_mmio_config(device: &mut BalloonMmioDevice, offset: u64, data: &[u8]) {
        let address = TEST_BALLOON_MMIO_BASE
            .checked_add(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + offset)
            .expect("test MMIO address should not overflow");
        let access = device
            .dispatcher()
            .lookup(
                address,
                u64::try_from(data.len()).expect("test data length should fit u64"),
            )
            .expect("balloon config access should resolve");
        let data = MmioAccessBytes::new(data).expect("test config bytes should build");
        device
            .dispatcher_mut()
            .dispatch(MmioOperation::write(access, data).expect("write operation should build"))
            .expect("balloon config write should dispatch");
    }

    fn queue_index_u32(queue_index: usize) -> u32 {
        u32::try_from(queue_index).expect("test queue index should fit u32")
    }

    fn queue_address(base: u64, queue_index: u32) -> GuestAddress {
        GuestAddress::new(
            base.checked_add(u64::from(queue_index) * TEST_QUEUE_STRIDE)
                .expect("test queue address should not overflow"),
        )
    }

    fn address_low(address: GuestAddress) -> u32 {
        u32::try_from(address.raw_value() & u64::from(u32::MAX))
            .expect("low address word should fit u32")
    }

    fn address_high(address: GuestAddress) -> u32 {
        u32::try_from(address.raw_value() >> u32::BITS).expect("high address word should fit u32")
    }

    fn deflate_queue() -> VirtioBalloonQueue {
        let available = VirtqueueAvailableRing::new(
            TEST_DESCRIPTOR_TABLE,
            deflate_available_ring(),
            TEST_QUEUE_SIZE,
        )
        .expect("deflate available ring should build");
        let used = VirtqueueUsedRing::new(deflate_used_ring(), TEST_QUEUE_SIZE)
            .expect("deflate used ring should build");
        VirtioBalloonQueue::new(available, used)
    }

    fn deflate_queue_with_used_ring(used_ring: GuestAddress) -> VirtioBalloonQueue {
        let available = VirtqueueAvailableRing::new(
            TEST_DESCRIPTOR_TABLE,
            deflate_available_ring(),
            TEST_QUEUE_SIZE,
        )
        .expect("deflate available ring should build");
        let used =
            VirtqueueUsedRing::new(used_ring, TEST_QUEUE_SIZE).expect("used ring should build");
        VirtioBalloonQueue::new(available, used)
    }

    fn inflate_queue() -> VirtioBalloonQueue {
        inflate_queue_with_used_ring(inflate_used_ring())
    }

    fn inflate_queue_with_used_ring(used_ring: GuestAddress) -> VirtioBalloonQueue {
        let available = VirtqueueAvailableRing::new(
            descriptor_table_for_queue(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX),
            inflate_available_ring(),
            TEST_QUEUE_SIZE,
        )
        .expect("inflate available ring should build");
        let used =
            VirtqueueUsedRing::new(used_ring, TEST_QUEUE_SIZE).expect("used ring should build");
        VirtioBalloonQueue::new(available, used)
    }

    fn hinting_queue(queue_index: usize) -> VirtioBalloonQueue {
        hinting_queue_with_used_ring(queue_index, queue_used_ring(queue_index))
    }

    fn reporting_queue(queue_index: usize) -> VirtioBalloonQueue {
        reporting_queue_with_used_ring(queue_index, queue_used_ring(queue_index))
    }

    fn statistics_queue(queue_index: usize) -> VirtioBalloonQueue {
        statistics_queue_with_used_ring(queue_index, queue_used_ring(queue_index))
    }

    fn statistics_queue_with_used_ring(
        queue_index: usize,
        used_ring: GuestAddress,
    ) -> VirtioBalloonQueue {
        VirtioBalloonQueue::new(
            VirtqueueAvailableRing::new(
                descriptor_table_for_queue(queue_index),
                queue_available_ring(queue_index),
                TEST_QUEUE_SIZE,
            )
            .expect("statistics available ring should build"),
            VirtqueueUsedRing::new(used_ring, TEST_QUEUE_SIZE)
                .expect("statistics used ring should build"),
        )
    }

    fn hinting_queue_with_used_ring(
        queue_index: usize,
        used_ring: GuestAddress,
    ) -> VirtioBalloonQueue {
        let available = VirtqueueAvailableRing::new(
            descriptor_table_for_queue(queue_index),
            queue_available_ring(queue_index),
            TEST_QUEUE_SIZE,
        )
        .expect("hinting available ring should build");
        let used = VirtqueueUsedRing::new(used_ring, TEST_QUEUE_SIZE)
            .expect("hinting used ring should build");
        VirtioBalloonQueue::new(available, used)
    }

    fn reporting_queue_with_used_ring(
        queue_index: usize,
        used_ring: GuestAddress,
    ) -> VirtioBalloonQueue {
        let available = VirtqueueAvailableRing::new(
            descriptor_table_for_queue(queue_index),
            queue_available_ring(queue_index),
            TEST_QUEUE_SIZE,
        )
        .expect("reporting available ring should build");
        let used = VirtqueueUsedRing::new(used_ring, TEST_QUEUE_SIZE)
            .expect("reporting used ring should build");
        VirtioBalloonQueue::new(available, used)
    }

    const fn hinting_context(
        host_cmd: u32,
        guest_cmd: Option<u32>,
    ) -> VirtioBalloonHintingDispatchContext {
        VirtioBalloonHintingDispatchContext::new(host_cmd, guest_cmd)
    }

    const fn default_hinting_context() -> VirtioBalloonHintingDispatchContext {
        hinting_context(VIRTIO_BALLOON_FREE_PAGE_HINT_STOP, None)
    }

    const fn statistics_context(
        pending_descriptor_head: Option<u16>,
        statistics: BalloonOptionalStats,
    ) -> VirtioBalloonStatisticsDispatchContext {
        VirtioBalloonStatisticsDispatchContext::new(pending_descriptor_head, statistics)
    }

    fn queue_available_ring(queue_index: usize) -> GuestAddress {
        queue_address(TEST_DRIVER_BASE, queue_index_u32(queue_index))
    }

    fn queue_used_ring(queue_index: usize) -> GuestAddress {
        queue_address(TEST_DEVICE_BASE, queue_index_u32(queue_index))
    }

    fn inflate_available_ring() -> GuestAddress {
        queue_address(
            TEST_DRIVER_BASE,
            queue_index_u32(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX),
        )
    }

    fn inflate_used_ring() -> GuestAddress {
        queue_address(
            TEST_DEVICE_BASE,
            queue_index_u32(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX),
        )
    }

    fn deflate_available_ring() -> GuestAddress {
        queue_address(
            TEST_DRIVER_BASE,
            queue_index_u32(VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX),
        )
    }

    fn deflate_used_ring() -> GuestAddress {
        queue_address(
            TEST_DEVICE_BASE,
            queue_index_u32(VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX),
        )
    }

    fn available_ring_idx_address(available_ring: GuestAddress) -> GuestAddress {
        available_ring
            .checked_add(2)
            .expect("available index address should not overflow")
    }

    fn available_ring_entry_address(available_ring: GuestAddress, index: usize) -> GuestAddress {
        available_ring
            .checked_add(4 + u64::try_from(index).expect("available index should fit u64") * 2)
            .expect("available ring entry address should not overflow")
    }

    fn used_ring_idx_address(used_ring: GuestAddress) -> GuestAddress {
        used_ring
            .checked_add(2)
            .expect("used index address should not overflow")
    }

    fn used_ring_entry_address(used_ring: GuestAddress, index: usize) -> GuestAddress {
        used_ring
            .checked_add(4 + u64::try_from(index).expect("used index should fit u64") * 8)
            .expect("used ring entry address should not overflow")
    }

    fn write_available_heads(
        memory: &mut GuestMemory,
        available_ring: GuestAddress,
        heads: &[u16],
    ) {
        for (index, head) in heads.iter().copied().enumerate() {
            write_u16(
                memory,
                available_ring_entry_address(available_ring, index),
                head,
            );
        }
        write_u16(
            memory,
            available_ring_idx_address(available_ring),
            u16::try_from(heads.len()).expect("available head count should fit u16"),
        );
    }

    fn read_used_idx(memory: &GuestMemory, used_ring: GuestAddress) -> u16 {
        read_u16(memory, used_ring_idx_address(used_ring))
    }

    fn read_used_element(
        memory: &GuestMemory,
        used_ring: GuestAddress,
        index: usize,
    ) -> (u32, u32) {
        let address = used_ring_entry_address(used_ring, index);
        (
            read_u32(memory, address),
            read_u32(
                memory,
                address
                    .checked_add(4)
                    .expect("used ring length address should not overflow"),
            ),
        )
    }

    fn write_u16(memory: &mut GuestMemory, address: GuestAddress, value: u16) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("u16 should write");
    }

    fn read_u16(memory: &GuestMemory, address: GuestAddress) -> u16 {
        let mut bytes = [0; 2];
        memory
            .read_slice(&mut bytes, address)
            .expect("u16 should read");
        u16::from_le_bytes(bytes)
    }

    fn read_u32(memory: &GuestMemory, address: GuestAddress) -> u32 {
        let mut bytes = [0; 4];
        memory
            .read_slice(&mut bytes, address)
            .expect("u32 should read");
        u32::from_le_bytes(bytes)
    }

    fn configure_queue_registers(queues: &mut VirtioMmioQueueRegisters, queue_index: u32) {
        queues
            .write_register(
                VirtioMmioRegister::QueueSel,
                queue_index,
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue select should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueNum,
                u32::from(TEST_QUEUE_SIZE),
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue size should write");

        let descriptor_table = queue_address(TEST_DESCRIPTOR_BASE, queue_index);
        let driver_ring = queue_address(TEST_DRIVER_BASE, queue_index);
        let device_ring = queue_address(TEST_DEVICE_BASE, queue_index);
        for (register, value) in [
            (
                VirtioMmioRegister::QueueDescLow,
                address_low(descriptor_table),
            ),
            (
                VirtioMmioRegister::QueueDescHigh,
                address_high(descriptor_table),
            ),
            (VirtioMmioRegister::QueueDriverLow, address_low(driver_ring)),
            (
                VirtioMmioRegister::QueueDriverHigh,
                address_high(driver_ring),
            ),
            (VirtioMmioRegister::QueueDeviceLow, address_low(device_ring)),
            (
                VirtioMmioRegister::QueueDeviceHigh,
                address_high(device_ring),
            ),
        ] {
            queues
                .write_register(register, value, QUEUE_CONFIG_STATUS)
                .expect("queue address should write");
        }
        queues
            .write_register(VirtioMmioRegister::QueueReady, 1, QUEUE_CONFIG_STATUS)
            .expect("queue ready should write");
    }

    fn configured_queue_registers(queue_count: usize) -> VirtioMmioQueueRegisters {
        let queue_sizes = vec![VIRTIO_BALLOON_QUEUE_SIZE; queue_count];
        let mut queues =
            VirtioMmioQueueRegisters::new(&queue_sizes).expect("queue table should build");
        for queue_index in 0..queue_count {
            configure_queue_registers(&mut queues, queue_index_u32(queue_index));
        }

        queues
    }

    fn activation_for_queues<'a>(
        device_registers: &'a VirtioMmioDeviceRegisters,
        queues: &'a VirtioMmioQueueRegisters,
    ) -> VirtioMmioDeviceActivation<'a> {
        VirtioMmioDeviceActivation::new(device_registers, queues)
    }

    fn configure_handler_queue(handler: &mut VirtioBalloonMmioHandler, queue_index: u32) {
        configure_handler_queue_with_device_ring(
            handler,
            queue_index,
            queue_address(TEST_DEVICE_BASE, queue_index),
        );
    }

    fn configure_handler_queue_with_device_ring(
        handler: &mut VirtioBalloonMmioHandler,
        queue_index: u32,
        device_ring: GuestAddress,
    ) {
        handler
            .write_register(VirtioMmioRegister::QueueSel, queue_index)
            .expect("queue select should write");
        handler
            .write_register(VirtioMmioRegister::QueueNum, u32::from(TEST_QUEUE_SIZE))
            .expect("queue size should write");

        let descriptor_table = queue_address(TEST_DESCRIPTOR_BASE, queue_index);
        let driver_ring = queue_address(TEST_DRIVER_BASE, queue_index);
        for (register, value) in [
            (
                VirtioMmioRegister::QueueDescLow,
                address_low(descriptor_table),
            ),
            (
                VirtioMmioRegister::QueueDescHigh,
                address_high(descriptor_table),
            ),
            (VirtioMmioRegister::QueueDriverLow, address_low(driver_ring)),
            (
                VirtioMmioRegister::QueueDriverHigh,
                address_high(driver_ring),
            ),
            (VirtioMmioRegister::QueueDeviceLow, address_low(device_ring)),
            (
                VirtioMmioRegister::QueueDeviceHigh,
                address_high(device_ring),
            ),
        ] {
            handler
                .write_register(register, value)
                .expect("queue address should write");
        }
        handler
            .write_register(VirtioMmioRegister::QueueReady, 1)
            .expect("queue ready should write");
    }

    fn set_handler_queue_config_status(handler: &mut VirtioBalloonMmioHandler) {
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("acknowledge status should write");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("driver status should write");
        handler
            .write_register(VirtioMmioRegister::Status, QUEUE_CONFIG_STATUS)
            .expect("features-ok status should write");
    }

    fn activate_handler(handler: &mut VirtioBalloonMmioHandler) {
        set_handler_queue_config_status(handler);
        for queue_index in 0..handler.queue_registers().queue_count() {
            configure_handler_queue(handler, queue_index_u32(queue_index));
        }
        handler
            .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
            .expect("driver-ok status should write");
    }

    fn balloon_mmio_device(config: BalloonConfig) -> BalloonMmioDevice {
        prepared(config)
            .register_mmio(BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                TEST_BALLOON_MMIO_REGION_ID,
            ))
            .expect("balloon MMIO device should register")
    }

    #[test]
    fn balloon_config_update_changes_only_target_amount() {
        let config = balloon_config(64, true, 60, true, false);

        let updated = config
            .updated(BalloonUpdateInput::new(128))
            .expect("balloon target update should validate");

        assert_eq!(updated.amount_mib(), 128);
        assert_eq!(updated.deflate_on_oom(), config.deflate_on_oom());
        assert_eq!(
            updated.stats_polling_interval_s(),
            config.stats_polling_interval_s()
        );
        assert_eq!(updated.free_page_hinting(), config.free_page_hinting());
        assert_eq!(updated.free_page_reporting(), config.free_page_reporting());
    }

    #[test]
    fn balloon_config_update_rejects_page_count_overflow() {
        let config = balloon_config(64, false, 0, false, false);

        let err = config
            .updated(BalloonUpdateInput::new(u32::MAX))
            .expect_err("oversized target should fail");

        assert!(matches!(err, BalloonUpdateError::PageCountOverflow(_)));
    }

    #[test]
    fn balloon_config_stats_update_changes_only_nonzero_interval() {
        let config = balloon_config(64, true, 60, true, false);

        let updated = config
            .updated_stats(BalloonStatsUpdateInput::new(30))
            .expect("balloon stats interval update should validate");

        assert_eq!(updated.amount_mib(), config.amount_mib());
        assert_eq!(updated.deflate_on_oom(), config.deflate_on_oom());
        assert_eq!(updated.stats_polling_interval_s(), 30);
        assert_eq!(updated.free_page_hinting(), config.free_page_hinting());
        assert_eq!(updated.free_page_reporting(), config.free_page_reporting());
    }

    #[test]
    fn balloon_config_stats_update_accepts_same_interval() {
        let config = balloon_config(64, true, 0, true, false);

        let updated = config
            .updated_stats(BalloonStatsUpdateInput::new(0))
            .expect("same stats interval should be a no-op");

        assert_eq!(updated, config);
    }

    #[test]
    fn balloon_config_stats_update_rejects_enabled_state_change() {
        for (current, updated) in [(0, 1), (1, 0)] {
            let config = balloon_config(64, false, current, false, false);

            let err = config
                .updated_stats(BalloonStatsUpdateInput::new(updated))
                .expect_err("stats enabled-state change should fail");

            assert_eq!(err, BalloonUpdateError::StatisticsStateChange);
        }
    }

    #[test]
    fn balloon_stats_use_target_config_and_actual_accounting() {
        let stats = BalloonStats::from_config_and_actual_pages(
            balloon_config(64, false, 0, false, false),
            513,
        )
        .expect("balloon stats should convert");

        assert_eq!(stats.target_pages(), 64 * VIRTIO_BALLOON_MIB_TO_4K_PAGES);
        assert_eq!(stats.actual_pages(), 513);
        assert_eq!(stats.target_mib(), 64);
        assert_eq!(stats.actual_mib(), 2);
    }

    #[test]
    fn balloon_stats_reject_actual_page_count_overflow() {
        let err = BalloonStats::from_config_and_actual_pages(
            balloon_config(64, false, 0, false, false),
            u64::from(u32::MAX) + 1,
        )
        .expect_err("oversized actual page count should fail");

        assert_eq!(
            err,
            BalloonStatsError::ActualPageCountTooLarge {
                actual_pages: u64::from(u32::MAX) + 1,
            }
        );
    }

    #[test]
    fn balloon_stats_include_optional_guest_reported_fields() {
        let mut optional = BalloonOptionalStats::default();
        assert!(optional.is_empty());
        assert!(optional.record_stat(VirtioBalloonStat::new(VIRTIO_BALLOON_S_SWAP_OUT, 9)));
        assert!(optional.record_stat(VirtioBalloonStat::new(VIRTIO_BALLOON_S_MEMFREE, 0x5678)));
        assert!(!optional.record_stat(VirtioBalloonStat::new(0xffff, 1)));

        let stats = BalloonStats::from_config_actual_pages_and_optional_stats(
            balloon_config(64, false, 0, false, false),
            513,
            optional,
        )
        .expect("balloon stats should convert with optional stats");

        assert_eq!(stats.optional().swap_out(), Some(9));
        assert_eq!(stats.optional().free_memory(), Some(0x5678));
        assert_eq!(stats.optional().swap_in(), None);
    }

    #[test]
    fn statistics_descriptor_reads_recognized_and_unknown_tags() {
        let mut memory = pfn_descriptor_memory();
        let bytes = stat_payload_bytes(&[
            (VIRTIO_BALLOON_S_SWAP_OUT, 9),
            (0xffff, 10),
            (VIRTIO_BALLOON_S_MEMFREE, 0x5678),
        ]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_statistics_descriptor(
            &mut memory,
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        let chain = read_descriptor_chain(
            &memory,
            descriptor_table_for_queue(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            TEST_QUEUE_SIZE,
            0,
        )
        .expect("statistics descriptor chain should read");

        let descriptor = VirtioBalloonStatisticsDescriptor::read(&memory, &chain)
            .expect("statistics descriptor should read");
        let payload = descriptor.payload();
        let stats = payload
            .report_stats()
            .expect("valid statistics descriptor should produce a report");

        assert!(!payload.is_oversized());
        assert_eq!(payload.stat_count(), 3);
        assert_eq!(payload.recognized_stat_count(), 2);
        assert_eq!(stats.swap_out(), Some(9));
        assert_eq!(stats.free_memory(), Some(0x5678));
        assert_eq!(stats.swap_in(), None);
    }

    #[test]
    fn statistics_descriptor_accepts_exact_maximum_payload() {
        let mut memory = pfn_descriptor_memory();
        let stats =
            vec![(VIRTIO_BALLOON_S_MEMFREE, 0x5678); VIRTIO_BALLOON_MAX_STATS_PER_DESCRIPTOR];
        let bytes = stat_payload_bytes(&stats);
        assert_eq!(bytes.len(), VIRTIO_BALLOON_MAX_STATS_PAYLOAD_SIZE);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_statistics_descriptor(
            &mut memory,
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        let chain = read_descriptor_chain(
            &memory,
            descriptor_table_for_queue(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            TEST_QUEUE_SIZE,
            0,
        )
        .expect("statistics descriptor chain should read");

        let payload = VirtioBalloonStatisticsDescriptor::read(&memory, &chain)
            .expect("maximum statistics descriptor should read")
            .payload();
        let parsed_stats = payload
            .report_stats()
            .expect("maximum statistics descriptor should produce a report");

        assert!(!payload.is_oversized());
        assert_eq!(
            payload.stat_count(),
            VIRTIO_BALLOON_MAX_STATS_PER_DESCRIPTOR
        );
        assert_eq!(
            payload.recognized_stat_count(),
            VIRTIO_BALLOON_MAX_STATS_PER_DESCRIPTOR
        );
        assert_eq!(parsed_stats.free_memory(), Some(0x5678));
    }

    #[test]
    fn default_prepared_device_has_version_feature_and_base_queues() {
        let device = prepared(balloon_config(64, false, 0, false, false));

        assert_eq!(
            device.available_features(),
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
        );
        assert_eq!(
            device.queue_layout().queue_count(),
            VIRTIO_BALLOON_MIN_QUEUE_COUNT
        );
        let queues: Vec<_> = device.queue_layout().iter().collect();
        assert_eq!(
            queues,
            vec![
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::Inflate,
                    VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::Deflate,
                    VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
            ]
        );
    }

    #[test]
    fn prepared_device_features_follow_balloon_config() {
        let device = prepared(balloon_config(64, true, 1, true, true));
        let features = device.available_features();

        assert!(has_feature(features, VIRTIO_FEATURE_VERSION_1));
        assert!(has_feature(features, VIRTIO_BALLOON_F_DEFLATE_ON_OOM));
        assert!(has_feature(features, VIRTIO_BALLOON_F_STATS_VQ));
        assert!(has_feature(features, VIRTIO_BALLOON_F_FREE_PAGE_HINTING));
        assert!(has_feature(features, VIRTIO_BALLOON_F_FREE_PAGE_REPORTING));
    }

    #[test]
    fn prepared_device_omits_disabled_optional_features() {
        let device = prepared(balloon_config(64, true, 0, false, true));
        let features = device.available_features();

        assert!(has_feature(features, VIRTIO_FEATURE_VERSION_1));
        assert!(has_feature(features, VIRTIO_BALLOON_F_DEFLATE_ON_OOM));
        assert!(!has_feature(features, VIRTIO_BALLOON_F_STATS_VQ));
        assert!(!has_feature(features, VIRTIO_BALLOON_F_FREE_PAGE_HINTING));
        assert!(has_feature(features, VIRTIO_BALLOON_F_FREE_PAGE_REPORTING));
    }

    #[test]
    fn prepared_device_converts_target_mib_to_config_pages() {
        let device = prepared(balloon_config(64, false, 0, false, false));
        let config_space = device.config_space();

        assert_eq!(
            config_space.num_pages(),
            64 * VIRTIO_BALLOON_MIB_TO_4K_PAGES
        );
        assert_eq!(config_space.actual_pages(), 0);
        assert_eq!(
            config_space.free_page_hint_cmd_id(),
            VIRTIO_BALLOON_FREE_PAGE_HINT_STOP
        );
    }

    #[test]
    fn page_conversion_rejects_overflow() {
        let amount_mib = VIRTIO_BALLOON_MAX_AMOUNT_MIB + 1;
        let err = mib_to_4k_pages(amount_mib).expect_err("page conversion should overflow");

        assert_eq!(err.amount_mib(), amount_mib);
        assert_eq!(err.max_amount_mib(), VIRTIO_BALLOON_MAX_AMOUNT_MIB);
        assert_eq!(
            err.to_string(),
            "balloon amount_mib 16777216 exceeds maximum 16777215 MiB representable as 4 KiB pages"
        );
        assert_eq!(
            PreparedBalloonDevice::from_config(balloon_config(amount_mib, false, 0, false, false))
                .expect_err("prepared device should reject page overflow"),
            err
        );
    }

    #[test]
    fn page_conversion_accepts_maximum_amount() {
        let pages = mib_to_4k_pages(VIRTIO_BALLOON_MAX_AMOUNT_MIB)
            .expect("maximum balloon amount should convert");
        let device = prepared(balloon_config(
            VIRTIO_BALLOON_MAX_AMOUNT_MIB,
            false,
            0,
            false,
            false,
        ));

        assert_eq!(pages, u32::MAX - 255);
        assert_eq!(device.config_space().num_pages(), pages);
    }

    #[test]
    fn page_conversion_accepts_zero_amount() {
        let pages = mib_to_4k_pages(0).expect("zero balloon amount should convert");
        let device = prepared(balloon_config(0, false, 0, false, false));

        assert_eq!(pages, 0);
        assert_eq!(device.config_space().num_pages(), 0);
    }

    #[test]
    fn config_space_uses_firecracker_little_endian_layout() {
        let config_space = VirtioBalloonConfigSpace::new(0x0102_0304, 0x0506_0708, 0x090a_0b0c);

        assert_eq!(
            config_space.to_le_bytes(),
            [
                0x04, 0x03, 0x02, 0x01, 0x08, 0x07, 0x06, 0x05, 0x0c, 0x0b, 0x0a, 0x09,
            ]
        );
        assert_eq!(
            config_space.to_le_bytes().len(),
            VIRTIO_BALLOON_CONFIG_SPACE_SIZE
        );
    }

    #[test]
    fn pfn_payload_parser_accepts_little_endian_pfns() {
        let bytes = pfn_payload_bytes(&[0x0102_0304, 0x0506_0708, 0xffff_ffff]);

        let payload = VirtioBalloonPfnPayload::parse(&bytes).expect("PFN payload should parse");

        assert_eq!(payload.pfns(), &[0x0102_0304, 0x0506_0708, 0xffff_ffff]);
        assert_eq!(payload.len(), 3);
        assert!(!payload.is_empty());
        assert_eq!(
            payload.into_vec(),
            vec![0x0102_0304, 0x0506_0708, 0xffff_ffff]
        );
    }

    #[test]
    fn pfn_payload_parser_rejects_empty_payload() {
        let err =
            VirtioBalloonPfnPayload::parse(&[]).expect_err("empty PFN payload should be rejected");

        assert!(matches!(
            err,
            VirtioBalloonPfnPayloadParseError::EmptyPayload
        ));
        assert_eq!(
            err.to_string(),
            "virtio-balloon PFN payload cannot be empty"
        );
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn pfn_payload_parser_rejects_unaligned_payload_length() {
        let err = VirtioBalloonPfnPayload::parse(&[1, 2, 3, 4, 5])
            .expect_err("unaligned PFN payload should be rejected");

        assert!(matches!(
            err,
            VirtioBalloonPfnPayloadParseError::UnalignedLength { len: 5 }
        ));
        assert_eq!(
            err.to_string(),
            "virtio-balloon PFN payload length 5 is not a multiple of 4"
        );
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn pfn_payload_parser_accepts_maximum_pfn_count() {
        let bytes = vec![0; VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE];

        let payload =
            VirtioBalloonPfnPayload::parse(&bytes).expect("maximum PFN payload should parse");

        assert_eq!(payload.len(), VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR);
        assert!(
            payload.pfns().iter().all(|pfn| *pfn == 0),
            "zero-filled payload should parse as zero PFNs"
        );
    }

    #[test]
    fn pfn_payload_parser_rejects_one_over_maximum_pfn_count() {
        let bytes = vec![0; VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE + VIRTIO_BALLOON_PFN_SIZE];

        let err = VirtioBalloonPfnPayload::parse(&bytes)
            .expect_err("oversized PFN payload should be rejected before parsing");

        assert!(matches!(
            err,
            VirtioBalloonPfnPayloadParseError::TooManyPfns {
                count,
                max: VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR
            } if count == VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR + 1
        ));
        assert_eq!(
            err.to_string(),
            "virtio-balloon PFN payload contains 257 PFNs, exceeding maximum 256"
        );
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn pfn_payload_parser_errors_return_no_partial_payload() {
        assert!(VirtioBalloonPfnPayload::parse(&[0, 1, 2]).is_err());
        let oversized_payload =
            vec![0; VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE + VIRTIO_BALLOON_PFN_SIZE];
        assert!(VirtioBalloonPfnPayload::parse(&oversized_payload).is_err());
    }

    #[test]
    fn pfn_descriptor_payload_reads_single_descriptor() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[0x0102_0304, 0x0506_0708, 0xffff_ffff]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        let chain = descriptor_chain(&memory, 0);

        let payload = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect("single PFN descriptor payload should read");

        assert_eq!(payload.bytes(), bytes.as_slice());
        assert_eq!(payload.len(), bytes.len());
        assert!(!payload.is_empty());
        assert_eq!(payload.clone().into_vec(), bytes);
        assert_eq!(
            payload
                .into_pfn_payload()
                .expect("descriptor payload should parse")
                .pfns(),
            &[0x0102_0304, 0x0506_0708, 0xffff_ffff]
        );
    }

    #[test]
    fn pfn_descriptor_payload_reads_split_descriptor_chain() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[1, 2, 3]);
        write_guest_bytes(
            &mut memory,
            TEST_PFN_DATA,
            &bytes[..VIRTIO_BALLOON_PFN_SIZE],
        );
        write_guest_bytes(
            &mut memory,
            TEST_PFN_DATA_SPLIT,
            &bytes[VIRTIO_BALLOON_PFN_SIZE..],
        );
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_PFN_DATA,
                u32::try_from(VIRTIO_BALLOON_PFN_SIZE).expect("PFN size should fit u32"),
                Some(1),
            ),
        );
        write_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(
                TEST_PFN_DATA_SPLIT,
                descriptor_len(&bytes[VIRTIO_BALLOON_PFN_SIZE..]),
                None,
            ),
        );
        let chain = descriptor_chain(&memory, 0);

        let payload = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect("split PFN descriptor payload should read");

        assert_eq!(payload.bytes(), bytes.as_slice());
    }

    #[test]
    fn pfn_descriptor_payload_rejects_write_only_descriptor() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[1]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        let chain = descriptor_chain(&memory, 0);

        let err = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect_err("write-only PFN descriptor should be rejected");

        assert!(matches!(
            err,
            VirtioBalloonPfnDescriptorPayloadReadError::DescriptorWriteOnly { index: 0 }
        ));
        assert_eq!(
            err.to_string(),
            "virtio-balloon PFN descriptor 0 is write-only"
        );
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn pfn_descriptor_payload_rejects_empty_descriptor() {
        let mut memory = pfn_descriptor_memory();
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, 0, None),
        );
        let chain = descriptor_chain(&memory, 0);

        let err = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect_err("empty PFN descriptor should be rejected");

        assert!(matches!(
            err,
            VirtioBalloonPfnDescriptorPayloadReadError::DescriptorEmpty { index: 0 }
        ));
        assert_eq!(err.to_string(), "virtio-balloon PFN descriptor 0 is empty");
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn pfn_descriptor_payload_rejects_unmapped_descriptor() {
        let mut memory = pfn_descriptor_memory();
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(GuestAddress::new(TEST_MEMORY_SIZE), 4, None),
        );
        let chain = descriptor_chain(&memory, 0);

        let err = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect_err("unmapped PFN descriptor should be rejected");

        assert!(matches!(
            err,
            VirtioBalloonPfnDescriptorPayloadReadError::DescriptorAccess {
                index: 0,
                address,
                len: 4,
                source: GuestMemoryAccessError::UnmappedRange { .. },
            } if address == GuestAddress::new(TEST_MEMORY_SIZE)
        ));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn pfn_descriptor_payload_rejects_range_overflow() {
        let mut memory = pfn_descriptor_memory();
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(GuestAddress::new(u64::MAX), 4, None),
        );
        let chain = descriptor_chain(&memory, 0);

        let err = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect_err("overflowing PFN descriptor range should be rejected");

        assert!(matches!(
            err,
            VirtioBalloonPfnDescriptorPayloadReadError::DescriptorRange {
                index: 0,
                address,
                len: 4,
                source: GuestMemoryError::AddressOverflow { .. },
            } if address == GuestAddress::new(u64::MAX)
        ));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn pfn_descriptor_payload_reports_unaligned_total_length() {
        let mut memory = pfn_descriptor_memory();
        let bytes = [1, 2, 3, 4, 5];
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        let chain = descriptor_chain(&memory, 0);
        let payload = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect("unaligned descriptor payload bytes should still read");

        let err = payload
            .parse_pfn_payload()
            .expect_err("unaligned PFN payload should fail parsing");

        assert!(matches!(
            err,
            VirtioBalloonPfnPayloadParseError::UnalignedLength { len: 5 }
        ));
    }

    #[test]
    fn pfn_descriptor_payload_accepts_exact_maximum_payload() {
        let mut memory = pfn_descriptor_memory();
        let bytes = vec![0; VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE];
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        let chain = descriptor_chain(&memory, 0);

        let payload = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect("maximum PFN descriptor payload should read");
        let parsed = payload
            .parse_pfn_payload()
            .expect("maximum PFN descriptor payload should parse");

        assert_eq!(payload.len(), VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE);
        assert_eq!(parsed.len(), VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR);
    }

    #[test]
    fn pfn_descriptor_payload_rejects_one_over_maximum_payload() {
        let mut memory = pfn_descriptor_memory();
        let len = u32::try_from(VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE + 1)
            .expect("test payload length should fit u32");
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, len, None),
        );
        let chain = descriptor_chain(&memory, 0);

        let err = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect_err("oversized PFN descriptor payload should be rejected");

        assert!(matches!(
            err,
            VirtioBalloonPfnDescriptorPayloadReadError::PayloadLengthTooLarge {
                len,
                max: VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE,
            } if len == VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE + 1
        ));
        assert_eq!(
            err.to_string(),
            "virtio-balloon PFN descriptor payload length 1025 exceeds maximum 1024"
        );
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn pfn_descriptor_payload_rejects_oversized_payload_before_guest_range_access() {
        let mut memory = pfn_descriptor_memory();
        let len = u32::try_from(VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE + 1)
            .expect("test payload length should fit u32");
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(GuestAddress::new(TEST_MEMORY_SIZE), len, None),
        );
        let chain = descriptor_chain(&memory, 0);

        let err = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect_err("oversized PFN descriptor payload should be rejected before access");

        assert!(matches!(
            err,
            VirtioBalloonPfnDescriptorPayloadReadError::PayloadLengthTooLarge {
                len,
                max: VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE,
            } if len == VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE + 1
        ));
    }

    #[test]
    fn pfn_descriptor_payload_converts_into_ranges() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[3, 1, 2, 2]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        let chain = descriptor_chain(&memory, 0);
        let payload = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect("PFN descriptor payload should read");

        let ranges = payload
            .parse_pfn_payload()
            .expect("PFN descriptor payload should parse")
            .into_page_ranges()
            .expect("PFN descriptor payload should compact");

        assert_eq!(
            ranges.ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 1,
                page_count: 3
            }]
        );
    }

    #[test]
    fn pfn_ranges_accept_empty_input() {
        let ranges =
            VirtioBalloonPfnRanges::from_pfns(Vec::new()).expect("empty PFN input should compact");

        assert!(ranges.is_empty());
        assert_eq!(ranges.len(), 0);
        assert_eq!(ranges.ranges(), &[]);
        assert_eq!(ranges.into_vec(), Vec::new());
    }

    #[test]
    fn pfn_ranges_compact_contiguous_pfns() {
        let ranges = VirtioBalloonPfnRanges::from_pfns(vec![0, 1, 2, 3])
            .expect("contiguous PFNs should compact");

        assert_eq!(
            ranges.ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 0,
                page_count: 4
            }]
        );
        assert_eq!(ranges.ranges()[0].start_pfn(), 0);
        assert_eq!(ranges.ranges()[0].page_count(), 4);
    }

    #[test]
    fn pfn_ranges_sort_unsorted_pfns() {
        let ranges = VirtioBalloonPfnRanges::from_pfns(vec![5, 2, 4, 3, 9])
            .expect("unsorted PFNs should compact");

        assert_eq!(
            ranges.ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 2,
                    page_count: 4
                },
                VirtioBalloonPfnRange {
                    start_pfn: 9,
                    page_count: 1
                }
            ]
        );
    }

    #[test]
    fn pfn_ranges_deduplicate_pfns() {
        let ranges = VirtioBalloonPfnRanges::from_pfns(vec![7, 7, 8, 8, 10, 9])
            .expect("duplicate PFNs should compact");

        assert_eq!(
            ranges.ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 7,
                page_count: 4
            }]
        );
    }

    #[test]
    fn pfn_ranges_keep_separated_ranges() {
        let ranges = VirtioBalloonPfnRanges::from_pfns(vec![0, 1, 3, 4, 6])
            .expect("separated PFNs should compact");

        assert_eq!(
            ranges.ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 0,
                    page_count: 2
                },
                VirtioBalloonPfnRange {
                    start_pfn: 3,
                    page_count: 2
                },
                VirtioBalloonPfnRange {
                    start_pfn: 6,
                    page_count: 1
                }
            ]
        );
    }

    #[test]
    fn pfn_ranges_handle_maximum_pfn_without_overflow() {
        let ranges = VirtioBalloonPfnRanges::from_pfns(vec![u32::MAX, u32::MAX - 1, u32::MAX, 0])
            .expect("maximum PFN should compact without overflow");

        assert_eq!(
            ranges.ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 0,
                    page_count: 1
                },
                VirtioBalloonPfnRange {
                    start_pfn: u32::MAX - 1,
                    page_count: 2
                }
            ]
        );
    }

    #[test]
    fn pfn_ranges_accept_maximum_pfn_count() {
        let max_page_count = u32::try_from(VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR)
            .expect("maximum PFN count should fit u32");
        let pfns = (0..max_page_count).collect();

        let ranges =
            VirtioBalloonPfnRanges::from_pfns(pfns).expect("maximum PFN count should compact");

        assert_eq!(
            ranges.ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 0,
                page_count: max_page_count
            }]
        );
    }

    #[test]
    fn pfn_ranges_reject_one_over_maximum_pfn_count() {
        let err =
            VirtioBalloonPfnRanges::from_pfns(vec![0; VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR + 1])
                .expect_err("oversized PFN range input should be rejected before sorting");

        assert!(matches!(
            err,
            VirtioBalloonPfnRangeCompactError::TooManyPfns {
                count,
                max: VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR
            } if count == VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR + 1
        ));
        assert_eq!(
            err.to_string(),
            "virtio-balloon PFN compaction input contains 257 PFNs, exceeding maximum 256"
        );
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn pfn_payload_converts_into_ranges() {
        let bytes = pfn_payload_bytes(&[3, 1, 2, 2]);
        let payload = VirtioBalloonPfnPayload::parse(&bytes).expect("PFN payload should parse");

        let ranges = payload
            .into_page_ranges()
            .expect("parsed PFNs should compact into ranges");

        assert_eq!(
            ranges.ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 1,
                page_count: 3
            }]
        );
    }

    #[test]
    fn memory_accounting_adds_and_merges_ranges() {
        let mut accounting = VirtioBalloonMemoryAccounting::default();

        accounting
            .add_inflated_ranges(&[
                VirtioBalloonPfnRange::new(10, 2),
                VirtioBalloonPfnRange::new(12, 1),
                VirtioBalloonPfnRange::new(8, 2),
                VirtioBalloonPfnRange::new(20, 1),
            ])
            .expect("inflated ranges should add");

        assert_eq!(
            accounting.inflated_page_ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 8,
                    page_count: 5,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 20,
                    page_count: 1,
                }
            ]
        );
        assert_eq!(accounting.inflated_page_count(), 6);
    }

    #[test]
    fn memory_accounting_removes_and_splits_ranges() {
        let mut accounting = VirtioBalloonMemoryAccounting::default();
        accounting
            .add_inflated_ranges(&[VirtioBalloonPfnRange::new(10, 10)])
            .expect("inflated range should add");

        accounting
            .remove_inflated_ranges(&[VirtioBalloonPfnRange::new(13, 3)])
            .expect("inflated range should remove");

        assert_eq!(
            accounting.inflated_page_ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 10,
                    page_count: 3,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 16,
                    page_count: 4,
                }
            ]
        );
        assert_eq!(accounting.inflated_page_count(), 7);
    }

    #[test]
    fn memory_accounting_removes_multiple_ranges_from_one_existing_range() {
        let mut accounting = VirtioBalloonMemoryAccounting::default();
        accounting
            .add_inflated_ranges(&[VirtioBalloonPfnRange::new(10, 10)])
            .expect("inflated range should add");

        accounting
            .remove_inflated_ranges(&[
                VirtioBalloonPfnRange::new(12, 2),
                VirtioBalloonPfnRange::new(16, 1),
            ])
            .expect("inflated ranges should remove");

        assert_eq!(
            accounting.inflated_page_ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 10,
                    page_count: 2,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 14,
                    page_count: 2,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 17,
                    page_count: 3,
                }
            ]
        );
        assert_eq!(accounting.inflated_page_count(), 7);
    }

    #[test]
    fn memory_accounting_removal_can_span_multiple_existing_ranges() {
        let mut accounting = VirtioBalloonMemoryAccounting::default();
        accounting
            .add_inflated_ranges(&[
                VirtioBalloonPfnRange::new(10, 2),
                VirtioBalloonPfnRange::new(20, 2),
                VirtioBalloonPfnRange::new(30, 2),
            ])
            .expect("inflated ranges should add");

        accounting
            .remove_inflated_ranges(&[VirtioBalloonPfnRange::new(11, 20)])
            .expect("spanning removal range should apply");

        assert_eq!(
            accounting.inflated_page_ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 10,
                    page_count: 1,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 31,
                    page_count: 1,
                }
            ]
        );
        assert_eq!(accounting.inflated_page_count(), 2);
    }

    #[test]
    fn memory_accounting_ignores_untracked_deflate_ranges() {
        let mut accounting = VirtioBalloonMemoryAccounting::default();
        accounting
            .add_inflated_ranges(&[VirtioBalloonPfnRange::new(30, 2)])
            .expect("inflated range should add");

        accounting
            .remove_inflated_ranges(&[VirtioBalloonPfnRange::new(10, 5)])
            .expect("untracked deflate range should be a no-op");

        assert_eq!(
            accounting.inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 30,
                page_count: 2,
            }]
        );
    }

    #[test]
    fn memory_accounting_handles_max_pfn_adjacency() {
        let mut accounting = VirtioBalloonMemoryAccounting::default();

        accounting
            .add_inflated_ranges(&[
                VirtioBalloonPfnRange::new(0, u32::MAX),
                VirtioBalloonPfnRange::new(u32::MAX, 1),
            ])
            .expect("maximum adjacent ranges should add");

        assert_eq!(
            accounting.inflated_page_ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 0,
                    page_count: u32::MAX,
                },
                VirtioBalloonPfnRange {
                    start_pfn: u32::MAX,
                    page_count: 1,
                }
            ]
        );
        assert_eq!(accounting.inflated_page_count(), u64::from(u32::MAX) + 1);
    }

    #[test]
    fn inflate_queue_dispatch_empty_available_ring_is_noop() {
        let mut memory = pfn_descriptor_memory();
        let mut queue = inflate_queue();

        let dispatch = queue
            .dispatch_inflate(&mut memory)
            .expect("empty inflate queue should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 0);
        assert!(!dispatch.needs_queue_interrupt());
        assert!(dispatch.inflated_page_ranges().is_empty());
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 0);
    }

    #[test]
    fn inflate_queue_dispatch_publishes_zero_length_used_element_and_records_ranges() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[3, 1, 2, 2]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        let mut queue = inflate_queue();

        let dispatch = queue
            .dispatch_inflate(&mut memory)
            .expect("inflate descriptor should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(
            dispatch.inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 1,
                page_count: 3,
            }]
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
    }

    #[test]
    fn inflate_queue_dispatch_records_successful_discard_outcome() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[40, 41, 42, 43]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        let mut queue = inflate_queue();
        let mut adviser = TestBalloonDiscardAdviser::new(VIRTIO_BALLOON_PAGE_SIZE);

        let dispatch = queue
            .dispatch_inflate_with_adviser(&mut memory, &mut adviser)
            .expect("inflate descriptor should dispatch with injected advice");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert_eq!(
            dispatch.inflate_discard(),
            VirtioBalloonDiscardOutcome {
                attempts: 1,
                requested_bytes: VIRTIO_BALLOON_PAGE_SIZE * 4,
                advised_bytes: VIRTIO_BALLOON_PAGE_SIZE * 4,
                skipped_bytes: 0,
                failed_bytes: 0,
                failures: 0,
            }
        );
        assert_eq!(adviser.zero_calls, 1);
        assert_eq!(adviser.free_calls, 1);
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
    }

    #[test]
    fn inflate_queue_discard_failure_preserves_completion_and_accounting() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[44]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        let mut queue = inflate_queue();
        let mut adviser =
            TestBalloonDiscardAdviser::new(VIRTIO_BALLOON_PAGE_SIZE).with_zero_failure();

        let dispatch = queue
            .dispatch_inflate_with_adviser(&mut memory, &mut adviser)
            .expect("advice failure should not fail inflate dispatch");
        let mut accounting = VirtioBalloonMemoryAccounting::new();
        apply_balloon_queue_accounting(&mut accounting, VirtioBalloonQueueKind::Inflate, &dispatch)
            .expect("completed inflate should still update accounting");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert_eq!(
            dispatch.inflate_discard(),
            VirtioBalloonDiscardOutcome {
                attempts: 1,
                requested_bytes: VIRTIO_BALLOON_PAGE_SIZE,
                advised_bytes: 0,
                skipped_bytes: 0,
                failed_bytes: VIRTIO_BALLOON_PAGE_SIZE,
                failures: 1,
            }
        );
        assert_eq!(adviser.zero_calls, 1);
        assert_eq!(adviser.free_calls, 0);
        assert_eq!(accounting.inflated_page_count(), 1);
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
    }

    #[test]
    fn inflate_queue_later_error_preserves_completed_discard_outcome() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[48]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0, TEST_QUEUE_SIZE]);
        let mut queue = inflate_queue();
        let mut adviser = TestBalloonDiscardAdviser::new(VIRTIO_BALLOON_PAGE_SIZE);

        let error = queue
            .dispatch_inflate_with_adviser(&mut memory, &mut adviser)
            .expect_err("invalid later head should retain completed discard");

        assert_eq!(error.completed_dispatch().completed_descriptors(), 1);
        assert_eq!(
            error.completed_dispatch().inflate_discard(),
            VirtioBalloonDiscardOutcome {
                attempts: 1,
                requested_bytes: VIRTIO_BALLOON_PAGE_SIZE,
                advised_bytes: VIRTIO_BALLOON_PAGE_SIZE,
                skipped_bytes: 0,
                failed_bytes: 0,
                failures: 0,
            }
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
    }

    #[test]
    fn inflate_queue_dispatch_reads_split_descriptor_payload() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[5, 6, 8]);
        write_guest_bytes(
            &mut memory,
            TEST_PFN_DATA,
            &bytes[..VIRTIO_BALLOON_PFN_SIZE],
        );
        write_guest_bytes(
            &mut memory,
            TEST_PFN_DATA_SPLIT,
            &bytes[VIRTIO_BALLOON_PFN_SIZE..],
        );
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_PFN_DATA,
                u32::try_from(VIRTIO_BALLOON_PFN_SIZE).expect("PFN size should fit u32"),
                Some(1),
            ),
        );
        write_inflate_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(
                TEST_PFN_DATA_SPLIT,
                descriptor_len(&bytes[VIRTIO_BALLOON_PFN_SIZE..]),
                None,
            ),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        let mut queue = inflate_queue();

        let dispatch = queue
            .dispatch_inflate(&mut memory)
            .expect("split inflate descriptor should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert_eq!(
            dispatch.inflated_page_ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 5,
                    page_count: 2,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 8,
                    page_count: 1,
                }
            ]
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
    }

    #[test]
    fn inflate_queue_dispatch_aggregates_multiple_descriptors() {
        let mut memory = pfn_descriptor_memory();
        let first = pfn_payload_bytes(&[10]);
        let second = pfn_payload_bytes(&[20, 21]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &first);
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &second);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&first), None),
        );
        write_inflate_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, descriptor_len(&second), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0, 1]);
        let mut queue = inflate_queue();

        let dispatch = queue
            .dispatch_inflate(&mut memory)
            .expect("inflate descriptors should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 2);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(
            dispatch.inflated_page_ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 10,
                    page_count: 1,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 20,
                    page_count: 2,
                }
            ]
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 2);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 1), (1, 0));
    }

    #[test]
    fn inflate_queue_dispatch_available_ring_error_preserves_completed_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[13, 14]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0, TEST_QUEUE_SIZE]);
        let mut queue = inflate_queue();

        let error = queue
            .dispatch_inflate(&mut memory)
            .expect_err("invalid second inflate queue head should fail");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::AvailableRing { .. }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 1);
        assert!(error.completed_dispatch().needs_queue_interrupt());
        assert_eq!(
            error.completed_dispatch().inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 13,
                page_count: 2,
            }]
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn inflate_queue_dispatch_parse_error_preserves_completed_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let first = pfn_payload_bytes(&[7]);
        let malformed = [1, 2, 3, 4, 5];
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &first);
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &malformed);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&first), None),
        );
        write_inflate_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, descriptor_len(&malformed), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0, 1]);
        let mut queue = inflate_queue();

        let error = queue
            .dispatch_inflate(&mut memory)
            .expect_err("malformed second inflate descriptor should fail");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::PfnPayloadParse {
                descriptor_head: 1,
                ..
            }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 1);
        assert!(error.completed_dispatch().needs_queue_interrupt());
        assert_eq!(
            error.completed_dispatch().inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 7,
                page_count: 1,
            }]
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn inflate_queue_dispatch_read_error_preserves_completed_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let first = pfn_payload_bytes(&[9]);
        let second = pfn_payload_bytes(&[11]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &first);
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &second);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&first), None),
        );
        write_inflate_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(TEST_PFN_DATA_SPLIT, descriptor_len(&second), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0, 1]);
        let mut queue = inflate_queue();

        let error = queue
            .dispatch_inflate(&mut memory)
            .expect_err("write-only second inflate descriptor should fail");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::PfnDescriptorRead {
                descriptor_head: 1,
                ..
            }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 1);
        assert_eq!(
            error.completed_dispatch().inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 9,
                page_count: 1,
            }]
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn inflate_queue_dispatch_rejects_unmapped_pfn_without_publication() {
        let mut memory = pfn_descriptor_memory();
        let unmapped_pfn = first_unmapped_test_pfn();
        let bytes = pfn_payload_bytes(&[unmapped_pfn]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        let mut queue = inflate_queue();

        let error = queue
            .dispatch_inflate(&mut memory)
            .expect_err("unmapped inflate PFN should fail before publication");

        assert!(matches!(
            &error,
            VirtioBalloonQueueDispatchError::PfnRangeAccess {
                queue: VirtioBalloonQueueKind::Inflate,
                descriptor_head: 0,
                source: VirtioBalloonPfnRangeAccessError::GuestMemory {
                    pfn_range: VirtioBalloonPfnRange {
                        start_pfn,
                        page_count: 1
                    },
                    source: GuestMemoryAccessError::UnmappedRange { .. },
                    ..
                },
                ..
            } if *start_pfn == unmapped_pfn
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 0);
        assert!(error.completed_dispatch().inflated_page_ranges().is_empty());
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 0);
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn inflate_queue_dispatch_used_ring_error_preserves_completed_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[12]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        let mut queue = inflate_queue_with_used_ring(GuestAddress::new(TEST_MEMORY_SIZE));

        let error = queue
            .dispatch_inflate(&mut memory)
            .expect_err("unmapped inflate used ring should fail publication");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::UsedRing {
                descriptor_head: 0,
                ..
            }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 0);
        assert!(!error.completed_dispatch().needs_queue_interrupt());
        assert!(error.completed_dispatch().inflated_page_ranges().is_empty());
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn deflate_queue_dispatch_empty_available_ring_is_noop() {
        let mut memory = pfn_descriptor_memory();
        let mut queue = deflate_queue();

        let dispatch = queue
            .dispatch_deflate(&mut memory)
            .expect("empty deflate queue should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 0);
        assert!(!dispatch.needs_queue_interrupt());
        assert!(dispatch.deflated_page_ranges().is_empty());
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 0);
    }

    #[test]
    fn deflate_queue_dispatch_publishes_zero_length_used_element_and_records_ranges() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[4, 5, 7]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0]);
        let mut queue = deflate_queue();

        let dispatch = queue
            .dispatch_deflate(&mut memory)
            .expect("deflate descriptor should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(
            dispatch.deflated_page_ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 4,
                    page_count: 2,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 7,
                    page_count: 1,
                }
            ]
        );
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, deflate_used_ring(), 0), (0, 0));
    }

    #[test]
    fn deflate_queue_dispatch_publishes_multiple_zero_length_used_elements_and_records_ranges() {
        let mut memory = pfn_descriptor_memory();
        let first = pfn_payload_bytes(&[15]);
        let second = pfn_payload_bytes(&[16, 18]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &first);
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &second);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&first), None),
        );
        write_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, descriptor_len(&second), None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0, 1]);
        let mut queue = deflate_queue();

        let dispatch = queue
            .dispatch_deflate(&mut memory)
            .expect("deflate descriptors should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 2);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(
            dispatch.deflated_page_ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 15,
                    page_count: 1,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 16,
                    page_count: 1,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 18,
                    page_count: 1,
                }
            ]
        );
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 2);
        assert_eq!(read_used_element(&memory, deflate_used_ring(), 0), (0, 0));
        assert_eq!(read_used_element(&memory, deflate_used_ring(), 1), (1, 0));
    }

    #[test]
    fn deflate_queue_dispatch_available_ring_error_preserves_completed_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[22]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0, TEST_QUEUE_SIZE]);
        let mut queue = deflate_queue();

        let error = queue
            .dispatch_deflate(&mut memory)
            .expect_err("invalid second available head should fail");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::AvailableRing { .. }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 1);
        assert!(error.completed_dispatch().needs_queue_interrupt());
        assert_eq!(
            error.completed_dispatch().deflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 22,
                page_count: 1,
            }]
        );
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, deflate_used_ring(), 0), (0, 0));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn deflate_queue_dispatch_parse_error_preserves_completed_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let first = pfn_payload_bytes(&[60]);
        let malformed = [1, 2, 3, 4, 5];
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &first);
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &malformed);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&first), None),
        );
        write_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, descriptor_len(&malformed), None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0, 1]);
        let mut queue = deflate_queue();

        let error = queue
            .dispatch_deflate(&mut memory)
            .expect_err("malformed second deflate descriptor should fail");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::PfnPayloadParse {
                queue: VirtioBalloonQueueKind::Deflate,
                descriptor_head: 1,
                ..
            }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 1);
        assert_eq!(
            error.completed_dispatch().deflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 60,
                page_count: 1,
            }]
        );
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 1);
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn deflate_queue_dispatch_used_ring_error_preserves_completed_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[24]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0]);
        let mut queue = deflate_queue_with_used_ring(GuestAddress::new(TEST_MEMORY_SIZE));

        let error = queue
            .dispatch_deflate(&mut memory)
            .expect_err("unmapped used ring should fail publication");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::UsedRing {
                descriptor_head: 0,
                ..
            }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 0);
        assert!(!error.completed_dispatch().needs_queue_interrupt());
        assert!(error.completed_dispatch().deflated_page_ranges().is_empty());
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn statistics_queue_dispatch_records_stats_and_holds_current_descriptor() {
        let mut memory = pfn_descriptor_memory();
        let bytes = stat_payload_bytes(&[
            (VIRTIO_BALLOON_S_SWAP_OUT, 9),
            (VIRTIO_BALLOON_S_MEMFREE, 0x5678),
        ]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_statistics_descriptor(
            &mut memory,
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(
            &mut memory,
            queue_available_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            &[0],
        );
        let mut queue = statistics_queue(VIRTIO_BALLOON_STATS_QUEUE_INDEX);

        let dispatch = queue
            .dispatch_statistics(
                &mut memory,
                statistics_context(None, BalloonOptionalStats::default()),
            )
            .expect("statistics descriptor should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 0);
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(dispatch.statistics_reports(), 1);
        assert_eq!(dispatch.statistics_oversized_reports(), 0);
        assert_eq!(dispatch.statistics_pending_descriptor_head(), Some(0));
        assert_eq!(dispatch.statistics().swap_out(), Some(9));
        assert_eq!(dispatch.statistics().free_memory(), Some(0x5678));
        assert_eq!(
            read_used_idx(&memory, queue_used_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX)),
            0
        );
    }

    #[test]
    fn statistics_queue_dispatch_merges_with_existing_stats() {
        let mut memory = pfn_descriptor_memory();
        let mut existing = BalloonOptionalStats::default();
        assert!(existing.record_stat(VirtioBalloonStat::new(VIRTIO_BALLOON_S_SWAP_OUT, 9,)));
        let bytes = stat_payload_bytes(&[(VIRTIO_BALLOON_S_MEMFREE, 0x5678)]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_statistics_descriptor(
            &mut memory,
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(
            &mut memory,
            queue_available_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            &[0],
        );
        let mut queue = statistics_queue(VIRTIO_BALLOON_STATS_QUEUE_INDEX);

        let dispatch = queue
            .dispatch_statistics(&mut memory, statistics_context(None, existing))
            .expect("statistics descriptor should dispatch");

        assert_eq!(dispatch.statistics().swap_out(), Some(9));
        assert_eq!(dispatch.statistics().free_memory(), Some(0x5678));
    }

    #[test]
    fn statistics_queue_dispatch_completes_previous_pending_descriptor() {
        let mut memory = pfn_descriptor_memory();
        let bytes = stat_payload_bytes(&[(VIRTIO_BALLOON_S_MEMFREE, 0x5678)]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_statistics_descriptor(
            &mut memory,
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(
            &mut memory,
            queue_available_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            &[0],
        );
        let mut queue = statistics_queue(VIRTIO_BALLOON_STATS_QUEUE_INDEX);

        let dispatch = queue
            .dispatch_statistics(
                &mut memory,
                statistics_context(Some(5), BalloonOptionalStats::default()),
            )
            .expect("statistics descriptor should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(dispatch.statistics_pending_descriptor_head(), Some(0));
        assert_eq!(
            read_used_idx(&memory, queue_used_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX)),
            1
        );
        assert_eq!(
            read_used_element(
                &memory,
                queue_used_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
                0
            ),
            (5, 0)
        );
    }

    #[test]
    fn statistics_queue_trigger_completes_pending_descriptor_without_new_notification() {
        let mut memory = pfn_descriptor_memory();
        let mut existing = BalloonOptionalStats::default();
        assert!(existing.record_stat(VirtioBalloonStat::new(VIRTIO_BALLOON_S_MEMFREE, 0x5678)));
        let mut queue = statistics_queue(VIRTIO_BALLOON_STATS_QUEUE_INDEX);

        let dispatch = queue
            .complete_pending_statistics(&mut memory, statistics_context(Some(5), existing))
            .expect("pending statistics descriptor should complete");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(dispatch.statistics_pending_descriptor_head(), None);
        assert_eq!(dispatch.statistics().free_memory(), Some(0x5678));
        assert_eq!(
            read_used_idx(&memory, queue_used_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX)),
            1
        );
        assert_eq!(
            read_used_element(
                &memory,
                queue_used_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
                0
            ),
            (5, 0)
        );
    }

    #[test]
    fn statistics_queue_trigger_without_pending_descriptor_is_noop() {
        let mut memory = pfn_descriptor_memory();
        let mut existing = BalloonOptionalStats::default();
        assert!(existing.record_stat(VirtioBalloonStat::new(VIRTIO_BALLOON_S_SWAP_OUT, 9)));
        let mut queue = statistics_queue(VIRTIO_BALLOON_STATS_QUEUE_INDEX);

        let dispatch = queue
            .complete_pending_statistics(&mut memory, statistics_context(None, existing))
            .expect("missing pending statistics descriptor should be a no-op");

        assert_eq!(dispatch.completed_descriptors(), 0);
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(dispatch.statistics_pending_descriptor_head(), None);
        assert_eq!(dispatch.statistics().swap_out(), Some(9));
        assert_eq!(
            read_used_idx(&memory, queue_used_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX)),
            0
        );
    }

    #[test]
    fn statistics_queue_trigger_preserves_pending_descriptor_on_used_ring_error() {
        let mut memory = pfn_descriptor_memory();
        let mut existing = BalloonOptionalStats::default();
        assert!(existing.record_stat(VirtioBalloonStat::new(VIRTIO_BALLOON_S_SWAP_OUT, 9)));
        let mut queue = statistics_queue_with_used_ring(
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
            GuestAddress::new(TEST_MEMORY_SIZE),
        );

        let error = queue
            .complete_pending_statistics(&mut memory, statistics_context(Some(5), existing))
            .expect_err("unmapped statistics used ring should fail publication");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::UsedRing {
                queue: VirtioBalloonQueueKind::Statistics,
                descriptor_head: 5,
                ..
            }
        ));
        let completed = error.completed_dispatch();
        assert_eq!(completed.completed_descriptors(), 0);
        assert!(!completed.needs_queue_interrupt());
        assert_eq!(completed.statistics_pending_descriptor_head(), Some(5));
        assert_eq!(completed.statistics().swap_out(), Some(9));
    }

    #[test]
    fn statistics_queue_dispatch_preserves_previous_pending_descriptor_on_used_ring_error() {
        let mut memory = pfn_descriptor_memory();
        let bytes = stat_payload_bytes(&[(VIRTIO_BALLOON_S_MEMFREE, 0x5678)]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_statistics_descriptor(
            &mut memory,
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(
            &mut memory,
            queue_available_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            &[0],
        );
        let mut existing = BalloonOptionalStats::default();
        assert!(existing.record_stat(VirtioBalloonStat::new(VIRTIO_BALLOON_S_SWAP_OUT, 9)));
        let mut queue = statistics_queue_with_used_ring(
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
            GuestAddress::new(TEST_MEMORY_SIZE),
        );

        let error = queue
            .dispatch_statistics(&mut memory, statistics_context(Some(5), existing))
            .expect_err("unmapped statistics used ring should fail publication");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::UsedRing {
                queue: VirtioBalloonQueueKind::Statistics,
                descriptor_head: 5,
                ..
            }
        ));
        let completed = error.completed_dispatch();
        assert_eq!(completed.completed_descriptors(), 0);
        assert!(!completed.needs_queue_interrupt());
        assert_eq!(completed.statistics_pending_descriptor_head(), Some(5));
        assert_eq!(completed.statistics().swap_out(), Some(9));
        assert_eq!(completed.statistics().free_memory(), None);
    }

    #[test]
    fn statistics_queue_dispatch_holds_oversized_descriptor_without_reading_payload() {
        let mut memory = pfn_descriptor_memory();
        write_statistics_descriptor(
            &mut memory,
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
            0,
            TestDescriptor::readable(
                GuestAddress::new(TEST_MEMORY_SIZE),
                u32::try_from(VIRTIO_BALLOON_MAX_STATS_PAYLOAD_SIZE + 1)
                    .expect("oversized stat payload length should fit u32"),
                None,
            ),
        );
        write_available_heads(
            &mut memory,
            queue_available_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            &[0],
        );
        let mut queue = statistics_queue(VIRTIO_BALLOON_STATS_QUEUE_INDEX);

        let dispatch = queue
            .dispatch_statistics(
                &mut memory,
                statistics_context(None, BalloonOptionalStats::default()),
            )
            .expect("oversized statistics descriptor should be held without guest memory access");

        assert_eq!(dispatch.completed_descriptors(), 0);
        assert_eq!(dispatch.statistics_reports(), 0);
        assert_eq!(dispatch.statistics_oversized_reports(), 1);
        assert_eq!(dispatch.statistics_pending_descriptor_head(), Some(0));
        assert!(dispatch.statistics().is_empty());
        assert_eq!(
            read_used_idx(&memory, queue_used_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX)),
            0
        );
    }

    #[test]
    fn statistics_queue_dispatch_rejects_unaligned_descriptor_without_publication() {
        let mut memory = pfn_descriptor_memory();
        let bytes = [1, 2, 3];
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_statistics_descriptor(
            &mut memory,
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(
            &mut memory,
            queue_available_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            &[0],
        );
        let mut queue = statistics_queue(VIRTIO_BALLOON_STATS_QUEUE_INDEX);

        let error = queue
            .dispatch_statistics(
                &mut memory,
                statistics_context(None, BalloonOptionalStats::default()),
            )
            .expect_err("unaligned statistics descriptor should fail");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::StatisticsDescriptorRead {
                descriptor_head: 0,
                ..
            }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 0);
        assert_eq!(
            error
                .completed_dispatch()
                .statistics_pending_descriptor_head(),
            None
        );
        assert_eq!(
            read_used_idx(&memory, queue_used_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX)),
            0
        );
    }

    #[test]
    fn hinting_queue_dispatch_records_guest_command_and_completes_descriptor() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        let command = 42_u32;
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32, None),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
        let mut queue = hinting_queue(queue_index);

        let dispatch = queue
            .dispatch_hinting_commands(&mut memory, default_hinting_context())
            .expect("hinting command descriptor should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(dispatch.hinting_guest_cmd(), Some(command));
        assert!(!dispatch.hinting_completed_run());
        assert!(dispatch.inflated_page_ranges().is_empty());
        assert!(dispatch.deflated_page_ranges().is_empty());
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
        assert_eq!(
            read_used_element(&memory, queue_used_ring(queue_index), 0),
            (0, 0)
        );
    }

    #[test]
    fn hinting_queue_dispatch_accepts_length_matched_command_descriptor() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        let command = 43_u32;
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::writable(TEST_PFN_DATA, VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32, None),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
        let mut queue = hinting_queue(queue_index);

        let dispatch = queue
            .dispatch_hinting_commands(&mut memory, default_hinting_context())
            .expect("hinting command descriptor should dispatch by length");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert_eq!(dispatch.hinting_guest_cmd(), Some(command));
        assert!(!dispatch.hinting_completed_run());
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
    }

    #[test]
    fn hinting_queue_dispatch_marks_stop_and_done_commands_complete() {
        for command in [
            VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
            VIRTIO_BALLOON_FREE_PAGE_HINT_DONE,
        ] {
            let mut memory = pfn_descriptor_memory();
            let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
            write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
            write_hinting_descriptor(
                &mut memory,
                queue_index,
                0,
                TestDescriptor::readable(
                    TEST_PFN_DATA,
                    VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32,
                    None,
                ),
            );
            write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
            let mut queue = hinting_queue(queue_index);

            let dispatch = queue
                .dispatch_hinting_commands(&mut memory, default_hinting_context())
                .expect("hinting completion command should dispatch");

            assert_eq!(dispatch.completed_descriptors(), 1);
            assert_eq!(dispatch.hinting_guest_cmd(), Some(command));
            assert!(dispatch.hinting_completed_run());
            assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
        }
    }

    #[test]
    fn hinting_queue_dispatch_later_guest_command_clears_completed_run() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        let later_command = 13_u32;
        write_guest_bytes(
            &mut memory,
            TEST_PFN_DATA,
            &VIRTIO_BALLOON_FREE_PAGE_HINT_STOP.to_le_bytes(),
        );
        write_guest_bytes(
            &mut memory,
            TEST_PFN_DATA_SPLIT,
            &later_command.to_le_bytes(),
        );
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32, None),
        );
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            1,
            TestDescriptor::readable(
                TEST_PFN_DATA_SPLIT,
                VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32,
                None,
            ),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0, 1]);
        let mut queue = hinting_queue(queue_index);

        let dispatch = queue
            .dispatch_hinting_commands(&mut memory, default_hinting_context())
            .expect("later hinting command should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 2);
        assert_eq!(dispatch.hinting_guest_cmd(), Some(later_command));
        assert!(!dispatch.hinting_completed_run());
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 2);
    }

    #[test]
    fn hinting_queue_dispatch_records_active_run_range_descriptor() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        let command = 7_u32;
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &[0xaa; 64]);
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::readable(
                TEST_PFN_DATA,
                VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32,
                Some(1),
            ),
        );
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            1,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, 64, None),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
        let mut queue = hinting_queue(queue_index);

        let dispatch = queue
            .dispatch_hinting_commands(&mut memory, hinting_context(command, None))
            .expect("hinting command with active range payload should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert_eq!(dispatch.hinting_guest_cmd(), Some(command));
        assert!(!dispatch.hinting_completed_run());
        assert_eq!(
            dispatch.hinting_page_ranges(),
            &[GuestMemoryRange::new(TEST_PFN_DATA_SPLIT, 64).expect("test range should be valid")]
        );
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
        assert_eq!(
            read_used_element(&memory, queue_used_ring(queue_index), 0),
            (0, 0)
        );
    }

    #[test]
    fn hinting_queue_discard_failure_preserves_active_range_completion() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        let command = 17_u32;
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::readable(
                TEST_PFN_DATA,
                VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32,
                Some(1),
            ),
        );
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            1,
            TestDescriptor::readable(
                TEST_PFN_DATA_SPLIT,
                u32::try_from(VIRTIO_BALLOON_PAGE_SIZE).expect("page size should fit u32"),
                None,
            ),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
        let mut queue = hinting_queue(queue_index);
        let mut adviser =
            TestBalloonDiscardAdviser::new(VIRTIO_BALLOON_PAGE_SIZE).with_free_failure();

        let dispatch = queue
            .dispatch_hinting_commands_with_adviser(
                &mut memory,
                hinting_context(command, None),
                &mut adviser,
            )
            .expect("advice failure should not fail active hint dispatch");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert_eq!(dispatch.hinting_guest_cmd(), Some(command));
        assert_eq!(
            dispatch.hinting_page_ranges(),
            &[
                GuestMemoryRange::new(TEST_PFN_DATA_SPLIT, VIRTIO_BALLOON_PAGE_SIZE)
                    .expect("test range should be valid")
            ]
        );
        assert_eq!(
            dispatch.hinting_discard(),
            VirtioBalloonDiscardOutcome {
                attempts: 1,
                requested_bytes: VIRTIO_BALLOON_PAGE_SIZE,
                advised_bytes: 0,
                skipped_bytes: 0,
                failed_bytes: VIRTIO_BALLOON_PAGE_SIZE,
                failures: 1,
            }
        );
        assert_eq!(adviser.zero_calls, 1);
        assert_eq!(adviser.free_calls, 1);
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
    }

    #[test]
    fn hinting_queue_dispatch_records_range_from_context_guest_command() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        let command = 9_u32;
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &[0xbb; 64]);
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, 64, None),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
        let mut queue = hinting_queue(queue_index);

        let dispatch = queue
            .dispatch_hinting_commands(&mut memory, hinting_context(command, Some(command)))
            .expect("hinting range with matching context command should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert_eq!(dispatch.hinting_guest_cmd(), None);
        assert_eq!(
            dispatch.hinting_page_ranges(),
            &[GuestMemoryRange::new(TEST_PFN_DATA_SPLIT, 64).expect("test range should be valid")]
        );
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
        assert_eq!(
            read_used_element(&memory, queue_used_ring(queue_index), 0),
            (0, 0)
        );
    }

    #[test]
    fn hinting_queue_dispatch_records_range_after_prior_command_head() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        let command = 11_u32;
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &[0xbc; 64]);
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32, None),
        );
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            1,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, 64, None),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0, 1]);
        let mut queue = hinting_queue(queue_index);

        let dispatch = queue
            .dispatch_hinting_commands(&mut memory, hinting_context(command, None))
            .expect("hinting range after matching command head should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 2);
        assert_eq!(dispatch.hinting_guest_cmd(), Some(command));
        assert_eq!(
            dispatch.hinting_page_ranges(),
            &[GuestMemoryRange::new(TEST_PFN_DATA_SPLIT, 64).expect("test range should be valid")]
        );
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 2);
        assert_eq!(
            read_used_element(&memory, queue_used_ring(queue_index), 0),
            (0, 0)
        );
        assert_eq!(
            read_used_element(&memory, queue_used_ring(queue_index), 1),
            (1, 0)
        );
    }

    #[test]
    fn hinting_queue_dispatch_ignores_range_without_guest_command() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        let command = 7_u32;
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &[0xcc; 64]);
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, 64, None),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
        let mut queue = hinting_queue(queue_index);

        let dispatch = queue
            .dispatch_hinting_commands(&mut memory, hinting_context(command, None))
            .expect("hinting range without guest command should be ignored");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert!(dispatch.hinting_page_ranges().is_empty());
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
    }

    #[test]
    fn hinting_queue_dispatch_ignores_range_for_stale_guest_command() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        let command = 7_u32;
        let active_host_command = 8_u32;
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &[0xdd; 64]);
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::readable(
                TEST_PFN_DATA,
                VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32,
                Some(1),
            ),
        );
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            1,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, 64, None),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
        let mut queue = hinting_queue(queue_index);

        let dispatch = queue
            .dispatch_hinting_commands(&mut memory, hinting_context(active_host_command, None))
            .expect("stale hinting range should be ignored");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert_eq!(dispatch.hinting_guest_cmd(), Some(command));
        assert!(dispatch.hinting_page_ranges().is_empty());
        assert_eq!(dispatch.hinting_discard().attempts(), 0);
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
    }

    #[test]
    fn hinting_queue_dispatch_ignores_range_when_host_command_is_inactive() {
        for command in [
            VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
            VIRTIO_BALLOON_FREE_PAGE_HINT_DONE,
        ] {
            let mut memory = pfn_descriptor_memory();
            let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
            write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
            write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &[0xee; 64]);
            write_hinting_descriptor(
                &mut memory,
                queue_index,
                0,
                TestDescriptor::readable(
                    TEST_PFN_DATA,
                    VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32,
                    Some(1),
                ),
            );
            write_hinting_descriptor(
                &mut memory,
                queue_index,
                1,
                TestDescriptor::readable(TEST_PFN_DATA_SPLIT, 64, None),
            );
            write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
            let mut queue = hinting_queue(queue_index);

            let dispatch = queue
                .dispatch_hinting_commands(&mut memory, hinting_context(command, Some(command)))
                .expect("inactive host command should ignore hinting range");

            assert_eq!(dispatch.completed_descriptors(), 1);
            assert_eq!(dispatch.hinting_guest_cmd(), Some(command));
            assert!(dispatch.hinting_completed_run());
            assert!(dispatch.hinting_page_ranges().is_empty());
            assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
        }
    }

    #[test]
    fn hinting_queue_dispatch_range_error_preserves_completed_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        let command = 7_u32;
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &[0xff; 64]);
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::readable(
                TEST_PFN_DATA,
                VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32,
                Some(1),
            ),
        );
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            1,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, 64, None),
        );
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            2,
            TestDescriptor::readable(GuestAddress::new(TEST_MEMORY_SIZE), 64, None),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0, 2]);
        let mut queue = hinting_queue(queue_index);

        let error = queue
            .dispatch_hinting_commands(&mut memory, hinting_context(command, None))
            .expect_err("unmapped hinting range should fail");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::HintingRange {
                descriptor_head: 2,
                descriptor_index: 2,
                ..
            }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 1);
        assert_eq!(
            error.completed_dispatch().hinting_page_ranges(),
            &[GuestMemoryRange::new(TEST_PFN_DATA_SPLIT, 64).expect("test range should be valid")]
        );
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
        assert_eq!(
            read_used_element(&memory, queue_used_ring(queue_index), 0),
            (0, 0)
        );
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn hinting_queue_dispatch_read_error_preserves_completed_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        let command = 13_u32;
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32, None),
        );
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            1,
            TestDescriptor::readable(
                GuestAddress::new(TEST_MEMORY_SIZE),
                VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32,
                None,
            ),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0, 1]);
        let mut queue = hinting_queue(queue_index);

        let error = queue
            .dispatch_hinting_commands(&mut memory, default_hinting_context())
            .expect_err("unmapped hinting command should fail");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::HintingCommandRead {
                descriptor_head: 1,
                descriptor_index: 1,
                ..
            }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 1);
        assert_eq!(
            error.completed_dispatch().hinting_guest_cmd(),
            Some(command)
        );
        assert!(!error.completed_dispatch().hinting_completed_run());
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
        assert_eq!(
            read_used_element(&memory, queue_used_ring(queue_index), 0),
            (0, 0)
        );
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn hinting_queue_dispatch_read_error_preserves_completed_run() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        let command = VIRTIO_BALLOON_FREE_PAGE_HINT_STOP;
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32, None),
        );
        write_hinting_descriptor(
            &mut memory,
            queue_index,
            1,
            TestDescriptor::readable(
                GuestAddress::new(TEST_MEMORY_SIZE),
                VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32,
                None,
            ),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0, 1]);
        let mut queue = hinting_queue(queue_index);

        let error = queue
            .dispatch_hinting_commands(&mut memory, default_hinting_context())
            .expect_err("unmapped hinting command should fail after completed stop");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::HintingCommandRead {
                descriptor_head: 1,
                descriptor_index: 1,
                ..
            }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 1);
        assert_eq!(
            error.completed_dispatch().hinting_guest_cmd(),
            Some(command)
        );
        assert!(error.completed_dispatch().hinting_completed_run());
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
    }

    #[test]
    fn reporting_range_requires_device_writable_nonempty_range() {
        let mut memory = pfn_descriptor_memory();
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, 4096, None),
        );
        let readable = descriptor_chain(&memory, 0).descriptors()[0];
        let error = balloon_reporting_range(readable)
            .expect_err("device-readable reporting range should fail");
        assert_eq!(
            error,
            VirtioBalloonReportingRangeError::DescriptorReadable { index: 0 }
        );
        assert_eq!(
            error.to_string(),
            "virtio-balloon free-page reporting descriptor 0 is device-readable"
        );

        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(TEST_PFN_DATA, 0, None),
        );
        let empty = descriptor_chain(&memory, 0).descriptors()[0];
        assert_eq!(
            balloon_reporting_range(empty).expect_err("empty reporting range should fail"),
            VirtioBalloonReportingRangeError::DescriptorEmpty { index: 0 }
        );

        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(GuestAddress::new(u64::MAX - 1), 4, None),
        );
        let overflowing = descriptor_chain(&memory, 0).descriptors()[0];
        let error = balloon_reporting_range(overflowing)
            .expect_err("overflowing reporting range should fail");
        assert!(matches!(
            error,
            VirtioBalloonReportingRangeError::Range {
                index: 0,
                address,
                len: 4,
                source: GuestMemoryError::AddressOverflow { .. },
            } if address == GuestAddress::new(u64::MAX - 1)
        ));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn reporting_queue_dispatches_scatter_gather_chain() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        write_reporting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::writable(TEST_PFN_DATA, 4096, Some(1)),
        );
        write_reporting_descriptor(
            &mut memory,
            queue_index,
            1,
            TestDescriptor::writable(TEST_PFN_DATA_SPLIT, 4096, None),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
        let mut queue = reporting_queue(queue_index);
        let mut adviser = TestBalloonDiscardAdviser::new(4096);

        let dispatch = queue
            .dispatch_free_page_reporting_with_adviser(&mut memory, &mut adviser)
            .expect("scatter-gather reporting chain should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(
            dispatch.reporting_discard(),
            VirtioBalloonDiscardOutcome {
                attempts: 2,
                requested_bytes: 8192,
                advised_bytes: 8192,
                skipped_bytes: 0,
                failed_bytes: 0,
                failures: 0,
            }
        );
        assert_eq!(adviser.zero_calls, 2);
        assert_eq!(adviser.free_calls, 2);
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
        assert_eq!(
            read_used_element(&memory, queue_used_ring(queue_index), 0),
            (0, 0)
        );
    }

    #[test]
    fn reporting_queue_dispatches_multiple_chains() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        for (index, address) in [(0, TEST_PFN_DATA), (1, TEST_PFN_DATA_SPLIT)] {
            write_reporting_descriptor(
                &mut memory,
                queue_index,
                index,
                TestDescriptor::writable(address, 4096, None),
            );
        }
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0, 1]);
        let mut queue = reporting_queue(queue_index);
        let mut adviser = TestBalloonDiscardAdviser::new(4096);

        let dispatch = queue
            .dispatch_free_page_reporting_with_adviser(&mut memory, &mut adviser)
            .expect("multiple reporting chains should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 2);
        assert_eq!(dispatch.reporting_discard().attempts(), 2);
        assert_eq!(dispatch.reporting_discard().advised_bytes(), 8192);
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 2);
        assert_eq!(
            read_used_element(&memory, queue_used_ring(queue_index), 0),
            (0, 0)
        );
        assert_eq!(
            read_used_element(&memory, queue_used_ring(queue_index), 1),
            (1, 0)
        );
    }

    #[test]
    fn reporting_queue_semantic_failures_do_not_block_valid_descriptor() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        write_reporting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, 4096, Some(1)),
        );
        write_reporting_descriptor(
            &mut memory,
            queue_index,
            1,
            TestDescriptor::writable(TEST_PFN_DATA, 0, Some(2)),
        );
        write_reporting_descriptor(
            &mut memory,
            queue_index,
            2,
            TestDescriptor::writable(GuestAddress::new(TEST_MEMORY_SIZE), 4096, Some(3)),
        );
        write_reporting_descriptor(
            &mut memory,
            queue_index,
            3,
            TestDescriptor::writable(TEST_PFN_DATA_SPLIT, 4096, None),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
        let mut queue = reporting_queue(queue_index);
        let mut adviser = TestBalloonDiscardAdviser::new(4096);

        let dispatch = queue
            .dispatch_free_page_reporting_with_adviser(&mut memory, &mut adviser)
            .expect("semantic reporting failures should remain best effort");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert_eq!(
            dispatch.reporting_discard(),
            VirtioBalloonDiscardOutcome {
                attempts: 4,
                requested_bytes: 12_288,
                advised_bytes: 4096,
                skipped_bytes: 0,
                failed_bytes: 8192,
                failures: 3,
            }
        );
        assert_eq!(adviser.zero_calls, 1);
        assert_eq!(adviser.free_calls, 1);
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
    }

    #[test]
    fn reporting_queue_overflow_is_failed_attempt_and_completes_chain() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        write_reporting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::writable(GuestAddress::new(u64::MAX - 1), 4, None),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
        let mut queue = reporting_queue(queue_index);
        let mut adviser = TestBalloonDiscardAdviser::new(4096);

        let dispatch = queue
            .dispatch_free_page_reporting_with_adviser(&mut memory, &mut adviser)
            .expect("overflowing range should be completed as failed attempt");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert_eq!(
            dispatch.reporting_discard(),
            VirtioBalloonDiscardOutcome {
                attempts: 1,
                requested_bytes: 4,
                advised_bytes: 0,
                skipped_bytes: 0,
                failed_bytes: 4,
                failures: 1,
            }
        );
        assert_eq!(adviser.zero_calls, 0);
        assert_eq!(adviser.free_calls, 0);
    }

    #[test]
    fn reporting_queue_platform_failure_does_not_block_later_range() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        write_reporting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::writable(TEST_PFN_DATA, 4096, Some(1)),
        );
        write_reporting_descriptor(
            &mut memory,
            queue_index,
            1,
            TestDescriptor::writable(TEST_PFN_DATA_SPLIT, 4096, None),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
        let mut queue = reporting_queue(queue_index);
        let mut adviser = TestBalloonDiscardAdviser::new(4096).with_zero_failure();

        let dispatch = queue
            .dispatch_free_page_reporting_with_adviser(&mut memory, &mut adviser)
            .expect("platform failure should not stop later reporting range");

        assert_eq!(
            dispatch.reporting_discard(),
            VirtioBalloonDiscardOutcome {
                attempts: 2,
                requested_bytes: 8192,
                advised_bytes: 4096,
                skipped_bytes: 0,
                failed_bytes: 4096,
                failures: 1,
            }
        );
        assert_eq!(adviser.zero_calls, 2);
        assert_eq!(adviser.free_calls, 1);
    }

    #[test]
    fn reporting_queue_skips_four_kibibytes_inside_sixteen_kibibytes() {
        const FOUR_KIB: u64 = 4096;
        const SIXTEEN_KIB: u64 = 16 * 1024;

        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        let pattern = vec![0xa5; usize::try_from(SIXTEEN_KIB).expect("size should fit")];
        write_guest_bytes(&mut memory, GuestAddress::new(0), &pattern);
        write_reporting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::writable(
                GuestAddress::new(FOUR_KIB),
                u32::try_from(FOUR_KIB).expect("descriptor length should fit"),
                None,
            ),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
        let mut original = vec![0; pattern.len()];
        memory
            .read_slice(&mut original, GuestAddress::new(0))
            .expect("pre-dispatch memory should read");
        let mut queue = reporting_queue(queue_index);
        let mut adviser = TestBalloonDiscardAdviser::new(SIXTEEN_KIB);

        let dispatch = queue
            .dispatch_free_page_reporting_with_adviser(&mut memory, &mut adviser)
            .expect("sub-host-page reporting range should complete safely");

        let mut observed = vec![0; original.len()];
        memory
            .read_slice(&mut observed, GuestAddress::new(0))
            .expect("post-dispatch memory should read");
        assert_eq!(
            dispatch.reporting_discard(),
            VirtioBalloonDiscardOutcome {
                attempts: 1,
                requested_bytes: FOUR_KIB,
                advised_bytes: 0,
                skipped_bytes: FOUR_KIB,
                failed_bytes: 0,
                failures: 0,
            }
        );
        assert_eq!(adviser.zero_calls, 0);
        assert_eq!(adviser.free_calls, 0);
        assert_eq!(observed, original);
    }

    #[test]
    fn reporting_queue_used_ring_failure_preserves_prior_discard() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        write_reporting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::writable(TEST_PFN_DATA, 4096, None),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
        let mut queue =
            reporting_queue_with_used_ring(queue_index, GuestAddress::new(TEST_MEMORY_SIZE));
        let mut adviser = TestBalloonDiscardAdviser::new(4096);

        let error = queue
            .dispatch_free_page_reporting_with_adviser(&mut memory, &mut adviser)
            .expect_err("unmapped used ring should fail after reporting discard");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::UsedRing {
                queue: VirtioBalloonQueueKind::FreePageReporting,
                descriptor_head: 0,
                ..
            }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 0);
        assert_eq!(
            error.completed_dispatch().reporting_discard(),
            VirtioBalloonDiscardOutcome {
                attempts: 1,
                requested_bytes: 4096,
                advised_bytes: 4096,
                skipped_bytes: 0,
                failed_bytes: 0,
                failures: 0,
            }
        );
        assert_eq!(adviser.zero_calls, 1);
        assert_eq!(adviser.free_calls, 1);
    }

    #[test]
    fn reporting_queue_later_available_error_preserves_completed_work() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        write_reporting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::writable(TEST_PFN_DATA, 4096, None),
        );
        write_available_heads(
            &mut memory,
            queue_available_ring(queue_index),
            &[0, TEST_QUEUE_SIZE],
        );
        let mut queue = reporting_queue(queue_index);
        let mut adviser = TestBalloonDiscardAdviser::new(4096);

        let error = queue
            .dispatch_free_page_reporting_with_adviser(&mut memory, &mut adviser)
            .expect_err("invalid later head should preserve completed reporting work");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::AvailableRing {
                queue: VirtioBalloonQueueKind::FreePageReporting,
                ..
            }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 1);
        assert_eq!(error.completed_dispatch().reporting_discard().attempts(), 1);
        assert_eq!(
            error
                .completed_dispatch()
                .reporting_discard()
                .advised_bytes(),
            4096
        );
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
    }

    #[test]
    fn optional_queue_metadata_is_deterministic_and_bounded() {
        let device = prepared(balloon_config(64, false, 1, true, true));
        let queues: Vec<_> = device.queue_layout().iter().collect();

        assert_eq!(queues.len(), VIRTIO_BALLOON_MAX_QUEUE_COUNT);
        assert_eq!(
            queues,
            vec![
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::Inflate,
                    VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::Deflate,
                    VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::Statistics,
                    VIRTIO_BALLOON_STATS_QUEUE_INDEX,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::FreePageHinting,
                    VIRTIO_BALLOON_STATS_QUEUE_INDEX + 1,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::FreePageReporting,
                    VIRTIO_BALLOON_STATS_QUEUE_INDEX + 2,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
            ]
        );
    }

    #[test]
    fn optional_queues_compact_when_statistics_queue_is_disabled() {
        let device = prepared(balloon_config(64, false, 0, true, true));
        let queues: Vec<_> = device.queue_layout().iter().collect();

        assert_eq!(queues.len(), VIRTIO_BALLOON_MIN_QUEUE_COUNT + 2);
        assert_eq!(queues[2].kind(), VirtioBalloonQueueKind::FreePageHinting);
        assert_eq!(queues[2].index(), VIRTIO_BALLOON_STATS_QUEUE_INDEX);
        assert_eq!(queues[3].kind(), VirtioBalloonQueueKind::FreePageReporting);
        assert_eq!(queues[3].index(), VIRTIO_BALLOON_STATS_QUEUE_INDEX + 1);
    }

    #[test]
    fn reporting_queue_compacts_when_hinting_and_statistics_are_disabled() {
        let device = prepared(balloon_config(64, false, 0, false, true));
        let queues: Vec<_> = device.queue_layout().iter().collect();

        assert_eq!(queues.len(), VIRTIO_BALLOON_MIN_QUEUE_COUNT + 1);
        assert_eq!(queues[2].kind(), VirtioBalloonQueueKind::FreePageReporting);
        assert_eq!(queues[2].index(), VIRTIO_BALLOON_STATS_QUEUE_INDEX);
    }

    #[test]
    fn sparse_optional_queue_combinations_keep_expected_indexes() {
        let stats_only = prepared(balloon_config(64, false, 1, false, false)).queue_layout();
        assert_eq!(stats_only.queue_count(), VIRTIO_BALLOON_MIN_QUEUE_COUNT + 1);
        assert_eq!(
            stats_only.statistics(),
            Some(VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::Statistics,
                VIRTIO_BALLOON_STATS_QUEUE_INDEX,
                VIRTIO_BALLOON_QUEUE_SIZE,
            ))
        );
        assert_eq!(stats_only.free_page_hinting(), None);
        assert_eq!(stats_only.free_page_reporting(), None);

        let hinting_only = prepared(balloon_config(64, false, 0, true, false)).queue_layout();
        assert_eq!(
            hinting_only.queue_count(),
            VIRTIO_BALLOON_MIN_QUEUE_COUNT + 1
        );
        assert_eq!(hinting_only.statistics(), None);
        assert_eq!(
            hinting_only.free_page_hinting(),
            Some(VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::FreePageHinting,
                VIRTIO_BALLOON_STATS_QUEUE_INDEX,
                VIRTIO_BALLOON_QUEUE_SIZE,
            ))
        );
        assert_eq!(hinting_only.free_page_reporting(), None);

        let stats_reporting = prepared(balloon_config(64, false, 1, false, true)).queue_layout();
        assert_eq!(
            stats_reporting.queue_count(),
            VIRTIO_BALLOON_MIN_QUEUE_COUNT + 2
        );
        assert_eq!(
            stats_reporting.statistics(),
            Some(VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::Statistics,
                VIRTIO_BALLOON_STATS_QUEUE_INDEX,
                VIRTIO_BALLOON_QUEUE_SIZE,
            ))
        );
        assert_eq!(stats_reporting.free_page_hinting(), None);
        assert_eq!(
            stats_reporting.free_page_reporting(),
            Some(VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::FreePageReporting,
                VIRTIO_BALLOON_STATS_QUEUE_INDEX + 1,
                VIRTIO_BALLOON_QUEUE_SIZE,
            ))
        );
    }

    #[test]
    fn queue_sizes_follow_compacted_queue_layout() {
        let base = prepared(balloon_config(64, false, 0, false, false)).queue_sizes();
        assert_eq!(base.as_slice(), &[VIRTIO_BALLOON_QUEUE_SIZE; 2]);

        let all_enabled = prepared(balloon_config(64, false, 1, true, true)).queue_sizes();
        assert_eq!(all_enabled.len(), VIRTIO_BALLOON_MAX_QUEUE_COUNT);
        assert_eq!(all_enabled.as_slice(), &[VIRTIO_BALLOON_QUEUE_SIZE; 5]);

        let reporting_only = prepared(balloon_config(64, false, 0, false, true)).queue_sizes();
        assert_eq!(
            reporting_only.as_slice(),
            &[VIRTIO_BALLOON_QUEUE_SIZE; VIRTIO_BALLOON_MIN_QUEUE_COUNT + 1]
        );
    }

    #[test]
    fn balloon_device_activation_builds_base_queues_from_mmio_state() {
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);

        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("balloon device should activate");

        let active = device
            .active_queues()
            .expect("balloon device should expose active queues");
        assert_eq!(active.queue_count(), VIRTIO_BALLOON_MIN_QUEUE_COUNT);
        assert_eq!(
            active.inflate().available_ring().descriptor_table(),
            queue_address(
                TEST_DESCRIPTOR_BASE,
                queue_index_u32(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX)
            )
        );
        assert_eq!(
            active.deflate().used_ring().used_ring(),
            queue_address(
                TEST_DEVICE_BASE,
                queue_index_u32(VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX)
            )
        );
        assert_eq!(
            active.inflate().available_ring().queue_size(),
            TEST_QUEUE_SIZE
        );
        assert!(active.statistics().is_none());
        assert!(active.free_page_hinting().is_none());
        assert!(active.free_page_reporting().is_none());
    }

    #[test]
    fn balloon_device_activation_builds_optional_queues_from_mmio_state() {
        let layout = prepared(balloon_config(64, false, 1, true, true)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);

        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("balloon device should activate optional queues");

        let active = device
            .active_queues()
            .expect("balloon device should expose active queues");
        assert_eq!(active.queue_count(), VIRTIO_BALLOON_MAX_QUEUE_COUNT);
        assert!(active.statistics().is_some());
        assert!(active.free_page_hinting().is_some());
        assert!(active.free_page_reporting().is_some());
    }

    #[test]
    fn balloon_device_hinting_status_reports_initial_state_before_activation() {
        let layout = prepared(balloon_config(64, false, 0, true, false)).queue_layout();
        let device = VirtioBalloonDevice::new(layout);

        let status = device
            .hinting_status()
            .expect("hinting-enabled device should report initial status");

        assert_eq!(status.host_cmd(), VIRTIO_BALLOON_FREE_PAGE_HINT_STOP);
        assert_eq!(status.guest_cmd(), None);
    }

    #[test]
    fn balloon_device_hinting_status_rejects_disabled_hinting() {
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device = VirtioBalloonDevice::new(layout);

        let err = device
            .hinting_status()
            .expect_err("hinting-disabled device should reject status");

        assert_eq!(err, BalloonHintingStatusError::HintingNotEnabled);
    }

    #[test]
    fn balloon_device_hinting_start_stop_updates_host_command_state() {
        let layout = prepared(balloon_config(64, false, 0, true, false)).queue_layout();
        let mut device = VirtioBalloonDevice::new(layout);

        let first = device
            .start_hinting(BalloonHintingStartInput::new(false))
            .expect("hinting start should assign command id");
        assert_eq!(first, VIRTIO_BALLOON_FREE_PAGE_HINT_DONE + 1);
        assert!(
            !device
                .hinting_acknowledge_on_stop()
                .expect("hinting ack setting should read")
        );
        assert_eq!(
            device
                .hinting_status()
                .expect("hinting status should read after start")
                .host_cmd(),
            first
        );

        let second = device
            .start_hinting(BalloonHintingStartInput::new(true))
            .expect("second hinting start should assign command id");
        assert_eq!(second, first + 1);
        assert!(
            device
                .hinting_acknowledge_on_stop()
                .expect("hinting ack setting should read")
        );

        let stopped = device
            .stop_hinting()
            .expect("hinting stop should set done command");
        assert_eq!(stopped, VIRTIO_BALLOON_FREE_PAGE_HINT_DONE);
        assert_eq!(
            device
                .hinting_status()
                .expect("hinting status should read after stop")
                .host_cmd(),
            VIRTIO_BALLOON_FREE_PAGE_HINT_DONE
        );

        let third = device
            .start_hinting(BalloonHintingStartInput::new(false))
            .expect("hinting start after stop should continue command ids");
        assert_eq!(third, second + 1);
    }

    #[test]
    fn balloon_device_hinting_start_skips_reserved_ids_after_wrap() {
        let layout = prepared(balloon_config(64, false, 0, true, false)).queue_layout();
        let mut device = VirtioBalloonDevice::new(layout);
        device.hinting_last_cmd = u32::MAX;

        let cmd = device
            .start_hinting(BalloonHintingStartInput::new(true))
            .expect("wrapped hinting command id should skip reserved values");

        assert_eq!(cmd, VIRTIO_BALLOON_FREE_PAGE_HINT_DONE + 1);
    }

    #[test]
    fn balloon_device_hinting_start_stop_rejects_disabled_hinting() {
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let mut device = VirtioBalloonDevice::new(layout);

        assert_eq!(
            device
                .start_hinting(BalloonHintingStartInput::new(true))
                .expect_err("hinting start should require hinting support"),
            BalloonHintingCommandError::HintingNotEnabled
        );
        assert_eq!(
            device
                .stop_hinting()
                .expect_err("hinting stop should require hinting support"),
            BalloonHintingCommandError::HintingNotEnabled
        );
    }

    #[test]
    fn balloon_device_activation_rejects_queue_count_mismatch() {
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(1);
        let mut device = VirtioBalloonDevice::new(layout);

        let error = device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect_err("queue count mismatch should fail activation");

        assert!(matches!(
            error,
            VirtioBalloonDeviceActivationError::QueueCountMismatch {
                expected: VIRTIO_BALLOON_MIN_QUEUE_COUNT,
                actual: 1
            }
        ));
        assert_eq!(
            error.to_string(),
            "virtio-balloon device requires 2 queue(s), got 1"
        );
        assert!(std::error::Error::source(&error).is_none());
        assert!(!device.is_activated());
    }

    #[test]
    fn balloon_device_activation_rejects_not_ready_queue() {
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queue_sizes = vec![VIRTIO_BALLOON_QUEUE_SIZE; VIRTIO_BALLOON_MIN_QUEUE_COUNT];
        let mut queues =
            VirtioMmioQueueRegisters::new(&queue_sizes).expect("queue table should build");
        configure_queue_registers(
            &mut queues,
            queue_index_u32(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX),
        );
        let mut device = VirtioBalloonDevice::new(layout);

        let error = device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect_err("not-ready deflate queue should fail activation");

        assert!(matches!(
            error,
            VirtioBalloonDeviceActivationError::QueueBuild {
                queue_index: 1,
                kind: VirtioBalloonQueueKind::Deflate,
                source: VirtioBalloonQueueBuildError::QueueNotReady
            }
        ));
        assert_eq!(
            error.to_string(),
            "failed to activate virtio-balloon deflate queue 1: virtio-balloon queue is not ready"
        );
        assert!(std::error::Error::source(&error).is_some());
        assert!(!device.is_activated());
    }

    #[test]
    fn balloon_device_activation_rejects_duplicate_activation() {
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let activation = activation_for_queues(&device_registers, &queues);
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation)
            .expect("first activation should succeed");

        let error = device
            .activate_balloon(activation)
            .expect_err("second activation should fail");

        assert!(matches!(
            error,
            VirtioBalloonDeviceActivationError::AlreadyActive
        ));
        assert_eq!(error.to_string(), "virtio-balloon device is already active");
        assert!(device.is_activated());
    }

    #[test]
    fn balloon_device_reset_clears_active_queues() {
        let layout = prepared(balloon_config(64, false, 0, false, true)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");
        device
            .memory_accounting
            .add_inflated_ranges(&[VirtioBalloonPfnRange::new(100, 2)])
            .expect("test accounting range should add");
        assert!(
            device
                .active_queues()
                .and_then(VirtioBalloonActiveQueues::free_page_reporting)
                .is_some()
        );
        assert!(!device.memory_accounting().is_empty());

        device.reset();

        assert!(!device.is_activated());
        assert!(device.active_queues().is_none());
        assert!(device.memory_accounting().is_empty());
    }

    #[test]
    fn balloon_devices_keep_memory_accounting_independent() {
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let mut first = VirtioBalloonDevice::new(layout);
        let second = VirtioBalloonDevice::new(layout);

        first
            .memory_accounting
            .add_inflated_ranges(&[VirtioBalloonPfnRange::new(100, 2)])
            .expect("test accounting range should add");

        assert_eq!(
            first.memory_accounting().inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 100,
                page_count: 2,
            }]
        );
        assert!(second.memory_accounting().is_empty());
    }

    #[test]
    fn balloon_notification_dispatch_accepts_empty_batch_before_activation() {
        let mut memory = pfn_descriptor_memory();
        let mut device = VirtioBalloonDevice::new(
            prepared(balloon_config(64, false, 0, false, false)).queue_layout(),
        );

        let dispatch = device
            .dispatch_drained_queue_notifications(&mut memory, Vec::new())
            .expect("empty notification batch should dispatch");

        assert!(dispatch.drained_notifications().is_empty());
        assert_eq!(dispatch.inflate_notifications(), 0);
        assert_eq!(dispatch.deflate_notifications(), 0);
        assert_eq!(dispatch.hinting_notifications(), 0);
        assert!(dispatch.inflate_queue_dispatch().is_none());
        assert!(dispatch.deflate_queue_dispatch().is_none());
        assert!(dispatch.hinting_queue_dispatch().is_none());
        assert!(!dispatch.needs_queue_interrupt());
    }

    #[test]
    fn balloon_notification_dispatch_rejects_supported_queue_before_activation() {
        let mut memory = pfn_descriptor_memory();
        let mut device = VirtioBalloonDevice::new(
            prepared(balloon_config(64, false, 0, false, false)).queue_layout(),
        );

        let error = device
            .dispatch_drained_queue_notifications(
                &mut memory,
                vec![VIRTIO_BALLOON_INFLATE_QUEUE_INDEX],
            )
            .expect_err("inactive device should reject supported queue notification");

        assert!(matches!(
            error,
            VirtioBalloonDeviceNotificationError::Inactive { .. }
        ));
        assert_eq!(
            error.drained_notifications(),
            &[VIRTIO_BALLOON_INFLATE_QUEUE_INDEX]
        );
        assert_eq!(
            error.to_string(),
            "virtio-balloon queue notification cannot be dispatched before activation"
        );
    }

    #[test]
    fn balloon_notification_dispatch_routes_compacted_reporting_queue_with_hinting() {
        let mut memory = pfn_descriptor_memory();
        let layout = prepared(balloon_config(64, false, 1, true, true)).queue_layout();
        let queue_index = layout
            .free_page_reporting()
            .expect("reporting queue should be configured")
            .index();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");

        let dispatch = device
            .dispatch_drained_queue_notifications(&mut memory, vec![queue_index])
            .expect("reporting queue should dispatch");

        assert_eq!(dispatch.drained_notifications(), &[queue_index]);
        assert_eq!(dispatch.reporting_notifications(), 1);
        assert_eq!(dispatch.hinting_notifications(), 0);
        assert_eq!(
            dispatch
                .reporting_queue_dispatch()
                .expect("reporting queue result should be retained")
                .completed_descriptors(),
            0
        );
        assert!(!dispatch.needs_queue_interrupt());
    }

    #[test]
    fn balloon_notification_dispatch_updates_hinting_guest_command_state() {
        let mut memory = pfn_descriptor_memory();
        let command = 55_u32;
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
        let layout = prepared(balloon_config(64, false, 0, true, false)).queue_layout();
        let hinting_queue_index = layout
            .free_page_hinting()
            .expect("hinting queue should be configured")
            .index();
        write_hinting_descriptor(
            &mut memory,
            hinting_queue_index,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32, None),
        );
        write_available_heads(&mut memory, queue_available_ring(hinting_queue_index), &[0]);
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");

        let dispatch = device
            .dispatch_drained_queue_notifications(&mut memory, vec![hinting_queue_index])
            .expect("hinting queue notification should dispatch");

        assert_eq!(dispatch.hinting_notifications(), 1);
        let hinting_dispatch = dispatch
            .hinting_queue_dispatch()
            .expect("hinting queue dispatch should be present");
        assert_eq!(hinting_dispatch.completed_descriptors(), 1);
        assert!(hinting_dispatch.needs_queue_interrupt());
        assert_eq!(hinting_dispatch.hinting_guest_cmd(), Some(command));
        assert!(!hinting_dispatch.hinting_completed_run());
        assert!(!dispatch.hinting_completed_run());
        assert_eq!(
            device
                .hinting_status()
                .expect("hinting status should read")
                .guest_cmd(),
            Some(command)
        );
        assert_eq!(
            read_used_idx(&memory, queue_used_ring(hinting_queue_index)),
            1
        );
        assert_eq!(
            read_used_element(&memory, queue_used_ring(hinting_queue_index), 0),
            (0, 0)
        );
    }

    #[test]
    fn balloon_notification_dispatch_records_completed_hinting_run() {
        let mut memory = pfn_descriptor_memory();
        let command = VIRTIO_BALLOON_FREE_PAGE_HINT_STOP;
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
        let layout = prepared(balloon_config(64, false, 0, true, false)).queue_layout();
        let hinting_queue_index = layout
            .free_page_hinting()
            .expect("hinting queue should be configured")
            .index();
        write_hinting_descriptor(
            &mut memory,
            hinting_queue_index,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32, None),
        );
        write_available_heads(&mut memory, queue_available_ring(hinting_queue_index), &[0]);
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        let host_cmd = device
            .start_hinting(BalloonHintingStartInput::new(true))
            .expect("hinting start should assign host command");
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");

        let dispatch = device
            .dispatch_drained_queue_notifications(&mut memory, vec![hinting_queue_index])
            .expect("completed hinting queue notification should dispatch");

        let hinting_dispatch = dispatch
            .hinting_queue_dispatch()
            .expect("hinting queue should dispatch");
        assert_eq!(hinting_dispatch.hinting_guest_cmd(), Some(command));
        assert!(hinting_dispatch.hinting_completed_run());
        assert!(dispatch.hinting_completed_run());
        let status = device.hinting_status().expect("hinting status should read");
        assert_eq!(status.guest_cmd(), Some(command));
        assert_eq!(status.host_cmd(), host_cmd);
    }

    #[test]
    fn balloon_notification_dispatch_preserves_hinting_guest_command_on_malformed_command() {
        let mut memory = pfn_descriptor_memory();
        let layout = prepared(balloon_config(64, false, 0, true, false)).queue_layout();
        let hinting_queue_index = layout
            .free_page_hinting()
            .expect("hinting queue should be configured")
            .index();
        write_hinting_descriptor(
            &mut memory,
            hinting_queue_index,
            0,
            TestDescriptor::readable(
                GuestAddress::new(TEST_MEMORY_SIZE),
                VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32,
                None,
            ),
        );
        write_available_heads(&mut memory, queue_available_ring(hinting_queue_index), &[0]);
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");
        device.hinting_guest_cmd = Some(11);

        let error = device
            .dispatch_drained_queue_notifications(&mut memory, vec![hinting_queue_index])
            .expect_err("malformed hinting command should fail dispatch");

        assert!(matches!(
            error,
            VirtioBalloonDeviceNotificationError::QueueDispatch {
                source: VirtioBalloonQueueDispatchError::HintingCommandRead { .. },
                ..
            }
        ));
        assert_eq!(
            device
                .hinting_status()
                .expect("hinting status should read")
                .guest_cmd(),
            Some(11)
        );
        assert_eq!(
            read_used_idx(&memory, queue_used_ring(hinting_queue_index)),
            0
        );
    }

    #[test]
    fn balloon_notification_dispatch_preserves_completed_deflate_queue_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[70]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0, TEST_QUEUE_SIZE]);
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");

        let error = device
            .dispatch_drained_queue_notifications(
                &mut memory,
                vec![VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX],
            )
            .expect_err("invalid deflate queue head should fail dispatch");

        assert!(matches!(
            error,
            VirtioBalloonDeviceNotificationError::QueueDispatch { .. }
        ));
        assert_eq!(
            error.drained_notifications(),
            &[VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX]
        );
        let completed = error
            .completed_dispatch()
            .expect("queue dispatch error should expose completed work");
        assert_eq!(completed.completed_descriptors(), 1);
        assert!(completed.needs_queue_interrupt());
        assert_eq!(
            completed.deflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 70,
                page_count: 1,
            }]
        );
        assert!(device.memory_accounting().is_empty());
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, deflate_used_ring(), 0), (0, 0));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn balloon_notification_dispatch_rejects_unmapped_deflate_pfn_without_accounting() {
        let mut memory = pfn_descriptor_memory();
        let unmapped_pfn = first_unmapped_test_pfn();
        let bytes = pfn_payload_bytes(&[unmapped_pfn]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0]);
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");
        device
            .memory_accounting
            .add_inflated_ranges(&[VirtioBalloonPfnRange::new(10, 1)])
            .expect("test accounting range should add");

        let error = device
            .dispatch_drained_queue_notifications(
                &mut memory,
                vec![VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX],
            )
            .expect_err("unmapped deflate PFN should fail notification dispatch");

        assert!(matches!(
            &error,
            VirtioBalloonDeviceNotificationError::QueueDispatch {
                source:
                    VirtioBalloonQueueDispatchError::PfnRangeAccess {
                        queue: VirtioBalloonQueueKind::Deflate,
                        descriptor_head: 0,
                        source:
                            VirtioBalloonPfnRangeAccessError::GuestMemory {
                                pfn_range:
                                    VirtioBalloonPfnRange {
                                        start_pfn,
                                        page_count: 1
                                    },
                                source: GuestMemoryAccessError::UnmappedRange { .. },
                                ..
                            },
                        ..
                    },
                ..
            } if *start_pfn == unmapped_pfn
        ));
        assert!(
            error
                .completed_dispatch()
                .expect("queue dispatch error should expose completed work")
                .deflated_page_ranges()
                .is_empty()
        );
        assert_eq!(
            device.memory_accounting().inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 10,
                page_count: 1,
            }]
        );
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 0);
    }

    #[test]
    fn balloon_notification_dispatch_preserves_completed_inflate_when_deflate_fails() {
        let mut memory = pfn_descriptor_memory();
        let inflate_bytes = pfn_payload_bytes(&[30, 31]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &inflate_bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&inflate_bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        write_available_heads(&mut memory, deflate_available_ring(), &[TEST_QUEUE_SIZE]);
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");

        let error = device
            .dispatch_drained_queue_notifications(
                &mut memory,
                vec![
                    VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
                ],
            )
            .expect_err("invalid deflate queue head should fail after inflate dispatch");

        assert!(matches!(
            error,
            VirtioBalloonDeviceNotificationError::QueueDispatch { .. }
        ));
        let completed = error
            .completed_notification_dispatch()
            .expect("queue dispatch error should expose completed notification state");
        let inflate_dispatch = completed
            .inflate_queue_dispatch()
            .expect("inflate queue dispatch should be preserved");
        assert_eq!(inflate_dispatch.completed_descriptors(), 1);
        assert_eq!(
            inflate_dispatch.inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 30,
                page_count: 2,
            }]
        );
        assert!(completed.needs_queue_interrupt());
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 0);
        assert_eq!(
            device.memory_accounting().inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 30,
                page_count: 2,
            }]
        );
    }

    #[test]
    fn balloon_notification_dispatch_updates_memory_accounting_from_inflate_and_deflate() {
        let mut memory = pfn_descriptor_memory();
        let inflate_bytes = pfn_payload_bytes(&[80, 81, 83]);
        let deflate_bytes = pfn_payload_bytes(&[81]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &inflate_bytes);
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &deflate_bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&inflate_bytes), None),
        );
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, descriptor_len(&deflate_bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        write_available_heads(&mut memory, deflate_available_ring(), &[0]);
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");

        let dispatch = device
            .dispatch_drained_queue_notifications(
                &mut memory,
                vec![
                    VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
                ],
            )
            .expect("inflate and deflate notifications should dispatch");

        assert_eq!(dispatch.inflate_notifications(), 1);
        assert_eq!(dispatch.deflate_notifications(), 1);
        assert_eq!(
            dispatch
                .deflate_queue_dispatch()
                .expect("deflate dispatch should be present")
                .deflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 81,
                page_count: 1,
            }]
        );
        assert_eq!(
            device.memory_accounting().inflated_page_ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 80,
                    page_count: 1,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 83,
                    page_count: 1,
                }
            ]
        );
        assert_eq!(device.memory_accounting().inflated_page_count(), 2);
    }

    #[test]
    fn balloon_notification_dispatch_coalesces_duplicate_deflate_notifications() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[90]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0]);
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");

        let dispatch = device
            .dispatch_drained_queue_notifications(
                &mut memory,
                vec![
                    VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
                ],
            )
            .expect("duplicate deflate notifications should dispatch once");

        assert_eq!(dispatch.deflate_notifications(), 2);
        let deflate_dispatch = dispatch
            .deflate_queue_dispatch()
            .expect("deflate queue should dispatch");
        assert_eq!(deflate_dispatch.completed_descriptors(), 1);
        assert!(deflate_dispatch.needs_queue_interrupt());
        assert_eq!(
            deflate_dispatch.deflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 90,
                page_count: 1,
            }]
        );
        let metrics = SharedBalloonDeviceMetrics::default();
        metrics.record_notification_dispatch(&dispatch);
        assert_eq!(
            metrics.snapshot(),
            BalloonDeviceMetrics::new(0, 0, 0, 0, 2, 0)
        );
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, deflate_used_ring(), 0), (0, 0));
    }

    #[test]
    fn balloon_notification_dispatch_coalesces_duplicate_inflate_notifications() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[40, 42]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");

        let dispatch = device
            .dispatch_drained_queue_notifications(
                &mut memory,
                vec![
                    VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                ],
            )
            .expect("duplicate inflate notifications should dispatch once");

        assert_eq!(dispatch.inflate_notifications(), 2);
        let inflate_dispatch = dispatch
            .inflate_queue_dispatch()
            .expect("inflate queue should dispatch");
        assert_eq!(inflate_dispatch.completed_descriptors(), 1);
        assert!(inflate_dispatch.needs_queue_interrupt());
        assert_eq!(
            inflate_dispatch.inflated_page_ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 40,
                    page_count: 1,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 42,
                    page_count: 1,
                }
            ]
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
        assert_eq!(
            device.memory_accounting().inflated_page_ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 40,
                    page_count: 1,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 42,
                    page_count: 1,
                }
            ]
        );
    }

    #[test]
    fn balloon_metrics_record_distinct_discard_and_reporting_outcomes() {
        let dispatch = VirtioBalloonDeviceNotificationDispatch {
            drained_notifications: vec![
                VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                VIRTIO_BALLOON_STATS_QUEUE_INDEX,
            ],
            inflate_notifications: 1,
            deflate_notifications: 0,
            statistics_notifications: 0,
            hinting_notifications: 1,
            reporting_notifications: 1,
            inflate_queue_dispatch: Some(VirtioBalloonQueueDispatch {
                inflate_discard: VirtioBalloonDiscardOutcome {
                    attempts: 2,
                    requested_bytes: 16_384,
                    advised_bytes: 8192,
                    skipped_bytes: 4096,
                    failed_bytes: 4096,
                    failures: 1,
                },
                ..Default::default()
            }),
            deflate_queue_dispatch: None,
            statistics_queue_dispatch: None,
            hinting_queue_dispatch: Some(VirtioBalloonQueueDispatch {
                hinting_discard: VirtioBalloonDiscardOutcome {
                    attempts: 3,
                    requested_bytes: 24_576,
                    advised_bytes: 16_384,
                    skipped_bytes: 8192,
                    failed_bytes: 0,
                    failures: 0,
                },
                ..Default::default()
            }),
            reporting_queue_dispatch: Some(VirtioBalloonQueueDispatch {
                reporting_discard: VirtioBalloonDiscardOutcome {
                    attempts: 4,
                    requested_bytes: 32_768,
                    advised_bytes: 12_288,
                    skipped_bytes: 4096,
                    failed_bytes: 16_384,
                    failures: 2,
                },
                ..Default::default()
            }),
        };
        let metrics = SharedBalloonDeviceMetrics::default();

        metrics.record_notification_dispatch(&dispatch);

        assert_eq!(
            metrics.snapshot(),
            BalloonDeviceMetrics::new(0, 1, 0, 0, 0, 0)
                .with_discard_metrics(
                    BalloonDiscardMetrics::new(2, 8192, 4096, 1),
                    BalloonDiscardMetrics::new(3, 16_384, 8192, 0),
                )
                .with_free_page_report_metrics(BalloonFreePageReportMetrics::new(
                    4, 32_768, 12_288, 4096, 2,
                ))
        );
    }

    #[test]
    fn balloon_mmio_handler_activates_device_with_configured_queues() {
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");

        activate_handler(handler);

        assert!(handler.is_device_activated());
        assert!(handler.activation_handler().is_activated());
        assert_eq!(
            handler
                .activation_handler()
                .active_queues()
                .expect("active queues should be present")
                .queue_count(),
            VIRTIO_BALLOON_MIN_QUEUE_COUNT
        );
    }

    #[test]
    fn balloon_mmio_handler_dispatches_inflate_and_deflate_notifications() {
        let mut memory = pfn_descriptor_memory();
        let inflate_bytes = pfn_payload_bytes(&[50, 51]);
        let deflate_bytes = pfn_payload_bytes(&[50]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &inflate_bytes);
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &deflate_bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&inflate_bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, descriptor_len(&deflate_bytes), None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0]);
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        activate_handler(handler);

        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                queue_index_u32(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX),
            )
            .expect("inflate queue notification should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                queue_index_u32(VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX),
            )
            .expect("deflate queue notification should write");

        let dispatch = handler
            .dispatch_balloon_queue_notifications(&mut memory)
            .expect("inflate and deflate notifications should dispatch");

        assert_eq!(
            dispatch.drained_notifications(),
            &[
                VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX
            ]
        );
        assert_eq!(dispatch.inflate_notifications(), 1);
        assert_eq!(dispatch.deflate_notifications(), 1);
        let inflate_dispatch = dispatch
            .inflate_queue_dispatch()
            .expect("inflate queue should dispatch");
        assert_eq!(inflate_dispatch.completed_descriptors(), 1);
        assert_eq!(
            inflate_dispatch.inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 50,
                page_count: 2,
            }]
        );
        let deflate_dispatch = dispatch
            .deflate_queue_dispatch()
            .expect("deflate queue should dispatch");
        assert_eq!(deflate_dispatch.completed_descriptors(), 1);
        assert!(deflate_dispatch.needs_queue_interrupt());
        assert_eq!(
            deflate_dispatch.deflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 50,
                page_count: 1,
            }]
        );
        assert_eq!(
            handler
                .activation_handler()
                .memory_accounting()
                .inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 51,
                page_count: 1,
            }]
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, deflate_used_ring(), 0), (0, 0));
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status should read"),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert!(handler.pending_queue_notifications().is_empty());
    }

    #[test]
    fn balloon_mmio_handler_dispatches_hinting_command_notification() {
        let mut memory = pfn_descriptor_memory();
        let command = 101_u32;
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
        let config = balloon_config(64, false, 0, true, false);
        let hinting_queue_index = prepared(config)
            .queue_layout()
            .free_page_hinting()
            .expect("hinting queue should be configured")
            .index();
        write_hinting_descriptor(
            &mut memory,
            hinting_queue_index,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32, None),
        );
        write_available_heads(&mut memory, queue_available_ring(hinting_queue_index), &[0]);
        let mut device = balloon_mmio_device(config);
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        activate_handler(handler);

        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                queue_index_u32(hinting_queue_index),
            )
            .expect("hinting queue notification should write");

        let dispatch = handler
            .dispatch_balloon_queue_notifications(&mut memory)
            .expect("hinting queue notification should dispatch");

        assert_eq!(dispatch.hinting_notifications(), 1);
        let hinting_dispatch = dispatch
            .hinting_queue_dispatch()
            .expect("hinting queue should dispatch");
        assert_eq!(hinting_dispatch.completed_descriptors(), 1);
        assert_eq!(hinting_dispatch.hinting_guest_cmd(), Some(command));
        assert!(!hinting_dispatch.hinting_completed_run());
        assert!(!dispatch.hinting_completed_run());
        assert_eq!(
            handler
                .balloon_hinting_status()
                .expect("hinting status should read")
                .guest_cmd(),
            Some(command)
        );
        assert_eq!(
            read_used_idx(&memory, queue_used_ring(hinting_queue_index)),
            1
        );
        assert_eq!(
            read_used_element(&memory, queue_used_ring(hinting_queue_index), 0),
            (0, 0)
        );
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status should read"),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert!(handler.pending_queue_notifications().is_empty());
    }

    #[test]
    fn balloon_mmio_handler_dispatches_active_hinting_range_notification() {
        let mut memory = pfn_descriptor_memory();
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &[0xab; 64]);
        let config = balloon_config(64, false, 0, true, false);
        let hinting_queue_index = prepared(config)
            .queue_layout()
            .free_page_hinting()
            .expect("hinting queue should be configured")
            .index();
        let mut device = balloon_mmio_device(config);
        {
            let handler = device
                .dispatcher_mut()
                .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
                .expect("balloon handler should be registered");
            activate_handler(handler);

            handler
                .start_balloon_hinting(BalloonHintingStartInput::new(true))
                .expect("hinting start should update active config");
            let command = handler
                .balloon_hinting_status()
                .expect("hinting status should read")
                .host_cmd();
            write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
            write_hinting_descriptor(
                &mut memory,
                hinting_queue_index,
                0,
                TestDescriptor::readable(
                    TEST_PFN_DATA,
                    VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32,
                    Some(1),
                ),
            );
            write_hinting_descriptor(
                &mut memory,
                hinting_queue_index,
                1,
                TestDescriptor::readable(TEST_PFN_DATA_SPLIT, 64, None),
            );
            write_available_heads(&mut memory, queue_available_ring(hinting_queue_index), &[0]);
            handler
                .write_register(
                    VirtioMmioRegister::InterruptAck,
                    DeviceInterruptKind::Config.status().bits(),
                )
                .expect("config interrupt should acknowledge");

            handler
                .write_register(
                    VirtioMmioRegister::QueueNotify,
                    queue_index_u32(hinting_queue_index),
                )
                .expect("hinting queue notification should write");

            let dispatch = handler
                .dispatch_balloon_queue_notifications(&mut memory)
                .expect("active hinting range notification should dispatch");

            let hinting_dispatch = dispatch
                .hinting_queue_dispatch()
                .expect("hinting queue should dispatch");
            assert_eq!(hinting_dispatch.hinting_guest_cmd(), Some(command));
            assert_eq!(
                hinting_dispatch.hinting_page_ranges(),
                &[GuestMemoryRange::new(TEST_PFN_DATA_SPLIT, 64)
                    .expect("test range should be valid")]
            );
            assert_eq!(
                handler
                    .read_register(VirtioMmioRegister::InterruptStatus)
                    .expect("interrupt status should read"),
                DeviceInterruptKind::Queue.status().bits()
            );
            assert!(handler.pending_queue_notifications().is_empty());
        }
        assert_eq!(
            read_used_idx(&memory, queue_used_ring(hinting_queue_index)),
            1
        );
        assert_eq!(
            read_used_element(&memory, queue_used_ring(hinting_queue_index), 0),
            (0, 0)
        );
    }

    #[test]
    fn balloon_mmio_handler_acknowledges_completed_hinting_run() {
        for command in [
            VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
            VIRTIO_BALLOON_FREE_PAGE_HINT_DONE,
        ] {
            let mut memory = pfn_descriptor_memory();
            write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
            let config = balloon_config(64, false, 0, true, false);
            let hinting_queue_index = prepared(config)
                .queue_layout()
                .free_page_hinting()
                .expect("hinting queue should be configured")
                .index();
            write_hinting_descriptor(
                &mut memory,
                hinting_queue_index,
                0,
                TestDescriptor::readable(
                    TEST_PFN_DATA,
                    VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32,
                    None,
                ),
            );
            write_available_heads(&mut memory, queue_available_ring(hinting_queue_index), &[0]);
            let mut device = balloon_mmio_device(config);
            {
                let handler = device
                    .dispatcher_mut()
                    .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
                    .expect("balloon handler should be registered");
                activate_handler(handler);

                handler
                    .start_balloon_hinting(BalloonHintingStartInput::new(true))
                    .expect("hinting start should update active config");
                assert_eq!(
                    handler
                        .read_register(VirtioMmioRegister::ConfigGeneration)
                        .expect("config generation should read"),
                    1
                );
                handler
                    .write_register(
                        VirtioMmioRegister::InterruptAck,
                        DeviceInterruptKind::Config.status().bits(),
                    )
                    .expect("config interrupt should acknowledge");
                assert_eq!(
                    handler
                        .read_register(VirtioMmioRegister::InterruptStatus)
                        .expect("interrupt status should read"),
                    0
                );

                handler
                    .write_register(
                        VirtioMmioRegister::QueueNotify,
                        queue_index_u32(hinting_queue_index),
                    )
                    .expect("hinting queue notification should write");

                let dispatch = handler
                    .dispatch_balloon_queue_notifications(&mut memory)
                    .expect("completed hinting queue notification should dispatch");

                assert!(dispatch.hinting_completed_run());
                let status = handler
                    .balloon_hinting_status()
                    .expect("hinting status should read");
                assert_eq!(status.guest_cmd(), Some(command));
                assert_eq!(status.host_cmd(), VIRTIO_BALLOON_FREE_PAGE_HINT_DONE);
                assert_eq!(
                    handler
                        .read_register(VirtioMmioRegister::ConfigGeneration)
                        .expect("config generation should read"),
                    2
                );
                let mut expected_status = DeviceInterruptKind::Queue.status();
                expected_status.insert(DeviceInterruptKind::Config);
                assert_eq!(
                    handler
                        .read_register(VirtioMmioRegister::InterruptStatus)
                        .expect("interrupt status should read"),
                    expected_status.bits()
                );
                assert!(handler.pending_queue_notifications().is_empty());
            }
            assert_eq!(
                read_mmio_config(&mut device, 8, 4).as_slice(),
                &VIRTIO_BALLOON_FREE_PAGE_HINT_DONE.to_le_bytes()
            );
        }
    }

    #[test]
    fn balloon_mmio_handler_preserves_host_command_when_hinting_ack_is_disabled() {
        let mut memory = pfn_descriptor_memory();
        let command = VIRTIO_BALLOON_FREE_PAGE_HINT_STOP;
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
        let config = balloon_config(64, false, 0, true, false);
        let hinting_queue_index = prepared(config)
            .queue_layout()
            .free_page_hinting()
            .expect("hinting queue should be configured")
            .index();
        write_hinting_descriptor(
            &mut memory,
            hinting_queue_index,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32, None),
        );
        write_available_heads(&mut memory, queue_available_ring(hinting_queue_index), &[0]);
        let mut device = balloon_mmio_device(config);
        let host_cmd;
        {
            let handler = device
                .dispatcher_mut()
                .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
                .expect("balloon handler should be registered");
            activate_handler(handler);

            handler
                .start_balloon_hinting(BalloonHintingStartInput::new(false))
                .expect("hinting start should update active config");
            host_cmd = handler
                .balloon_hinting_status()
                .expect("hinting status should read")
                .host_cmd();
            handler
                .write_register(
                    VirtioMmioRegister::InterruptAck,
                    DeviceInterruptKind::Config.status().bits(),
                )
                .expect("config interrupt should acknowledge");

            handler
                .write_register(
                    VirtioMmioRegister::QueueNotify,
                    queue_index_u32(hinting_queue_index),
                )
                .expect("hinting queue notification should write");

            let dispatch = handler
                .dispatch_balloon_queue_notifications(&mut memory)
                .expect("completed hinting queue notification should dispatch");

            assert!(dispatch.hinting_completed_run());
            let status = handler
                .balloon_hinting_status()
                .expect("hinting status should read");
            assert_eq!(status.guest_cmd(), Some(command));
            assert_eq!(status.host_cmd(), host_cmd);
            assert_eq!(
                handler
                    .read_register(VirtioMmioRegister::ConfigGeneration)
                    .expect("config generation should read"),
                1
            );
            assert_eq!(
                handler
                    .read_register(VirtioMmioRegister::InterruptStatus)
                    .expect("interrupt status should read"),
                DeviceInterruptKind::Queue.status().bits()
            );
            assert!(handler.pending_queue_notifications().is_empty());
        }
        assert_eq!(
            read_mmio_config(&mut device, 8, 4).as_slice(),
            &host_cmd.to_le_bytes()
        );
    }

    #[test]
    fn balloon_mmio_handler_acknowledges_completed_hinting_run_before_malformed_command() {
        let mut memory = pfn_descriptor_memory();
        let command = VIRTIO_BALLOON_FREE_PAGE_HINT_STOP;
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &command.to_le_bytes());
        let config = balloon_config(64, false, 0, true, false);
        let hinting_queue_index = prepared(config)
            .queue_layout()
            .free_page_hinting()
            .expect("hinting queue should be configured")
            .index();
        write_hinting_descriptor(
            &mut memory,
            hinting_queue_index,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32, None),
        );
        write_hinting_descriptor(
            &mut memory,
            hinting_queue_index,
            1,
            TestDescriptor::readable(
                GuestAddress::new(TEST_MEMORY_SIZE),
                VIRTIO_BALLOON_HINTING_COMMAND_SIZE_U32,
                None,
            ),
        );
        write_available_heads(
            &mut memory,
            queue_available_ring(hinting_queue_index),
            &[0, 1],
        );
        let mut device = balloon_mmio_device(config);
        {
            let handler = device
                .dispatcher_mut()
                .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
                .expect("balloon handler should be registered");
            activate_handler(handler);

            handler
                .start_balloon_hinting(BalloonHintingStartInput::new(true))
                .expect("hinting start should update active config");
            handler
                .write_register(
                    VirtioMmioRegister::InterruptAck,
                    DeviceInterruptKind::Config.status().bits(),
                )
                .expect("config interrupt should acknowledge");

            handler
                .write_register(
                    VirtioMmioRegister::QueueNotify,
                    queue_index_u32(hinting_queue_index),
                )
                .expect("hinting queue notification should write");

            let error = handler
                .dispatch_balloon_queue_notifications(&mut memory)
                .expect_err("malformed later hinting command should fail dispatch");

            assert!(matches!(
                error,
                VirtioBalloonDeviceNotificationError::QueueDispatch {
                    source: VirtioBalloonQueueDispatchError::HintingCommandRead { .. },
                    ..
                }
            ));
            let completed = error
                .completed_notification_dispatch()
                .expect("completed hinting dispatch should be preserved");
            assert!(completed.hinting_completed_run());
            let status = handler
                .balloon_hinting_status()
                .expect("hinting status should read");
            assert_eq!(status.guest_cmd(), Some(command));
            assert_eq!(status.host_cmd(), VIRTIO_BALLOON_FREE_PAGE_HINT_DONE);
            assert_eq!(
                handler
                    .read_register(VirtioMmioRegister::ConfigGeneration)
                    .expect("config generation should read"),
                2
            );
            let mut expected_status = DeviceInterruptKind::Queue.status();
            expected_status.insert(DeviceInterruptKind::Config);
            assert_eq!(
                handler
                    .read_register(VirtioMmioRegister::InterruptStatus)
                    .expect("interrupt status should read"),
                expected_status.bits()
            );
            assert!(handler.pending_queue_notifications().is_empty());
        }
        assert_eq!(
            read_mmio_config(&mut device, 8, 4).as_slice(),
            &VIRTIO_BALLOON_FREE_PAGE_HINT_DONE.to_le_bytes()
        );
        assert_eq!(
            read_used_idx(&memory, queue_used_ring(hinting_queue_index)),
            1
        );
    }

    #[test]
    fn balloon_mmio_handler_dispatches_statistics_notification() {
        let mut memory = pfn_descriptor_memory();
        let bytes = stat_payload_bytes(&[
            (VIRTIO_BALLOON_S_SWAP_OUT, 9),
            (VIRTIO_BALLOON_S_MEMFREE, 0x5678),
        ]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_statistics_descriptor(
            &mut memory,
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(
            &mut memory,
            queue_available_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            &[0],
        );
        let mut device = balloon_mmio_device(balloon_config(64, false, 1, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        activate_handler(handler);

        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                queue_index_u32(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            )
            .expect("statistics queue notification should write");

        let dispatch = handler
            .dispatch_balloon_queue_notifications(&mut memory)
            .expect("statistics queue notification should dispatch");

        assert_eq!(dispatch.statistics_notifications(), 1);
        let statistics_dispatch = dispatch
            .statistics_queue_dispatch()
            .expect("statistics queue dispatch should be recorded");
        assert_eq!(statistics_dispatch.statistics_reports(), 1);
        assert_eq!(
            statistics_dispatch.statistics_pending_descriptor_head(),
            Some(0)
        );
        assert_eq!(statistics_dispatch.statistics().swap_out(), Some(9));
        assert_eq!(statistics_dispatch.statistics().free_memory(), Some(0x5678));
        assert_eq!(
            handler.activation_handler().statistics().swap_out(),
            Some(9)
        );
        assert_eq!(
            read_used_idx(&memory, queue_used_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX)),
            0
        );
        assert!(handler.pending_queue_notifications().is_empty());
    }

    #[test]
    fn balloon_mmio_handler_triggers_pending_statistics_update() {
        let mut memory = pfn_descriptor_memory();
        let bytes = stat_payload_bytes(&[
            (VIRTIO_BALLOON_S_SWAP_OUT, 9),
            (VIRTIO_BALLOON_S_MEMFREE, 0x5678),
        ]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_statistics_descriptor(
            &mut memory,
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(
            &mut memory,
            queue_available_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            &[0],
        );
        let mut device = balloon_mmio_device(balloon_config(64, false, 1, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        activate_handler(handler);

        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                queue_index_u32(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            )
            .expect("statistics queue notification should write");
        handler
            .dispatch_balloon_queue_notifications(&mut memory)
            .expect("statistics queue notification should dispatch");

        let dispatch = handler
            .trigger_balloon_statistics_update(&mut memory)
            .expect("pending statistics update should trigger");

        assert!(dispatch.drained_notifications().is_empty());
        let statistics_dispatch = dispatch
            .statistics_queue_dispatch()
            .expect("statistics trigger dispatch should be recorded");
        assert_eq!(statistics_dispatch.completed_descriptors(), 1);
        assert!(statistics_dispatch.needs_queue_interrupt());
        assert_eq!(
            statistics_dispatch.statistics_pending_descriptor_head(),
            None
        );
        assert_eq!(statistics_dispatch.statistics().swap_out(), Some(9));
        assert_eq!(statistics_dispatch.statistics().free_memory(), Some(0x5678));
        let metrics = SharedBalloonDeviceMetrics::default();
        metrics.record_notification_dispatch(&dispatch);
        assert_eq!(
            metrics.snapshot(),
            BalloonDeviceMetrics::new(0, 0, 1, 0, 0, 0)
        );
        assert_eq!(
            read_used_idx(&memory, queue_used_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX)),
            1
        );
        assert_eq!(
            read_used_element(
                &memory,
                queue_used_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
                0
            ),
            (0, 0)
        );
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status should read"),
            DeviceInterruptKind::Queue.status().bits()
        );
        let repeat = handler
            .trigger_balloon_statistics_update(&mut memory)
            .expect("repeated trigger without pending descriptor should be a no-op");
        assert_eq!(
            repeat
                .statistics_queue_dispatch()
                .expect("repeat trigger dispatch should be recorded")
                .completed_descriptors(),
            0
        );
        assert_eq!(
            read_used_idx(&memory, queue_used_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX)),
            1
        );
    }

    #[test]
    fn balloon_mmio_handler_statistics_update_trigger_without_pending_descriptor_is_noop() {
        let mut memory = pfn_descriptor_memory();
        let mut device = balloon_mmio_device(balloon_config(64, false, 1, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        activate_handler(handler);

        let dispatch = handler
            .trigger_balloon_statistics_update(&mut memory)
            .expect("statistics update trigger without pending descriptor should be a no-op");

        assert!(dispatch.drained_notifications().is_empty());
        let statistics_dispatch = dispatch
            .statistics_queue_dispatch()
            .expect("statistics no-op dispatch should be recorded");
        assert_eq!(statistics_dispatch.completed_descriptors(), 0);
        assert!(!statistics_dispatch.needs_queue_interrupt());
        assert_eq!(
            statistics_dispatch.statistics_pending_descriptor_head(),
            None
        );
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status should read"),
            0
        );
        assert_eq!(
            read_used_idx(&memory, queue_used_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX)),
            0
        );
    }

    #[test]
    fn balloon_mmio_handler_statistics_update_trigger_before_activation_reports_inactive() {
        let mut memory = pfn_descriptor_memory();
        let mut device = balloon_mmio_device(balloon_config(64, false, 1, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");

        let error = handler
            .trigger_balloon_statistics_update(&mut memory)
            .expect_err("statistics update trigger before activation should fail");

        assert!(matches!(
            error,
            VirtioBalloonDeviceNotificationError::Inactive { .. }
        ));
        assert!(error.drained_notifications().is_empty());
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status should read"),
            0
        );
    }

    #[test]
    fn balloon_mmio_handler_statistics_update_trigger_without_statistics_queue_is_noop() {
        let mut memory = pfn_descriptor_memory();
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        activate_handler(handler);

        let dispatch = handler
            .trigger_balloon_statistics_update(&mut memory)
            .expect("statistics update trigger without statistics queue should be a no-op");

        assert!(dispatch.drained_notifications().is_empty());
        assert!(dispatch.statistics_queue_dispatch().is_none());
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status should read"),
            0
        );
    }

    #[test]
    fn balloon_mmio_handler_statistics_update_trigger_preserves_pending_on_used_ring_error() {
        let mut memory = pfn_descriptor_memory();
        let bytes = stat_payload_bytes(&[(VIRTIO_BALLOON_S_MEMFREE, 0x5678)]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_statistics_descriptor(
            &mut memory,
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(
            &mut memory,
            queue_available_ring(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            &[0],
        );
        let mut device = balloon_mmio_device(balloon_config(64, false, 1, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        set_handler_queue_config_status(handler);
        for queue_index in 0..handler.queue_registers().queue_count() {
            let device_ring = if queue_index == VIRTIO_BALLOON_STATS_QUEUE_INDEX {
                GuestAddress::new(TEST_MEMORY_SIZE)
            } else {
                queue_address(TEST_DEVICE_BASE, queue_index_u32(queue_index))
            };
            configure_handler_queue_with_device_ring(
                handler,
                queue_index_u32(queue_index),
                device_ring,
            );
        }
        handler
            .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
            .expect("driver-ok status should write");

        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                queue_index_u32(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            )
            .expect("statistics queue notification should write");
        handler
            .dispatch_balloon_queue_notifications(&mut memory)
            .expect("statistics queue notification should hold pending descriptor");

        let error = handler
            .trigger_balloon_statistics_update(&mut memory)
            .expect_err("unmapped used ring should fail statistics update trigger");

        assert!(matches!(
            error,
            VirtioBalloonDeviceNotificationError::QueueDispatch {
                source: VirtioBalloonQueueDispatchError::UsedRing {
                    queue: VirtioBalloonQueueKind::Statistics,
                    descriptor_head: 0,
                    ..
                },
                ..
            }
        ));
        let completed = error
            .completed_notification_dispatch()
            .expect("completed statistics trigger dispatch should be preserved")
            .statistics_queue_dispatch()
            .expect("statistics dispatch should be preserved");
        assert_eq!(completed.statistics_pending_descriptor_head(), Some(0));
        assert_eq!(completed.statistics().free_memory(), Some(0x5678));
        assert_eq!(
            handler.activation_handler().statistics().free_memory(),
            Some(0x5678)
        );
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status should read"),
            0
        );
    }

    #[test]
    fn balloon_mmio_handler_dispatches_reporting_queue_notification() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        write_reporting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::writable(TEST_PFN_DATA, 4096, None),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, true));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        activate_handler(handler);

        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                queue_index_u32(queue_index),
            )
            .expect("reporting queue notification should write");

        let dispatch = handler
            .dispatch_balloon_queue_notifications(&mut memory)
            .expect("reporting queue notification should dispatch");

        assert_eq!(dispatch.drained_notifications(), &[queue_index]);
        assert_eq!(dispatch.reporting_notifications(), 1);
        let reporting = dispatch
            .reporting_queue_dispatch()
            .expect("reporting dispatch should be retained");
        assert_eq!(reporting.completed_descriptors(), 1);
        assert_eq!(reporting.reporting_discard().attempts(), 1);
        assert_eq!(reporting.reporting_discard().requested_bytes(), 4096);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(read_used_idx(&memory, queue_used_ring(queue_index)), 1);
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status should read"),
            1
        );
        assert!(handler.pending_queue_notifications().is_empty());
    }

    #[test]
    fn balloon_reporting_metrics_preserve_discard_after_used_ring_failure() {
        let mut memory = pfn_descriptor_memory();
        let queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;
        write_reporting_descriptor(
            &mut memory,
            queue_index,
            0,
            TestDescriptor::writable(TEST_PFN_DATA, 4096, None),
        );
        write_available_heads(&mut memory, queue_available_ring(queue_index), &[0]);
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, true));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        set_handler_queue_config_status(handler);
        configure_handler_queue(handler, queue_index_u32(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX));
        configure_handler_queue(handler, queue_index_u32(VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX));
        configure_handler_queue_with_device_ring(
            handler,
            queue_index_u32(queue_index),
            GuestAddress::new(TEST_MEMORY_SIZE),
        );
        handler
            .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
            .expect("driver-ok status should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                queue_index_u32(queue_index),
            )
            .expect("reporting queue notification should write");

        let error = handler
            .dispatch_balloon_queue_notifications(&mut memory)
            .expect_err("unmapped reporting used ring should fail after discard");

        let completed = error
            .completed_notification_dispatch()
            .expect("completed reporting notification should be preserved");
        let reporting = completed
            .reporting_queue_dispatch()
            .expect("reporting discard should be preserved");
        assert_eq!(reporting.completed_descriptors(), 0);
        assert!(!reporting.needs_queue_interrupt());
        assert_eq!(reporting.reporting_discard().attempts(), 1);
        assert_eq!(reporting.reporting_discard().requested_bytes(), 4096);
        let metrics = SharedBalloonDeviceMetrics::default();
        metrics.record_notification_dispatch(completed);
        assert_eq!(
            metrics.snapshot(),
            BalloonDeviceMetrics::default().with_free_page_report_metrics(
                BalloonFreePageReportMetrics::from(reporting.reporting_discard())
            )
        );
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status should read"),
            0
        );
    }

    #[test]
    fn balloon_mmio_handler_status_reset_clears_active_queues_and_notifications() {
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, true));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        activate_handler(handler);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                queue_index_u32(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            )
            .expect("reporting queue notification should write");

        handler
            .write_register(VirtioMmioRegister::Status, 0)
            .expect("status reset should write");

        assert!(!handler.is_device_activated());
        assert!(!handler.activation_handler().is_activated());
        assert!(handler.pending_queue_notifications().is_empty());
    }

    #[test]
    fn balloon_mmio_handler_updates_statistics_interval_without_resetting_on_status_reset() {
        let mut device = balloon_mmio_device(balloon_config(64, false, 60, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");

        assert_eq!(handler.activation_handler().stats_polling_interval_s(), 60);
        handler
            .update_balloon_statistics(BalloonStatsUpdateInput::new(30))
            .expect("statistics interval should update");
        assert_eq!(handler.activation_handler().stats_polling_interval_s(), 30);

        handler
            .write_register(VirtioMmioRegister::Status, 0)
            .expect("status reset should write");

        assert_eq!(handler.activation_handler().stats_polling_interval_s(), 30);
    }

    #[test]
    fn balloon_mmio_handler_rejects_statistics_interval_enabled_state_change() {
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");

        let err = handler
            .update_balloon_statistics(BalloonStatsUpdateInput::new(1))
            .expect_err("statistics enabled-state change should fail");

        assert_eq!(err, BalloonUpdateError::StatisticsStateChange);
        assert_eq!(handler.activation_handler().stats_polling_interval_s(), 0);
    }

    #[test]
    fn mmio_registration_exposes_identity_features_and_queues() {
        let mut device = balloon_mmio_device(balloon_config(64, true, 1, true, true));
        let registration = device.registration();

        assert_eq!(registration.region_id(), TEST_BALLOON_MMIO_REGION_ID);
        assert_eq!(registration.address(), TEST_BALLOON_MMIO_BASE);

        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        assert_eq!(
            handler.device_registers().device_id(),
            VIRTIO_BALLOON_DEVICE_ID
        );
        assert!(has_feature(
            handler.device_registers().device_features(),
            VIRTIO_FEATURE_VERSION_1
        ));
        assert!(has_feature(
            handler.device_registers().device_features(),
            VIRTIO_BALLOON_F_DEFLATE_ON_OOM
        ));
        assert_eq!(
            handler.queue_registers().queue_count(),
            VIRTIO_BALLOON_MAX_QUEUE_COUNT
        );
        assert_eq!(
            handler
                .queue_registers()
                .queue(0)
                .expect("inflate queue should exist")
                .max_size(),
            VIRTIO_BALLOON_QUEUE_SIZE
        );
    }

    #[test]
    fn mmio_config_space_reads_firecracker_layout() {
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));

        assert_eq!(
            read_mmio_config(&mut device, 0, 4).as_slice(),
            &(64 * VIRTIO_BALLOON_MIB_TO_4K_PAGES).to_le_bytes()
        );
        assert_eq!(
            read_mmio_config(&mut device, 4, 4).as_slice(),
            &[0, 0, 0, 0]
        );
        assert_eq!(
            read_mmio_config(&mut device, 8, 4).as_slice(),
            &[0, 0, 0, 0]
        );
        let mut config_space = [0; VIRTIO_BALLOON_CONFIG_SPACE_SIZE];
        config_space[0..4].copy_from_slice(read_mmio_config(&mut device, 0, 4).as_slice());
        config_space[4..8].copy_from_slice(read_mmio_config(&mut device, 4, 4).as_slice());
        config_space[8..12].copy_from_slice(read_mmio_config(&mut device, 8, 4).as_slice());
        assert_eq!(
            config_space,
            VirtioBalloonConfigSpace::from_config(balloon_config(64, false, 0, false, false))
                .expect("balloon config space should build")
                .to_le_bytes()
        );
    }

    #[test]
    fn mmio_config_update_marks_config_interrupt_and_generation() {
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));
        {
            let handler = device
                .dispatcher_mut()
                .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
                .expect("balloon handler should be registered");
            handler
                .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
                .expect("acknowledge status should write");
            handler
                .write_register(
                    VirtioMmioRegister::Status,
                    VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
                )
                .expect("driver status should write");
        }
        write_mmio_config(&mut device, 4, &0x1234_u32.to_le_bytes());
        write_mmio_config(&mut device, 8, &0x5678_u32.to_le_bytes());
        {
            let handler = device
                .dispatcher_mut()
                .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
                .expect("balloon handler should be registered");

            assert_eq!(
                handler
                    .read_register(VirtioMmioRegister::ConfigGeneration)
                    .expect("config generation should read"),
                0
            );

            handler
                .update_balloon_config(balloon_config(128, false, 0, false, false))
                .expect("balloon config update should succeed");

            assert_eq!(
                handler
                    .read_register(VirtioMmioRegister::ConfigGeneration)
                    .expect("config generation should read"),
                1
            );
            assert_eq!(
                handler
                    .read_register(VirtioMmioRegister::InterruptStatus)
                    .expect("interrupt status should read"),
                DeviceInterruptKind::Config.status().bits()
            );
        }

        assert_eq!(
            read_mmio_config(&mut device, 0, 4).as_slice(),
            &(128 * VIRTIO_BALLOON_MIB_TO_4K_PAGES).to_le_bytes()
        );
        assert_eq!(
            read_mmio_config(&mut device, 4, 4).as_slice(),
            &0x1234_u32.to_le_bytes()
        );
        assert_eq!(
            read_mmio_config(&mut device, 8, 4).as_slice(),
            &0x5678_u32.to_le_bytes()
        );
    }

    #[test]
    fn mmio_hinting_start_stop_marks_config_interrupt_and_generation() {
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, true, false));
        {
            let handler = device
                .dispatcher_mut()
                .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
                .expect("balloon handler should be registered");
            handler
                .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
                .expect("acknowledge status should write");
            handler
                .write_register(
                    VirtioMmioRegister::Status,
                    VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
                )
                .expect("driver status should write");
        }
        {
            let handler = device
                .dispatcher_mut()
                .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
                .expect("balloon handler should be registered");

            handler
                .start_balloon_hinting(BalloonHintingStartInput::new(false))
                .expect("hinting start should update active config");

            assert_eq!(
                handler
                    .read_register(VirtioMmioRegister::ConfigGeneration)
                    .expect("config generation should read"),
                1
            );
            assert_eq!(
                handler
                    .read_register(VirtioMmioRegister::InterruptStatus)
                    .expect("interrupt status should read"),
                DeviceInterruptKind::Config.status().bits()
            );
            assert!(
                !handler
                    .activation_handler()
                    .hinting_acknowledge_on_stop()
                    .expect("hinting ack setting should read")
            );
            assert_eq!(
                handler
                    .balloon_hinting_status()
                    .expect("hinting status should read")
                    .host_cmd(),
                VIRTIO_BALLOON_FREE_PAGE_HINT_DONE + 1
            );
        }
        assert_eq!(
            read_mmio_config(&mut device, 8, 4).as_slice(),
            &(VIRTIO_BALLOON_FREE_PAGE_HINT_DONE + 1).to_le_bytes()
        );
        {
            let handler = device
                .dispatcher_mut()
                .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
                .expect("balloon handler should be registered");

            handler
                .stop_balloon_hinting()
                .expect("hinting stop should update active config");

            assert_eq!(
                handler
                    .read_register(VirtioMmioRegister::ConfigGeneration)
                    .expect("config generation should read"),
                2
            );
            assert_eq!(
                handler
                    .balloon_hinting_status()
                    .expect("hinting status should read")
                    .host_cmd(),
                VIRTIO_BALLOON_FREE_PAGE_HINT_DONE
            );
        }
        assert_eq!(
            read_mmio_config(&mut device, 8, 4).as_slice(),
            &VIRTIO_BALLOON_FREE_PAGE_HINT_DONE.to_le_bytes()
        );
    }

    #[test]
    fn mmio_config_space_keeps_guest_writes_in_local_state() {
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");

        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("acknowledge status should write");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("driver status should write");

        write_mmio_config(&mut device, 4, &0x0102_0304_u32.to_le_bytes());
        write_mmio_config(&mut device, 8, &0x0506_0708_u32.to_le_bytes());

        assert_eq!(
            read_mmio_config(&mut device, 4, 4).as_slice(),
            &0x0102_0304_u32.to_le_bytes()
        );
        assert_eq!(
            read_mmio_config(&mut device, 8, 4).as_slice(),
            &0x0506_0708_u32.to_le_bytes()
        );
    }

    #[test]
    fn mmio_config_space_rejects_out_of_bounds_accesses() {
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));
        let read_access = device
            .dispatcher()
            .lookup(
                TEST_BALLOON_MMIO_BASE
                    .checked_add(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + 12)
                    .expect("test MMIO address should not overflow"),
                1,
            )
            .expect("access should resolve inside MMIO window");
        let read = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered")
            .read_access(read_access);
        assert_eq!(
            read,
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 12, len: 1 })
        );

        let write_access = device
            .dispatcher()
            .lookup(
                TEST_BALLOON_MMIO_BASE
                    .checked_add(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + 11)
                    .expect("test MMIO address should not overflow"),
                2,
            )
            .expect("access should resolve inside MMIO window");
        let write_data = MmioAccessBytes::new(&[1, 2]).expect("test bytes should build");
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("acknowledge status should write");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("driver status should write");
        let write = handler.write_access(write_access, write_data);
        assert_eq!(
            write,
            Err(
                VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigWrite { offset: 11, len: 2 }
            )
        );
    }

    #[test]
    fn mmio_queue_notifications_are_recorded_until_balloon_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");

        activate_handler(handler);
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 0)
            .expect("queue notification should be accepted");

        assert_eq!(handler.pending_queue_notifications(), vec![0]);
        let dispatch = handler
            .dispatch_balloon_queue_notifications(&mut memory)
            .expect("queue notification should drain");
        assert_eq!(dispatch.drained_notifications(), &[0]);
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status should read"),
            0
        );
        assert!(handler.pending_queue_notifications().is_empty());
    }
}
