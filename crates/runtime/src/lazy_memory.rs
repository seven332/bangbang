//! Bounded, backend-neutral ownership for externally populated anonymous pages.
//!
//! This module owns anonymous mappings and the page-content state machine. It
//! deliberately installs no Mach exception port, HVF mapping, pager transport,
//! path authority, or public snapshot behavior.

use std::collections::TryReserveError;
use std::ffi::c_void;
use std::fmt;
use std::ptr::{self, NonNull};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use bangbang_pager::{
    MAX_IN_FLIGHT, MAX_REGIONS, MIN_PAGE_SIZE, PageAccess, PagerGeneration, PagerLimits,
    PagerRegion, PagerRegionId,
};

use crate::memory::{
    GuestAddress, GuestMemory, GuestMemoryAllocationError, GuestMemoryLayout, GuestMemoryRange,
    GuestMemoryRegion, GuestMemoryRegionBacking, aarch64,
};

/// Maximum local duplicate/action waiters admitted by one lazy owner.
pub const MAX_LAZY_MEMORY_WAITERS: u16 = 4_096;

/// Maximum number of 4-KiB coordinator pages representable by arm64 DRAM.
pub const MAX_LAZY_MEMORY_PAGES: u64 = aarch64::DRAM_MEM_MAX_SIZE / (MIN_PAGE_SIZE as u64);

/// Local resource bounds layered on the negotiated pager limits.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LazyGuestMemoryLimits {
    pager: PagerLimits,
    max_pages: u64,
    max_waiters: u16,
}

impl LazyGuestMemoryLimits {
    /// Constructs one bounded coordinator configuration.
    pub fn new(
        pager: PagerLimits,
        max_pages: u64,
        max_waiters: u16,
    ) -> Result<Self, LazyGuestMemoryError> {
        let selected_max_pages = aarch64::DRAM_MEM_MAX_SIZE / u64::from(pager.page_size());
        if max_pages == 0
            || max_pages > MAX_LAZY_MEMORY_PAGES
            || max_pages > selected_max_pages
            || max_waiters == 0
            || max_waiters > MAX_LAZY_MEMORY_WAITERS
        {
            return Err(LazyGuestMemoryError::InvalidConfiguration);
        }
        Ok(Self {
            pager,
            max_pages,
            max_waiters,
        })
    }

    /// Returns the exact negotiated protocol limits.
    pub const fn pager(self) -> PagerLimits {
        self.pager
    }

    /// Returns the maximum admitted page-state count.
    pub const fn max_pages(self) -> u64 {
        self.max_pages
    }

    /// Returns the maximum number of local waiters.
    pub const fn max_waiters(self) -> u16 {
        self.max_waiters
    }
}

impl fmt::Debug for LazyGuestMemoryLimits {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LazyGuestMemoryLimits")
            .field("pager", &"<redacted>")
            .field("max_pages", &"<redacted>")
            .field("max_waiters", &"<redacted>")
            .finish()
    }
}

/// One guest/source range admitted to a lazy owner.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LazyGuestMemoryRegion {
    guest: GuestMemoryRange,
    source: PagerRegion,
}

impl LazyGuestMemoryRegion {
    /// Binds one guest range to one offset-only pager source range.
    pub fn new(
        id: PagerRegionId,
        guest: GuestMemoryRange,
        source_offset: u64,
        page_size: u32,
    ) -> Result<Self, LazyGuestMemoryError> {
        guest
            .validate_alignment(u64::from(page_size))
            .map_err(|_| LazyGuestMemoryError::InvalidConfiguration)?;
        let source = PagerRegion::new(id, source_offset, guest.size(), page_size)
            .map_err(|_| LazyGuestMemoryError::InvalidConfiguration)?;
        Ok(Self { guest, source })
    }

    /// Returns the opaque protocol region identity.
    pub const fn id(self) -> PagerRegionId {
        self.source.id()
    }

    /// Returns the owned guest-physical range.
    pub const fn guest_range(self) -> GuestMemoryRange {
        self.guest
    }

    /// Returns the peer-owned source offset.
    pub const fn source_offset(self) -> u64 {
        self.source.source_offset()
    }

    /// Returns the common guest/source length.
    pub const fn length(self) -> u64 {
        self.source.length()
    }

    /// Returns the exact protocol region record.
    pub const fn pager_region(self) -> PagerRegion {
        self.source
    }
}

impl fmt::Debug for LazyGuestMemoryRegion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LazyGuestMemoryRegion(<redacted>)")
    }
}

/// Public page lifecycle observed through the coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LazyPageState {
    /// No external contents are installed.
    Absent,
    /// One population request owns the page generation.
    Loading,
    /// A validated response is installing contents and later permissions.
    Publishing,
    /// Contents were committed for the current generation.
    Present,
    /// Local removal and its later peer acknowledgement are incomplete.
    Removing,
    /// The complete owner is closed to new work.
    Terminal,
}

/// Stable reason for the one owner-wide terminal outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LazyGuestMemoryTerminalReason {
    /// The local restore operation was cancelled.
    Requested,
    /// The external content owner failed.
    PeerFailure,
    /// An admitted transition was abandoned or failed.
    TransitionFailure,
    /// The owning VM is tearing down.
    Teardown,
    /// Synchronization state was poisoned.
    StatePoisoned,
}

/// Result of acquiring one page for a fault.
pub enum LazyPageFault {
    /// Contents are already committed.
    Present,
    /// This caller owns the sole external population request.
    Populate(LazyPagePopulation),
}

impl fmt::Debug for LazyPageFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Present => formatter.write_str("LazyPageFault::Present"),
            Self::Populate(_) => formatter.write_str("LazyPageFault::Populate(<redacted>)"),
        }
    }
}

/// One non-cloneable population operation.
pub struct LazyPagePopulation {
    inner: Arc<LazyGuestMemoryInner>,
    location: PageLocation,
    region: PagerRegionId,
    generation: PagerGeneration,
    access: PageAccess,
    offset: u64,
    source_offset: u64,
    guest_range: GuestMemoryRange,
    length: u32,
    armed: bool,
}

impl LazyPagePopulation {
    /// Returns the opaque source-region identity.
    pub const fn region(&self) -> PagerRegionId {
        self.region
    }

    /// Returns the exact population generation.
    pub const fn generation(&self) -> PagerGeneration {
        self.generation
    }

    /// Returns the immutable access in the external request tuple.
    pub const fn access(&self) -> PageAccess {
        self.access
    }

    /// Returns the region-relative page offset.
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// Returns the peer-owned source offset.
    pub const fn source_offset(&self) -> u64 {
        self.source_offset
    }

    /// Returns the exact guest page range.
    pub const fn guest_range(&self) -> GuestMemoryRange {
        self.guest_range
    }

    /// Returns the selected page length.
    pub const fn length(&self) -> u32 {
        self.length
    }

    /// Consumes a current response and enters the serialized publication phase.
    pub fn begin_publication(mut self) -> Result<LazyPagePublication, LazyGuestMemoryError> {
        self.armed = false;
        self.inner.begin_publication(self.location, self.generation)
    }
}

impl fmt::Debug for LazyPagePopulation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LazyPagePopulation(<redacted>)")
    }
}

impl Drop for LazyPagePopulation {
    fn drop(&mut self) {
        if self.armed {
            self.inner.drop_population(self.location, self.generation);
        }
    }
}

/// Scoped page publication after exact generation validation.
pub struct LazyPagePublication {
    inner: Arc<LazyGuestMemoryInner>,
    location: PageLocation,
    region_offset: u64,
    generation: PagerGeneration,
    initialized: bool,
    finished: bool,
}

impl LazyPagePublication {
    /// Returns the exact page target while this publication owns it.
    pub fn target(&mut self) -> Result<LazyGuestMemoryTarget<'_>, LazyGuestMemoryError> {
        self.inner.target(
            self.location.region_index,
            self.region_offset,
            u64::from(self.inner.limits.pager.page_size()),
            &mut self.initialized,
        )
    }

    /// Commits installed contents for the exact generation.
    pub fn commit(mut self) -> Result<(), LazyGuestMemoryError> {
        if !self.initialized {
            return Err(LazyGuestMemoryError::ContentMissing);
        }
        let result = self
            .inner
            .commit_publication(self.location, self.generation);
        if result.is_ok() {
            self.finished = true;
        }
        result
    }
}

impl fmt::Debug for LazyPagePublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LazyPagePublication(<redacted>)")
    }
}

impl Drop for LazyPagePublication {
    fn drop(&mut self) {
        if !self.finished {
            self.inner.drop_publication(self.location, self.generation);
        }
    }
}

/// Scoped range removal held through local work and later peer acknowledgement.
pub struct LazyPageRemoval {
    inner: Arc<LazyGuestMemoryInner>,
    region: PagerRegionId,
    region_index: usize,
    first_page: usize,
    page_count: usize,
    region_offset: u64,
    source_offset: u64,
    length: u64,
    generation: PagerGeneration,
    zeroed: bool,
    finished: bool,
}

impl LazyPageRemoval {
    /// Returns the opaque source-region identity.
    pub const fn region(&self) -> PagerRegionId {
        self.region
    }

    /// Returns the exact removal generation.
    pub const fn generation(&self) -> PagerGeneration {
        self.generation
    }

    /// Returns the region-relative removal offset.
    pub const fn offset(&self) -> u64 {
        self.region_offset
    }

    /// Returns the peer-owned source offset.
    pub const fn source_offset(&self) -> u64 {
        self.source_offset
    }

    /// Returns the exact removal length.
    pub const fn length(&self) -> u64 {
        self.length
    }

    /// Returns the exact mapped target while removal owns it.
    pub fn target(&mut self) -> Result<LazyGuestMemoryTarget<'_>, LazyGuestMemoryError> {
        self.inner.target(
            self.region_index,
            self.region_offset,
            self.length,
            &mut self.zeroed,
        )
    }

    /// Commits a simulated or later validated peer acknowledgement.
    pub fn commit_acknowledged(mut self) -> Result<(), LazyGuestMemoryError> {
        if !self.zeroed {
            return Err(LazyGuestMemoryError::ContentMissing);
        }
        let result = self.inner.commit_removal(
            self.region_index,
            self.first_page,
            self.page_count,
            self.generation,
        );
        if result.is_ok() {
            self.finished = true;
        }
        result
    }
}

impl fmt::Debug for LazyPageRemoval {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LazyPageRemoval(<redacted>)")
    }
}

impl Drop for LazyPageRemoval {
    fn drop(&mut self) {
        if !self.finished {
            self.inner.drop_removal(
                self.region_index,
                self.first_page,
                self.page_count,
                self.generation,
            );
        }
    }
}

/// One mapping target available only while a transition guard is live.
pub struct LazyGuestMemoryTarget<'a> {
    address: NonNull<c_void>,
    range: GuestMemoryRange,
    length: usize,
    initialized: &'a mut bool,
    _owner: &'a LazyGuestMemoryInner,
}

impl LazyGuestMemoryTarget<'_> {
    /// Returns the exact guest range represented by this target.
    pub const fn range(&self) -> GuestMemoryRange {
        self.range
    }

    /// Returns the exact target length.
    pub const fn len(&self) -> usize {
        self.length
    }

    /// Returns whether this nonempty target is empty.
    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Returns the in-process host address for later platform adapters.
    pub const fn host_address(&self) -> NonNull<c_void> {
        self.address
    }

    /// Installs one exact data page or range.
    pub fn copy_from_slice(&mut self, source: &[u8]) -> Result<(), LazyGuestMemoryError> {
        if *self.initialized {
            return Err(LazyGuestMemoryError::ContentAlreadyInstalled);
        }
        if source.len() != self.length {
            return Err(LazyGuestMemoryError::ContentLength);
        }
        // SAFETY: construction proves that `address..address + length` is
        // wholly inside one retained anonymous mapping. `ptr::copy` permits a
        // source that aliases the same mapping.
        unsafe {
            ptr::copy(source.as_ptr(), self.address.as_ptr().cast(), self.length);
        }
        *self.initialized = true;
        Ok(())
    }

    /// Installs zero contents across the exact target.
    pub fn zero(&mut self) -> Result<(), LazyGuestMemoryError> {
        if *self.initialized {
            return Err(LazyGuestMemoryError::ContentAlreadyInstalled);
        }
        // SAFETY: construction proves that the complete nonempty target is
        // writable and retained for this scoped transition.
        unsafe {
            ptr::write_bytes(self.address.as_ptr().cast::<u8>(), 0, self.length);
        }
        *self.initialized = true;
        Ok(())
    }

    /// Records that one platform-private write path initialized this target.
    ///
    /// # Safety
    ///
    /// Before calling this method, the caller must prove that its private write
    /// path (such as a retained same-object alias) addresses the same memory
    /// object and exact byte range as this target, that every target byte has
    /// been initialized through that path, and that those writes happen before
    /// any platform permission exposes the original mapping to uncoordinated
    /// access. All involved mappings must remain valid for the complete call.
    pub unsafe fn assume_initialized_by_platform(&mut self) -> Result<(), LazyGuestMemoryError> {
        if *self.initialized {
            return Err(LazyGuestMemoryError::ContentAlreadyInstalled);
        }
        *self.initialized = true;
        Ok(())
    }
}

impl fmt::Debug for LazyGuestMemoryTarget<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LazyGuestMemoryTarget(<redacted>)")
    }
}

/// Private-anonymous mappings plus one bounded page coordinator.
pub struct LazyGuestMemory {
    inner: Arc<LazyGuestMemoryInner>,
}

impl LazyGuestMemory {
    /// Validates all metadata and transactionally constructs absent mappings.
    pub fn new(
        limits: LazyGuestMemoryLimits,
        regions: Vec<LazyGuestMemoryRegion>,
    ) -> Result<Self, LazyGuestMemoryError> {
        let page_size = limits.pager.page_size();
        if regions.len() != usize::from(limits.pager.region_count())
            || regions.is_empty()
            || regions.len() > usize::from(MAX_REGIONS)
            || limits.pager.max_in_flight() == 0
            || limits.pager.max_in_flight() > MAX_IN_FLIGHT
        {
            return Err(LazyGuestMemoryError::InvalidConfiguration);
        }

        validate_lazy_regions(&regions, page_size)?;

        let mut layout_ranges = Vec::new();
        layout_ranges
            .try_reserve_exact(regions.len())
            .map_err(|source| LazyGuestMemoryError::MetadataAllocationFailed { source })?;
        layout_ranges.extend(regions.iter().map(|region| region.guest));
        let layout = GuestMemoryLayout::new(layout_ranges)
            .map_err(|_| LazyGuestMemoryError::InvalidConfiguration)?;

        let mut records = Vec::new();
        records
            .try_reserve_exact(regions.len())
            .map_err(|source| LazyGuestMemoryError::MetadataAllocationFailed { source })?;
        let mut total_pages = 0_u64;
        for region in regions {
            let page_count = region
                .length()
                .checked_div(u64::from(page_size))
                .ok_or(LazyGuestMemoryError::InvalidConfiguration)?;
            let first_page = usize::try_from(total_pages)
                .map_err(|_| LazyGuestMemoryError::InvalidConfiguration)?;
            let page_count_usize = usize::try_from(page_count)
                .map_err(|_| LazyGuestMemoryError::InvalidConfiguration)?;
            total_pages = total_pages
                .checked_add(page_count)
                .ok_or(LazyGuestMemoryError::InvalidConfiguration)?;
            records.push(LazyRegionRecord {
                source: region,
                first_page,
                page_count: page_count_usize,
            });
        }
        if total_pages == 0 || total_pages > limits.max_pages {
            return Err(LazyGuestMemoryError::PageLimitExceeded);
        }
        let total_pages =
            usize::try_from(total_pages).map_err(|_| LazyGuestMemoryError::InvalidConfiguration)?;

        let mut pages = Vec::new();
        pages
            .try_reserve_exact(total_pages)
            .map_err(|source| LazyGuestMemoryError::MetadataAllocationFailed { source })?;
        pages.resize(total_pages, PageTag::Absent);

        let operation_capacity = usize::from(limits.pager.max_in_flight());
        let mut operations = Vec::new();
        operations
            .try_reserve_exact(operation_capacity)
            .map_err(|source| LazyGuestMemoryError::MetadataAllocationFailed { source })?;
        let mut completions = Vec::new();
        completions
            .try_reserve_exact(usize::from(limits.max_waiters))
            .map_err(|source| LazyGuestMemoryError::MetadataAllocationFailed { source })?;

        let memory = GuestMemory::allocate(&layout)
            .map_err(|source| LazyGuestMemoryError::GuestMemoryAllocation { source })?;
        if memory.regions().len() != records.len()
            || memory
                .regions()
                .iter()
                .any(|region| region.backing() != GuestMemoryRegionBacking::Anonymous)
        {
            return Err(LazyGuestMemoryError::InvalidConfiguration);
        }

        Ok(Self {
            inner: Arc::new(LazyGuestMemoryInner {
                limits,
                regions: records,
                memory,
                state: Mutex::new(CoordinatorState {
                    phase: CoordinatorPhase::Active,
                    pages,
                    operations,
                    completions,
                    waiter_count: 0,
                    action_count: 0,
                    next_generation: 1,
                }),
                changed: Condvar::new(),
            }),
        })
    }

    /// Returns mapping metadata for later in-process backend adapters.
    pub fn mapping_regions(&self) -> &[GuestMemoryRegion] {
        self.inner.memory.regions()
    }

    /// Returns the exact coordinator page size selected by the pager limits.
    pub fn page_size(&self) -> u32 {
        self.inner.limits.pager.page_size()
    }

    /// Returns the exact immutable coordinator region count.
    pub fn region_count(&self) -> usize {
        self.inner.regions.len()
    }

    /// Acquires the page containing one guest address.
    pub fn fault_address(
        &self,
        address: GuestAddress,
        access: PageAccess,
    ) -> Result<LazyPageFault, LazyGuestMemoryError> {
        let location = self.inner.location_for_address(address)?;
        self.inner.fault(location, access)
    }

    /// Acquires one exact region-relative page.
    pub fn fault_page(
        &self,
        region: PagerRegionId,
        offset: u64,
        access: PageAccess,
    ) -> Result<LazyPageFault, LazyGuestMemoryError> {
        let location = self.inner.location_for_page(region, offset)?;
        self.inner.fault(location, access)
    }

    /// Starts one aligned, nonempty, single-region removal.
    ///
    /// This waits for overlapping publication/removal guards. Callers must not
    /// retain such a guard on the same thread while invoking this method.
    pub fn begin_removal(
        &self,
        region: PagerRegionId,
        offset: u64,
        length: u64,
    ) -> Result<LazyPageRemoval, LazyGuestMemoryError> {
        self.inner.begin_removal(region, offset, length)
    }

    /// Returns one page's current public state.
    pub fn page_state(
        &self,
        region: PagerRegionId,
        offset: u64,
    ) -> Result<LazyPageState, LazyGuestMemoryError> {
        let location = self.inner.location_for_page(region, offset)?;
        self.inner.page_state(location)
    }

    /// Returns the current owner terminal reason, if admission is closed.
    pub fn terminal_reason(
        &self,
    ) -> Result<Option<LazyGuestMemoryTerminalReason>, LazyGuestMemoryError> {
        let state = self.inner.lock_state()?;
        Ok(state.phase.reason())
    }

    /// Returns the currently retained protocol-operation count.
    pub fn operation_count(&self) -> Result<usize, LazyGuestMemoryError> {
        let state = self.inner.lock_state()?;
        Ok(state.operations.len())
    }

    /// Returns the currently admitted waiter count.
    pub fn waiter_count(&self) -> Result<usize, LazyGuestMemoryError> {
        let state = self.inner.lock_state()?;
        Ok(state.waiter_count)
    }

    /// Closes admission and waits for already-linearized actions.
    ///
    /// Actions are non-reentrant: call this only from a thread that retains no
    /// publication or removal guard for this owner.
    pub fn terminate(
        &self,
        reason: LazyGuestMemoryTerminalReason,
    ) -> Result<(), LazyGuestMemoryError> {
        self.inner.terminate(reason)
    }
}

impl fmt::Debug for LazyGuestMemory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LazyGuestMemory")
            .field("regions", &"<redacted>")
            .field("contents", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl Drop for LazyGuestMemory {
    fn drop(&mut self) {
        self.inner
            .close_nonblocking(LazyGuestMemoryTerminalReason::Teardown);
    }
}

#[derive(Clone, Copy)]
struct LazyRegionRecord {
    source: LazyGuestMemoryRegion,
    first_page: usize,
    page_count: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PageLocation {
    region_index: usize,
    page_in_region: usize,
    flat_index: usize,
}

struct LazyGuestMemoryInner {
    limits: LazyGuestMemoryLimits,
    regions: Vec<LazyRegionRecord>,
    memory: GuestMemory,
    state: Mutex<CoordinatorState>,
    changed: Condvar,
}

struct CoordinatorState {
    phase: CoordinatorPhase,
    pages: Vec<PageTag>,
    operations: Vec<Operation>,
    completions: Vec<Completion>,
    waiter_count: usize,
    action_count: usize,
    next_generation: u64,
}

#[derive(Clone, Copy)]
enum CoordinatorPhase {
    Active,
    Closing(LazyGuestMemoryTerminalReason),
    Terminal(LazyGuestMemoryTerminalReason),
}

impl CoordinatorPhase {
    const fn reason(self) -> Option<LazyGuestMemoryTerminalReason> {
        match self {
            Self::Active => None,
            Self::Closing(reason) | Self::Terminal(reason) => Some(reason),
        }
    }

    const fn active(self) -> bool {
        matches!(self, Self::Active)
    }
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum PageTag {
    Absent,
    Loading,
    Publishing,
    Present,
    Removing,
}

const _: () = assert!(std::mem::size_of::<PageTag>() == 1);

struct Operation {
    generation: PagerGeneration,
    kind: OperationKind,
}

enum OperationKind {
    Population {
        location: PageLocation,
        stage: PopulationStage,
        waiters: usize,
    },
    Removal {
        region_index: usize,
        first_page: usize,
        page_count: usize,
    },
}

impl OperationKind {
    const fn is_action(&self) -> bool {
        matches!(
            self,
            Self::Population {
                stage: PopulationStage::Publishing,
                ..
            } | Self::Removal { .. }
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PopulationStage {
    Loading,
    Publishing,
    Retired,
}

struct Completion {
    location: PageLocation,
    generation: PagerGeneration,
    outcome: CompletionOutcome,
    remaining: usize,
}

#[derive(Clone, Copy)]
enum CompletionOutcome {
    Present,
    Superseded,
}

impl LazyGuestMemoryInner {
    fn region(
        &self,
        location: PageLocation,
    ) -> Result<LazyGuestMemoryRegion, LazyGuestMemoryError> {
        self.regions
            .get(location.region_index)
            .map(|region| region.source)
            .ok_or(LazyGuestMemoryError::InvalidPage)
    }

    fn page_offset(&self, location: PageLocation) -> Result<u64, LazyGuestMemoryError> {
        u64::try_from(location.page_in_region)
            .map_err(|_| LazyGuestMemoryError::InvalidPage)?
            .checked_mul(u64::from(self.limits.pager.page_size()))
            .ok_or(LazyGuestMemoryError::InvalidPage)
    }

    fn source_offset(&self, location: PageLocation) -> Result<u64, LazyGuestMemoryError> {
        self.region(location)?
            .source_offset()
            .checked_add(self.page_offset(location)?)
            .ok_or(LazyGuestMemoryError::InvalidPage)
    }

    fn page_guest_range(
        &self,
        location: PageLocation,
    ) -> Result<GuestMemoryRange, LazyGuestMemoryError> {
        let region = self.region(location)?;
        let start = region
            .guest_range()
            .start()
            .checked_add(self.page_offset(location)?)
            .ok_or(LazyGuestMemoryError::InvalidPage)?;
        GuestMemoryRange::new(start, u64::from(self.limits.pager.page_size()))
            .map_err(|_| LazyGuestMemoryError::InvalidPage)
    }

    fn location_for_address(
        &self,
        address: GuestAddress,
    ) -> Result<PageLocation, LazyGuestMemoryError> {
        let page_size = u64::from(self.limits.pager.page_size());
        for (region_index, record) in self.regions.iter().enumerate() {
            let range = record.source.guest_range();
            if !range.contains(address) {
                continue;
            }
            let offset = address.raw_value() - range.start().raw_value();
            let page_in_region = offset / page_size;
            return self.location(region_index, page_in_region);
        }
        Err(LazyGuestMemoryError::InvalidPage)
    }

    fn location_for_page(
        &self,
        region: PagerRegionId,
        offset: u64,
    ) -> Result<PageLocation, LazyGuestMemoryError> {
        let page_size = u64::from(self.limits.pager.page_size());
        if !offset.is_multiple_of(page_size) {
            return Err(LazyGuestMemoryError::InvalidPage);
        }
        for (region_index, record) in self.regions.iter().enumerate() {
            if record.source.id() != region {
                continue;
            }
            if offset
                .checked_add(page_size)
                .is_none_or(|end| end > record.source.length())
            {
                return Err(LazyGuestMemoryError::InvalidPage);
            }
            return self.location(region_index, offset / page_size);
        }
        Err(LazyGuestMemoryError::InvalidPage)
    }

    fn location(
        &self,
        region_index: usize,
        page_in_region: u64,
    ) -> Result<PageLocation, LazyGuestMemoryError> {
        let record = self
            .regions
            .get(region_index)
            .ok_or(LazyGuestMemoryError::InvalidPage)?;
        let page_in_region =
            usize::try_from(page_in_region).map_err(|_| LazyGuestMemoryError::InvalidPage)?;
        if page_in_region >= record.page_count {
            return Err(LazyGuestMemoryError::InvalidPage);
        }
        let flat_index = record
            .first_page
            .checked_add(page_in_region)
            .ok_or(LazyGuestMemoryError::InvalidPage)?;
        Ok(PageLocation {
            region_index,
            page_in_region,
            flat_index,
        })
    }

    fn target<'a>(
        &'a self,
        region_index: usize,
        region_offset: u64,
        length: u64,
        initialized: &'a mut bool,
    ) -> Result<LazyGuestMemoryTarget<'a>, LazyGuestMemoryError> {
        let record = self
            .regions
            .get(region_index)
            .ok_or(LazyGuestMemoryError::InvalidPage)?;
        let end = region_offset
            .checked_add(length)
            .ok_or(LazyGuestMemoryError::InvalidPage)?;
        if length == 0 || end > record.source.length() {
            return Err(LazyGuestMemoryError::InvalidPage);
        }
        let mapping = self
            .memory
            .regions()
            .get(region_index)
            .ok_or(LazyGuestMemoryError::InvalidPage)?;
        let offset =
            usize::try_from(region_offset).map_err(|_| LazyGuestMemoryError::InvalidPage)?;
        let length_usize =
            usize::try_from(length).map_err(|_| LazyGuestMemoryError::InvalidPage)?;
        if offset
            .checked_add(length_usize)
            .is_none_or(|mapping_end| mapping_end > mapping.host_size())
        {
            return Err(LazyGuestMemoryError::InvalidPage);
        }
        let address = mapping
            .host_address()
            .as_ptr()
            .cast::<u8>()
            .wrapping_add(offset)
            .cast::<c_void>();
        // SAFETY: the retained mapping has a non-null base, and the checked
        // nonempty offset/length range remains wholly inside it.
        let address = unsafe { NonNull::new_unchecked(address) };
        let start = record
            .source
            .guest_range()
            .start()
            .checked_add(region_offset)
            .ok_or(LazyGuestMemoryError::InvalidPage)?;
        let range =
            GuestMemoryRange::new(start, length).map_err(|_| LazyGuestMemoryError::InvalidPage)?;
        Ok(LazyGuestMemoryTarget {
            address,
            range,
            length: length_usize,
            initialized,
            _owner: self,
        })
    }

    fn fault(
        self: &Arc<Self>,
        location: PageLocation,
        access: PageAccess,
    ) -> Result<LazyPageFault, LazyGuestMemoryError> {
        let mut state = self.lock_state()?;
        loop {
            self.require_active(&state)?;
            let page = *state
                .pages
                .get(location.flat_index)
                .ok_or(LazyGuestMemoryError::InvalidPage)?;
            match page {
                PageTag::Absent => {
                    if state.operations.len() >= usize::from(self.limits.pager.max_in_flight()) {
                        return Err(LazyGuestMemoryError::InFlightLimitExceeded);
                    }
                    let region = self.region(location)?;
                    let offset = self.page_offset(location)?;
                    let source_offset = self.source_offset(location)?;
                    let guest_range = self.page_guest_range(location)?;
                    let generation = self.allocate_generation_locked(&mut state)?;
                    let page = state
                        .pages
                        .get_mut(location.flat_index)
                        .ok_or(LazyGuestMemoryError::InvalidPage)?;
                    *page = PageTag::Loading;
                    state.operations.push(Operation {
                        generation,
                        kind: OperationKind::Population {
                            location,
                            stage: PopulationStage::Loading,
                            waiters: 0,
                        },
                    });
                    return Ok(LazyPageFault::Populate(LazyPagePopulation {
                        inner: Arc::clone(self),
                        location,
                        region: region.id(),
                        generation,
                        access,
                        offset,
                        source_offset,
                        guest_range,
                        length: self.limits.pager.page_size(),
                        armed: true,
                    }));
                }
                PageTag::Present => return Ok(LazyPageFault::Present),
                PageTag::Loading | PageTag::Publishing => {
                    let (operation_index, generation) = population_operation_for_page(
                        &state.operations,
                        location,
                        page == PageTag::Publishing,
                    )
                    .ok_or(LazyGuestMemoryError::InvalidLifecycle)?;
                    self.admit_waiter(&mut state)?;
                    let operation = state
                        .operations
                        .get_mut(operation_index)
                        .ok_or(LazyGuestMemoryError::InvalidLifecycle)?;
                    let OperationKind::Population { waiters, .. } = &mut operation.kind else {
                        return Err(LazyGuestMemoryError::InvalidLifecycle);
                    };
                    *waiters = waiters
                        .checked_add(1)
                        .ok_or(LazyGuestMemoryError::WaiterLimitExceeded)?;
                    return self.wait_for_population(state, location, generation);
                }
                PageTag::Removing => {
                    state =
                        self.wait_for_page_change(state, location.flat_index, PageTag::Removing)?;
                }
            }
        }
    }

    fn wait_for_population(
        &self,
        mut state: MutexGuard<'_, CoordinatorState>,
        location: PageLocation,
        generation: PagerGeneration,
    ) -> Result<LazyPageFault, LazyGuestMemoryError> {
        loop {
            if let Some(reason) = state.phase.reason() {
                state.waiter_count = state.waiter_count.saturating_sub(1);
                return Err(LazyGuestMemoryError::Terminal { reason });
            }
            if let Some(index) = state.completions.iter().position(|completion| {
                completion.location == location && completion.generation == generation
            }) {
                let completion = state
                    .completions
                    .get_mut(index)
                    .ok_or(LazyGuestMemoryError::InvalidLifecycle)?;
                let outcome = completion.outcome;
                completion.remaining = completion.remaining.saturating_sub(1);
                let remove = completion.remaining == 0;
                if remove {
                    state.completions.remove(index);
                }
                state.waiter_count = state.waiter_count.saturating_sub(1);
                return match outcome {
                    CompletionOutcome::Present => Ok(LazyPageFault::Present),
                    CompletionOutcome::Superseded => Err(LazyGuestMemoryError::StaleGeneration),
                };
            }
            let retained = state.operations.iter().any(|operation| {
                operation.generation == generation
                    && matches!(
                        operation.kind,
                        OperationKind::Population {
                            location: operation_location,
                            ..
                        } if operation_location == location
                    )
            });
            if !retained {
                state.waiter_count = state.waiter_count.saturating_sub(1);
                return Err(LazyGuestMemoryError::InvalidLifecycle);
            }
            state = self.wait_state(state)?;
        }
    }

    fn wait_for_page_change<'a>(
        &self,
        mut state: MutexGuard<'a, CoordinatorState>,
        flat_index: usize,
        blocked: PageTag,
    ) -> Result<MutexGuard<'a, CoordinatorState>, LazyGuestMemoryError> {
        self.admit_waiter(&mut state)?;
        loop {
            if let Some(reason) = state.phase.reason() {
                state.waiter_count = state.waiter_count.saturating_sub(1);
                return Err(LazyGuestMemoryError::Terminal { reason });
            }
            if state.pages.get(flat_index).copied() != Some(blocked) {
                state.waiter_count = state.waiter_count.saturating_sub(1);
                return Ok(state);
            }
            state = self.wait_state(state)?;
        }
    }

    fn begin_publication(
        self: &Arc<Self>,
        location: PageLocation,
        generation: PagerGeneration,
    ) -> Result<LazyPagePublication, LazyGuestMemoryError> {
        let region_offset = self.page_offset(location)?;
        let mut state = self.lock_state()?;
        self.require_active(&state)?;
        let Some(index) = state
            .operations
            .iter()
            .position(|operation| operation.generation == generation)
        else {
            return Err(LazyGuestMemoryError::StaleGeneration);
        };
        let stage = match state.operations.get(index).map(|operation| &operation.kind) {
            Some(OperationKind::Population {
                location: operation_location,
                stage,
                ..
            }) if *operation_location == location => *stage,
            _ => {
                self.begin_closing_locked(
                    &mut state,
                    LazyGuestMemoryTerminalReason::TransitionFailure,
                );
                self.changed.notify_all();
                return Err(LazyGuestMemoryError::InvalidLifecycle);
            }
        };
        match stage {
            PopulationStage::Retired => {
                state.operations.remove(index);
                self.changed.notify_all();
                Err(LazyGuestMemoryError::StaleGeneration)
            }
            PopulationStage::Loading => {
                if state.pages.get(location.flat_index).copied() != Some(PageTag::Loading) {
                    self.begin_closing_locked(
                        &mut state,
                        LazyGuestMemoryTerminalReason::TransitionFailure,
                    );
                    self.changed.notify_all();
                    return Err(LazyGuestMemoryError::InvalidLifecycle);
                }
                let operation = state
                    .operations
                    .get_mut(index)
                    .ok_or(LazyGuestMemoryError::InvalidLifecycle)?;
                let OperationKind::Population { stage, .. } = &mut operation.kind else {
                    return Err(LazyGuestMemoryError::InvalidLifecycle);
                };
                *stage = PopulationStage::Publishing;
                let page = state
                    .pages
                    .get_mut(location.flat_index)
                    .ok_or(LazyGuestMemoryError::InvalidPage)?;
                *page = PageTag::Publishing;
                state.action_count = state
                    .action_count
                    .checked_add(1)
                    .ok_or(LazyGuestMemoryError::InvalidLifecycle)?;
                Ok(LazyPagePublication {
                    inner: Arc::clone(self),
                    location,
                    region_offset,
                    generation,
                    initialized: false,
                    finished: false,
                })
            }
            PopulationStage::Publishing => {
                self.begin_closing_locked(
                    &mut state,
                    LazyGuestMemoryTerminalReason::TransitionFailure,
                );
                self.changed.notify_all();
                Err(LazyGuestMemoryError::InvalidLifecycle)
            }
        }
    }

    fn commit_publication(
        &self,
        location: PageLocation,
        generation: PagerGeneration,
    ) -> Result<(), LazyGuestMemoryError> {
        let mut state = self.lock_state()?;
        if !state.phase.active() {
            self.finish_action_locked(&mut state, generation);
            self.changed.notify_all();
            return Err(self.phase_error(&state));
        }
        self.require_action_locked(&mut state)?;
        let index = publishing_operation(&state.operations, location, generation)
            .ok_or(LazyGuestMemoryError::InvalidLifecycle)?;
        let waiters = match state.operations.get(index).map(|operation| &operation.kind) {
            Some(OperationKind::Population { waiters, .. }) => *waiters,
            _ => return Err(LazyGuestMemoryError::InvalidLifecycle),
        };
        state.operations.remove(index);
        let page = state
            .pages
            .get_mut(location.flat_index)
            .ok_or(LazyGuestMemoryError::InvalidPage)?;
        *page = PageTag::Present;
        self.record_completion(
            &mut state,
            location,
            generation,
            CompletionOutcome::Present,
            waiters,
        );
        state.action_count -= 1;
        self.finalize_if_drained_locked(&mut state);
        self.changed.notify_all();
        Ok(())
    }

    fn drop_population(&self, location: PageLocation, generation: PagerGeneration) {
        let mut state = self.lock_state_for_drop();
        let Some(index) = state
            .operations
            .iter()
            .position(|operation| operation.generation == generation)
        else {
            return;
        };
        let stage = match state.operations.get(index).map(|operation| &operation.kind) {
            Some(OperationKind::Population {
                location: operation_location,
                stage,
                ..
            }) if *operation_location == location => *stage,
            _ => return,
        };
        match stage {
            PopulationStage::Retired => {
                state.operations.remove(index);
            }
            PopulationStage::Loading | PopulationStage::Publishing => {
                self.begin_closing_locked(
                    &mut state,
                    LazyGuestMemoryTerminalReason::TransitionFailure,
                );
            }
        }
        self.changed.notify_all();
    }

    fn drop_publication(&self, location: PageLocation, generation: PagerGeneration) {
        let mut state = self.lock_state_for_drop();
        if publishing_operation(&state.operations, location, generation).is_some() {
            self.finish_action_locked(&mut state, generation);
            self.begin_closing_locked(&mut state, LazyGuestMemoryTerminalReason::TransitionFailure);
            self.changed.notify_all();
        }
    }

    fn begin_removal(
        self: &Arc<Self>,
        region: PagerRegionId,
        offset: u64,
        length: u64,
    ) -> Result<LazyPageRemoval, LazyGuestMemoryError> {
        let (region_index, first_page, page_count) =
            self.removal_location(region, offset, length)?;
        let source_offset = self
            .regions
            .get(region_index)
            .ok_or(LazyGuestMemoryError::InvalidPage)?
            .source
            .source_offset()
            .checked_add(offset)
            .ok_or(LazyGuestMemoryError::InvalidPage)?;
        let mut state = self.lock_state()?;
        loop {
            self.require_active(&state)?;
            if self.range_has_action(&state, first_page, page_count)? {
                state = self.wait_for_range_actions(state, first_page, page_count)?;
                continue;
            }
            if state.operations.len() >= usize::from(self.limits.pager.max_in_flight()) {
                return Err(LazyGuestMemoryError::InFlightLimitExceeded);
            }
            self.validate_removal_transition(&state, first_page, page_count)?;
            let generation = self.allocate_generation_locked(&mut state)?;
            state.operations.push(Operation {
                generation,
                kind: OperationKind::Removal {
                    region_index,
                    first_page,
                    page_count,
                },
            });
            state.action_count = state
                .action_count
                .checked_add(1)
                .ok_or(LazyGuestMemoryError::InvalidLifecycle)?;

            let end = first_page
                .checked_add(page_count)
                .ok_or(LazyGuestMemoryError::InvalidPage)?;
            for flat_index in first_page..end {
                if state.pages.get(flat_index).copied() == Some(PageTag::Loading) {
                    self.retire_population_for_page(&mut state, flat_index)?;
                }
                let page = state
                    .pages
                    .get_mut(flat_index)
                    .ok_or(LazyGuestMemoryError::InvalidPage)?;
                *page = PageTag::Removing;
            }
            self.changed.notify_all();
            return Ok(LazyPageRemoval {
                inner: Arc::clone(self),
                region,
                region_index,
                first_page,
                page_count,
                region_offset: offset,
                source_offset,
                length,
                generation,
                zeroed: false,
                finished: false,
            });
        }
    }

    fn removal_location(
        &self,
        region: PagerRegionId,
        offset: u64,
        length: u64,
    ) -> Result<(usize, usize, usize), LazyGuestMemoryError> {
        let page_size = u64::from(self.limits.pager.page_size());
        if length == 0 || !offset.is_multiple_of(page_size) || !length.is_multiple_of(page_size) {
            return Err(LazyGuestMemoryError::InvalidPage);
        }
        for (region_index, record) in self.regions.iter().enumerate() {
            if record.source.id() != region {
                continue;
            }
            if offset
                .checked_add(length)
                .is_none_or(|end| end > record.source.length())
            {
                return Err(LazyGuestMemoryError::InvalidPage);
            }
            let page_in_region = usize::try_from(offset / page_size)
                .map_err(|_| LazyGuestMemoryError::InvalidPage)?;
            let page_count = usize::try_from(length / page_size)
                .map_err(|_| LazyGuestMemoryError::InvalidPage)?;
            let first_page = record
                .first_page
                .checked_add(page_in_region)
                .ok_or(LazyGuestMemoryError::InvalidPage)?;
            return Ok((region_index, first_page, page_count));
        }
        Err(LazyGuestMemoryError::InvalidPage)
    }

    fn range_has_action(
        &self,
        state: &CoordinatorState,
        first_page: usize,
        page_count: usize,
    ) -> Result<bool, LazyGuestMemoryError> {
        let end = first_page
            .checked_add(page_count)
            .ok_or(LazyGuestMemoryError::InvalidPage)?;
        for index in first_page..end {
            if matches!(
                state.pages.get(index),
                Some(PageTag::Publishing | PageTag::Removing)
            ) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn wait_for_range_actions<'a>(
        &self,
        mut state: MutexGuard<'a, CoordinatorState>,
        first_page: usize,
        page_count: usize,
    ) -> Result<MutexGuard<'a, CoordinatorState>, LazyGuestMemoryError> {
        self.admit_waiter(&mut state)?;
        loop {
            if let Some(reason) = state.phase.reason() {
                state.waiter_count = state.waiter_count.saturating_sub(1);
                return Err(LazyGuestMemoryError::Terminal { reason });
            }
            if !self.range_has_action(&state, first_page, page_count)? {
                state.waiter_count = state.waiter_count.saturating_sub(1);
                return Ok(state);
            }
            state = self.wait_state(state)?;
        }
    }

    fn validate_removal_transition(
        &self,
        state: &CoordinatorState,
        first_page: usize,
        page_count: usize,
    ) -> Result<(), LazyGuestMemoryError> {
        let end = first_page
            .checked_add(page_count)
            .ok_or(LazyGuestMemoryError::InvalidPage)?;
        let mut completion_slots = 0_usize;
        for flat_index in first_page..end {
            let page = state
                .pages
                .get(flat_index)
                .copied()
                .ok_or(LazyGuestMemoryError::InvalidPage)?;
            match page {
                PageTag::Absent | PageTag::Present => {}
                PageTag::Loading => {
                    let Some(operation) = state.operations.iter().find(|operation| {
                        matches!(
                            operation.kind,
                            OperationKind::Population {
                                location,
                                stage: PopulationStage::Loading,
                                ..
                            } if location.flat_index == flat_index
                        )
                    }) else {
                        return Err(LazyGuestMemoryError::InvalidLifecycle);
                    };
                    if matches!(
                        operation.kind,
                        OperationKind::Population { waiters, .. } if waiters != 0
                    ) {
                        completion_slots = completion_slots
                            .checked_add(1)
                            .ok_or(LazyGuestMemoryError::InvalidLifecycle)?;
                    }
                }
                PageTag::Publishing | PageTag::Removing => {
                    return Err(LazyGuestMemoryError::InvalidLifecycle);
                }
            }
        }
        if state
            .completions
            .len()
            .checked_add(completion_slots)
            .is_none_or(|required| required > state.completions.capacity())
        {
            return Err(LazyGuestMemoryError::WaiterLimitExceeded);
        }
        Ok(())
    }

    fn retire_population_for_page(
        &self,
        state: &mut CoordinatorState,
        flat_index: usize,
    ) -> Result<(), LazyGuestMemoryError> {
        let index = state
            .operations
            .iter()
            .position(|operation| {
                matches!(
                    operation.kind,
                    OperationKind::Population {
                        location,
                        stage: PopulationStage::Loading,
                        ..
                    } if location.flat_index == flat_index
                )
            })
            .ok_or(LazyGuestMemoryError::InvalidLifecycle)?;
        let (location, generation, waiters) = {
            let operation = state
                .operations
                .get_mut(index)
                .ok_or(LazyGuestMemoryError::InvalidLifecycle)?;
            let OperationKind::Population {
                location,
                stage,
                waiters,
            } = &mut operation.kind
            else {
                return Err(LazyGuestMemoryError::InvalidLifecycle);
            };
            let result = (*location, operation.generation, *waiters);
            *stage = PopulationStage::Retired;
            *waiters = 0;
            result
        };
        self.record_completion(
            state,
            location,
            generation,
            CompletionOutcome::Superseded,
            waiters,
        );
        Ok(())
    }

    fn commit_removal(
        &self,
        region_index: usize,
        first_page: usize,
        page_count: usize,
        generation: PagerGeneration,
    ) -> Result<(), LazyGuestMemoryError> {
        let mut state = self.lock_state()?;
        if !state.phase.active() {
            self.finish_action_locked(&mut state, generation);
            self.changed.notify_all();
            return Err(self.phase_error(&state));
        }
        self.require_action_locked(&mut state)?;
        let index = removal_operation(
            &state.operations,
            region_index,
            first_page,
            page_count,
            generation,
        )
        .ok_or(LazyGuestMemoryError::InvalidLifecycle)?;
        let end = first_page
            .checked_add(page_count)
            .ok_or(LazyGuestMemoryError::InvalidPage)?;
        for flat_index in first_page..end {
            let page = state
                .pages
                .get_mut(flat_index)
                .ok_or(LazyGuestMemoryError::InvalidPage)?;
            if *page != PageTag::Removing {
                return Err(LazyGuestMemoryError::InvalidLifecycle);
            }
            *page = PageTag::Absent;
        }
        state.operations.remove(index);
        state.action_count -= 1;
        self.finalize_if_drained_locked(&mut state);
        self.changed.notify_all();
        Ok(())
    }

    fn drop_removal(
        &self,
        region_index: usize,
        first_page: usize,
        page_count: usize,
        generation: PagerGeneration,
    ) {
        let mut state = self.lock_state_for_drop();
        if removal_operation(
            &state.operations,
            region_index,
            first_page,
            page_count,
            generation,
        )
        .is_some()
        {
            self.finish_action_locked(&mut state, generation);
            self.begin_closing_locked(&mut state, LazyGuestMemoryTerminalReason::TransitionFailure);
            self.changed.notify_all();
        }
    }

    fn page_state(&self, location: PageLocation) -> Result<LazyPageState, LazyGuestMemoryError> {
        let state = self.lock_state()?;
        if !state.phase.active() {
            return Ok(LazyPageState::Terminal);
        }
        match state.pages.get(location.flat_index).copied() {
            Some(PageTag::Absent) => Ok(LazyPageState::Absent),
            Some(PageTag::Loading) => Ok(LazyPageState::Loading),
            Some(PageTag::Publishing) => Ok(LazyPageState::Publishing),
            Some(PageTag::Present) => Ok(LazyPageState::Present),
            Some(PageTag::Removing) => Ok(LazyPageState::Removing),
            None => Err(LazyGuestMemoryError::InvalidPage),
        }
    }

    fn terminate(&self, reason: LazyGuestMemoryTerminalReason) -> Result<(), LazyGuestMemoryError> {
        let mut state = self.lock_state()?;
        self.begin_closing_locked(&mut state, reason);
        self.changed.notify_all();
        while state.action_count != 0 {
            state = self.wait_state(state)?;
        }
        self.finalize_if_drained_locked(&mut state);
        self.changed.notify_all();
        Ok(())
    }

    fn close_nonblocking(&self, reason: LazyGuestMemoryTerminalReason) {
        let mut state = self.lock_state_for_drop();
        self.begin_closing_locked(&mut state, reason);
        self.changed.notify_all();
    }

    fn admit_waiter(&self, state: &mut CoordinatorState) -> Result<(), LazyGuestMemoryError> {
        if state.waiter_count >= usize::from(self.limits.max_waiters) {
            return Err(LazyGuestMemoryError::WaiterLimitExceeded);
        }
        state.waiter_count = state
            .waiter_count
            .checked_add(1)
            .ok_or(LazyGuestMemoryError::WaiterLimitExceeded)?;
        Ok(())
    }

    fn allocate_generation_locked(
        &self,
        state: &mut CoordinatorState,
    ) -> Result<PagerGeneration, LazyGuestMemoryError> {
        match allocate_generation(state) {
            Ok(generation) => Ok(generation),
            Err(error) => {
                self.begin_closing_locked(state, LazyGuestMemoryTerminalReason::TransitionFailure);
                self.changed.notify_all();
                Err(error)
            }
        }
    }

    fn record_completion(
        &self,
        state: &mut CoordinatorState,
        location: PageLocation,
        generation: PagerGeneration,
        outcome: CompletionOutcome,
        waiters: usize,
    ) {
        if waiters == 0 {
            return;
        }
        debug_assert!(state.completions.len() < state.completions.capacity());
        state.completions.push(Completion {
            location,
            generation,
            outcome,
            remaining: waiters,
        });
    }

    fn require_active(&self, state: &CoordinatorState) -> Result<(), LazyGuestMemoryError> {
        if state.phase.active() {
            Ok(())
        } else {
            Err(self.phase_error(state))
        }
    }

    fn require_action_locked(
        &self,
        state: &mut CoordinatorState,
    ) -> Result<(), LazyGuestMemoryError> {
        if state.action_count != 0 {
            return Ok(());
        }
        self.begin_closing_locked(state, LazyGuestMemoryTerminalReason::TransitionFailure);
        self.changed.notify_all();
        Err(LazyGuestMemoryError::InvalidLifecycle)
    }

    fn phase_error(&self, state: &CoordinatorState) -> LazyGuestMemoryError {
        LazyGuestMemoryError::Terminal {
            reason: state
                .phase
                .reason()
                .unwrap_or(LazyGuestMemoryTerminalReason::TransitionFailure),
        }
    }

    fn begin_closing_locked(
        &self,
        state: &mut CoordinatorState,
        reason: LazyGuestMemoryTerminalReason,
    ) {
        if !state.phase.active() {
            return;
        }
        state.phase = CoordinatorPhase::Closing(reason);
        state
            .operations
            .retain(|operation| operation.kind.is_action());
        state.completions.clear();
        self.finalize_if_drained_locked(state);
    }

    fn finalize_if_drained_locked(&self, state: &mut CoordinatorState) {
        if state.action_count != 0 {
            return;
        }
        if let CoordinatorPhase::Closing(reason) = state.phase {
            state.phase = CoordinatorPhase::Terminal(reason);
            state.operations.clear();
            state.completions.clear();
        }
    }

    fn finish_action_locked(&self, state: &mut CoordinatorState, generation: PagerGeneration) {
        if let Some(index) = state
            .operations
            .iter()
            .position(|operation| operation.generation == generation && operation.kind.is_action())
        {
            state.operations.remove(index);
            if state.action_count == 0 {
                self.begin_closing_locked(state, LazyGuestMemoryTerminalReason::TransitionFailure);
            } else {
                state.action_count -= 1;
            }
        }
        self.finalize_if_drained_locked(state);
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, CoordinatorState>, LazyGuestMemoryError> {
        match self.state.lock() {
            Ok(state) => Ok(state),
            Err(poisoned) => {
                let mut state = self.recover_poison(poisoned);
                self.begin_closing_locked(&mut state, LazyGuestMemoryTerminalReason::StatePoisoned);
                state.waiter_count = 0;
                self.changed.notify_all();
                Err(LazyGuestMemoryError::StatePoisoned)
            }
        }
    }

    fn lock_state_for_drop(&self) -> MutexGuard<'_, CoordinatorState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = self.recover_poison(poisoned);
                self.begin_closing_locked(&mut state, LazyGuestMemoryTerminalReason::StatePoisoned);
                state.waiter_count = 0;
                state
            }
        }
    }

    fn wait_state<'a>(
        &self,
        state: MutexGuard<'a, CoordinatorState>,
    ) -> Result<MutexGuard<'a, CoordinatorState>, LazyGuestMemoryError> {
        match self.changed.wait(state) {
            Ok(state) => Ok(state),
            Err(poisoned) => {
                let mut state = self.recover_poison(poisoned);
                self.begin_closing_locked(&mut state, LazyGuestMemoryTerminalReason::StatePoisoned);
                state.waiter_count = 0;
                self.changed.notify_all();
                Err(LazyGuestMemoryError::StatePoisoned)
            }
        }
    }

    fn recover_poison<'a>(
        &self,
        poisoned: PoisonError<MutexGuard<'a, CoordinatorState>>,
    ) -> MutexGuard<'a, CoordinatorState> {
        poisoned.into_inner()
    }
}

fn validate_lazy_regions(
    regions: &[LazyGuestMemoryRegion],
    page_size: u32,
) -> Result<(), LazyGuestMemoryError> {
    for (index, region) in regions.iter().copied().enumerate() {
        region
            .guest_range()
            .validate_alignment(u64::from(page_size))
            .map_err(|_| LazyGuestMemoryError::InvalidConfiguration)?;
        PagerRegion::new(
            region.id(),
            region.source_offset(),
            region.length(),
            page_size,
        )
        .map_err(|_| LazyGuestMemoryError::InvalidConfiguration)?;
        for previous in regions.iter().copied().take(index) {
            if previous.id() == region.id()
                || source_ranges_overlap(
                    previous.source_offset(),
                    previous.length(),
                    region.source_offset(),
                    region.length(),
                )
            {
                return Err(LazyGuestMemoryError::InvalidConfiguration);
            }
        }
    }
    Ok(())
}

fn source_ranges_overlap(
    first_start: u64,
    first_len: u64,
    second_start: u64,
    second_len: u64,
) -> bool {
    let Some(first_end) = first_start.checked_add(first_len) else {
        return true;
    };
    let Some(second_end) = second_start.checked_add(second_len) else {
        return true;
    };
    first_start < second_end && second_start < first_end
}

fn allocate_generation(
    state: &mut CoordinatorState,
) -> Result<PagerGeneration, LazyGuestMemoryError> {
    let next = state
        .next_generation
        .checked_add(1)
        .ok_or(LazyGuestMemoryError::GenerationExhausted)?;
    let generation = PagerGeneration::new(state.next_generation)
        .map_err(|_| LazyGuestMemoryError::GenerationExhausted)?;
    state.next_generation = next;
    Ok(generation)
}

fn population_operation_for_page(
    operations: &[Operation],
    location: PageLocation,
    publishing: bool,
) -> Option<(usize, PagerGeneration)> {
    let expected = if publishing {
        PopulationStage::Publishing
    } else {
        PopulationStage::Loading
    };
    operations
        .iter()
        .enumerate()
        .find_map(|(index, operation)| {
            matches!(
                operation.kind,
                OperationKind::Population {
                    location: operation_location,
                    stage,
                    ..
                } if operation_location == location && stage == expected
            )
            .then_some((index, operation.generation))
        })
}

fn publishing_operation(
    operations: &[Operation],
    location: PageLocation,
    generation: PagerGeneration,
) -> Option<usize> {
    operations.iter().position(|operation| {
        operation.generation == generation
            && matches!(
                operation.kind,
                OperationKind::Population {
                    location: operation_location,
                    stage: PopulationStage::Publishing,
                    ..
                } if operation_location == location
            )
    })
}

fn removal_operation(
    operations: &[Operation],
    region_index: usize,
    first_page: usize,
    page_count: usize,
    generation: PagerGeneration,
) -> Option<usize> {
    operations.iter().position(|operation| {
        operation.generation == generation
            && matches!(
                operation.kind,
                OperationKind::Removal {
                    region_index: operation_region,
                    first_page: operation_first,
                    page_count: operation_count,
                } if operation_region == region_index
                    && operation_first == first_page
                    && operation_count == page_count
            )
    })
}

/// Value-redacted lazy-memory construction or lifecycle failure.
pub enum LazyGuestMemoryError {
    /// Configuration or region metadata is invalid.
    InvalidConfiguration,
    /// Coordinator metadata could not be reserved.
    MetadataAllocationFailed { source: TryReserveError },
    /// Anonymous mapping construction failed.
    GuestMemoryAllocation { source: GuestMemoryAllocationError },
    /// The requested page or range is invalid.
    InvalidPage,
    /// The configured total page bound was exceeded.
    PageLimitExceeded,
    /// The negotiated operation bound was reached.
    InFlightLimitExceeded,
    /// The local waiter bound was reached.
    WaiterLimitExceeded,
    /// The supplied population generation was superseded.
    StaleGeneration,
    /// The requested transition is not legal in the current state.
    InvalidLifecycle,
    /// Supplied contents do not exactly fill the target.
    ContentLength,
    /// Contents were already installed through this guard.
    ContentAlreadyInstalled,
    /// A commit was attempted without installing data or zeroes.
    ContentMissing,
    /// The generation space cannot allocate another exact identity.
    GenerationExhausted,
    /// The complete owner is terminal.
    Terminal {
        reason: LazyGuestMemoryTerminalReason,
    },
    /// Synchronization state was poisoned.
    StatePoisoned,
}

impl fmt::Debug for LazyGuestMemoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "LazyGuestMemoryError::InvalidConfiguration",
            Self::MetadataAllocationFailed { .. } => {
                "LazyGuestMemoryError::MetadataAllocationFailed(<redacted>)"
            }
            Self::GuestMemoryAllocation { .. } => {
                "LazyGuestMemoryError::GuestMemoryAllocation(<redacted>)"
            }
            Self::InvalidPage => "LazyGuestMemoryError::InvalidPage",
            Self::PageLimitExceeded => "LazyGuestMemoryError::PageLimitExceeded",
            Self::InFlightLimitExceeded => "LazyGuestMemoryError::InFlightLimitExceeded",
            Self::WaiterLimitExceeded => "LazyGuestMemoryError::WaiterLimitExceeded",
            Self::StaleGeneration => "LazyGuestMemoryError::StaleGeneration",
            Self::InvalidLifecycle => "LazyGuestMemoryError::InvalidLifecycle",
            Self::ContentLength => "LazyGuestMemoryError::ContentLength",
            Self::ContentAlreadyInstalled => "LazyGuestMemoryError::ContentAlreadyInstalled",
            Self::ContentMissing => "LazyGuestMemoryError::ContentMissing",
            Self::GenerationExhausted => "LazyGuestMemoryError::GenerationExhausted",
            Self::Terminal { .. } => "LazyGuestMemoryError::Terminal(<redacted>)",
            Self::StatePoisoned => "LazyGuestMemoryError::StatePoisoned",
        })
    }
}

impl fmt::Display for LazyGuestMemoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "invalid lazy guest-memory configuration",
            Self::MetadataAllocationFailed { .. } => "lazy guest-memory metadata allocation failed",
            Self::GuestMemoryAllocation { .. } => "lazy guest-memory mapping allocation failed",
            Self::InvalidPage => "invalid lazy guest-memory page",
            Self::PageLimitExceeded => "lazy guest-memory page limit exceeded",
            Self::InFlightLimitExceeded => "lazy guest-memory operation limit exceeded",
            Self::WaiterLimitExceeded => "lazy guest-memory waiter limit exceeded",
            Self::StaleGeneration => "stale lazy guest-memory generation",
            Self::InvalidLifecycle => "invalid lazy guest-memory lifecycle transition",
            Self::ContentLength => "invalid lazy guest-memory content length",
            Self::ContentAlreadyInstalled => "lazy guest-memory content is already installed",
            Self::ContentMissing => "lazy guest-memory content was not installed",
            Self::GenerationExhausted => "lazy guest-memory generation space exhausted",
            Self::Terminal { .. } => "lazy guest memory is terminal",
            Self::StatePoisoned => "lazy guest-memory state is unavailable",
        })
    }
}

impl std::error::Error for LazyGuestMemoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MetadataAllocationFailed { source } => Some(source),
            Self::GuestMemoryAllocation { source } => Some(source),
            Self::InvalidConfiguration
            | Self::InvalidPage
            | Self::PageLimitExceeded
            | Self::InFlightLimitExceeded
            | Self::WaiterLimitExceeded
            | Self::StaleGeneration
            | Self::InvalidLifecycle
            | Self::ContentLength
            | Self::ContentAlreadyInstalled
            | Self::ContentMissing
            | Self::GenerationExhausted
            | Self::Terminal { .. }
            | Self::StatePoisoned => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant};

    use bangbang_pager::{MAX_FRAME_BYTES, PagerOperations};

    use super::*;

    const PAGE_SIZE: u32 = 16 * 1024;
    const GUEST_BASE: u64 = 0x8000_0000;
    const SOURCE_BASE: u64 = 0x10_0000;
    const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

    fn region_id(value: u32) -> PagerRegionId {
        PagerRegionId::new(value).expect("test region identity should be nonzero")
    }

    fn guest_range(start: u64, length: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(GuestAddress::new(start), length)
            .expect("test guest range should be valid")
    }

    fn pager_limits(region_count: u16, max_in_flight: u16, page_size: u32) -> PagerLimits {
        PagerLimits::new(
            page_size,
            region_count,
            max_in_flight,
            u32::try_from(MAX_FRAME_BYTES).expect("maximum frame size should fit u32"),
            PagerOperations::v1(),
        )
        .expect("test pager limits should be valid")
    }

    fn limits(
        region_count: u16,
        max_in_flight: u16,
        max_pages: u64,
        max_waiters: u16,
    ) -> LazyGuestMemoryLimits {
        LazyGuestMemoryLimits::new(
            pager_limits(region_count, max_in_flight, PAGE_SIZE),
            max_pages,
            max_waiters,
        )
        .expect("test lazy-memory limits should be valid")
    }

    fn lazy_region(
        id: u32,
        guest_start: u64,
        source_offset: u64,
        page_count: u64,
    ) -> LazyGuestMemoryRegion {
        let length = u64::from(PAGE_SIZE)
            .checked_mul(page_count)
            .expect("test region length should fit u64");
        LazyGuestMemoryRegion::new(
            region_id(id),
            guest_range(guest_start, length),
            source_offset,
            PAGE_SIZE,
        )
        .expect("test lazy-memory region should be valid")
    }

    fn owner(page_count: u64, max_in_flight: u16, max_waiters: u16) -> LazyGuestMemory {
        LazyGuestMemory::new(
            limits(1, max_in_flight, page_count, max_waiters),
            vec![lazy_region(1, GUEST_BASE, SOURCE_BASE, page_count)],
        )
        .expect("test lazy guest memory should construct")
    }

    fn population(fault: LazyPageFault) -> LazyPagePopulation {
        match fault {
            LazyPageFault::Populate(ticket) => ticket,
            LazyPageFault::Present => panic!("test fault should require population"),
        }
    }

    fn fault(memory: &LazyGuestMemory, offset: u64, access: PageAccess) -> LazyPagePopulation {
        population(
            memory
                .fault_page(region_id(1), offset, access)
                .expect("test page fault should succeed"),
        )
    }

    fn publish_bytes(ticket: LazyPagePopulation, contents: &[u8]) {
        let mut publication = ticket
            .begin_publication()
            .expect("test response should enter publication");
        publication
            .target()
            .expect("test publication target should exist")
            .copy_from_slice(contents)
            .expect("test contents should fit the target");
        publication
            .commit()
            .expect("test publication should commit");
    }

    fn publish_zero(ticket: LazyPagePopulation) {
        let mut publication = ticket
            .begin_publication()
            .expect("test response should enter publication");
        publication
            .target()
            .expect("test publication target should exist")
            .zero()
            .expect("test target should zero");
        publication
            .commit()
            .expect("test publication should commit");
    }

    fn read_page(memory: &LazyGuestMemory, offset: u64) -> Vec<u8> {
        let mut contents = vec![0_u8; PAGE_SIZE as usize];
        memory
            .inner
            .memory
            .read_slice(
                &mut contents,
                GuestAddress::new(
                    GUEST_BASE
                        .checked_add(offset)
                        .expect("test guest address should fit"),
                ),
            )
            .expect("test page should be readable internally");
        contents
    }

    fn wait_for_waiters(memory: &LazyGuestMemory, expected: usize) {
        let deadline = Instant::now() + WAIT_TIMEOUT;
        loop {
            if memory
                .waiter_count()
                .expect("test waiter count should be available")
                == expected
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {expected} lazy-memory waiters"
            );
            thread::yield_now();
        }
    }

    fn wait_for_reason(memory: &LazyGuestMemory, expected: LazyGuestMemoryTerminalReason) {
        let deadline = Instant::now() + WAIT_TIMEOUT;
        loop {
            if memory
                .terminal_reason()
                .expect("test terminal reason should be available")
                == Some(expected)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for a lazy-memory terminal reason"
            );
            thread::yield_now();
        }
    }

    #[test]
    fn constructs_absent_private_anonymous_regions_without_source_contents() {
        let memory = owner(2, 2, 4);

        assert_eq!(memory.region_count(), 1);
        assert_eq!(memory.page_size(), PAGE_SIZE);
        assert_eq!(memory.mapping_regions().len(), 1);
        assert_eq!(
            memory.mapping_regions()[0].backing(),
            GuestMemoryRegionBacking::Anonymous
        );
        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("first page state should be available"),
            LazyPageState::Absent
        );
        assert_eq!(
            memory
                .page_state(region_id(1), u64::from(PAGE_SIZE))
                .expect("second page state should be available"),
            LazyPageState::Absent
        );
        assert_eq!(
            memory
                .operation_count()
                .expect("operation count should be available"),
            0
        );
        assert_eq!(read_page(&memory, 0), vec![0; PAGE_SIZE as usize]);
    }

    #[test]
    fn platform_alias_proof_marks_one_exact_publication_once() {
        let memory = owner(1, 1, 1);
        let population = fault(&memory, 0, PageAccess::Read);
        let mut publication = population
            .begin_publication()
            .expect("test population should enter publication");
        let mut target = publication
            .target()
            .expect("test publication target should resolve");
        let installed = vec![0x5a; PAGE_SIZE as usize];
        // SAFETY: the target is live for exactly PAGE_SIZE bytes and the test
        // writes every byte before recording the platform publication proof.
        unsafe {
            std::ptr::copy_nonoverlapping(
                installed.as_ptr(),
                target.host_address().as_ptr().cast::<u8>(),
                installed.len(),
            );
            target
                .assume_initialized_by_platform()
                .expect("first platform proof should succeed");
        }
        // SAFETY: this second call intentionally exercises duplicate-proof
        // rejection while the same exact initialized target remains live.
        let duplicate = unsafe { target.assume_initialized_by_platform() };
        assert!(matches!(
            duplicate,
            Err(LazyGuestMemoryError::ContentAlreadyInstalled)
        ));
        publication
            .commit()
            .expect("platform-proven publication should commit");
        assert_eq!(read_page(&memory, 0), installed);
    }

    #[test]
    fn validates_local_limits_and_region_relationships_before_mapping() {
        let pager = pager_limits(1, 1, PAGE_SIZE);
        let selected_max_pages = aarch64::DRAM_MEM_MAX_SIZE / u64::from(PAGE_SIZE);
        assert!(matches!(
            LazyGuestMemoryLimits::new(pager, 0, 1),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));
        assert!(matches!(
            LazyGuestMemoryLimits::new(pager, selected_max_pages + 1, 1),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));
        assert!(LazyGuestMemoryLimits::new(pager, selected_max_pages, 1).is_ok());
        let minimum_page_pager = pager_limits(1, 1, MIN_PAGE_SIZE);
        assert!(LazyGuestMemoryLimits::new(minimum_page_pager, MAX_LAZY_MEMORY_PAGES, 1).is_ok());
        assert!(matches!(
            LazyGuestMemoryLimits::new(minimum_page_pager, MAX_LAZY_MEMORY_PAGES + 1, 1),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));
        assert!(matches!(
            LazyGuestMemoryLimits::new(pager, 1, 0),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));
        assert!(matches!(
            LazyGuestMemoryLimits::new(pager, 1, MAX_LAZY_MEMORY_WAITERS + 1),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));
        assert!(LazyGuestMemoryLimits::new(pager, 1, MAX_LAZY_MEMORY_WAITERS).is_ok());

        assert!(matches!(
            LazyGuestMemoryRegion::new(
                region_id(1),
                guest_range(GUEST_BASE + 1, u64::from(PAGE_SIZE)),
                SOURCE_BASE,
                PAGE_SIZE,
            ),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));
        assert!(matches!(
            LazyGuestMemoryRegion::new(
                region_id(1),
                guest_range(GUEST_BASE, u64::from(PAGE_SIZE) + 1),
                SOURCE_BASE,
                PAGE_SIZE,
            ),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));
        assert!(matches!(
            LazyGuestMemoryRegion::new(
                region_id(1),
                guest_range(GUEST_BASE, u64::from(PAGE_SIZE)),
                SOURCE_BASE + 1,
                PAGE_SIZE,
            ),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));
        let overflowing_source = u64::MAX - (u64::from(PAGE_SIZE) - 1);
        assert!(matches!(
            LazyGuestMemoryRegion::new(
                region_id(1),
                guest_range(GUEST_BASE, u64::from(PAGE_SIZE)),
                overflowing_source,
                PAGE_SIZE,
            ),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));

        assert!(matches!(
            LazyGuestMemory::new(limits(1, 1, 1, 1), Vec::new()),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));
        assert!(matches!(
            LazyGuestMemory::new(
                limits(2, 1, 2, 1),
                vec![lazy_region(1, GUEST_BASE, SOURCE_BASE, 1)]
            ),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));
        assert!(matches!(
            LazyGuestMemory::new(
                limits(2, 1, 2, 1),
                vec![
                    lazy_region(1, GUEST_BASE, SOURCE_BASE, 1),
                    lazy_region(
                        1,
                        GUEST_BASE + u64::from(PAGE_SIZE),
                        SOURCE_BASE + u64::from(PAGE_SIZE),
                        1,
                    ),
                ]
            ),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));
        assert!(matches!(
            LazyGuestMemory::new(
                limits(2, 1, 2, 1),
                vec![
                    lazy_region(1, GUEST_BASE, SOURCE_BASE, 1),
                    lazy_region(2, GUEST_BASE + u64::from(PAGE_SIZE), SOURCE_BASE, 1,),
                ]
            ),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));
        assert!(matches!(
            LazyGuestMemory::new(
                limits(2, 1, 2, 1),
                vec![
                    lazy_region(1, GUEST_BASE + u64::from(PAGE_SIZE), SOURCE_BASE, 1,),
                    lazy_region(2, GUEST_BASE, SOURCE_BASE + u64::from(PAGE_SIZE), 1,),
                ]
            ),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));
        assert!(matches!(
            LazyGuestMemory::new(
                limits(2, 1, 2, 1),
                vec![
                    lazy_region(1, GUEST_BASE, SOURCE_BASE, 1),
                    lazy_region(2, GUEST_BASE, SOURCE_BASE + u64::from(PAGE_SIZE), 1,),
                ]
            ),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));
        assert!(matches!(
            LazyGuestMemory::new(
                limits(1, 1, 1, 1),
                vec![lazy_region(1, GUEST_BASE, SOURCE_BASE, 2)]
            ),
            Err(LazyGuestMemoryError::PageLimitExceeded)
        ));

        let mismatched = LazyGuestMemoryRegion::new(
            region_id(1),
            guest_range(
                GUEST_BASE + u64::from(MIN_PAGE_SIZE),
                u64::from(MIN_PAGE_SIZE),
            ),
            u64::from(MIN_PAGE_SIZE),
            MIN_PAGE_SIZE,
        )
        .expect("4-KiB test region should validate in isolation");
        assert!(matches!(
            LazyGuestMemory::new(limits(1, 1, 1, 1), vec![mismatched],),
            Err(LazyGuestMemoryError::InvalidConfiguration)
        ));
    }

    #[test]
    fn rejects_invalid_page_and_removal_coordinates() {
        let memory = owner(2, 2, 2);
        let page = u64::from(PAGE_SIZE);

        assert!(matches!(
            memory.fault_page(region_id(2), 0, PageAccess::Read),
            Err(LazyGuestMemoryError::InvalidPage)
        ));
        assert!(matches!(
            memory.fault_page(region_id(1), 1, PageAccess::Read),
            Err(LazyGuestMemoryError::InvalidPage)
        ));
        assert!(matches!(
            memory.fault_page(region_id(1), page * 2, PageAccess::Read),
            Err(LazyGuestMemoryError::InvalidPage)
        ));
        assert!(matches!(
            memory.fault_address(GuestAddress::new(GUEST_BASE - 1), PageAccess::Read),
            Err(LazyGuestMemoryError::InvalidPage)
        ));
        assert!(matches!(
            memory.begin_removal(region_id(1), 0, 0),
            Err(LazyGuestMemoryError::InvalidPage)
        ));
        assert!(matches!(
            memory.begin_removal(region_id(1), 1, page),
            Err(LazyGuestMemoryError::InvalidPage)
        ));
        assert!(matches!(
            memory.begin_removal(region_id(1), page, page * 2),
            Err(LazyGuestMemoryError::InvalidPage)
        ));
    }

    #[test]
    fn accepts_the_exact_protocol_region_limit_and_rejects_one_more() {
        let regions = (0..MAX_REGIONS)
            .map(|index| {
                let offset = u64::from(index) * u64::from(PAGE_SIZE);
                lazy_region(
                    u32::from(index) + 1,
                    GUEST_BASE + offset,
                    SOURCE_BASE + offset,
                    1,
                )
            })
            .collect::<Vec<_>>();
        let memory =
            LazyGuestMemory::new(limits(MAX_REGIONS, 1, u64::from(MAX_REGIONS), 1), regions)
                .expect("exact maximum region count should construct");
        assert_eq!(memory.region_count(), usize::from(MAX_REGIONS));

        assert!(
            PagerLimits::new(
                PAGE_SIZE,
                MAX_REGIONS + 1,
                1,
                u32::try_from(MAX_FRAME_BYTES).expect("maximum frame size should fit u32"),
                PagerOperations::v1(),
            )
            .is_err()
        );
    }

    #[test]
    fn publishes_exact_data_and_zero_pages_through_scoped_targets() {
        let memory = owner(2, 2, 2);
        let page = u64::from(PAGE_SIZE);
        let contents = vec![0xa5; PAGE_SIZE as usize];
        let ticket = population(
            memory
                .fault_address(GuestAddress::new(GUEST_BASE + 7), PageAccess::Write)
                .expect("address fault should succeed"),
        );

        assert_eq!(ticket.region(), region_id(1));
        assert_eq!(ticket.access(), PageAccess::Write);
        assert_eq!(ticket.offset(), 0);
        assert_eq!(ticket.source_offset(), SOURCE_BASE);
        assert_eq!(
            ticket.guest_range(),
            guest_range(GUEST_BASE, u64::from(PAGE_SIZE))
        );
        assert_eq!(ticket.length(), PAGE_SIZE);
        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("loading page state should be available"),
            LazyPageState::Loading
        );
        assert_eq!(
            memory
                .operation_count()
                .expect("operation count should be available"),
            1
        );

        let mut publication = ticket
            .begin_publication()
            .expect("response should enter publication");
        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("publishing page state should be available"),
            LazyPageState::Publishing
        );
        {
            let mut target = publication
                .target()
                .expect("publication target should be available");
            assert_eq!(
                target.range(),
                guest_range(GUEST_BASE, u64::from(PAGE_SIZE))
            );
            assert_eq!(target.len(), PAGE_SIZE as usize);
            assert!(!target.is_empty());
            assert!(matches!(
                target.copy_from_slice(&contents[..contents.len() - 1]),
                Err(LazyGuestMemoryError::ContentLength)
            ));
            target
                .copy_from_slice(&contents)
                .expect("exact contents should install");
            assert!(matches!(
                target.zero(),
                Err(LazyGuestMemoryError::ContentAlreadyInstalled)
            ));
        }
        publication.commit().expect("publication should commit");
        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("present page state should be available"),
            LazyPageState::Present
        );
        assert!(matches!(
            memory
                .fault_page(region_id(1), 0, PageAccess::Read)
                .expect("present page fault should succeed"),
            LazyPageFault::Present
        ));
        assert_eq!(read_page(&memory, 0), contents);

        let zero_ticket = fault(&memory, page, PageAccess::Read);
        assert_eq!(zero_ticket.offset(), page);
        assert_eq!(zero_ticket.source_offset(), SOURCE_BASE + page);
        publish_zero(zero_ticket);
        assert_eq!(read_page(&memory, page), vec![0; PAGE_SIZE as usize]);
        assert_eq!(
            memory
                .operation_count()
                .expect("operation count should be available"),
            0
        );
    }

    #[test]
    fn duplicate_faults_coalesce_to_one_generation_and_result() {
        const DUPLICATES: usize = 16;

        let memory = Arc::new(owner(1, 1, DUPLICATES as u16));
        let ticket = fault(&memory, 0, PageAccess::Read);
        let generation = ticket.generation();
        let start = Arc::new(Barrier::new(DUPLICATES + 1));

        thread::scope(|scope| {
            let mut workers = Vec::with_capacity(DUPLICATES);
            for index in 0..DUPLICATES {
                let memory = Arc::clone(&memory);
                let start = Arc::clone(&start);
                workers.push(scope.spawn(move || {
                    start.wait();
                    memory.fault_page(
                        region_id(1),
                        0,
                        if index.is_multiple_of(2) {
                            PageAccess::Read
                        } else {
                            PageAccess::Write
                        },
                    )
                }));
            }

            start.wait();
            wait_for_waiters(&memory, DUPLICATES);
            assert_eq!(
                memory
                    .operation_count()
                    .expect("operation count should be available"),
                1
            );
            publish_bytes(ticket, &vec![0x5a; PAGE_SIZE as usize]);

            for worker in workers {
                assert!(matches!(
                    worker.join().expect("duplicate worker should join"),
                    Ok(LazyPageFault::Present)
                ));
            }
        });

        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("page state should be available"),
            LazyPageState::Present
        );
        assert_eq!(
            memory
                .operation_count()
                .expect("operation count should be available"),
            0
        );
        assert_eq!(
            memory
                .waiter_count()
                .expect("waiter count should be available"),
            0
        );
        assert_eq!(generation.get(), 1);
    }

    #[test]
    fn enforces_waiter_and_in_flight_bounds_then_reuses_capacity() {
        let memory = Arc::new(owner(3, 2, 1));
        let first = fault(&memory, 0, PageAccess::Read);
        let duplicate_memory = Arc::clone(&memory);
        let duplicate =
            thread::spawn(move || duplicate_memory.fault_page(region_id(1), 0, PageAccess::Write));
        wait_for_waiters(&memory, 1);
        assert!(matches!(
            memory.fault_page(region_id(1), 0, PageAccess::Read),
            Err(LazyGuestMemoryError::WaiterLimitExceeded)
        ));

        let second = fault(&memory, u64::from(PAGE_SIZE), PageAccess::Read);
        assert!(matches!(
            memory.fault_page(region_id(1), u64::from(PAGE_SIZE) * 2, PageAccess::Read),
            Err(LazyGuestMemoryError::InFlightLimitExceeded)
        ));
        publish_zero(first);
        assert!(matches!(
            duplicate.join().expect("duplicate worker should join"),
            Ok(LazyPageFault::Present)
        ));

        let third = fault(&memory, u64::from(PAGE_SIZE) * 2, PageAccess::Write);
        assert_eq!(
            memory
                .operation_count()
                .expect("operation count should be available"),
            2
        );
        publish_zero(second);
        publish_zero(third);
        assert_eq!(
            memory
                .operation_count()
                .expect("operation count should be available"),
            0
        );
    }

    #[test]
    fn stale_and_replayed_responses_cannot_publish() {
        let memory = owner(2, 2, 2);
        let ticket = fault(&memory, 0, PageAccess::Read);
        let generation = ticket.generation();
        let location = memory
            .inner
            .location_for_page(region_id(1), 0)
            .expect("test location should resolve");
        let wrong_generation = PagerGeneration::new(generation.get() + 1)
            .expect("test wrong generation should be nonzero");

        assert!(matches!(
            memory.inner.begin_publication(location, wrong_generation),
            Err(LazyGuestMemoryError::StaleGeneration)
        ));
        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("page state should be available"),
            LazyPageState::Loading
        );
        publish_zero(ticket);
        assert!(matches!(
            memory.inner.begin_publication(location, generation),
            Err(LazyGuestMemoryError::StaleGeneration)
        ));
        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("page state should be available"),
            LazyPageState::Present
        );

        let second = owner(2, 2, 2);
        let current = fault(&second, 0, PageAccess::Read);
        let wrong_location = second
            .inner
            .location_for_page(region_id(1), u64::from(PAGE_SIZE))
            .expect("second test location should resolve");
        assert!(matches!(
            second
                .inner
                .begin_publication(wrong_location, current.generation()),
            Err(LazyGuestMemoryError::InvalidLifecycle)
        ));
        assert_eq!(
            second
                .terminal_reason()
                .expect("terminal reason should be available"),
            Some(LazyGuestMemoryTerminalReason::TransitionFailure)
        );
        drop(current);
    }

    #[test]
    fn removal_stays_counted_and_removing_until_acknowledged() {
        let memory = Arc::new(owner(1, 2, 2));
        let mut absent_removal = memory
            .begin_removal(region_id(1), 0, u64::from(PAGE_SIZE))
            .expect("absent page removal should begin");
        let absent_removal_generation = absent_removal.generation();
        absent_removal
            .target()
            .expect("absent removal target should be available")
            .zero()
            .expect("absent removal target should zero");
        absent_removal
            .commit_acknowledged()
            .expect("absent removal acknowledgement should commit");

        let initial = fault(&memory, 0, PageAccess::Write);
        let initial_generation = initial.generation();
        assert!(initial_generation > absent_removal_generation);
        publish_bytes(initial, &vec![0x7c; PAGE_SIZE as usize]);

        let mut removal = memory
            .begin_removal(region_id(1), 0, u64::from(PAGE_SIZE))
            .expect("present page removal should begin");
        assert_eq!(removal.region(), region_id(1));
        assert_eq!(removal.offset(), 0);
        assert_eq!(removal.source_offset(), SOURCE_BASE);
        assert_eq!(removal.length(), u64::from(PAGE_SIZE));
        assert!(removal.generation() > initial_generation);
        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("page state should be available"),
            LazyPageState::Removing
        );
        assert_eq!(
            memory
                .operation_count()
                .expect("operation count should be available"),
            1
        );
        removal
            .target()
            .expect("removal target should be available")
            .zero()
            .expect("removal target should zero");
        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("page state should be available"),
            LazyPageState::Removing
        );

        let fault_memory = Arc::clone(&memory);
        let refault = thread::spawn(move || {
            let ticket = population(
                fault_memory
                    .fault_page(region_id(1), 0, PageAccess::Read)
                    .expect("refault should eventually succeed"),
            );
            let generation = ticket.generation();
            publish_zero(ticket);
            generation
        });
        wait_for_waiters(&memory, 1);
        let removal_generation = removal.generation();
        removal
            .commit_acknowledged()
            .expect("acknowledged removal should commit");
        let refault_generation = refault.join().expect("refault worker should join");
        assert!(refault_generation > removal_generation);
        assert_eq!(read_page(&memory, 0), vec![0; PAGE_SIZE as usize]);
        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("page state should be available"),
            LazyPageState::Present
        );
    }

    #[test]
    fn removal_reserves_a_distinct_slot_before_superseding_loading() {
        let memory = Arc::new(owner(2, 2, 2));
        let stale = fault(&memory, 0, PageAccess::Read);
        let stale_generation = stale.generation();
        let duplicate_memory = Arc::clone(&memory);
        let duplicate =
            thread::spawn(move || duplicate_memory.fault_page(region_id(1), 0, PageAccess::Write));
        wait_for_waiters(&memory, 1);

        let mut removal = memory
            .begin_removal(region_id(1), 0, u64::from(PAGE_SIZE))
            .expect("loading page removal should reserve its own slot");
        assert_eq!(
            memory
                .operation_count()
                .expect("retired and removal operations should be counted"),
            2
        );
        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("page state should be available"),
            LazyPageState::Removing
        );
        assert!(matches!(
            duplicate.join().expect("duplicate worker should join"),
            Err(LazyGuestMemoryError::StaleGeneration)
        ));
        assert!(matches!(
            memory.fault_page(region_id(1), u64::from(PAGE_SIZE), PageAccess::Read),
            Err(LazyGuestMemoryError::InFlightLimitExceeded)
        ));

        assert!(matches!(
            stale.begin_publication(),
            Err(LazyGuestMemoryError::StaleGeneration)
        ));
        assert_eq!(
            memory
                .operation_count()
                .expect("only removal should remain counted"),
            1
        );
        removal
            .target()
            .expect("removal target should be available")
            .zero()
            .expect("removal target should zero");
        let removal_generation = removal.generation();
        removal
            .commit_acknowledged()
            .expect("acknowledged removal should commit");

        let refault = fault(&memory, 0, PageAccess::Read);
        assert!(refault.generation() > stale_generation);
        assert!(refault.generation() > removal_generation);
        publish_zero(refault);
    }

    #[test]
    fn removal_limit_failure_does_not_mutate_loading_state() {
        let memory = owner(1, 1, 1);
        let ticket = fault(&memory, 0, PageAccess::Read);
        let generation = ticket.generation();

        assert!(matches!(
            memory.begin_removal(region_id(1), 0, u64::from(PAGE_SIZE)),
            Err(LazyGuestMemoryError::InFlightLimitExceeded)
        ));
        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("page state should be available"),
            LazyPageState::Loading
        );
        assert_eq!(
            memory
                .operation_count()
                .expect("population should remain counted"),
            1
        );
        assert_eq!(ticket.generation(), generation);
        publish_zero(ticket);
    }

    #[test]
    fn dropping_a_retired_population_releases_only_its_protocol_slot() {
        let memory = owner(2, 2, 1);
        let retired = fault(&memory, 0, PageAccess::Read);
        let mut removal = memory
            .begin_removal(region_id(1), 0, u64::from(PAGE_SIZE))
            .expect("loading page removal should begin");
        assert_eq!(
            memory
                .operation_count()
                .expect("retired and removal operations should be counted"),
            2
        );

        drop(retired);
        assert_eq!(
            memory
                .operation_count()
                .expect("only removal should remain counted"),
            1
        );
        let independent = fault(&memory, u64::from(PAGE_SIZE), PageAccess::Write);
        publish_zero(independent);
        removal
            .target()
            .expect("removal target should be available")
            .zero()
            .expect("removal target should zero");
        removal
            .commit_acknowledged()
            .expect("acknowledged removal should commit");
        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("removed page state should be available"),
            LazyPageState::Absent
        );
    }

    #[test]
    fn removal_waits_for_an_already_linearized_publication() {
        let memory = Arc::new(owner(1, 2, 1));
        let ticket = fault(&memory, 0, PageAccess::Read);
        let mut publication = ticket
            .begin_publication()
            .expect("response should enter publication");
        publication
            .target()
            .expect("publication target should be available")
            .copy_from_slice(&vec![0x35; PAGE_SIZE as usize])
            .expect("publication contents should install");

        let removal_memory = Arc::clone(&memory);
        let worker = thread::spawn(move || {
            let mut removal = removal_memory
                .begin_removal(region_id(1), 0, u64::from(PAGE_SIZE))
                .expect("removal should begin after publication");
            let generation = removal.generation();
            removal
                .target()
                .expect("removal target should be available")
                .zero()
                .expect("removal target should zero");
            removal
                .commit_acknowledged()
                .expect("removal acknowledgement should commit");
            generation
        });
        wait_for_waiters(&memory, 1);
        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("page state should be available"),
            LazyPageState::Publishing
        );
        publication.commit().expect("publication should commit");
        let removal_generation = worker.join().expect("removal worker should join");
        assert!(removal_generation.get() > 1);
        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("page state should be available"),
            LazyPageState::Absent
        );
        assert_eq!(read_page(&memory, 0), vec![0; PAGE_SIZE as usize]);
    }

    #[test]
    fn abandoned_tickets_and_guards_fail_closed() {
        let memory = owner(1, 1, 1);
        drop(fault(&memory, 0, PageAccess::Read));
        assert_eq!(
            memory
                .terminal_reason()
                .expect("terminal reason should be available"),
            Some(LazyGuestMemoryTerminalReason::TransitionFailure)
        );

        let publication_memory = owner(1, 1, 1);
        let publication = fault(&publication_memory, 0, PageAccess::Read)
            .begin_publication()
            .expect("response should enter publication");
        assert!(matches!(
            publication.commit(),
            Err(LazyGuestMemoryError::ContentMissing)
        ));
        assert_eq!(
            publication_memory
                .terminal_reason()
                .expect("terminal reason should be available"),
            Some(LazyGuestMemoryTerminalReason::TransitionFailure)
        );

        let removal_memory = owner(1, 1, 1);
        let removal = removal_memory
            .begin_removal(region_id(1), 0, u64::from(PAGE_SIZE))
            .expect("absent page removal should begin");
        assert!(matches!(
            removal.commit_acknowledged(),
            Err(LazyGuestMemoryError::ContentMissing)
        ));
        assert_eq!(
            removal_memory
                .terminal_reason()
                .expect("terminal reason should be available"),
            Some(LazyGuestMemoryTerminalReason::TransitionFailure)
        );
    }

    #[test]
    fn requested_peer_and_teardown_outcomes_wake_waiters() {
        for reason in [
            LazyGuestMemoryTerminalReason::Requested,
            LazyGuestMemoryTerminalReason::PeerFailure,
            LazyGuestMemoryTerminalReason::Teardown,
        ] {
            let memory = Arc::new(owner(1, 1, 1));
            let ticket = fault(&memory, 0, PageAccess::Read);
            let waiter_memory = Arc::clone(&memory);
            let waiter =
                thread::spawn(move || waiter_memory.fault_page(region_id(1), 0, PageAccess::Write));
            wait_for_waiters(&memory, 1);

            memory
                .terminate(reason)
                .expect("terminal transition should drain");
            assert!(matches!(
                waiter.join().expect("waiter should join"),
                Err(LazyGuestMemoryError::Terminal {
                    reason: observed
                }) if observed == reason
            ));
            assert_eq!(
                memory
                    .page_state(region_id(1), 0)
                    .expect("page state should be available"),
                LazyPageState::Terminal
            );
            assert_eq!(
                memory
                    .operation_count()
                    .expect("operation count should be available"),
                0
            );
            drop(ticket);
        }
    }

    #[test]
    fn explicit_termination_waits_for_linearized_actions() {
        let memory = Arc::new(owner(1, 1, 1));
        let mut publication = fault(&memory, 0, PageAccess::Read)
            .begin_publication()
            .expect("response should enter publication");
        publication
            .target()
            .expect("publication target should be available")
            .zero()
            .expect("publication target should zero");

        let terminating_memory = Arc::clone(&memory);
        let terminator = thread::spawn(move || {
            terminating_memory.terminate(LazyGuestMemoryTerminalReason::Requested)
        });
        wait_for_reason(&memory, LazyGuestMemoryTerminalReason::Requested);
        assert!(matches!(
            publication.commit(),
            Err(LazyGuestMemoryError::Terminal {
                reason: LazyGuestMemoryTerminalReason::Requested
            })
        ));
        terminator
            .join()
            .expect("terminator should join")
            .expect("termination should finish after the action");
        assert_eq!(
            memory
                .operation_count()
                .expect("operation count should be available"),
            0
        );

        let removal_memory = Arc::new(owner(1, 1, 1));
        let mut removal = removal_memory
            .begin_removal(region_id(1), 0, u64::from(PAGE_SIZE))
            .expect("removal should begin");
        removal
            .target()
            .expect("removal target should be available")
            .zero()
            .expect("removal target should zero");
        let terminating_memory = Arc::clone(&removal_memory);
        let terminator = thread::spawn(move || {
            terminating_memory.terminate(LazyGuestMemoryTerminalReason::PeerFailure)
        });
        wait_for_reason(&removal_memory, LazyGuestMemoryTerminalReason::PeerFailure);
        assert!(matches!(
            removal.commit_acknowledged(),
            Err(LazyGuestMemoryError::Terminal {
                reason: LazyGuestMemoryTerminalReason::PeerFailure
            })
        ));
        terminator
            .join()
            .expect("removal terminator should join")
            .expect("termination should finish after removal");
    }

    #[test]
    fn owner_drop_is_nonblocking_and_invalidates_retained_loading_work() {
        let memory = owner(1, 1, 1);
        let ticket = fault(&memory, 0, PageAccess::Read);
        drop(memory);

        assert!(matches!(
            ticket.begin_publication(),
            Err(LazyGuestMemoryError::Terminal {
                reason: LazyGuestMemoryTerminalReason::Teardown
            })
        ));
    }

    #[test]
    fn poisoned_state_wakes_waiters_and_fails_closed() {
        let memory = Arc::new(owner(1, 1, 1));
        let ticket = fault(&memory, 0, PageAccess::Read);
        let waiter_memory = Arc::clone(&memory);
        let waiter =
            thread::spawn(move || waiter_memory.fault_page(region_id(1), 0, PageAccess::Read));
        wait_for_waiters(&memory, 1);

        let inner = Arc::clone(&memory.inner);
        let poisoner = thread::spawn(move || {
            let _state = inner
                .state
                .lock()
                .expect("test coordinator state should initially be available");
            panic!("intentional lazy-memory state poison");
        });
        assert!(poisoner.join().is_err());
        memory.inner.changed.notify_all();
        assert!(matches!(
            waiter.join().expect("poisoned waiter should join"),
            Err(LazyGuestMemoryError::StatePoisoned)
        ));
        let state = memory.inner.lock_state_for_drop();
        assert_eq!(
            state.phase.reason(),
            Some(LazyGuestMemoryTerminalReason::StatePoisoned)
        );
        drop(state);
        drop(ticket);
    }

    #[test]
    fn generation_exhaustion_is_owner_terminal() {
        let memory = owner(1, 1, 1);
        {
            let mut state = memory
                .inner
                .state
                .lock()
                .expect("test coordinator state should be available");
            state.next_generation = u64::MAX;
        }

        assert!(matches!(
            memory.fault_page(region_id(1), 0, PageAccess::Read),
            Err(LazyGuestMemoryError::GenerationExhausted)
        ));
        assert_eq!(
            memory
                .terminal_reason()
                .expect("terminal reason should be available"),
            Some(LazyGuestMemoryTerminalReason::TransitionFailure)
        );
        assert_eq!(
            memory
                .page_state(region_id(1), 0)
                .expect("page state should be available"),
            LazyPageState::Terminal
        );
    }

    #[test]
    fn public_diagnostics_redact_addresses_generations_and_contents() {
        let memory = owner(1, 1, 1);
        let ticket = fault(&memory, 0, PageAccess::Read);

        assert!(format!("{memory:?}").contains("<redacted>"));
        assert!(!format!("{memory:?}").contains("80000000"));
        assert_eq!(
            format!("{:?}", memory.inner.regions[0].source),
            "LazyGuestMemoryRegion(<redacted>)"
        );
        assert_eq!(format!("{ticket:?}"), "LazyPagePopulation(<redacted>)");
        assert_eq!(
            format!(
                "{:?}",
                LazyGuestMemoryError::Terminal {
                    reason: LazyGuestMemoryTerminalReason::PeerFailure
                }
            ),
            "LazyGuestMemoryError::Terminal(<redacted>)"
        );
        assert_eq!(
            LazyGuestMemoryError::Terminal {
                reason: LazyGuestMemoryTerminalReason::PeerFailure
            }
            .to_string(),
            "lazy guest memory is terminal"
        );
        drop(ticket);
    }

    #[test]
    fn repeated_construction_and_destruction_leaves_no_retained_work() {
        for _ in 0..128 {
            let memory = owner(1, 1, 1);
            let ticket = fault(&memory, 0, PageAccess::Read);
            publish_zero(ticket);
            assert_eq!(
                memory
                    .operation_count()
                    .expect("operation count should be available"),
                0
            );
        }
    }
}
