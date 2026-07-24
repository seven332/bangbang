use std::collections::{TryReserveError, VecDeque};
use std::ffi::c_void;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::mem::{MaybeUninit, align_of, size_of};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
use std::sync::atomic::AtomicUsize;

use crate::memory_dirty::{GuestMemoryDirtyTracker, GuestMemoryDirtyTrackerError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GuestAddress(u64);

impl GuestAddress {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn raw_value(self) -> u64 {
        self.0
    }

    pub const fn checked_add(self, offset: u64) -> Option<Self> {
        match self.0.checked_add(offset) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub fn is_aligned(self, alignment: u64) -> Result<bool, GuestMemoryError> {
        validate_alignment(alignment)?;
        Ok(self.0.is_multiple_of(alignment))
    }
}

impl fmt::Display for GuestAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:x}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestMemoryRange {
    start: GuestAddress,
    size: u64,
    end_exclusive: GuestAddress,
}

impl GuestMemoryRange {
    pub fn new(start: GuestAddress, size: u64) -> Result<Self, GuestMemoryError> {
        if size == 0 {
            return Err(GuestMemoryError::EmptyRange { start });
        }

        let Some(end_exclusive) = start.checked_add(size) else {
            return Err(GuestMemoryError::AddressOverflow { start, size });
        };

        Ok(Self {
            start,
            size,
            end_exclusive,
        })
    }

    pub const fn start(self) -> GuestAddress {
        self.start
    }

    pub const fn size(self) -> u64 {
        self.size
    }

    pub const fn end_exclusive(self) -> GuestAddress {
        self.end_exclusive
    }

    pub const fn overlaps(self, other: Self) -> bool {
        self.start.0 < other.end_exclusive.0 && other.start.0 < self.end_exclusive.0
    }

    pub const fn is_adjacent_to(self, other: Self) -> bool {
        self.end_exclusive.0 == other.start.0 || other.end_exclusive.0 == self.start.0
    }

    pub const fn contains(self, address: GuestAddress) -> bool {
        self.start.0 <= address.0 && address.0 < self.end_exclusive.0
    }

    pub fn validate_alignment(self, alignment: u64) -> Result<(), GuestMemoryError> {
        validate_alignment(alignment)?;

        if self.start.0.is_multiple_of(alignment) && self.size.is_multiple_of(alignment) {
            Ok(())
        } else {
            Err(GuestMemoryError::UnalignedRange {
                range: self,
                alignment,
            })
        }
    }
}

impl fmt::Display for GuestMemoryRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}..{}) ({} bytes)",
            self.start, self.end_exclusive, self.size
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestMemoryLayout {
    ranges: Vec<GuestMemoryRange>,
}

impl GuestMemoryLayout {
    pub fn new(ranges: Vec<GuestMemoryRange>) -> Result<Self, GuestMemoryError> {
        if ranges.is_empty() {
            return Err(GuestMemoryError::EmptyLayout);
        }

        let mut previous: Option<GuestMemoryRange> = None;
        for range in ranges.iter().copied() {
            if let Some(previous_range) = previous {
                if range.start() < previous_range.start() {
                    return Err(GuestMemoryError::UnorderedRange {
                        previous: previous_range,
                        next: range,
                    });
                }

                if previous_range.overlaps(range) {
                    return Err(GuestMemoryError::OverlappingRange {
                        previous: previous_range,
                        next: range,
                    });
                }
            }

            previous = Some(range);
        }

        Ok(Self { ranges })
    }

    pub fn ranges(&self) -> &[GuestMemoryRange] {
        &self.ranges
    }

    pub fn total_size(&self) -> u64 {
        self.ranges.iter().map(|range| range.size()).sum::<u64>()
    }
}

/// Classifies a guest-memory discard failure without exposing a host address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestMemoryDiscardFailureKind {
    /// The requested guest range was not fully backed by owned memory regions.
    RangeValidation,
    /// The current target has no supported zero-safe discard implementation.
    UnsupportedTarget,
    /// The host page size was unavailable, invalid, or not representable.
    InvalidHostPageSize,
    /// A bounded host-address calculation could not be represented.
    HostAddress,
    /// Zeroing a host-page-aligned interior failed.
    ZeroAdvice,
    /// Marking a successfully zeroed interior as free failed.
    FreeAdvice,
    /// Zeroing and deallocating a descriptor-backed shared range failed.
    SharedReclaim,
    /// Replacing a private-file range with anonymous zero pages failed.
    PrivateFileReclaim,
}

/// Redacted failure counts from one guest-memory discard attempt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GuestMemoryDiscardFailures {
    range_validation: u64,
    unsupported_target: u64,
    invalid_host_page_size: u64,
    host_address: u64,
    zero_advice: u64,
    free_advice: u64,
    shared_reclaim: u64,
    private_file_reclaim: u64,
}

impl GuestMemoryDiscardFailures {
    /// Returns the number of failures in one class.
    pub const fn count(self, kind: GuestMemoryDiscardFailureKind) -> u64 {
        match kind {
            GuestMemoryDiscardFailureKind::RangeValidation => self.range_validation,
            GuestMemoryDiscardFailureKind::UnsupportedTarget => self.unsupported_target,
            GuestMemoryDiscardFailureKind::InvalidHostPageSize => self.invalid_host_page_size,
            GuestMemoryDiscardFailureKind::HostAddress => self.host_address,
            GuestMemoryDiscardFailureKind::ZeroAdvice => self.zero_advice,
            GuestMemoryDiscardFailureKind::FreeAdvice => self.free_advice,
            GuestMemoryDiscardFailureKind::SharedReclaim => self.shared_reclaim,
            GuestMemoryDiscardFailureKind::PrivateFileReclaim => self.private_file_reclaim,
        }
    }

    /// Returns the total number of classified failures.
    pub const fn total(self) -> u64 {
        self.range_validation
            .saturating_add(self.unsupported_target)
            .saturating_add(self.invalid_host_page_size)
            .saturating_add(self.host_address)
            .saturating_add(self.zero_advice)
            .saturating_add(self.free_advice)
            .saturating_add(self.shared_reclaim)
            .saturating_add(self.private_file_reclaim)
    }

    const fn with_failure(mut self, kind: GuestMemoryDiscardFailureKind) -> Self {
        match kind {
            GuestMemoryDiscardFailureKind::RangeValidation => {
                self.range_validation = self.range_validation.saturating_add(1);
            }
            GuestMemoryDiscardFailureKind::UnsupportedTarget => {
                self.unsupported_target = self.unsupported_target.saturating_add(1);
            }
            GuestMemoryDiscardFailureKind::InvalidHostPageSize => {
                self.invalid_host_page_size = self.invalid_host_page_size.saturating_add(1);
            }
            GuestMemoryDiscardFailureKind::HostAddress => {
                self.host_address = self.host_address.saturating_add(1);
            }
            GuestMemoryDiscardFailureKind::ZeroAdvice => {
                self.zero_advice = self.zero_advice.saturating_add(1);
            }
            GuestMemoryDiscardFailureKind::FreeAdvice => {
                self.free_advice = self.free_advice.saturating_add(1);
            }
            GuestMemoryDiscardFailureKind::SharedReclaim => {
                self.shared_reclaim = self.shared_reclaim.saturating_add(1);
            }
            GuestMemoryDiscardFailureKind::PrivateFileReclaim => {
                self.private_file_reclaim = self.private_file_reclaim.saturating_add(1);
            }
        }
        self
    }
}

impl fmt::Display for GuestMemoryDiscardFailures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "guest memory discard failures: range_validation={}, unsupported_target={}, invalid_host_page_size={}, host_address={}, zero_advice={}, free_advice={}, shared_reclaim={}, private_file_reclaim={}",
            self.range_validation,
            self.unsupported_target,
            self.invalid_host_page_size,
            self.host_address,
            self.zero_advice,
            self.free_advice,
            self.shared_reclaim,
            self.private_file_reclaim
        )
    }
}

/// Byte accounting and redacted failures from one guest-memory discard attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestMemoryDiscardOutcome {
    requested_bytes: u64,
    advised_bytes: u64,
    skipped_bytes: u64,
    failed_bytes: u64,
    failures: GuestMemoryDiscardFailures,
}

impl GuestMemoryDiscardOutcome {
    const fn new(requested_bytes: u64) -> Self {
        Self {
            requested_bytes,
            advised_bytes: 0,
            skipped_bytes: 0,
            failed_bytes: 0,
            failures: GuestMemoryDiscardFailures {
                range_validation: 0,
                unsupported_target: 0,
                invalid_host_page_size: 0,
                host_address: 0,
                zero_advice: 0,
                free_advice: 0,
                shared_reclaim: 0,
                private_file_reclaim: 0,
            },
        }
    }

    /// Returns the number of bytes in the requested guest range.
    pub const fn requested_bytes(self) -> u64 {
        self.requested_bytes
    }

    /// Returns bytes whose aligned host interiors completed zero-safe reclaim.
    pub const fn advised_bytes(self) -> u64 {
        self.advised_bytes
    }

    /// Returns requested edge bytes skipped to preserve neighboring host pages.
    pub const fn skipped_bytes(self) -> u64 {
        self.skipped_bytes
    }

    /// Returns bytes that could not complete the selected reclaim operation.
    pub const fn failed_bytes(self) -> u64 {
        self.failed_bytes
    }

    /// Returns redacted, stage-classified failure counts.
    pub const fn failures(self) -> GuestMemoryDiscardFailures {
        self.failures
    }

    /// Returns whether the attempt completed without a classified failure.
    pub const fn is_complete(self) -> bool {
        self.failures.total() == 0
    }

    const fn fail_all(mut self, kind: GuestMemoryDiscardFailureKind) -> Self {
        self.failed_bytes = self.requested_bytes;
        self.failures = self.failures.with_failure(kind);
        self
    }

    fn record_skipped(&mut self, bytes: u64) {
        self.skipped_bytes = self.skipped_bytes.saturating_add(bytes);
    }

    fn record_advised(&mut self, bytes: u64) {
        self.advised_bytes = self.advised_bytes.saturating_add(bytes);
    }

    fn record_failed(&mut self, bytes: u64, kind: GuestMemoryDiscardFailureKind) {
        self.failed_bytes = self.failed_bytes.saturating_add(bytes);
        self.failures = self.failures.with_failure(kind);
    }
}

pub(crate) trait GuestMemoryDiscardAdviser {
    fn host_page_size(&mut self) -> Result<u64, GuestMemoryDiscardFailureKind>;

    fn zero(&mut self, address: NonNull<c_void>, size: usize) -> io::Result<()>;

    fn free(&mut self, address: NonNull<c_void>, size: usize) -> io::Result<()>;

    fn reclaim(
        &mut self,
        mapping: &GuestMemoryMapping,
        offset: usize,
        address: NonNull<c_void>,
        size: usize,
    ) -> Result<(), GuestMemoryDiscardFailureKind> {
        let _ = (mapping, offset);
        self.zero(address, size)
            .map_err(|_| GuestMemoryDiscardFailureKind::ZeroAdvice)?;
        self.free(address, size)
            .map_err(|_| GuestMemoryDiscardFailureKind::FreeAdvice)
    }
}

#[derive(Debug, Default)]
pub(crate) struct SystemGuestMemoryDiscardAdviser;

/// Selects how process-owned guest memory is backed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GuestMemoryBacking {
    /// Private anonymous mappings retained entirely by this process.
    #[default]
    Anonymous,
    /// Shared mappings backed by unlinked, owner-only file descriptors.
    Shared,
}

/// Describes the owner of one currently mapped guest-memory region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GuestMemoryRegionBacking {
    /// A private anonymous mapping.
    Anonymous,
    /// A shared mapping backed by an exportable descriptor.
    Shared,
    /// A writable private mapping backed by a retained read-only snapshot file.
    PrivateFile,
}

static NEXT_GUEST_MEMORY_MAPPING_IDENTITY: AtomicU64 = AtomicU64::new(1);

fn next_guest_memory_mapping_identity()
-> Result<GuestMemoryMappingIdentity, GuestMemoryAllocationError> {
    NEXT_GUEST_MEMORY_MAPPING_IDENTITY
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map(GuestMemoryMappingIdentity)
        .map_err(|_| GuestMemoryAllocationError::MappingIdentityExhausted)
}

/// Opaque process-local value identifying one mapping without retaining it or
/// exposing its address or descriptor.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GuestMemoryMappingIdentity(u64);

impl fmt::Debug for GuestMemoryMappingIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GuestMemoryMappingIdentity(<redacted>)")
    }
}

/// Detached identity and range for one descriptor-backed shared reservation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GuestMemorySharedReservationCaptureState {
    range: GuestMemoryRange,
    mapping_identity: GuestMemoryMappingIdentity,
}

impl GuestMemorySharedReservationCaptureState {
    pub const fn range(self) -> GuestMemoryRange {
        self.range
    }

    pub const fn mapping_identity(self) -> GuestMemoryMappingIdentity {
        self.mapping_identity
    }
}

impl fmt::Debug for GuestMemorySharedReservationCaptureState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GuestMemorySharedReservationCaptureState")
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Debug)]
pub enum GuestMemorySharedReservationCaptureError {
    Missing {
        range: GuestMemoryRange,
    },
    InvalidBacking {
        source: GuestMemorySharedBackingError,
    },
}

impl fmt::Display for GuestMemorySharedReservationCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { .. } => {
                formatter.write_str("shared guest-memory reservation is missing")
            }
            Self::InvalidBacking { .. } => {
                formatter.write_str("shared guest-memory reservation backing is invalid")
            }
        }
    }
}

impl std::error::Error for GuestMemorySharedReservationCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidBacking { source } => Some(source),
            Self::Missing { .. } => None,
        }
    }
}

#[derive(Debug)]
pub struct GuestMemory {
    regions: Vec<GuestMemoryRegion>,
    shared_reservations: Vec<GuestMemoryRegion>,
    dirty_tracker: Option<Arc<GuestMemoryDirtyTracker>>,
    backing: GuestMemoryBacking,
    access_profile: GuestMemoryAccessProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuestMemoryAccessProfile {
    Eager,
    ProtectedLazy,
}

/// Retained, aligned 64-bit word inside owned guest memory.
///
/// The lease keeps the underlying mapping alive without exposing its host
/// address. Stores mark the configured dirty tracker before publishing one
/// little-endian, release-ordered value. Callers must reserve the word for
/// atomic access before sharing this lease with another thread.
#[derive(Clone)]
pub struct GuestMemoryAtomicU64 {
    mapping: Arc<GuestMemoryMapping>,
    mapping_offset: usize,
    range: GuestMemoryRange,
    dirty_tracker: Option<Arc<GuestMemoryDirtyTracker>>,
}

impl GuestMemoryAtomicU64 {
    /// Publish one host value as a little-endian single-copy atomic word.
    pub fn store_le(&self, value: u64) -> Result<(), GuestMemoryAccessError> {
        if let Some(tracker) = self.dirty_tracker.as_ref() {
            tracker
                .mark_range(self.range)
                .map_err(|_| GuestMemoryAccessError::DirtyTrackingState)?;
        }

        // SAFETY: `GuestMemory::atomic_u64` proves that the retained mapping
        // covers this complete word and that both its guest and derived host
        // addresses satisfy `AtomicU64` alignment. The mapping remains live
        // through `self.mapping`, and this lease is the caller-designated
        // atomic access path for the word.
        unsafe {
            self.atomic().store(value.to_le(), Ordering::Release);
        }
        Ok(())
    }

    /// Read one little-endian word with acquire ordering.
    pub fn load_le(&self) -> u64 {
        // SAFETY: construction and retained ownership provide the same bounds,
        // alignment, and lifetime guarantees as `store_le`.
        u64::from_le(unsafe { self.atomic().load(Ordering::Acquire) })
    }

    /// Return the exact guest range covered by this lease.
    pub const fn range(&self) -> GuestMemoryRange {
        self.range
    }

    unsafe fn atomic(&self) -> &AtomicU64 {
        let address = self
            .mapping
            .address()
            .as_ptr()
            .cast::<u8>()
            .wrapping_add(self.mapping_offset)
            .cast::<AtomicU64>();
        // SAFETY: the caller relies on the construction proof documented by
        // `store_le` and `load_le`.
        unsafe { &*address }
    }
}

impl fmt::Debug for GuestMemoryAtomicU64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuestMemoryAtomicU64")
            .field("range", &self.range)
            .field("dirty_tracking", &self.dirty_tracker.is_some())
            .finish_non_exhaustive()
    }
}

impl GuestMemory {
    pub fn allocate(layout: &GuestMemoryLayout) -> Result<Self, GuestMemoryAllocationError> {
        Self::allocate_with_backing(layout, GuestMemoryBacking::Anonymous)
    }

    /// Allocate every region using one explicit backing profile.
    pub fn allocate_with_backing(
        layout: &GuestMemoryLayout,
        backing: GuestMemoryBacking,
    ) -> Result<Self, GuestMemoryAllocationError> {
        let page_size = host_page_size()?;
        validate_allocation_ranges(layout, page_size)?;
        let mut mapper = match backing {
            GuestMemoryBacking::Anonymous => SystemGuestMemoryMapper::anonymous(),
            GuestMemoryBacking::Shared => {
                preflight_shared_memory_resources(layout.ranges().len(), layout.ranges())?;
                SystemGuestMemoryMapper::shared(prepare_shared_memory_files(layout.ranges())?)
            }
        };

        Self::allocate_with_mapper(layout, page_size, &mut mapper)
    }

    fn allocate_with_mapper(
        layout: &GuestMemoryLayout,
        page_size: u64,
        mapper: &mut impl GuestMemoryMapper,
    ) -> Result<Self, GuestMemoryAllocationError> {
        let backing = mapper.backing();
        validate_allocation_ranges(layout, page_size)?;
        let mut regions = Vec::new();
        regions
            .try_reserve_exact(layout.ranges().len())
            .map_err(
                |source| GuestMemoryAllocationError::RegionMetadataAllocationFailed { source },
            )?;

        for range in layout.ranges().iter().copied() {
            let host_size = allocation_host_size(range)?;
            regions.push(GuestMemoryRegion {
                range,
                mapping: Arc::new(mapper.map(host_size)?),
                mapping_offset: 0,
                host_size,
            });
        }

        Ok(Self {
            regions,
            shared_reservations: Vec::new(),
            dirty_tracker: None,
            backing,
            access_profile: GuestMemoryAccessProfile::Eager,
        })
    }

    /// Constructs guest memory from already validated private-file extents.
    ///
    /// The one descriptor owner is shared by every mapping without duplicating
    /// the descriptor. Each mapped extent is independently revalidated at the
    /// unsafe boundary so partial construction drops prior mappings normally.
    pub(crate) fn from_private_file_ranges(
        ranges: &[(GuestMemoryRange, u64)],
        file: Arc<File>,
        backing: GuestMemoryBacking,
    ) -> Result<Self, GuestMemoryAllocationError> {
        Self::from_private_file_ranges_with_mapper(
            ranges,
            file,
            backing,
            &mut GuestMemoryMapping::map_private_file,
        )
    }

    fn from_private_file_ranges_with_mapper(
        ranges: &[(GuestMemoryRange, u64)],
        file: Arc<File>,
        backing: GuestMemoryBacking,
        mapper: &mut impl FnMut(
            usize,
            Arc<File>,
            u64,
        ) -> Result<GuestMemoryMapping, GuestMemoryAllocationError>,
    ) -> Result<Self, GuestMemoryAllocationError> {
        let page_size = host_page_size()?;
        let mut layout_ranges = Vec::new();
        layout_ranges
            .try_reserve_exact(ranges.len())
            .map_err(
                |source| GuestMemoryAllocationError::RegionMetadataAllocationFailed { source },
            )?;
        layout_ranges.extend(ranges.iter().map(|(range, _)| *range));
        let layout = GuestMemoryLayout::new(layout_ranges)?;
        validate_allocation_ranges(&layout, page_size)?;

        let file_length = file
            .metadata()
            .map_err(|source| GuestMemoryAllocationError::PrivateFileInspectFailed { source })?
            .len();
        let mut regions = Vec::new();
        regions.try_reserve_exact(ranges.len()).map_err(|source| {
            GuestMemoryAllocationError::RegionMetadataAllocationFailed { source }
        })?;

        // Validate every extent before the first mmap. The mapping constructor
        // repeats the representation check at the unsafe boundary.
        for (range, file_offset) in ranges.iter().copied() {
            if !file_offset.is_multiple_of(page_size) {
                return Err(GuestMemoryAllocationError::PrivateFileOffsetUnaligned);
            }
            let host_size = allocation_host_size(range)?;
            libc::off_t::try_from(file_offset)
                .map_err(|_| GuestMemoryAllocationError::PrivateFileOffsetTooLarge)?;
            let host_size_u64 = u64::try_from(host_size)
                .map_err(|_| GuestMemoryAllocationError::SizeTooLarge { range })?;
            if file_offset
                .checked_add(host_size_u64)
                .is_none_or(|end| end > file_length)
            {
                return Err(GuestMemoryAllocationError::PrivateFileRangeBeyondEnd);
            }
        }

        for (range, file_offset) in ranges.iter().copied() {
            let host_size = allocation_host_size(range)?;
            regions.push(GuestMemoryRegion {
                range,
                mapping: Arc::new(mapper(host_size, Arc::clone(&file), file_offset)?),
                mapping_offset: 0,
                host_size,
            });
        }

        Ok(Self {
            regions,
            shared_reservations: Vec::new(),
            dirty_tracker: None,
            backing,
            access_profile: GuestMemoryAccessProfile::Eager,
        })
    }

    /// Return the backing profile inherited by dynamically inserted regions.
    pub const fn backing(&self) -> GuestMemoryBacking {
        self.backing
    }

    /// Return whether this is the non-cloneable consumer view retained by a
    /// platform lazy-fault bridge.
    #[doc(hidden)]
    pub const fn is_protected_lazy(&self) -> bool {
        matches!(self.access_profile, GuestMemoryAccessProfile::ProtectedLazy)
    }

    pub(crate) fn try_protected_lazy_view(&self) -> Result<Self, TryReserveError> {
        debug_assert_eq!(self.backing, GuestMemoryBacking::Anonymous);
        debug_assert!(self.shared_reservations.is_empty());
        debug_assert!(self.dirty_tracker.is_none());
        debug_assert_eq!(self.access_profile, GuestMemoryAccessProfile::Eager);

        let mut regions = Vec::new();
        regions.try_reserve_exact(self.regions.len())?;
        regions.extend(self.regions.iter().map(|region| GuestMemoryRegion {
            range: region.range,
            mapping: Arc::clone(&region.mapping),
            mapping_offset: region.mapping_offset,
            host_size: region.host_size,
        }));

        Ok(Self {
            regions,
            shared_reservations: Vec::new(),
            dirty_tracker: None,
            backing: GuestMemoryBacking::Anonymous,
            access_profile: GuestMemoryAccessProfile::ProtectedLazy,
        })
    }

    /// Install one shared dirty-page generation over every current region.
    ///
    /// Calling this before normal boot population records boot-loader and
    /// device initialization writes. Calling it after snapshot image loading
    /// establishes that image as a clean baseline.
    pub fn enable_dirty_tracking(
        &mut self,
    ) -> Result<Arc<GuestMemoryDirtyTracker>, GuestMemoryDirtyTrackerError> {
        if self.is_protected_lazy() {
            return Err(GuestMemoryDirtyTrackerError::ProtectedLazyMemory);
        }
        if let Some(tracker) = self.dirty_tracker.as_ref() {
            return Ok(Arc::clone(tracker));
        }
        let page_size = system_host_page_size()
            .ok_or(GuestMemoryDirtyTrackerError::InvalidPageSize { page_size: 0 })?;
        let tracker = Arc::new(GuestMemoryDirtyTracker::new(
            self.regions.iter().map(GuestMemoryRegion::range),
            page_size,
        )?);
        self.dirty_tracker = Some(Arc::clone(&tracker));
        Ok(tracker)
    }

    /// Return the active shared dirty-page tracker, if configured.
    pub fn dirty_tracker(&self) -> Option<Arc<GuestMemoryDirtyTracker>> {
        self.dirty_tracker.as_ref().map(Arc::clone)
    }

    /// Capture one exact shared reservation without cloning its mapping or
    /// descriptor.
    pub fn shared_reservation_capture_state(
        &self,
        range: GuestMemoryRange,
    ) -> Result<GuestMemorySharedReservationCaptureState, GuestMemorySharedReservationCaptureError>
    {
        let reservation = self
            .shared_reservations
            .iter()
            .find(|reservation| reservation.range() == range)
            .ok_or(GuestMemorySharedReservationCaptureError::Missing { range })?;
        let shared = reservation.validate_shared_backing().map_err(|source| {
            GuestMemorySharedReservationCaptureError::InvalidBacking { source }
        })?;
        if !shared {
            return Err(GuestMemorySharedReservationCaptureError::Missing { range });
        }
        Ok(GuestMemorySharedReservationCaptureState {
            range,
            mapping_identity: reservation.mapping_identity(),
        })
    }

    /// Allocate and add one process-owned guest memory region.
    ///
    /// This only updates the backend-neutral memory owner. Hypervisor backends
    /// must still map the added range before exposing it to a running guest.
    pub fn insert_region(
        &mut self,
        range: GuestMemoryRange,
    ) -> Result<(), GuestMemoryAllocationError> {
        if self.is_protected_lazy() {
            return Err(GuestMemoryAllocationError::ProtectedLazyMutation);
        }
        let page_size = host_page_size()?;
        let insert_index = self.validate_insert_region(range, page_size)?;
        if let Some(reservation) = self.shared_reservation_containing(range) {
            let mapping_offset = usize::try_from(
                range.start().raw_value() - reservation.range().start().raw_value(),
            )
            .map_err(|_| GuestMemoryAllocationError::SizeTooLarge { range })?;
            let host_size = allocation_host_size(range)?;
            let mapping_end = mapping_offset
                .checked_add(host_size)
                .ok_or(GuestMemoryAllocationError::SizeTooLarge { range })?;
            if mapping_end > reservation.mapping.size() {
                return Err(GuestMemoryAllocationError::SizeTooLarge { range });
            }
            let region = GuestMemoryRegion {
                range,
                mapping: Arc::clone(&reservation.mapping),
                mapping_offset,
                host_size,
            };
            return self.insert_prepared_region(insert_index, region);
        }
        let mut mapper = match self.backing {
            GuestMemoryBacking::Anonymous => SystemGuestMemoryMapper::anonymous(),
            GuestMemoryBacking::Shared => {
                preflight_shared_memory_resources(1, std::slice::from_ref(&range))?;
                SystemGuestMemoryMapper::shared(prepare_shared_memory_files(std::slice::from_ref(
                    &range,
                ))?)
            }
        };

        self.insert_region_with_mapper(range, page_size, &mut mapper)
    }

    fn insert_region_with_mapper(
        &mut self,
        range: GuestMemoryRange,
        page_size: u64,
        mapper: &mut impl GuestMemoryMapper,
    ) -> Result<(), GuestMemoryAllocationError> {
        let insert_index = self.validate_insert_region(range, page_size)?;
        let region = allocate_region_with_mapper(range, page_size, mapper)?;
        self.insert_prepared_region(insert_index, region)
    }

    fn insert_prepared_region(
        &mut self,
        insert_index: usize,
        region: GuestMemoryRegion,
    ) -> Result<(), GuestMemoryAllocationError> {
        self.regions.try_reserve_exact(1).map_err(|source| {
            GuestMemoryAllocationError::RegionMetadataAllocationFailed { source }
        })?;

        let range = region.range();
        if let Some(tracker) = self.dirty_tracker.as_ref() {
            tracker
                .insert_region(range, true)
                .map_err(|source| GuestMemoryAllocationError::DirtyTrackingMetadata { source })?;
        }
        self.regions.insert(insert_index, region);
        Ok(())
    }

    fn validate_insert_region(
        &self,
        range: GuestMemoryRange,
        page_size: u64,
    ) -> Result<usize, GuestMemoryAllocationError> {
        validate_allocation_range(range, page_size)?;

        let mut insert_index = self.regions.len();
        for (index, region) in self.regions.iter().enumerate() {
            let existing_range = region.range();
            if existing_range.overlaps(range) {
                return Err(GuestMemoryAllocationError::InvalidLayout(
                    overlapping_ranges_error(existing_range, range),
                ));
            }

            if insert_index == self.regions.len() && range.start() < existing_range.start() {
                insert_index = index;
            }
        }

        for reservation in &self.shared_reservations {
            let reservation_range = reservation.range();
            if reservation_range.overlaps(range)
                && !guest_memory_range_contains(reservation_range, range)
            {
                return Err(GuestMemoryAllocationError::InvalidLayout(
                    overlapping_ranges_error(reservation_range, range),
                ));
            }
        }

        Ok(insert_index)
    }

    /// Reserve one descriptor-backed guest-physical aperture without making
    /// any byte in it active guest RAM.
    ///
    /// Dynamically inserted ranges wholly contained by this aperture become
    /// bounded views of the retained mapping. The reservation itself is
    /// excluded from [`Self::regions`], byte access, dirty tracking, and
    /// [`Self::total_size`].
    pub fn reserve_shared_region(
        &mut self,
        range: GuestMemoryRange,
    ) -> Result<(), GuestMemoryAllocationError> {
        if self.is_protected_lazy() {
            return Err(GuestMemoryAllocationError::ProtectedLazyMutation);
        }
        let page_size = host_page_size()?;
        let host_size = validate_allocation_range(range, page_size)?;
        for region in &self.regions {
            if region.range().overlaps(range) {
                return Err(GuestMemoryAllocationError::InvalidLayout(
                    overlapping_ranges_error(region.range(), range),
                ));
            }
        }

        let mut insert_index = self.shared_reservations.len();
        for (index, reservation) in self.shared_reservations.iter().enumerate() {
            if reservation.range().overlaps(range) {
                return Err(GuestMemoryAllocationError::InvalidLayout(
                    overlapping_ranges_error(reservation.range(), range),
                ));
            }
            if insert_index == self.shared_reservations.len()
                && range.start() < reservation.range().start()
            {
                insert_index = index;
            }
        }

        let retained_descriptors = self.shared_export_regions().count().saturating_add(1);
        let largest_region = self
            .regions
            .iter()
            .chain(self.shared_reservations.iter())
            .map(|region| region.range().size())
            .chain(std::iter::once(range.size()))
            .max()
            .unwrap_or(0);
        preflight_shared_memory_resource_values(retained_descriptors, largest_region)?;

        self.shared_reservations
            .try_reserve_exact(1)
            .map_err(
                |source| GuestMemoryAllocationError::RegionMetadataAllocationFailed { source },
            )?;
        let file = match create_shared_memory_file(host_size) {
            Err(GuestMemoryAllocationError::SharedBackingCreateFailed { source })
                if source.raw_os_error() == Some(libc::EMFILE) =>
            {
                return Err(
                    GuestMemoryAllocationError::SharedFileDescriptorLimitExceeded {
                        regions: retained_descriptors,
                    },
                );
            }
            result => result?,
        };
        let mapping = Arc::new(GuestMemoryMapping::map_shared(host_size, file)?);
        self.shared_reservations.insert(
            insert_index,
            GuestMemoryRegion {
                range,
                mapping,
                mapping_offset: 0,
                host_size,
            },
        );
        Ok(())
    }

    fn shared_reservation_containing(&self, range: GuestMemoryRange) -> Option<&GuestMemoryRegion> {
        self.shared_reservations
            .iter()
            .find(|reservation| guest_memory_range_contains(reservation.range(), range))
    }

    pub(crate) fn shared_export_regions(&self) -> impl Iterator<Item = &GuestMemoryRegion> {
        let export_allowed = !self.is_protected_lazy();
        let mut active = self
            .regions
            .iter()
            .filter(move |region| {
                export_allowed
                    && region.backing() == GuestMemoryRegionBacking::Shared
                    && self.shared_reservation_containing(region.range()).is_none()
            })
            .peekable();
        let mut reservations = self
            .shared_reservations
            .iter()
            .filter(move |_| export_allowed)
            .peekable();

        std::iter::from_fn(move || {
            let take_active = match (active.peek(), reservations.peek()) {
                (Some(active), Some(reservation)) => {
                    active.range().start() < reservation.range().start()
                }
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => return None,
            };
            if take_active {
                active.next()
            } else {
                reservations.next()
            }
        })
    }

    /// Remove and drop one exactly matching process-owned guest memory region.
    ///
    /// This only updates the backend-neutral memory owner. Hypervisor backends
    /// must unmap the range before removing memory that may be visible to a
    /// running guest.
    pub fn remove_region(
        &mut self,
        range: GuestMemoryRange,
    ) -> Result<(), GuestMemoryRegionRemovalError> {
        if self.is_protected_lazy() {
            return Err(GuestMemoryRegionRemovalError::ProtectedLazyMutation);
        }
        let Some(index) = self
            .regions
            .iter()
            .position(|region| region.range() == range)
        else {
            return Err(GuestMemoryRegionRemovalError::MissingRange { range });
        };

        if let Some(tracker) = self.dirty_tracker.as_ref() {
            let removed = tracker.remove_region(range);
            debug_assert!(removed, "dirty tracker must mirror guest-memory regions");
        }
        self.regions.remove(index);
        Ok(())
    }

    pub fn regions(&self) -> &[GuestMemoryRegion] {
        &self.regions
    }

    pub fn total_size(&self) -> u64 {
        self.regions
            .iter()
            .map(|region| region.range().size())
            .sum::<u64>()
    }

    /// Makes the host-page-aligned interior of one mapped guest range zero-safe
    /// and reclaimable when the current target supports that operation.
    ///
    /// The complete guest range is validated before host reclaim. Partial host
    /// pages at each owned mapping edge are skipped, and failures are reported
    /// without exposing host addresses or changing guest-memory ownership.
    pub fn discard_range(&self, range: GuestMemoryRange) -> GuestMemoryDiscardOutcome {
        let mut adviser = SystemGuestMemoryDiscardAdviser;
        self.discard_range_with_adviser(range, &mut adviser)
    }

    pub(crate) fn discard_range_with_adviser(
        &self,
        range: GuestMemoryRange,
        adviser: &mut impl GuestMemoryDiscardAdviser,
    ) -> GuestMemoryDiscardOutcome {
        let mut outcome = GuestMemoryDiscardOutcome::new(range.size());
        if self.is_protected_lazy() {
            return outcome.fail_all(GuestMemoryDiscardFailureKind::UnsupportedTarget);
        }
        if self.validate_mapped_range(range).is_err() {
            return outcome.fail_all(GuestMemoryDiscardFailureKind::RangeValidation);
        }

        let page_size = match adviser.host_page_size() {
            Ok(page_size) => page_size,
            Err(kind) => return outcome.fail_all(kind),
        };
        let Ok(page_size) = usize::try_from(page_size) else {
            return outcome.fail_all(GuestMemoryDiscardFailureKind::InvalidHostPageSize);
        };
        if page_size == 0 || !page_size.is_power_of_two() {
            return outcome.fail_all(GuestMemoryDiscardFailureKind::InvalidHostPageSize);
        }

        for region in &self.regions {
            let Some(segment) = discard_segment(region, range) else {
                continue;
            };
            let Ok(segment_size) = usize::try_from(segment.size) else {
                outcome.record_failed(segment.size, GuestMemoryDiscardFailureKind::HostAddress);
                continue;
            };
            let Ok(offset) = usize::try_from(segment.offset) else {
                outcome.record_failed(segment.size, GuestMemoryDiscardFailureKind::HostAddress);
                continue;
            };
            if offset
                .checked_add(segment_size)
                .is_none_or(|end| end > region.host_size())
            {
                outcome.record_failed(segment.size, GuestMemoryDiscardFailureKind::HostAddress);
                continue;
            }

            let mapping_start = region.host_address().as_ptr().cast::<u8>();
            let segment_start = mapping_start.wrapping_add(offset);
            let segment_start_address = segment_start.addr();
            let Some(segment_end_address) = segment_start_address.checked_add(segment_size) else {
                outcome.record_failed(segment.size, GuestMemoryDiscardFailureKind::HostAddress);
                continue;
            };
            let Some(advice_start_address) = align_up_usize(segment_start_address, page_size)
            else {
                outcome.record_failed(segment.size, GuestMemoryDiscardFailureKind::HostAddress);
                continue;
            };
            let advice_end_address = align_down_usize(segment_end_address, page_size);
            if advice_start_address >= advice_end_address {
                outcome.record_skipped(segment.size);
                continue;
            }

            let advice_size = advice_end_address - advice_start_address;
            let skipped_size = segment_size - advice_size;
            let (Ok(advice_size_u64), Ok(skipped_size_u64)) =
                (u64::try_from(advice_size), u64::try_from(skipped_size))
            else {
                outcome.record_failed(segment.size, GuestMemoryDiscardFailureKind::HostAddress);
                continue;
            };
            outcome.record_skipped(skipped_size_u64);

            let advice_address = segment_start
                .with_addr(advice_start_address)
                .cast::<c_void>();
            let Some(advice_address) = NonNull::new(advice_address) else {
                outcome.record_failed(advice_size_u64, GuestMemoryDiscardFailureKind::HostAddress);
                continue;
            };
            if let Some(tracker) = self.dirty_tracker.as_ref() {
                let Some(advice_guest_start) = region.range().start().checked_add(
                    u64::try_from(advice_start_address - mapping_start.addr()).unwrap_or(u64::MAX),
                ) else {
                    outcome
                        .record_failed(advice_size_u64, GuestMemoryDiscardFailureKind::HostAddress);
                    continue;
                };
                let Ok(advice_range) = GuestMemoryRange::new(advice_guest_start, advice_size_u64)
                else {
                    outcome
                        .record_failed(advice_size_u64, GuestMemoryDiscardFailureKind::HostAddress);
                    continue;
                };
                if tracker.mark_range(advice_range).is_err() {
                    outcome.record_failed(
                        advice_size_u64,
                        GuestMemoryDiscardFailureKind::RangeValidation,
                    );
                    continue;
                }
            }
            let Some(mapping_offset) = region
                .mapping_offset
                .checked_add(advice_start_address - mapping_start.addr())
            else {
                outcome.record_failed(advice_size_u64, GuestMemoryDiscardFailureKind::HostAddress);
                continue;
            };
            if let Err(kind) = adviser.reclaim(
                region.mapping.as_ref(),
                mapping_offset,
                advice_address,
                advice_size,
            ) {
                outcome.record_failed(advice_size_u64, kind);
                continue;
            }

            outcome.record_advised(advice_size_u64);
        }

        outcome
    }

    pub fn write_slice(
        &mut self,
        source: &[u8],
        guest_address: GuestAddress,
    ) -> Result<(), GuestMemoryAccessError> {
        let Some(range) = access_range(guest_address, source.len())? else {
            return Ok(());
        };

        self.validate_mapped_range(range)?;

        if let Some(tracker) = self.dirty_tracker.as_ref() {
            tracker
                .mark_range(range)
                .map_err(|_| GuestMemoryAccessError::DirtyTrackingState)?;
        }

        let mut remaining = source;
        let mut current = range.start();
        for region in &mut self.regions {
            if remaining.is_empty() {
                break;
            }
            if region.range().end_exclusive() <= current {
                continue;
            }

            let segment = access_segment(region, current, range.end_exclusive())?;
            let (source_segment, next_remaining) = remaining.split_at(segment.size);
            let destination = region
                .host_address()
                .as_ptr()
                .cast::<u8>()
                .wrapping_add(segment.offset);

            // SAFETY: `validate_mapped_range` proved the whole requested guest
            // range is backed by live mappings. `access_segment` bounds this
            // segment to `region`, and the destination pointer is within that
            // mapping. `ptr::copy` permits overlap if `source` was derived from
            // the same guest mapping through a raw host pointer.
            unsafe {
                ptr::copy(source_segment.as_ptr(), destination, segment.size);
            }

            remaining = next_remaining;
            current = advance_address(current, segment.size)?;
        }

        Ok(())
    }

    /// Retain one aligned 64-bit word for cross-thread atomic publication.
    pub fn atomic_u64(
        &self,
        guest_address: GuestAddress,
    ) -> Result<GuestMemoryAtomicU64, GuestMemoryAccessError> {
        const WORD_SIZE: usize = size_of::<u64>();
        const WORD_ALIGNMENT: u64 = align_of::<AtomicU64>() as u64;

        if !guest_address.is_aligned(WORD_ALIGNMENT).map_err(|_| {
            GuestMemoryAccessError::UnalignedAtomicAccess {
                address: guest_address,
                alignment: WORD_ALIGNMENT,
            }
        })? {
            return Err(GuestMemoryAccessError::UnalignedAtomicAccess {
                address: guest_address,
                alignment: WORD_ALIGNMENT,
            });
        }
        let Some(range) = access_range(guest_address, WORD_SIZE)? else {
            return Err(GuestMemoryAccessError::SizeTooLarge { size: WORD_SIZE });
        };
        self.validate_mapped_range(range)?;

        let region = self
            .regions
            .iter()
            .find(|region| {
                region.range().contains(range.start())
                    && range.end_exclusive() <= region.range().end_exclusive()
            })
            .ok_or(GuestMemoryAccessError::AtomicAccessCrossesRegion { range })?;
        let offset = range.start().raw_value() - region.range().start().raw_value();
        let offset =
            usize::try_from(offset).map_err(|_| GuestMemoryAccessError::SegmentOffsetTooLarge {
                range: region.range(),
                offset,
            })?;
        let mapping_offset = region.mapping_offset.checked_add(offset).ok_or(
            GuestMemoryAccessError::AtomicHostOffsetOverflow {
                address: guest_address,
            },
        )?;
        let host_address = region
            .mapping
            .address()
            .as_ptr()
            .cast::<u8>()
            .wrapping_add(mapping_offset)
            .addr();
        if !host_address.is_multiple_of(WORD_ALIGNMENT as usize) {
            return Err(GuestMemoryAccessError::UnalignedAtomicHostAddress {
                address: guest_address,
                alignment: WORD_ALIGNMENT,
            });
        }

        Ok(GuestMemoryAtomicU64 {
            mapping: Arc::clone(&region.mapping),
            mapping_offset,
            range,
            dirty_tracker: self.dirty_tracker.as_ref().map(Arc::clone),
        })
    }

    pub fn read_slice(
        &self,
        destination: &mut [u8],
        guest_address: GuestAddress,
    ) -> Result<(), GuestMemoryAccessError> {
        let Some(range) = access_range(guest_address, destination.len())? else {
            return Ok(());
        };

        self.validate_mapped_range(range)?;

        let mut remaining = destination;
        let mut current = range.start();
        for region in &self.regions {
            if remaining.is_empty() {
                break;
            }
            if region.range().end_exclusive() <= current {
                continue;
            }

            let segment = access_segment(region, current, range.end_exclusive())?;
            let (destination_segment, next_remaining) = remaining.split_at_mut(segment.size);
            let source = region
                .host_address()
                .as_ptr()
                .cast::<u8>()
                .wrapping_add(segment.offset);

            // SAFETY: `validate_mapped_range` proved the whole requested guest
            // range is backed by live mappings. `access_segment` bounds this
            // segment to `region`, and the source pointer is within that
            // mapping. `ptr::copy` permits overlap if `destination` was derived
            // from the same guest mapping through a raw host pointer.
            unsafe {
                ptr::copy(source, destination_segment.as_mut_ptr(), segment.size);
            }

            remaining = next_remaining;
            current = advance_address(current, segment.size)?;
        }

        Ok(())
    }

    pub(crate) fn validate_mapped_range(
        &self,
        range: GuestMemoryRange,
    ) -> Result<(), GuestMemoryAccessError> {
        let mut current = range.start();
        for region in &self.regions {
            if region.range().end_exclusive() <= current {
                continue;
            }
            if !region.range().contains(current) {
                return Err(GuestMemoryAccessError::UnmappedRange { range });
            }

            let segment = access_segment(region, current, range.end_exclusive())?;
            current = advance_address(current, segment.size)?;
            if current == range.end_exclusive() {
                return Ok(());
            }
        }

        Err(GuestMemoryAccessError::UnmappedRange { range })
    }
}

pub struct GuestMemoryRegion {
    range: GuestMemoryRange,
    mapping: Arc<GuestMemoryMapping>,
    mapping_offset: usize,
    host_size: usize,
}

impl GuestMemoryRegion {
    pub const fn range(&self) -> GuestMemoryRange {
        self.range
    }

    pub fn host_address(&self) -> NonNull<c_void> {
        let address = self
            .mapping
            .address()
            .as_ptr()
            .cast::<u8>()
            .wrapping_add(self.mapping_offset)
            .cast::<c_void>();
        // SAFETY: every mapping has a non-null live base and construction
        // validates `mapping_offset + host_size` within that mapping. The
        // resulting view start therefore remains inside the same mapping.
        unsafe { NonNull::new_unchecked(address) }
    }

    pub const fn host_size(&self) -> usize {
        self.host_size
    }

    /// Return the actual backing owner for this region.
    pub fn backing(&self) -> GuestMemoryRegionBacking {
        self.mapping.backing()
    }

    /// Return an opaque value identity without retaining the backing mapping.
    pub fn mapping_identity(&self) -> GuestMemoryMappingIdentity {
        self.mapping.identity()
    }

    /// Validates shared descriptor metadata without duplicating the descriptor.
    ///
    /// Returns `false` for anonymous regions and `true` for a valid shared
    /// region whose descriptor still has the exact mapping length.
    pub fn validate_shared_backing(&self) -> Result<bool, GuestMemorySharedBackingError> {
        self.mapping
            .validate_shared_backing(self.mapping_offset, self.host_size)
    }

    /// Clone the descriptor-backed export metadata for a shared region.
    ///
    /// Anonymous regions return `Ok(None)`. The cloned descriptor owns the
    /// same unlinked object and is independent of the mapping owner's handle.
    pub fn try_clone_shared_backing(
        &self,
    ) -> Result<Option<GuestMemorySharedBacking>, GuestMemorySharedBackingError> {
        self.mapping
            .try_clone_shared_backing(self.mapping_offset, self.host_size)
    }
}

impl fmt::Debug for GuestMemoryRegion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuestMemoryRegion")
            .field("range", &self.range)
            .field("host_size", &self.host_size)
            .field("mapping_offset", &self.mapping_offset)
            .finish_non_exhaustive()
    }
}

/// An owned descriptor export for one shared guest-memory region.
pub struct GuestMemorySharedBacking {
    file: File,
    offset: u64,
    len: u64,
}

impl GuestMemorySharedBacking {
    /// Return the byte offset at which this region starts in the descriptor.
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    /// Return the exact byte length of this region.
    pub const fn len(&self) -> u64 {
        self.len
    }

    /// Return whether this export describes an empty region.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl AsFd for GuestMemorySharedBacking {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

impl fmt::Debug for GuestMemorySharedBacking {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuestMemorySharedBacking")
            .field("offset", &self.offset)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum GuestMemorySharedBackingError {
    DuplicateDescriptor {
        source: io::Error,
    },
    InspectDescriptor {
        source: io::Error,
    },
    UnexpectedLength {
        expected: u64,
        actual: u64,
    },
    LengthTooLarge {
        size: usize,
    },
    InvalidRange {
        offset: usize,
        len: usize,
        mapping_size: usize,
    },
}

impl fmt::Display for GuestMemorySharedBackingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateDescriptor { source } => {
                write!(
                    f,
                    "failed to duplicate shared guest-memory descriptor: {source}"
                )
            }
            Self::InspectDescriptor { source } => {
                write!(
                    f,
                    "failed to inspect shared guest-memory descriptor: {source}"
                )
            }
            Self::UnexpectedLength { expected, actual } => write!(
                f,
                "shared guest-memory descriptor length changed: expected {expected} bytes, found {actual} bytes"
            ),
            Self::LengthTooLarge { size } => write!(
                f,
                "shared guest-memory descriptor length {size} bytes cannot be represented"
            ),
            Self::InvalidRange {
                offset,
                len,
                mapping_size,
            } => write!(
                f,
                "shared guest-memory export [{offset}..{}) exceeds its {mapping_size}-byte mapping",
                offset.saturating_add(*len)
            ),
        }
    }
}

impl std::error::Error for GuestMemorySharedBackingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DuplicateDescriptor { source } | Self::InspectDescriptor { source } => {
                Some(source)
            }
            Self::UnexpectedLength { .. }
            | Self::LengthTooLarge { .. }
            | Self::InvalidRange { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum GuestMemoryAllocationError {
    InvalidLayout(GuestMemoryError),
    ProtectedLazyMutation,
    InvalidHostPageSize,
    SizeTooLarge {
        range: GuestMemoryRange,
    },
    RegionMetadataAllocationFailed {
        source: TryReserveError,
    },
    DirtyTrackingMetadata {
        source: GuestMemoryDirtyTrackerError,
    },
    AnonymousMmapFailed {
        size: usize,
        source: io::Error,
    },
    AnonymousMmapReturnedNull {
        size: usize,
    },
    SharedResourceLimitQueryFailed {
        resource: GuestMemorySharedResource,
        source: io::Error,
    },
    SharedFileSizeLimitExceeded {
        size: u64,
    },
    SharedFileDescriptorLimitExceeded {
        regions: usize,
    },
    SharedNameGenerationFailed {
        source: getrandom::Error,
    },
    SharedBackingCreateFailed {
        source: io::Error,
    },
    SharedBackingUnlinkFailed {
        source: io::Error,
    },
    SharedBackingResizeFailed {
        size: u64,
        source: io::Error,
    },
    SharedBackingSizeTooLarge {
        size: usize,
    },
    SharedBackingReservationMissing {
        size: usize,
    },
    SharedMmapFailed {
        size: usize,
        source: io::Error,
    },
    SharedMmapReturnedNull {
        size: usize,
    },
    PrivateFileInspectFailed {
        source: io::Error,
    },
    PrivateFileMappingSizeInvalid,
    PrivateFileOffsetUnaligned,
    PrivateFileOffsetTooLarge,
    PrivateFileRangeBeyondEnd,
    PrivateFileMmapFailed {
        size: usize,
        source: io::Error,
    },
    PrivateFileMmapReturnedNull {
        size: usize,
    },
    MappingIdentityExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestMemorySharedResource {
    FileSize,
    FileDescriptors,
}

impl fmt::Display for GuestMemorySharedResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileSize => f.write_str("file-size limit"),
            Self::FileDescriptors => f.write_str("file-descriptor limit"),
        }
    }
}

impl fmt::Display for GuestMemoryAllocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLayout(source) => {
                write!(f, "invalid guest memory layout for allocation: {source}")
            }
            Self::ProtectedLazyMutation => {
                f.write_str("protected lazy guest memory has immutable region topology")
            }
            Self::InvalidHostPageSize => f.write_str("host page size is unavailable or invalid"),
            Self::SizeTooLarge { range } => {
                write!(
                    f,
                    "guest memory range {range} is too large to allocate on this host"
                )
            }
            Self::RegionMetadataAllocationFailed { source } => {
                write!(
                    f,
                    "failed to reserve guest memory region metadata: {source}"
                )
            }
            Self::DirtyTrackingMetadata { source } => {
                write!(f, "failed to extend guest-memory dirty tracking: {source}")
            }
            Self::AnonymousMmapFailed { size, source } => {
                write!(
                    f,
                    "failed to allocate anonymous guest memory mapping of {size} bytes: {source}"
                )
            }
            Self::AnonymousMmapReturnedNull { size } => {
                write!(
                    f,
                    "anonymous guest memory mapping of {size} bytes returned a null address"
                )
            }
            Self::SharedResourceLimitQueryFailed { resource, source } => {
                write!(
                    f,
                    "failed to query shared guest-memory {resource}: {source}"
                )
            }
            Self::SharedFileSizeLimitExceeded { size } => write!(
                f,
                "shared guest memory region of {size} bytes exceeds the process file-size limit"
            ),
            Self::SharedFileDescriptorLimitExceeded { regions } => write!(
                f,
                "shared guest memory could not reserve {regions} retained descriptors within the process file-descriptor limit"
            ),
            Self::SharedNameGenerationFailed { source } => {
                write!(
                    f,
                    "failed to generate a private shared-memory name: {source}"
                )
            }
            Self::SharedBackingCreateFailed { source } => {
                write!(
                    f,
                    "failed to create an owner-only shared-memory object: {source}"
                )
            }
            Self::SharedBackingUnlinkFailed { source } => {
                write!(f, "failed to unlink a shared-memory object: {source}")
            }
            Self::SharedBackingResizeFailed { size, source } => write!(
                f,
                "failed to size a shared-memory object to {size} bytes: {source}"
            ),
            Self::SharedBackingSizeTooLarge { size } => write!(
                f,
                "shared guest-memory backing size {size} bytes cannot be represented"
            ),
            Self::SharedBackingReservationMissing { size } => write!(
                f,
                "shared guest-memory backing reservation is missing for {size} bytes"
            ),
            Self::SharedMmapFailed { size, source } => write!(
                f,
                "failed to map {size} bytes of descriptor-backed shared guest memory: {source}"
            ),
            Self::SharedMmapReturnedNull { size } => write!(
                f,
                "descriptor-backed shared guest memory mapping of {size} bytes returned a null address"
            ),
            Self::PrivateFileInspectFailed { source } => write!(
                f,
                "failed to inspect private-file guest memory backing: {source}"
            ),
            Self::PrivateFileMappingSizeInvalid => {
                f.write_str("private-file guest memory mapping size is invalid")
            }
            Self::PrivateFileOffsetUnaligned => {
                f.write_str("private-file guest memory offset is not host-page aligned")
            }
            Self::PrivateFileOffsetTooLarge => {
                f.write_str("private-file guest memory offset cannot be represented")
            }
            Self::PrivateFileRangeBeyondEnd => {
                f.write_str("private-file guest memory range exceeds its backing file")
            }
            Self::PrivateFileMmapFailed { size, source } => write!(
                f,
                "failed to map {size} bytes of private-file guest memory: {source}"
            ),
            Self::PrivateFileMmapReturnedNull { size } => write!(
                f,
                "private-file guest memory mapping of {size} bytes returned a null address"
            ),
            Self::MappingIdentityExhausted => {
                f.write_str("guest-memory mapping identity space is exhausted")
            }
        }
    }
}

impl std::error::Error for GuestMemoryAllocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidLayout(source) => Some(source),
            Self::RegionMetadataAllocationFailed { source } => Some(source),
            Self::DirtyTrackingMetadata { source } => Some(source),
            Self::AnonymousMmapFailed { source, .. } => Some(source),
            Self::SharedResourceLimitQueryFailed { source, .. }
            | Self::SharedBackingCreateFailed { source }
            | Self::SharedBackingUnlinkFailed { source }
            | Self::SharedBackingResizeFailed { source, .. }
            | Self::SharedMmapFailed { source, .. }
            | Self::PrivateFileInspectFailed { source }
            | Self::PrivateFileMmapFailed { source, .. } => Some(source),
            Self::SharedNameGenerationFailed { .. } => None,
            Self::ProtectedLazyMutation
            | Self::InvalidHostPageSize
            | Self::SizeTooLarge { .. }
            | Self::AnonymousMmapReturnedNull { .. }
            | Self::SharedFileSizeLimitExceeded { .. }
            | Self::SharedFileDescriptorLimitExceeded { .. }
            | Self::SharedBackingSizeTooLarge { .. }
            | Self::SharedBackingReservationMissing { .. }
            | Self::SharedMmapReturnedNull { .. }
            | Self::PrivateFileMappingSizeInvalid
            | Self::PrivateFileOffsetUnaligned
            | Self::PrivateFileOffsetTooLarge
            | Self::PrivateFileRangeBeyondEnd
            | Self::PrivateFileMmapReturnedNull { .. }
            | Self::MappingIdentityExhausted => None,
        }
    }
}

impl From<GuestMemoryError> for GuestMemoryAllocationError {
    fn from(source: GuestMemoryError) -> Self {
        Self::InvalidLayout(source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestMemoryRegionRemovalError {
    /// Protected lazy memory is immutable outside its page coordinator.
    ProtectedLazyMutation,
    /// No owned region exactly matches the requested guest range.
    MissingRange { range: GuestMemoryRange },
}

impl fmt::Display for GuestMemoryRegionRemovalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProtectedLazyMutation => {
                f.write_str("protected lazy guest memory has immutable region topology")
            }
            Self::MissingRange { range } => {
                write!(f, "guest memory region {range} is not mapped")
            }
        }
    }
}

impl std::error::Error for GuestMemoryRegionRemovalError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestMemoryAccessError {
    SizeTooLarge {
        size: usize,
    },
    AddressOverflow {
        start: GuestAddress,
        size: u64,
    },
    UnmappedRange {
        range: GuestMemoryRange,
    },
    SegmentOffsetTooLarge {
        range: GuestMemoryRange,
        offset: u64,
    },
    SegmentSizeTooLarge {
        range: GuestMemoryRange,
        size: u64,
    },
    UnalignedAtomicAccess {
        address: GuestAddress,
        alignment: u64,
    },
    UnalignedAtomicHostAddress {
        address: GuestAddress,
        alignment: u64,
    },
    AtomicAccessCrossesRegion {
        range: GuestMemoryRange,
    },
    AtomicHostOffsetOverflow {
        address: GuestAddress,
    },
    DirtyTrackingState,
}

impl fmt::Display for GuestMemoryAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SizeTooLarge { size } => {
                write!(
                    f,
                    "guest memory access size {size} bytes is too large to represent"
                )
            }
            Self::AddressOverflow { start, size } => {
                write!(
                    f,
                    "guest memory access overflows address space: start={start}, size={size}"
                )
            }
            Self::UnmappedRange { range } => {
                write!(f, "guest memory access range {range} is not fully mapped")
            }
            Self::SegmentOffsetTooLarge { range, offset } => {
                write!(
                    f,
                    "guest memory access offset {offset} in range {range} is too large for this host"
                )
            }
            Self::SegmentSizeTooLarge { range, size } => {
                write!(
                    f,
                    "guest memory access segment of {size} bytes in range {range} is too large for this host"
                )
            }
            Self::UnalignedAtomicAccess { address, alignment } => write!(
                f,
                "guest memory atomic access at {address} is not aligned to {alignment} bytes"
            ),
            Self::UnalignedAtomicHostAddress { address, alignment } => write!(
                f,
                "host mapping for guest atomic access at {address} is not aligned to {alignment} bytes"
            ),
            Self::AtomicAccessCrossesRegion { range } => write!(
                f,
                "guest memory atomic access range {range} crosses a mapping boundary"
            ),
            Self::AtomicHostOffsetOverflow { address } => write!(
                f,
                "host mapping offset for guest atomic access at {address} overflows"
            ),
            Self::DirtyTrackingState => {
                f.write_str("guest memory dirty tracking does not cover the write")
            }
        }
    }
}

impl std::error::Error for GuestMemoryAccessError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestMemoryError {
    EmptyLayout,
    EmptyRange {
        start: GuestAddress,
    },
    AddressOverflow {
        start: GuestAddress,
        size: u64,
    },
    InvalidAlignment {
        alignment: u64,
    },
    UnalignedRange {
        range: GuestMemoryRange,
        alignment: u64,
    },
    UnorderedRange {
        previous: GuestMemoryRange,
        next: GuestMemoryRange,
    },
    OverlappingRange {
        previous: GuestMemoryRange,
        next: GuestMemoryRange,
    },
}

impl fmt::Display for GuestMemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyLayout => f.write_str("guest memory layout must contain at least one range"),
            Self::EmptyRange { start } => {
                write!(f, "guest memory range at {start} must not be empty")
            }
            Self::AddressOverflow { start, size } => {
                write!(
                    f,
                    "guest memory range overflows address space: start={start}, size={size}"
                )
            }
            Self::InvalidAlignment { alignment } => {
                write!(
                    f,
                    "guest memory alignment must be a nonzero power of two: {alignment}"
                )
            }
            Self::UnalignedRange { range, alignment } => {
                write!(
                    f,
                    "guest memory range {range} is not aligned to {alignment} bytes"
                )
            }
            Self::UnorderedRange { previous, next } => {
                write!(
                    f,
                    "guest memory ranges must be ordered by start address: {previous} before {next}"
                )
            }
            Self::OverlappingRange { previous, next } => {
                write!(
                    f,
                    "guest memory ranges must not overlap: {previous} overlaps {next}"
                )
            }
        }
    }
}

impl std::error::Error for GuestMemoryError {}

pub mod aarch64 {
    use super::{GuestAddress, GuestMemoryError, GuestMemoryLayout, GuestMemoryRange};

    pub const DRAM_MEM_START: u64 = 0x8000_0000;
    pub const DRAM_MEM_MAX_SIZE: u64 = 0x00FF_8000_0000;
    pub const SYSTEM_MEM_START: u64 = DRAM_MEM_START;
    pub const SYSTEM_MEM_SIZE: u64 = 0x20_0000;
    pub const CMDLINE_MAX_SIZE: usize = 2048;
    pub const FDT_MAX_SIZE: u64 = 0x20_0000;
    pub const GUEST_PAGE_SIZE: u64 = 4096;
    pub const MMIO64_MEM_START: u64 = 256 << 30;
    pub const MMIO64_MEM_SIZE: u64 = 256 << 30;
    pub const FIRST_ADDR_PAST_64BITS_MMIO: u64 = MMIO64_MEM_START + MMIO64_MEM_SIZE;

    pub const fn effective_dram_size(requested_size: u64) -> u64 {
        if requested_size > DRAM_MEM_MAX_SIZE {
            DRAM_MEM_MAX_SIZE
        } else {
            requested_size
        }
    }

    pub fn dram_layout(requested_size: u64) -> Result<GuestMemoryLayout, GuestMemoryError> {
        if requested_size == 0 {
            return Err(GuestMemoryError::EmptyLayout);
        }

        let dram_size = effective_dram_size(requested_size);
        let size_before_mmio64_gap = MMIO64_MEM_START - DRAM_MEM_START;

        let ranges = if dram_size <= size_before_mmio64_gap {
            vec![GuestMemoryRange::new(
                GuestAddress::new(DRAM_MEM_START),
                dram_size,
            )?]
        } else {
            vec![
                GuestMemoryRange::new(GuestAddress::new(DRAM_MEM_START), size_before_mmio64_gap)?,
                GuestMemoryRange::new(
                    GuestAddress::new(FIRST_ADDR_PAST_64BITS_MMIO),
                    dram_size - size_before_mmio64_gap,
                )?,
            ]
        };

        GuestMemoryLayout::new(ranges)
    }

    pub const fn kernel_load_address() -> GuestAddress {
        GuestAddress::new(SYSTEM_MEM_START + SYSTEM_MEM_SIZE)
    }

    pub fn fdt_address(layout: &GuestMemoryLayout) -> Result<GuestAddress, GuestMemoryError> {
        let first_range = first_range(layout)?;
        let candidate = match first_range
            .end_exclusive()
            .raw_value()
            .checked_sub(FDT_MAX_SIZE)
        {
            Some(address) => GuestAddress::new(address),
            None => return Ok(first_range.start()),
        };

        if first_range.contains(candidate) {
            Ok(candidate)
        } else {
            Ok(first_range.start())
        }
    }

    pub fn initrd_load_address(
        layout: &GuestMemoryLayout,
        initrd_size: u64,
    ) -> Result<Option<GuestAddress>, GuestMemoryError> {
        let fdt_address = fdt_address(layout)?;
        let Some(rounded_size) = align_up(initrd_size, GUEST_PAGE_SIZE) else {
            return Ok(None);
        };
        let Some(load_address) = fdt_address.raw_value().checked_sub(rounded_size) else {
            return Ok(None);
        };
        let load_address = GuestAddress::new(load_address);

        if first_range(layout)?.contains(load_address) {
            Ok(Some(load_address))
        } else {
            Ok(None)
        }
    }

    fn first_range(layout: &GuestMemoryLayout) -> Result<GuestMemoryRange, GuestMemoryError> {
        layout
            .ranges()
            .first()
            .copied()
            .ok_or(GuestMemoryError::EmptyLayout)
    }

    const fn align_up(value: u64, alignment: u64) -> Option<u64> {
        if alignment == 0 || !alignment.is_power_of_two() {
            return None;
        }

        let mask = alignment - 1;
        match value.checked_add(mask) {
            Some(rounded) => Some(rounded & !mask),
            None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GuestMemorySegment {
    offset: usize,
    size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GuestMemoryDiscardSegment {
    offset: u64,
    size: u64,
}

fn discard_segment(
    region: &GuestMemoryRegion,
    requested: GuestMemoryRange,
) -> Option<GuestMemoryDiscardSegment> {
    let region_range = region.range();
    let start = region_range
        .start()
        .raw_value()
        .max(requested.start().raw_value());
    let end = region_range
        .end_exclusive()
        .raw_value()
        .min(requested.end_exclusive().raw_value());
    if start >= end {
        return None;
    }

    Some(GuestMemoryDiscardSegment {
        offset: start - region_range.start().raw_value(),
        size: end - start,
    })
}

fn align_up_usize(value: usize, alignment: usize) -> Option<usize> {
    let mask = alignment.checked_sub(1)?;
    value.checked_add(mask).map(|rounded| rounded & !mask)
}

const fn align_down_usize(value: usize, alignment: usize) -> usize {
    value & !(alignment - 1)
}

fn access_range(
    start: GuestAddress,
    size: usize,
) -> Result<Option<GuestMemoryRange>, GuestMemoryAccessError> {
    if size == 0 {
        return Ok(None);
    }

    let size = u64::try_from(size).map_err(|_| GuestMemoryAccessError::SizeTooLarge { size })?;
    let end_exclusive = start
        .checked_add(size)
        .ok_or(GuestMemoryAccessError::AddressOverflow { start, size })?;

    Ok(Some(GuestMemoryRange {
        start,
        size,
        end_exclusive,
    }))
}

fn access_segment(
    region: &GuestMemoryRegion,
    current: GuestAddress,
    end: GuestAddress,
) -> Result<GuestMemorySegment, GuestMemoryAccessError> {
    let range = region.range();
    let offset = current.raw_value() - range.start().raw_value();
    let offset = usize::try_from(offset)
        .map_err(|_| GuestMemoryAccessError::SegmentOffsetTooLarge { range, offset })?;
    let size = (range.end_exclusive().raw_value() - current.raw_value())
        .min(end.raw_value() - current.raw_value());
    let size = usize::try_from(size)
        .map_err(|_| GuestMemoryAccessError::SegmentSizeTooLarge { range, size })?;

    Ok(GuestMemorySegment { offset, size })
}

fn advance_address(
    address: GuestAddress,
    offset: usize,
) -> Result<GuestAddress, GuestMemoryAccessError> {
    let size =
        u64::try_from(offset).map_err(|_| GuestMemoryAccessError::SizeTooLarge { size: offset })?;
    address
        .checked_add(size)
        .ok_or(GuestMemoryAccessError::AddressOverflow {
            start: address,
            size,
        })
}

fn validate_alignment(alignment: u64) -> Result<(), GuestMemoryError> {
    if alignment == 0 || !alignment.is_power_of_two() {
        Err(GuestMemoryError::InvalidAlignment { alignment })
    } else {
        Ok(())
    }
}

fn validate_allocation_ranges(
    layout: &GuestMemoryLayout,
    page_size: u64,
) -> Result<(), GuestMemoryAllocationError> {
    validate_host_page_size(page_size)?;

    for range in layout.ranges().iter().copied() {
        validate_allocation_range(range, page_size)?;
    }

    Ok(())
}

fn validate_allocation_range(
    range: GuestMemoryRange,
    page_size: u64,
) -> Result<usize, GuestMemoryAllocationError> {
    range.validate_alignment(page_size)?;
    allocation_host_size(range)
}

fn allocation_host_size(range: GuestMemoryRange) -> Result<usize, GuestMemoryAllocationError> {
    usize::try_from(range.size()).map_err(|_| GuestMemoryAllocationError::SizeTooLarge { range })
}

fn allocate_region_with_mapper(
    range: GuestMemoryRange,
    page_size: u64,
    mapper: &mut impl GuestMemoryMapper,
) -> Result<GuestMemoryRegion, GuestMemoryAllocationError> {
    let host_size = validate_allocation_range(range, page_size)?;

    Ok(GuestMemoryRegion {
        range,
        mapping: Arc::new(mapper.map(host_size)?),
        mapping_offset: 0,
        host_size,
    })
}

fn overlapping_ranges_error(first: GuestMemoryRange, second: GuestMemoryRange) -> GuestMemoryError {
    if first.start() <= second.start() {
        GuestMemoryError::OverlappingRange {
            previous: first,
            next: second,
        }
    } else {
        GuestMemoryError::OverlappingRange {
            previous: second,
            next: first,
        }
    }
}

fn guest_memory_range_contains(outer: GuestMemoryRange, inner: GuestMemoryRange) -> bool {
    outer.start() <= inner.start() && inner.end_exclusive() <= outer.end_exclusive()
}

fn system_host_page_size() -> Option<u64> {
    // SAFETY: `sysconf(_SC_PAGESIZE)` has no pointer arguments and does not
    // require process-local invariants from Rust.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u64::try_from(page_size).ok()?;

    (page_size != 0 && page_size.is_power_of_two()).then_some(page_size)
}

fn host_page_size() -> Result<u64, GuestMemoryAllocationError> {
    let page_size =
        system_host_page_size().ok_or(GuestMemoryAllocationError::InvalidHostPageSize)?;

    validate_host_page_size(page_size)?;
    Ok(page_size)
}

fn validate_host_page_size(page_size: u64) -> Result<(), GuestMemoryAllocationError> {
    if page_size == 0 || !page_size.is_power_of_two() {
        Err(GuestMemoryAllocationError::InvalidHostPageSize)
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct SharedMemoryResourceLimits {
    file_size: libc::rlim_t,
    file_descriptors: libc::rlim_t,
}

fn preflight_shared_memory_resources(
    region_count: usize,
    ranges: &[GuestMemoryRange],
) -> Result<(), GuestMemoryAllocationError> {
    let limits = query_shared_memory_resource_limits()?;
    preflight_shared_memory_resources_with_limits(region_count, ranges, limits)
}

fn preflight_shared_memory_resources_with_limits(
    region_count: usize,
    ranges: &[GuestMemoryRange],
    limits: SharedMemoryResourceLimits,
) -> Result<(), GuestMemoryAllocationError> {
    let largest_region = ranges.iter().map(|range| range.size()).max().unwrap_or(0);
    preflight_shared_memory_resource_values_with_limits(region_count, largest_region, limits)
}

fn preflight_shared_memory_resource_values(
    region_count: usize,
    largest_region: u64,
) -> Result<(), GuestMemoryAllocationError> {
    let limits = query_shared_memory_resource_limits()?;
    preflight_shared_memory_resource_values_with_limits(region_count, largest_region, limits)
}

fn preflight_shared_memory_resource_values_with_limits(
    region_count: usize,
    largest_region: u64,
    limits: SharedMemoryResourceLimits,
) -> Result<(), GuestMemoryAllocationError> {
    if limits.file_size != libc::RLIM_INFINITY && largest_region > limits.file_size {
        return Err(GuestMemoryAllocationError::SharedFileSizeLimitExceeded {
            size: largest_region,
        });
    }

    let retained_descriptors = u64::try_from(region_count).unwrap_or(u64::MAX);
    if limits.file_descriptors != libc::RLIM_INFINITY
        && retained_descriptors > limits.file_descriptors
    {
        return Err(
            GuestMemoryAllocationError::SharedFileDescriptorLimitExceeded {
                regions: region_count,
            },
        );
    }

    Ok(())
}

fn query_shared_memory_resource_limits()
-> Result<SharedMemoryResourceLimits, GuestMemoryAllocationError> {
    let mut file_size = MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: `file_size` points to writable storage for one `rlimit`, and the
    // resource selector has no additional pointer or lifetime requirements.
    let result = unsafe { libc::getrlimit(libc::RLIMIT_FSIZE, file_size.as_mut_ptr()) };
    if result != 0 {
        return Err(GuestMemoryAllocationError::SharedResourceLimitQueryFailed {
            resource: GuestMemorySharedResource::FileSize,
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: successful `getrlimit` initialized the complete structure.
    let file_size = unsafe { file_size.assume_init() }.rlim_cur;

    let mut file_descriptors = MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: as above, this points to valid writable storage for one result.
    let result = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, file_descriptors.as_mut_ptr()) };
    if result != 0 {
        return Err(GuestMemoryAllocationError::SharedResourceLimitQueryFailed {
            resource: GuestMemorySharedResource::FileDescriptors,
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: successful `getrlimit` initialized the complete structure.
    let file_descriptors = unsafe { file_descriptors.assume_init() }.rlim_cur;

    Ok(SharedMemoryResourceLimits {
        file_size,
        file_descriptors,
    })
}

const SHARED_MEMORY_NAME_ATTEMPTS: usize = 16;

fn create_shared_memory_file(size: usize) -> Result<File, GuestMemoryAllocationError> {
    let size = u64::try_from(size)
        .map_err(|_| GuestMemoryAllocationError::SharedBackingSizeTooLarge { size })?;
    let mut directories = Vec::with_capacity(2);
    if let Ok(current_directory) = std::env::current_dir() {
        directories.push(current_directory);
    }
    let temporary_directory = std::env::temp_dir();
    if !directories.contains(&temporary_directory) {
        directories.push(temporary_directory);
    }

    let mut last_create_error = None;
    for directory in directories {
        match create_unlinked_shared_memory_file(&directory) {
            Ok(file) => {
                file.set_len(size).map_err(|source| {
                    GuestMemoryAllocationError::SharedBackingResizeFailed { size, source }
                })?;
                return Ok(file);
            }
            Err(SharedMemoryFileCreationError::Random(source)) => {
                return Err(GuestMemoryAllocationError::SharedNameGenerationFailed { source });
            }
            Err(SharedMemoryFileCreationError::Unlink(source)) => {
                return Err(GuestMemoryAllocationError::SharedBackingUnlinkFailed { source });
            }
            Err(SharedMemoryFileCreationError::Create(source))
                if source.raw_os_error() == Some(libc::EMFILE) =>
            {
                return Err(GuestMemoryAllocationError::SharedBackingCreateFailed { source });
            }
            Err(SharedMemoryFileCreationError::Create(source)) => {
                last_create_error = Some(source);
            }
        }
    }

    Err(GuestMemoryAllocationError::SharedBackingCreateFailed {
        source: last_create_error.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no shared-memory creation directory is available",
            )
        }),
    })
}

fn prepare_shared_memory_files(
    ranges: &[GuestMemoryRange],
) -> Result<VecDeque<File>, GuestMemoryAllocationError> {
    let mut files = VecDeque::new();
    files
        .try_reserve_exact(ranges.len())
        .map_err(|source| GuestMemoryAllocationError::RegionMetadataAllocationFailed { source })?;
    for range in ranges {
        let size = allocation_host_size(*range)?;
        match create_shared_memory_file(size) {
            Ok(file) => files.push_back(file),
            Err(GuestMemoryAllocationError::SharedBackingCreateFailed { source })
                if source.raw_os_error() == Some(libc::EMFILE) =>
            {
                return Err(
                    GuestMemoryAllocationError::SharedFileDescriptorLimitExceeded {
                        regions: ranges.len(),
                    },
                );
            }
            Err(source) => return Err(source),
        }
    }
    Ok(files)
}

#[derive(Debug)]
enum SharedMemoryFileCreationError {
    Random(getrandom::Error),
    Create(io::Error),
    Unlink(io::Error),
}

fn create_unlinked_shared_memory_file(
    directory: &Path,
) -> Result<File, SharedMemoryFileCreationError> {
    let mut last_collision = None;
    for _ in 0..SHARED_MEMORY_NAME_ATTEMPTS {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(SharedMemoryFileCreationError::Random)?;
        let name = format!(".bangbang-memory-{:032x}", u128::from_le_bytes(random));
        let path = directory.join(name);
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => file,
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                last_collision = Some(source);
                continue;
            }
            Err(source) => return Err(SharedMemoryFileCreationError::Create(source)),
        };

        return LinkedSharedMemoryFile::new(path, file).unlink();
    }

    Err(SharedMemoryFileCreationError::Create(
        last_collision.unwrap_or_else(|| io::Error::other("shared-memory name attempts exhausted")),
    ))
}

#[derive(Debug)]
struct LinkedSharedMemoryFile {
    path: Option<PathBuf>,
    file: Option<File>,
}

impl LinkedSharedMemoryFile {
    fn new(path: PathBuf, file: File) -> Self {
        Self {
            path: Some(path),
            file: Some(file),
        }
    }

    fn unlink(mut self) -> Result<File, SharedMemoryFileCreationError> {
        let Some(path) = self.path.as_ref() else {
            return Err(SharedMemoryFileCreationError::Unlink(io::Error::other(
                "shared-memory path ownership is missing",
            )));
        };
        fs::remove_file(path).map_err(SharedMemoryFileCreationError::Unlink)?;
        self.path = None;
        self.file.take().ok_or_else(|| {
            SharedMemoryFileCreationError::Create(io::Error::other(
                "shared-memory descriptor ownership is missing",
            ))
        })
    }
}

impl Drop for LinkedSharedMemoryFile {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
        self.file.take();
    }
}

impl GuestMemoryDiscardAdviser for SystemGuestMemoryDiscardAdviser {
    fn host_page_size(&mut self) -> Result<u64, GuestMemoryDiscardFailureKind> {
        #[cfg(target_os = "macos")]
        {
            system_host_page_size().ok_or(GuestMemoryDiscardFailureKind::InvalidHostPageSize)
        }

        #[cfg(not(target_os = "macos"))]
        {
            Err(GuestMemoryDiscardFailureKind::UnsupportedTarget)
        }
    }

    fn zero(&mut self, address: NonNull<c_void>, size: usize) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            madvise_zero(address, size)
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = (address, size);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "guest memory zero advice is unsupported on this target",
            ))
        }
    }

    fn free(&mut self, address: NonNull<c_void>, size: usize) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            madvise_free(address, size)
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = (address, size);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "guest memory free advice is unsupported on this target",
            ))
        }
    }

    fn reclaim(
        &mut self,
        mapping: &GuestMemoryMapping,
        offset: usize,
        address: NonNull<c_void>,
        size: usize,
    ) -> Result<(), GuestMemoryDiscardFailureKind> {
        if mapping.is_private_file() {
            #[cfg(target_os = "macos")]
            {
                return replace_private_file_with_anonymous(address, size)
                    .map_err(|_| GuestMemoryDiscardFailureKind::PrivateFileReclaim);
            }

            #[cfg(not(target_os = "macos"))]
            {
                let _ = (offset, address, size);
                return Err(GuestMemoryDiscardFailureKind::UnsupportedTarget);
            }
        }

        if let Some(file) = mapping.shared_file() {
            #[cfg(target_os = "macos")]
            {
                return punch_shared_memory_hole(file, offset, size)
                    .map_err(|_| GuestMemoryDiscardFailureKind::SharedReclaim);
            }

            #[cfg(not(target_os = "macos"))]
            {
                let _ = (file, offset, address, size);
                return Err(GuestMemoryDiscardFailureKind::UnsupportedTarget);
            }
        }

        self.zero(address, size)
            .map_err(|_| GuestMemoryDiscardFailureKind::ZeroAdvice)?;
        self.free(address, size)
            .map_err(|_| GuestMemoryDiscardFailureKind::FreeAdvice)
    }
}

#[cfg(target_os = "macos")]
fn replace_private_file_with_anonymous(address: NonNull<c_void>, size: usize) -> io::Result<()> {
    // SAFETY: discard validation supplies an exact host-page-aligned subrange
    // of a live private-file mapping. `MAP_FIXED` atomically replaces only
    // that range with private anonymous zero pages; the mapping owner retains
    // responsibility for the same address range and final `munmap`.
    let replacement = unsafe {
        libc::mmap(
            address.as_ptr(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_FIXED | libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
            -1,
            0,
        )
    };
    if replacement == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    if replacement != address.as_ptr() {
        // SAFETY: `mmap` returned a successful unexpected mapping of this
        // exact size. Release it before reporting the invariant violation.
        unsafe {
            let _ = libc::munmap(replacement, size);
        }
        return Err(io::Error::other(
            "private-file discard replacement returned an unexpected address",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn punch_shared_memory_hole(file: &File, offset: usize, size: usize) -> io::Result<()> {
    let offset = libc::off_t::try_from(offset)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "offset is too large"))?;
    let size = libc::off_t::try_from(size)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "size is too large"))?;
    let mut request = libc::fpunchhole_t {
        fp_flags: 0,
        reserved: 0,
        fp_offset: offset,
        fp_length: size,
    };

    // SAFETY: the descriptor and request pointer remain live for the complete
    // variadic call, and the requested range was validated within the file.
    let result = unsafe {
        libc::fcntl(
            file.as_fd().as_raw_fd(),
            libc::F_PUNCHHOLE,
            ptr::from_mut(&mut request),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn madvise_zero(address: NonNull<c_void>, size: usize) -> io::Result<()> {
    // SAFETY: `GuestMemory::discard_range_with_adviser` derives `address` and
    // `size` from the host-page-aligned interior of a validated live mapping.
    let result = unsafe { libc::madvise(address.as_ptr(), size, libc::MADV_ZERO) };
    checked_madvise_result(result)
}

#[cfg(target_os = "macos")]
fn madvise_free(address: NonNull<c_void>, size: usize) -> io::Result<()> {
    // SAFETY: `GuestMemory::discard_range_with_adviser` calls this only after
    // zero advice succeeds for the same live, host-page-aligned mapping range.
    let result = unsafe { libc::madvise(address.as_ptr(), size, libc::MADV_FREE) };
    checked_madvise_result(result)
}

#[cfg(target_os = "macos")]
fn checked_madvise_result(result: libc::c_int) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

trait GuestMemoryMapper {
    fn map(&mut self, size: usize) -> Result<GuestMemoryMapping, GuestMemoryAllocationError>;

    fn backing(&self) -> GuestMemoryBacking {
        GuestMemoryBacking::Anonymous
    }
}

#[derive(Debug)]
struct SystemGuestMemoryMapper {
    backing: GuestMemoryBacking,
    shared_files: VecDeque<File>,
}

impl SystemGuestMemoryMapper {
    fn anonymous() -> Self {
        Self {
            backing: GuestMemoryBacking::Anonymous,
            shared_files: VecDeque::new(),
        }
    }

    fn shared(shared_files: VecDeque<File>) -> Self {
        Self {
            backing: GuestMemoryBacking::Shared,
            shared_files,
        }
    }
}

impl GuestMemoryMapper for SystemGuestMemoryMapper {
    fn map(&mut self, size: usize) -> Result<GuestMemoryMapping, GuestMemoryAllocationError> {
        match self.backing {
            GuestMemoryBacking::Anonymous => GuestMemoryMapping::map_anonymous(size),
            GuestMemoryBacking::Shared => {
                let file = self
                    .shared_files
                    .pop_front()
                    .ok_or(GuestMemoryAllocationError::SharedBackingReservationMissing { size })?;
                GuestMemoryMapping::map_shared(size, file)
            }
        }
    }

    fn backing(&self) -> GuestMemoryBacking {
        self.backing
    }
}

pub(crate) struct GuestMemoryMapping {
    address: NonNull<c_void>,
    size: usize,
    identity: GuestMemoryMappingIdentity,
    kind: GuestMemoryMappingKind,
}

// SAFETY: `GuestMemoryMapping` owns a process-local mmap region. Moving ownership
// to another thread does not invalidate the mapping, and `munmap` may run from
// any thread when the owner is dropped.
unsafe impl Send for GuestMemoryMapping {}

// SAFETY: Shared references expose only copyable metadata and a raw pointer.
// Safe Rust cannot mutate the mapped bytes through this type, and unsafe users
// must uphold the usual raw-pointer aliasing and lifetime requirements.
unsafe impl Sync for GuestMemoryMapping {}

impl GuestMemoryMapping {
    fn map_anonymous(size: usize) -> Result<Self, GuestMemoryAllocationError> {
        let identity = next_guest_memory_mapping_identity()?;
        // SAFETY: The call requests a new private anonymous read/write mapping.
        // `size` was validated from a non-empty guest memory range before this
        // function is called. No aliasing Rust reference is created here.
        let address = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };

        if address == libc::MAP_FAILED {
            return Err(GuestMemoryAllocationError::AnonymousMmapFailed {
                size,
                source: io::Error::last_os_error(),
            });
        }

        let Some(address) = NonNull::new(address) else {
            // SAFETY: `mmap` reported success, so the returned address and size
            // describe a live mapping even if the address is null.
            unsafe {
                let _ = libc::munmap(address, size);
            }

            return Err(GuestMemoryAllocationError::AnonymousMmapReturnedNull { size });
        };

        Ok(Self {
            address,
            size,
            identity,
            kind: GuestMemoryMappingKind::Anonymous,
        })
    }

    fn map_shared(size: usize, file: File) -> Result<Self, GuestMemoryAllocationError> {
        let identity = next_guest_memory_mapping_identity()?;
        // SAFETY: The descriptor owns an exact-sized object and remains live in
        // the returned mapping owner. No Rust reference is created here.
        let address = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_NORESERVE,
                file.as_fd().as_raw_fd(),
                0,
            )
        };

        if address == libc::MAP_FAILED {
            return Err(GuestMemoryAllocationError::SharedMmapFailed {
                size,
                source: io::Error::last_os_error(),
            });
        }

        let Some(address) = NonNull::new(address) else {
            // SAFETY: `mmap` reported success, so this address and size still
            // identify a live mapping even if the address is null.
            unsafe {
                let _ = libc::munmap(address, size);
            }
            return Err(GuestMemoryAllocationError::SharedMmapReturnedNull { size });
        };

        Ok(Self {
            address,
            size,
            identity,
            kind: GuestMemoryMappingKind::Shared { file },
        })
    }

    fn map_private_file(
        size: usize,
        file: Arc<File>,
        file_offset: u64,
    ) -> Result<Self, GuestMemoryAllocationError> {
        let page_size = host_page_size()?;
        let size_u64 = u64::try_from(size)
            .map_err(|_| GuestMemoryAllocationError::PrivateFileMappingSizeInvalid)?;
        if size == 0 || !size_u64.is_multiple_of(page_size) {
            return Err(GuestMemoryAllocationError::PrivateFileMappingSizeInvalid);
        }
        if !file_offset.is_multiple_of(page_size) {
            return Err(GuestMemoryAllocationError::PrivateFileOffsetUnaligned);
        }
        let mmap_offset = libc::off_t::try_from(file_offset)
            .map_err(|_| GuestMemoryAllocationError::PrivateFileOffsetTooLarge)?;
        let file_length = file
            .metadata()
            .map_err(|source| GuestMemoryAllocationError::PrivateFileInspectFailed { source })?
            .len();
        if file_offset
            .checked_add(size_u64)
            .is_none_or(|end| end > file_length)
        {
            return Err(GuestMemoryAllocationError::PrivateFileRangeBeyondEnd);
        }
        let identity = next_guest_memory_mapping_identity()?;
        // SAFETY: the caller validated the descriptor length and host-page
        // alignment, and this boundary repeated both checks immediately above.
        // The retained `Arc<File>` outlives this private mapping, and no Rust
        // reference is created by `mmap`.
        let address = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_NORESERVE,
                file.as_fd().as_raw_fd(),
                mmap_offset,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(GuestMemoryAllocationError::PrivateFileMmapFailed {
                size,
                source: io::Error::last_os_error(),
            });
        }
        let Some(address) = NonNull::new(address) else {
            // SAFETY: `mmap` reported success, so the returned address and
            // size describe the mapping even if the address is null.
            unsafe {
                let _ = libc::munmap(address, size);
            }
            return Err(GuestMemoryAllocationError::PrivateFileMmapReturnedNull { size });
        };

        Ok(Self {
            address,
            size,
            identity,
            kind: GuestMemoryMappingKind::PrivateFile { file, file_offset },
        })
    }

    #[cfg(test)]
    fn test_mapping(size: usize, drop_count: Arc<AtomicUsize>) -> Self {
        Self {
            address: NonNull::<u8>::dangling().cast(),
            size,
            identity: next_guest_memory_mapping_identity()
                .expect("test mapping identity should remain available"),
            kind: GuestMemoryMappingKind::Test { drop_count },
        }
    }

    const fn address(&self) -> NonNull<c_void> {
        self.address
    }

    const fn size(&self) -> usize {
        self.size
    }

    const fn identity(&self) -> GuestMemoryMappingIdentity {
        self.identity
    }

    const fn backing(&self) -> GuestMemoryRegionBacking {
        match &self.kind {
            GuestMemoryMappingKind::Anonymous => GuestMemoryRegionBacking::Anonymous,
            GuestMemoryMappingKind::Shared { .. } => GuestMemoryRegionBacking::Shared,
            GuestMemoryMappingKind::PrivateFile { .. } => GuestMemoryRegionBacking::PrivateFile,
            #[cfg(test)]
            GuestMemoryMappingKind::Test { .. } => GuestMemoryRegionBacking::Anonymous,
        }
    }

    const fn is_private_file(&self) -> bool {
        matches!(&self.kind, GuestMemoryMappingKind::PrivateFile { .. })
    }

    fn shared_file(&self) -> Option<&File> {
        match &self.kind {
            GuestMemoryMappingKind::Shared { file } => Some(file),
            GuestMemoryMappingKind::Anonymous | GuestMemoryMappingKind::PrivateFile { .. } => None,
            #[cfg(test)]
            GuestMemoryMappingKind::Test { .. } => None,
        }
    }

    fn try_clone_shared_backing(
        &self,
        offset: usize,
        len: usize,
    ) -> Result<Option<GuestMemorySharedBacking>, GuestMemorySharedBackingError> {
        let Some((file, _expected)) = self.validated_shared_file()? else {
            return Ok(None);
        };
        let Some(end) = offset.checked_add(len) else {
            return Err(GuestMemorySharedBackingError::InvalidRange {
                offset,
                len,
                mapping_size: self.size,
            });
        };
        if end > self.size {
            return Err(GuestMemorySharedBackingError::InvalidRange {
                offset,
                len,
                mapping_size: self.size,
            });
        }
        let offset = u64::try_from(offset)
            .map_err(|_| GuestMemorySharedBackingError::LengthTooLarge { size: offset })?;
        let len = u64::try_from(len)
            .map_err(|_| GuestMemorySharedBackingError::LengthTooLarge { size: len })?;
        let file = file
            .try_clone()
            .map_err(|source| GuestMemorySharedBackingError::DuplicateDescriptor { source })?;

        Ok(Some(GuestMemorySharedBacking { file, offset, len }))
    }

    fn validate_shared_backing(
        &self,
        offset: usize,
        len: usize,
    ) -> Result<bool, GuestMemorySharedBackingError> {
        let Some((_file, _expected)) = self.validated_shared_file()? else {
            return Ok(false);
        };
        if offset.checked_add(len).is_none_or(|end| end > self.size) {
            return Err(GuestMemorySharedBackingError::InvalidRange {
                offset,
                len,
                mapping_size: self.size,
            });
        }
        Ok(true)
    }

    fn validated_shared_file(&self) -> Result<Option<(&File, u64)>, GuestMemorySharedBackingError> {
        let GuestMemoryMappingKind::Shared { file } = &self.kind else {
            return Ok(None);
        };
        let expected = u64::try_from(self.size)
            .map_err(|_| GuestMemorySharedBackingError::LengthTooLarge { size: self.size })?;
        let actual = file
            .metadata()
            .map_err(|source| GuestMemorySharedBackingError::InspectDescriptor { source })?
            .len();
        if actual != expected {
            return Err(GuestMemorySharedBackingError::UnexpectedLength { expected, actual });
        }
        Ok(Some((file, expected)))
    }
}

impl fmt::Debug for GuestMemoryMapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuestMemoryMapping")
            .field("size", &self.size)
            .field("backing", &self.backing())
            .finish_non_exhaustive()
    }
}

enum GuestMemoryMappingKind {
    Anonymous,
    Shared {
        file: File,
    },
    PrivateFile {
        file: Arc<File>,
        file_offset: u64,
    },
    #[cfg(test)]
    Test {
        drop_count: Arc<AtomicUsize>,
    },
}

impl fmt::Debug for GuestMemoryMappingKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Anonymous => formatter.write_str("Anonymous"),
            Self::Shared { .. } => formatter.write_str("Shared(<redacted>)"),
            Self::PrivateFile { file, file_offset } => {
                let _ = (Arc::strong_count(file), file_offset);
                formatter.write_str("PrivateFile(<redacted>)")
            }
            #[cfg(test)]
            Self::Test { .. } => formatter.write_str("Test(<redacted>)"),
        }
    }
}

impl Drop for GuestMemoryMapping {
    fn drop(&mut self) {
        match &self.kind {
            GuestMemoryMappingKind::Anonymous
            | GuestMemoryMappingKind::Shared { .. }
            | GuestMemoryMappingKind::PrivateFile { .. } => {
                // SAFETY: the constructors store only successful mmap results,
                // and each `GuestMemoryMapping` owns exactly one mapping.
                unsafe {
                    let _ = libc::munmap(self.address.as_ptr(), self.size);
                }
            }
            #[cfg(test)]
            GuestMemoryMappingKind::Test { drop_count } => {
                drop_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::ffi::c_void;
    use std::io;
    use std::os::fd::{AsFd, AsRawFd};
    use std::os::unix::fs::{FileExt, MetadataExt};
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::{
        GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryAllocationError,
        GuestMemoryBacking, GuestMemoryDiscardAdviser, GuestMemoryDiscardFailureKind,
        GuestMemoryError, GuestMemoryLayout, GuestMemoryMapper, GuestMemoryMapping,
        GuestMemoryMappingKind, GuestMemoryRange, GuestMemoryRegion, GuestMemoryRegionBacking,
        GuestMemorySharedBackingError, GuestMemorySharedReservationCaptureError,
        SharedMemoryResourceLimits, aarch64, host_page_size,
        preflight_shared_memory_resource_values_with_limits,
        preflight_shared_memory_resources_with_limits,
    };

    const PAGE_SIZE: u64 = 4096;

    fn range(start: u64, size: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(GuestAddress::new(start), size)
            .expect("range should be valid for test")
    }

    fn allocate_memory(ranges: Vec<GuestMemoryRange>) -> GuestMemory {
        let layout =
            GuestMemoryLayout::new(ranges).expect("guest memory layout should be valid for test");

        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed")
    }

    fn memory_ranges(memory: &GuestMemory) -> Vec<GuestMemoryRange> {
        memory
            .regions()
            .iter()
            .map(|region| region.range())
            .collect()
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestAdviceKind {
        Zero,
        Free,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct TestAdviceCall {
        kind: TestAdviceKind,
        address: usize,
        size: usize,
    }

    #[derive(Debug)]
    struct TestDiscardAdviser {
        page_size: Result<u64, GuestMemoryDiscardFailureKind>,
        calls: Vec<TestAdviceCall>,
        zero_calls: usize,
        free_calls: usize,
        failing_zero_calls: HashSet<usize>,
        failing_free_calls: HashSet<usize>,
    }

    impl TestDiscardAdviser {
        fn new(page_size: u64) -> Self {
            Self {
                page_size: Ok(page_size),
                calls: Vec::new(),
                zero_calls: 0,
                free_calls: 0,
                failing_zero_calls: HashSet::new(),
                failing_free_calls: HashSet::new(),
            }
        }

        fn unavailable(kind: GuestMemoryDiscardFailureKind) -> Self {
            Self {
                page_size: Err(kind),
                calls: Vec::new(),
                zero_calls: 0,
                free_calls: 0,
                failing_zero_calls: HashSet::new(),
                failing_free_calls: HashSet::new(),
            }
        }

        fn fail_zero_call(mut self, call: usize) -> Self {
            self.failing_zero_calls.insert(call);
            self
        }

        fn fail_free_call(mut self, call: usize) -> Self {
            self.failing_free_calls.insert(call);
            self
        }
    }

    impl GuestMemoryDiscardAdviser for TestDiscardAdviser {
        fn host_page_size(&mut self) -> Result<u64, GuestMemoryDiscardFailureKind> {
            self.page_size
        }

        fn zero(&mut self, address: NonNull<c_void>, size: usize) -> io::Result<()> {
            let call = self.zero_calls;
            self.zero_calls += 1;
            self.calls.push(TestAdviceCall {
                kind: TestAdviceKind::Zero,
                address: address.as_ptr().addr(),
                size,
            });
            if self.failing_zero_calls.contains(&call) {
                Err(io::Error::other("injected zero advice failure"))
            } else {
                Ok(())
            }
        }

        fn free(&mut self, address: NonNull<c_void>, size: usize) -> io::Result<()> {
            let call = self.free_calls;
            self.free_calls += 1;
            self.calls.push(TestAdviceCall {
                kind: TestAdviceKind::Free,
                address: address.as_ptr().addr(),
                size,
            });
            if self.failing_free_calls.contains(&call) {
                Err(io::Error::other("injected free advice failure"))
            } else {
                Ok(())
            }
        }
    }

    #[derive(Debug)]
    struct TestSharedReclaimAdviser {
        page_size: u64,
        calls: Vec<(usize, usize)>,
    }

    impl GuestMemoryDiscardAdviser for TestSharedReclaimAdviser {
        fn host_page_size(&mut self) -> Result<u64, GuestMemoryDiscardFailureKind> {
            Ok(self.page_size)
        }

        fn zero(&mut self, _address: NonNull<c_void>, _size: usize) -> io::Result<()> {
            Err(io::Error::other(
                "shared reclaim test must use descriptor reclaim",
            ))
        }

        fn free(&mut self, _address: NonNull<c_void>, _size: usize) -> io::Result<()> {
            Err(io::Error::other(
                "shared reclaim test must use descriptor reclaim",
            ))
        }

        fn reclaim(
            &mut self,
            _mapping: &GuestMemoryMapping,
            offset: usize,
            _address: NonNull<c_void>,
            size: usize,
        ) -> Result<(), GuestMemoryDiscardFailureKind> {
            self.calls.push((offset, size));
            Ok(())
        }
    }

    #[test]
    fn guest_memory_discard_validates_whole_range_before_advice() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let memory = allocate_memory(vec![range(0, page_size), range(page_size * 2, page_size)]);
        let requested = range(page_size / 2, page_size * 2);
        let mut adviser = TestDiscardAdviser::new(page_size);

        let outcome = memory.discard_range_with_adviser(requested, &mut adviser);

        assert_eq!(outcome.requested_bytes(), page_size * 2);
        assert_eq!(outcome.advised_bytes(), 0);
        assert_eq!(outcome.skipped_bytes(), 0);
        assert_eq!(outcome.failed_bytes(), page_size * 2);
        assert_eq!(outcome.failures().total(), 1);
        assert_eq!(
            outcome
                .failures()
                .count(GuestMemoryDiscardFailureKind::RangeValidation),
            1
        );
        assert!(adviser.calls.is_empty());
    }

    #[test]
    fn guest_memory_discard_segments_adjacent_owned_regions() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let memory = allocate_memory(vec![
            range(0, page_size * 2),
            range(page_size * 2, page_size * 2),
        ]);
        let first_address = memory
            .regions()
            .first()
            .expect("first region should exist")
            .host_address()
            .as_ptr()
            .addr();
        let second_address = memory
            .regions()
            .get(1)
            .expect("second region should exist")
            .host_address()
            .as_ptr()
            .addr();
        let page_size_usize = usize::try_from(page_size).expect("page size should fit usize");
        let mut adviser = TestDiscardAdviser::new(page_size);

        let outcome = memory.discard_range_with_adviser(range(0, page_size * 4), &mut adviser);

        assert!(outcome.is_complete());
        assert_eq!(outcome.requested_bytes(), page_size * 4);
        assert_eq!(outcome.advised_bytes(), page_size * 4);
        assert_eq!(outcome.skipped_bytes(), 0);
        assert_eq!(outcome.failed_bytes(), 0);
        assert_eq!(
            adviser.calls,
            [
                TestAdviceCall {
                    kind: TestAdviceKind::Zero,
                    address: first_address,
                    size: page_size_usize * 2,
                },
                TestAdviceCall {
                    kind: TestAdviceKind::Free,
                    address: first_address,
                    size: page_size_usize * 2,
                },
                TestAdviceCall {
                    kind: TestAdviceKind::Zero,
                    address: second_address,
                    size: page_size_usize * 2,
                },
                TestAdviceCall {
                    kind: TestAdviceKind::Free,
                    address: second_address,
                    size: page_size_usize * 2,
                },
            ]
        );
    }

    #[test]
    fn guest_memory_discard_aligns_each_segment_inward() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let memory = allocate_memory(vec![range(0, page_size * 4)]);
        let host_address = memory
            .regions()
            .first()
            .expect("region should exist")
            .host_address()
            .as_ptr()
            .addr();
        let page_size_usize = usize::try_from(page_size).expect("page size should fit usize");
        let mut adviser = TestDiscardAdviser::new(page_size);

        let outcome =
            memory.discard_range_with_adviser(range(page_size / 2, page_size * 3), &mut adviser);

        assert!(outcome.is_complete());
        assert_eq!(outcome.requested_bytes(), page_size * 3);
        assert_eq!(outcome.advised_bytes(), page_size * 2);
        assert_eq!(outcome.skipped_bytes(), page_size);
        assert_eq!(outcome.failed_bytes(), 0);
        assert_eq!(
            adviser.calls,
            [
                TestAdviceCall {
                    kind: TestAdviceKind::Zero,
                    address: host_address + page_size_usize,
                    size: page_size_usize * 2,
                },
                TestAdviceCall {
                    kind: TestAdviceKind::Free,
                    address: host_address + page_size_usize,
                    size: page_size_usize * 2,
                },
            ]
        );
    }

    #[test]
    fn guest_memory_discard_skips_four_kibibytes_inside_sixteen_kibibytes() {
        const FOUR_KIB: u64 = 4096;
        const SIXTEEN_KIB: u64 = 16 * 1024;

        let host_page_size =
            host_page_size().expect("host page size should be available for tests");
        let memory_size = host_page_size.max(SIXTEEN_KIB);
        let mut memory = allocate_memory(vec![range(0, memory_size)]);
        let original = vec![0xa5; usize::try_from(memory_size).expect("size should fit usize")];
        memory
            .write_slice(&original, GuestAddress::new(0))
            .expect("test pattern should write");
        let mut adviser = TestDiscardAdviser::new(SIXTEEN_KIB);

        let outcome = memory.discard_range_with_adviser(range(FOUR_KIB, FOUR_KIB), &mut adviser);

        let mut observed = vec![0; original.len()];
        memory
            .read_slice(&mut observed, GuestAddress::new(0))
            .expect("test pattern should read");
        assert!(outcome.is_complete());
        assert_eq!(outcome.requested_bytes(), FOUR_KIB);
        assert_eq!(outcome.advised_bytes(), 0);
        assert_eq!(outcome.skipped_bytes(), FOUR_KIB);
        assert_eq!(outcome.failed_bytes(), 0);
        assert!(adviser.calls.is_empty());
        assert_eq!(observed, original);
    }

    #[test]
    fn guest_memory_discard_reports_partial_failures_and_continues() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let mut memory = allocate_memory(vec![
            range(0, page_size),
            range(page_size, page_size),
            range(page_size * 2, page_size),
        ]);
        let tracker = memory
            .enable_dirty_tracking()
            .expect("discard test should enable dirty tracking");
        let addresses = memory
            .regions()
            .iter()
            .map(|region| region.host_address().as_ptr().addr())
            .collect::<Vec<_>>();
        let first_address = *addresses.first().expect("first address should exist");
        let second_address = *addresses.get(1).expect("second address should exist");
        let third_address = *addresses.get(2).expect("third address should exist");
        let page_size_usize = usize::try_from(page_size).expect("page size should fit usize");
        let mut adviser = TestDiscardAdviser::new(page_size)
            .fail_zero_call(0)
            .fail_free_call(0);

        let outcome = memory.discard_range_with_adviser(range(0, page_size * 3), &mut adviser);

        assert!(!outcome.is_complete());
        assert_eq!(outcome.requested_bytes(), page_size * 3);
        assert_eq!(outcome.advised_bytes(), page_size);
        assert_eq!(outcome.skipped_bytes(), 0);
        assert_eq!(outcome.failed_bytes(), page_size * 2);
        assert_eq!(outcome.failures().total(), 2);
        assert_eq!(
            outcome
                .failures()
                .count(GuestMemoryDiscardFailureKind::ZeroAdvice),
            1
        );
        assert_eq!(
            outcome
                .failures()
                .count(GuestMemoryDiscardFailureKind::FreeAdvice),
            1
        );
        assert_eq!(
            adviser.calls,
            [
                TestAdviceCall {
                    kind: TestAdviceKind::Zero,
                    address: first_address,
                    size: page_size_usize,
                },
                TestAdviceCall {
                    kind: TestAdviceKind::Zero,
                    address: second_address,
                    size: page_size_usize,
                },
                TestAdviceCall {
                    kind: TestAdviceKind::Free,
                    address: second_address,
                    size: page_size_usize,
                },
                TestAdviceCall {
                    kind: TestAdviceKind::Zero,
                    address: third_address,
                    size: page_size_usize,
                },
                TestAdviceCall {
                    kind: TestAdviceKind::Free,
                    address: third_address,
                    size: page_size_usize,
                },
            ]
        );
        assert_eq!(
            tracker
                .dirty_pages()
                .expect("failed and successful discard interiors should remain conservative"),
            [
                GuestAddress::new(0),
                GuestAddress::new(page_size),
                GuestAddress::new(page_size * 2),
            ]
        );
    }

    #[test]
    fn guest_memory_discard_rejects_invalid_page_size_without_advice() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let memory = allocate_memory(vec![range(0, page_size)]);
        let mut adviser = TestDiscardAdviser::new(3);

        let outcome = memory.discard_range_with_adviser(range(0, page_size), &mut adviser);

        assert_eq!(outcome.advised_bytes(), 0);
        assert_eq!(outcome.skipped_bytes(), 0);
        assert_eq!(outcome.failed_bytes(), page_size);
        assert_eq!(
            outcome
                .failures()
                .count(GuestMemoryDiscardFailureKind::InvalidHostPageSize),
            1
        );
        assert!(adviser.calls.is_empty());
    }

    #[test]
    fn guest_memory_discard_reports_unsupported_target_after_validation() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let memory = allocate_memory(vec![range(0, page_size)]);
        let mut adviser =
            TestDiscardAdviser::unavailable(GuestMemoryDiscardFailureKind::UnsupportedTarget);

        let outcome = memory.discard_range_with_adviser(range(0, page_size), &mut adviser);

        assert_eq!(outcome.requested_bytes(), page_size);
        assert_eq!(outcome.advised_bytes(), 0);
        assert_eq!(outcome.failed_bytes(), page_size);
        assert_eq!(
            outcome
                .failures()
                .count(GuestMemoryDiscardFailureKind::UnsupportedTarget),
            1
        );
        assert!(adviser.calls.is_empty());

        let unmapped = range(page_size, page_size);
        let mut unavailable =
            TestDiscardAdviser::unavailable(GuestMemoryDiscardFailureKind::UnsupportedTarget);
        let unmapped_outcome = memory.discard_range_with_adviser(unmapped, &mut unavailable);
        assert_eq!(
            unmapped_outcome
                .failures()
                .count(GuestMemoryDiscardFailureKind::RangeValidation),
            1
        );
        assert_eq!(
            unmapped_outcome
                .failures()
                .count(GuestMemoryDiscardFailureKind::UnsupportedTarget),
            0
        );
    }

    #[test]
    fn guest_memory_discard_redacts_host_address_failures() {
        let page_size = usize::try_from(PAGE_SIZE).expect("test page size should fit usize");
        let host_address_value = usize::MAX - (page_size / 2);
        let host_address = NonNull::new(host_address_value as *mut c_void)
            .expect("synthetic host address should be non-null");
        let drop_count = Arc::new(AtomicUsize::new(0));
        let memory = GuestMemory {
            regions: vec![GuestMemoryRegion {
                range: range(0, PAGE_SIZE),
                mapping: Arc::new(GuestMemoryMapping {
                    address: host_address,
                    size: page_size,
                    identity: super::next_guest_memory_mapping_identity()
                        .expect("test mapping identity should remain available"),
                    kind: GuestMemoryMappingKind::Test {
                        drop_count: Arc::clone(&drop_count),
                    },
                }),
                mapping_offset: 0,
                host_size: page_size,
            }],
            shared_reservations: Vec::new(),
            dirty_tracker: None,
            backing: super::GuestMemoryBacking::Anonymous,
            access_profile: super::GuestMemoryAccessProfile::Eager,
        };
        let mut adviser = TestDiscardAdviser::new(PAGE_SIZE);

        let outcome = memory.discard_range_with_adviser(range(0, PAGE_SIZE), &mut adviser);

        assert_eq!(outcome.failed_bytes(), PAGE_SIZE);
        assert_eq!(
            outcome
                .failures()
                .count(GuestMemoryDiscardFailureKind::HostAddress),
            1
        );
        assert!(adviser.calls.is_empty());
        let host_address_text = format!("{host_address_value:#x}");
        let diagnostic = format!("{outcome:?}; {}", outcome.failures());
        assert!(!diagnostic.contains(&host_address_text));

        drop(memory);
        assert_eq!(drop_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn guest_memory_discard_keeps_independent_owners_isolated() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let first = allocate_memory(vec![range(0, page_size)]);
        let second = allocate_memory(vec![range(0, page_size)]);
        let mut first_adviser = TestDiscardAdviser::new(page_size);
        let mut second_adviser = TestDiscardAdviser::new(page_size);

        let first_outcome =
            first.discard_range_with_adviser(range(0, page_size), &mut first_adviser);
        let second_outcome =
            second.discard_range_with_adviser(range(0, page_size), &mut second_adviser);

        assert_eq!(first_outcome, second_outcome);
        assert!(first_outcome.is_complete());
        let first_call = first_adviser
            .calls
            .first()
            .expect("first owner should issue zero advice");
        let second_call = second_adviser
            .calls
            .first()
            .expect("second owner should issue zero advice");
        assert_ne!(first_call.address, second_call.address);
    }

    #[test]
    fn guest_memory_discard_repeated_range_has_no_persistent_state() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let memory = allocate_memory(vec![range(0, page_size)]);
        let requested = range(0, page_size);
        let mut adviser = TestDiscardAdviser::new(page_size);

        let first = memory.discard_range_with_adviser(requested, &mut adviser);
        let second = memory.discard_range_with_adviser(requested, &mut adviser);

        assert_eq!(first, second);
        assert!(first.is_complete());
        assert_eq!(adviser.calls.len(), 4);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn guest_memory_system_discard_reuses_zero_contents_on_darwin() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let mut memory = allocate_memory(vec![range(0, page_size * 2)]);
        let page_size_usize = usize::try_from(page_size).expect("page size should fit usize");
        memory
            .write_slice(&vec![0x5a; page_size_usize], GuestAddress::new(0))
            .expect("nonzero page should write");

        let outcome = memory.discard_range(range(0, page_size));

        let mut observed = vec![0xff; page_size_usize];
        memory
            .read_slice(&mut observed, GuestAddress::new(0))
            .expect("discarded page should read");
        assert!(outcome.is_complete());
        assert_eq!(outcome.advised_bytes(), page_size);
        assert_eq!(outcome.skipped_bytes(), 0);
        assert_eq!(outcome.failed_bytes(), 0);
        assert!(observed.iter().all(|byte| *byte == 0));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn guest_memory_system_discard_reports_unsupported_target() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let memory = allocate_memory(vec![range(0, page_size)]);

        let outcome = memory.discard_range(range(0, page_size));

        assert_eq!(outcome.advised_bytes(), 0);
        assert_eq!(outcome.failed_bytes(), page_size);
        assert_eq!(
            outcome
                .failures()
                .count(GuestMemoryDiscardFailureKind::UnsupportedTarget),
            1
        );
    }

    #[test]
    fn guest_address_returns_raw_value() {
        assert_eq!(GuestAddress::new(0x8000_0000).raw_value(), 0x8000_0000);
    }

    #[test]
    fn guest_address_checked_add_succeeds() {
        assert_eq!(
            GuestAddress::new(0x1000).checked_add(0x2000),
            Some(GuestAddress::new(0x3000))
        );
    }

    #[test]
    fn guest_address_checked_add_rejects_overflow() {
        assert_eq!(GuestAddress::new(u64::MAX).checked_add(1), None);
    }

    #[test]
    fn guest_address_alignment_checks_value() {
        assert_eq!(GuestAddress::new(0x2000).is_aligned(PAGE_SIZE), Ok(true));
        assert_eq!(GuestAddress::new(0x2001).is_aligned(PAGE_SIZE), Ok(false));
    }

    #[test]
    fn guest_address_alignment_rejects_invalid_alignment() {
        assert_eq!(
            GuestAddress::new(0x2000).is_aligned(0),
            Err(GuestMemoryError::InvalidAlignment { alignment: 0 })
        );
        assert_eq!(
            GuestAddress::new(0x2000).is_aligned(3),
            Err(GuestMemoryError::InvalidAlignment { alignment: 3 })
        );
    }

    #[test]
    fn guest_memory_range_rejects_empty_range() {
        assert_eq!(
            GuestMemoryRange::new(GuestAddress::new(0x1000), 0),
            Err(GuestMemoryError::EmptyRange {
                start: GuestAddress::new(0x1000)
            })
        );
    }

    #[test]
    fn guest_memory_range_returns_end_exclusive_address() {
        let guest_range = range(0x1000, 0x3000);

        assert_eq!(guest_range.start(), GuestAddress::new(0x1000));
        assert_eq!(guest_range.size(), 0x3000);
        assert_eq!(guest_range.end_exclusive(), GuestAddress::new(0x4000));
    }

    #[test]
    fn guest_memory_range_rejects_end_exclusive_overflow() {
        assert_eq!(
            GuestMemoryRange::new(GuestAddress::new(u64::MAX), 1),
            Err(GuestMemoryError::AddressOverflow {
                start: GuestAddress::new(u64::MAX),
                size: 1
            })
        );
    }

    #[test]
    fn guest_memory_range_validates_alignment() {
        assert_eq!(range(0x2000, 0x4000).validate_alignment(PAGE_SIZE), Ok(()));
    }

    #[test]
    fn guest_memory_range_rejects_unaligned_start() {
        let guest_range = range(0x2001, 0x4000);

        assert_eq!(
            guest_range.validate_alignment(PAGE_SIZE),
            Err(GuestMemoryError::UnalignedRange {
                range: guest_range,
                alignment: PAGE_SIZE
            })
        );
    }

    #[test]
    fn guest_memory_range_rejects_unaligned_size() {
        let guest_range = range(0x2000, 0x4001);

        assert_eq!(
            guest_range.validate_alignment(PAGE_SIZE),
            Err(GuestMemoryError::UnalignedRange {
                range: guest_range,
                alignment: PAGE_SIZE
            })
        );
    }

    #[test]
    fn guest_memory_range_rejects_invalid_alignment_without_panicking() {
        assert_eq!(
            range(0x2000, 0x4000).validate_alignment(0),
            Err(GuestMemoryError::InvalidAlignment { alignment: 0 })
        );
    }

    #[test]
    fn guest_memory_range_detects_overlap() {
        assert!(range(0x1000, 0x2000).overlaps(range(0x2000, 0x1000)));
    }

    #[test]
    fn guest_memory_range_detects_adjacency() {
        assert!(range(0x1000, 0x1000).is_adjacent_to(range(0x2000, 0x1000)));
    }

    #[test]
    fn guest_memory_layout_rejects_empty_layout() {
        assert_eq!(
            GuestMemoryLayout::new(Vec::new()),
            Err(GuestMemoryError::EmptyLayout)
        );
    }

    #[test]
    fn guest_memory_layout_accepts_adjacent_ranges() {
        let layout = GuestMemoryLayout::new(vec![range(0x1000, 0x1000), range(0x2000, 0x1000)])
            .expect("adjacent ranges should be valid");

        assert_eq!(layout.ranges().len(), 2);
        assert_eq!(layout.total_size(), 0x2000);
    }

    #[test]
    fn guest_memory_layout_rejects_unsorted_ranges() {
        let previous = range(0x2000, 0x1000);
        let next = range(0x1000, 0x800);

        assert_eq!(
            GuestMemoryLayout::new(vec![previous, next]),
            Err(GuestMemoryError::UnorderedRange { previous, next })
        );
    }

    #[test]
    fn guest_memory_layout_rejects_overlapping_ranges() {
        let previous = range(0x1000, 0x2000);
        let next = range(0x2000, 0x1000);

        assert_eq!(
            GuestMemoryLayout::new(vec![previous, next]),
            Err(GuestMemoryError::OverlappingRange { previous, next })
        );
    }

    #[test]
    fn guest_memory_layout_rejects_duplicate_start_ranges() {
        let previous = range(0x1000, 0x1000);
        let next = range(0x1000, 0x1000);

        assert_eq!(
            GuestMemoryLayout::new(vec![previous, next]),
            Err(GuestMemoryError::OverlappingRange { previous, next })
        );
    }

    #[test]
    fn aarch64_dram_layout_rejects_zero_requested_size() {
        assert_eq!(aarch64::dram_layout(0), Err(GuestMemoryError::EmptyLayout));
    }

    #[test]
    fn aarch64_dram_layout_returns_one_range_for_small_memory() {
        let layout = aarch64::dram_layout(128 << 20).expect("small memory layout should be valid");
        let guest_range = layout
            .ranges()
            .first()
            .copied()
            .expect("layout should contain one range");

        assert_eq!(layout.ranges().len(), 1);
        assert_eq!(guest_range.start().raw_value(), aarch64::DRAM_MEM_START);
        assert_eq!(guest_range.size(), 128 << 20);
    }

    #[test]
    fn aarch64_dram_layout_returns_one_range_ending_before_mmio64_gap() {
        let requested_size = aarch64::MMIO64_MEM_START - aarch64::DRAM_MEM_START - PAGE_SIZE;
        let layout =
            aarch64::dram_layout(requested_size).expect("layout ending before gap should be valid");

        assert_eq!(layout.ranges().len(), 1);
        assert_eq!(layout.total_size(), requested_size);
    }

    #[test]
    fn aarch64_dram_layout_returns_one_range_ending_at_mmio64_gap() {
        let requested_size = aarch64::MMIO64_MEM_START - aarch64::DRAM_MEM_START;
        let layout =
            aarch64::dram_layout(requested_size).expect("layout ending at gap should be valid");
        let guest_range = layout
            .ranges()
            .first()
            .copied()
            .expect("layout should contain one range");

        assert_eq!(layout.ranges().len(), 1);
        assert_eq!(
            guest_range.end_exclusive().raw_value(),
            aarch64::MMIO64_MEM_START
        );
    }

    #[test]
    fn aarch64_dram_layout_splits_range_crossing_mmio64_gap() {
        let size_before_gap = aarch64::MMIO64_MEM_START - aarch64::DRAM_MEM_START;
        let layout = aarch64::dram_layout(size_before_gap + PAGE_SIZE)
            .expect("split layout should be valid");
        let mut ranges = layout.ranges().iter().copied();
        let first = ranges.next().expect("split layout should have first range");
        let second = ranges
            .next()
            .expect("split layout should have second range");

        assert_eq!(ranges.next(), None);
        assert_eq!(first.start().raw_value(), aarch64::DRAM_MEM_START);
        assert_eq!(first.end_exclusive().raw_value(), aarch64::MMIO64_MEM_START);
        assert_eq!(
            second.start().raw_value(),
            aarch64::FIRST_ADDR_PAST_64BITS_MMIO
        );
        assert_eq!(second.size(), PAGE_SIZE);
    }

    #[test]
    fn aarch64_dram_layout_caps_memory_above_architectural_maximum() {
        let layout = aarch64::dram_layout(aarch64::DRAM_MEM_MAX_SIZE + PAGE_SIZE)
            .expect("capped layout should be valid");

        assert_eq!(layout.total_size(), aarch64::DRAM_MEM_MAX_SIZE);
    }

    #[test]
    fn aarch64_dram_layout_keeps_ranges_outside_mmio64_gap() {
        let layout = aarch64::dram_layout(aarch64::DRAM_MEM_MAX_SIZE)
            .expect("maximum layout should be valid");

        assert!(layout.ranges().iter().all(|range| {
            range.end_exclusive().raw_value() <= aarch64::MMIO64_MEM_START
                || range.start().raw_value() >= aarch64::FIRST_ADDR_PAST_64BITS_MMIO
        }));
    }

    #[test]
    fn aarch64_boot_constants_match_firecracker_layout() {
        assert_eq!(aarch64::SYSTEM_MEM_START, aarch64::DRAM_MEM_START);
        assert_eq!(aarch64::SYSTEM_MEM_SIZE, 0x20_0000);
        assert_eq!(aarch64::CMDLINE_MAX_SIZE, 2048);
        assert_eq!(aarch64::FDT_MAX_SIZE, 0x20_0000);
        assert_eq!(aarch64::GUEST_PAGE_SIZE, 4096);
    }

    #[test]
    fn aarch64_kernel_load_address_follows_system_memory() {
        assert_eq!(
            aarch64::kernel_load_address(),
            GuestAddress::new(aarch64::DRAM_MEM_START + aarch64::SYSTEM_MEM_SIZE)
        );
    }

    #[test]
    fn aarch64_fdt_address_uses_dram_start_for_small_or_equal_memory() {
        for size in [aarch64::FDT_MAX_SIZE - PAGE_SIZE, aarch64::FDT_MAX_SIZE] {
            let layout =
                aarch64::dram_layout(size).expect("small fdt layout should be valid for test");

            assert_eq!(
                aarch64::fdt_address(&layout),
                Ok(GuestAddress::new(aarch64::DRAM_MEM_START))
            );
        }
    }

    #[test]
    fn aarch64_fdt_address_reserves_last_fdt_window_for_larger_memory() {
        let layout = aarch64::dram_layout(aarch64::FDT_MAX_SIZE + PAGE_SIZE)
            .expect("large fdt layout should be valid");

        assert_eq!(
            aarch64::fdt_address(&layout),
            Ok(GuestAddress::new(aarch64::DRAM_MEM_START + PAGE_SIZE))
        );
    }

    #[test]
    fn aarch64_initrd_load_address_aligns_before_fdt() {
        let layout = aarch64::dram_layout(aarch64::FDT_MAX_SIZE + (4 * PAGE_SIZE))
            .expect("initrd layout should be valid");

        assert_eq!(
            aarch64::initrd_load_address(&layout, PAGE_SIZE + 1),
            Ok(Some(GuestAddress::new(
                aarch64::DRAM_MEM_START + (2 * PAGE_SIZE)
            )))
        );
    }

    #[test]
    fn aarch64_initrd_load_address_returns_fdt_address_for_empty_payload() {
        let layout =
            aarch64::dram_layout(aarch64::FDT_MAX_SIZE).expect("fdt-only layout should be valid");

        assert_eq!(
            aarch64::initrd_load_address(&layout, 0),
            Ok(Some(GuestAddress::new(aarch64::DRAM_MEM_START)))
        );
    }

    #[test]
    fn aarch64_initrd_load_address_returns_none_without_space() {
        let layout =
            aarch64::dram_layout(aarch64::FDT_MAX_SIZE).expect("fdt-only layout should be valid");

        assert_eq!(aarch64::initrd_load_address(&layout, 1), Ok(None));
    }

    #[test]
    fn aarch64_initrd_load_address_returns_none_when_rounded_size_overflows() {
        let layout = aarch64::dram_layout(aarch64::FDT_MAX_SIZE + PAGE_SIZE)
            .expect("fdt layout should be valid");

        assert_eq!(aarch64::initrd_load_address(&layout, u64::MAX), Ok(None));
    }

    #[test]
    fn guest_memory_allocates_small_layout() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let layout = GuestMemoryLayout::new(vec![range(0, page_size)])
            .expect("page-aligned layout should be valid");

        let memory =
            GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
        let region = memory
            .regions()
            .first()
            .expect("guest memory should contain one region");
        let page_size_usize =
            usize::try_from(page_size).expect("host page size should fit in usize");

        assert_eq!(memory.regions().len(), 1);
        assert_eq!(memory.total_size(), page_size);
        assert_eq!(region.range(), range(0, page_size));
        assert_eq!(region.host_size(), page_size_usize);
        assert_eq!(region.host_address().as_ptr() as usize % page_size_usize, 0);

        let byte = region.host_address().as_ptr().cast::<u8>();
        // SAFETY: `region` owns a live read/write anonymous mapping of at
        // least one byte for the duration of this test.
        unsafe {
            byte.write(0xab);
            assert_eq!(byte.read(), 0xab);
        }
    }

    #[test]
    fn guest_memory_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<GuestMemory>();
        assert_send_sync::<super::GuestMemoryRegion>();
    }

    #[test]
    fn guest_memory_debug_omits_host_address() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let layout = GuestMemoryLayout::new(vec![range(0, page_size)])
            .expect("page-aligned layout should be valid");
        let memory =
            GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
        let region = memory
            .regions()
            .first()
            .expect("guest memory should contain one region");
        let host_address = format!("{:p}", region.host_address().as_ptr());

        let debug = format!("{memory:?}");

        assert!(!debug.contains(&host_address));
        assert!(debug.contains("host_size"));
    }

    #[test]
    fn guest_memory_write_and_read_slice_round_trip() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let mut memory = allocate_memory(vec![range(page_size, page_size)]);
        let address = GuestAddress::new(page_size + 128);
        let source = [0xde, 0xad, 0xbe, 0xef];
        let mut destination = [0; 4];

        memory
            .write_slice(&source, address)
            .expect("guest memory write should succeed");
        memory
            .read_slice(&mut destination, address)
            .expect("guest memory read should succeed");

        assert_eq!(destination, source);
    }

    #[test]
    fn guest_memory_atomic_u64_publishes_little_endian_and_marks_dirty() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let mut memory = allocate_memory(vec![range(0, page_size)]);
        let tracker = memory
            .enable_dirty_tracking()
            .expect("dirty tracking should enable");
        let _ = tracker.clear_quiesced();
        let address = GuestAddress::new(64);
        let word = memory
            .atomic_u64(address)
            .expect("aligned atomic word should be retained");

        word.store_le(0x0102_0304_0506_0708)
            .expect("atomic publication should succeed");

        assert_eq!(word.load_le(), 0x0102_0304_0506_0708);
        let mut bytes = [0; 8];
        memory
            .read_slice(&mut bytes, address)
            .expect("published bytes should read");
        assert_eq!(bytes, 0x0102_0304_0506_0708_u64.to_le_bytes());
        assert_eq!(
            tracker.dirty_pages().expect("dirty pages should query"),
            [GuestAddress::new(0)]
        );
    }

    #[test]
    fn guest_memory_atomic_u64_rejects_unaligned_and_unmapped_words() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let memory = allocate_memory(vec![range(page_size, page_size)]);

        assert!(matches!(
            memory.atomic_u64(GuestAddress::new(page_size + 1)),
            Err(GuestMemoryAccessError::UnalignedAtomicAccess { .. })
        ));
        assert!(matches!(
            memory.atomic_u64(GuestAddress::new(0)),
            Err(GuestMemoryAccessError::UnmappedRange { .. })
        ));
    }

    #[test]
    fn guest_memory_atomic_u64_lease_retains_mapping_without_exposing_host_address() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let memory = allocate_memory(vec![range(0, page_size)]);
        let host_address = format!(
            "{:p}",
            memory
                .regions()
                .first()
                .expect("memory should contain a region")
                .host_address()
                .as_ptr()
        );
        let word = memory
            .atomic_u64(GuestAddress::new(128))
            .expect("aligned atomic word should be retained");
        let thread_word = word.clone();
        drop(memory);

        std::thread::spawn(move || {
            thread_word
                .store_le(u64::MAX)
                .expect("retained mapping should remain writable");
        })
        .join()
        .expect("atomic writer thread should join");

        assert_eq!(word.load_le(), u64::MAX);
        assert!(!format!("{word:?}").contains(&host_address));
    }

    #[test]
    fn guest_memory_dirty_tracking_marks_exact_writes_and_dynamic_regions() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let mut memory = allocate_memory(vec![range(0, page_size), range(page_size, page_size)]);
        let tracker = memory
            .enable_dirty_tracking()
            .expect("dirty tracking should enable");

        memory
            .write_slice(&[0xaa, 0xbb], GuestAddress::new(page_size - 1))
            .expect("cross-region write should succeed");
        assert_eq!(
            tracker.dirty_pages().expect("dirty pages should query"),
            [GuestAddress::new(0), GuestAddress::new(page_size)]
        );
        assert_eq!(tracker.clear_quiesced(), 1);
        assert_eq!(
            memory.write_slice(&[0xcc], GuestAddress::new(page_size * 3)),
            Err(GuestMemoryAccessError::UnmappedRange {
                range: range(page_size * 3, 1),
            })
        );
        assert!(
            tracker
                .dirty_pages()
                .expect("failed write should leave generation clean")
                .is_empty()
        );

        let dynamic = range(page_size * 4, page_size);
        memory
            .insert_region(dynamic)
            .expect("tracked dynamic region should insert");
        assert_eq!(
            tracker
                .dirty_pages()
                .expect("new dynamic region should be wholly dirty"),
            vec![dynamic.start()]
        );
        memory
            .remove_region(dynamic)
            .expect("tracked dynamic region should remove");
        assert!(
            tracker
                .dirty_pages()
                .expect("removed region should leave no dirty metadata")
                .is_empty()
        );
    }

    #[test]
    fn guest_memory_access_accepts_exact_end_boundary() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let mut memory = allocate_memory(vec![range(0, page_size)]);
        let source = [1, 2, 3, 4];
        let address = GuestAddress::new(page_size - u64::try_from(source.len()).unwrap());
        let mut destination = [0; 4];

        memory
            .write_slice(&source, address)
            .expect("guest memory write ending at range boundary should succeed");
        memory
            .read_slice(&mut destination, address)
            .expect("guest memory read ending at range boundary should succeed");

        assert_eq!(destination, source);
    }

    #[test]
    fn guest_memory_access_treats_zero_length_as_noop() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let mut memory = allocate_memory(vec![range(0, page_size)]);
        let mut destination: [u8; 0] = [];

        memory
            .write_slice(&[], GuestAddress::new(u64::MAX))
            .expect("zero-length write should not validate address");
        memory
            .read_slice(&mut destination, GuestAddress::new(u64::MAX))
            .expect("zero-length read should not validate address");
    }

    #[test]
    fn guest_memory_access_rejects_address_overflow() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let mut memory = allocate_memory(vec![range(0, page_size)]);

        assert_eq!(
            memory.write_slice(&[0], GuestAddress::new(u64::MAX)),
            Err(GuestMemoryAccessError::AddressOverflow {
                start: GuestAddress::new(u64::MAX),
                size: 1
            })
        );
    }

    #[test]
    fn displays_guest_memory_access_errors() {
        let access_range = range(0x1000, 0x20);

        assert_eq!(
            GuestMemoryAccessError::SizeTooLarge { size: 7 }.to_string(),
            "guest memory access size 7 bytes is too large to represent"
        );
        assert_eq!(
            GuestMemoryAccessError::AddressOverflow {
                start: GuestAddress::new(0xffff),
                size: 2
            }
            .to_string(),
            "guest memory access overflows address space: start=0xffff, size=2"
        );
        assert_eq!(
            GuestMemoryAccessError::UnmappedRange {
                range: access_range
            }
            .to_string(),
            "guest memory access range [0x1000..0x1020) (32 bytes) is not fully mapped"
        );
        assert_eq!(
            GuestMemoryAccessError::SegmentOffsetTooLarge {
                range: access_range,
                offset: 5
            }
            .to_string(),
            "guest memory access offset 5 in range [0x1000..0x1020) (32 bytes) is too large for this host"
        );
        assert_eq!(
            GuestMemoryAccessError::SegmentSizeTooLarge {
                range: access_range,
                size: 6
            }
            .to_string(),
            "guest memory access segment of 6 bytes in range [0x1000..0x1020) (32 bytes) is too large for this host"
        );
    }

    #[test]
    fn guest_memory_read_rejects_address_overflow_without_mutating_destination() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let memory = allocate_memory(vec![range(0, page_size)]);
        let mut destination = [0x55];

        assert_eq!(
            memory.read_slice(&mut destination, GuestAddress::new(u64::MAX)),
            Err(GuestMemoryAccessError::AddressOverflow {
                start: GuestAddress::new(u64::MAX),
                size: 1
            })
        );
        assert_eq!(destination, [0x55]);
    }

    #[test]
    fn guest_memory_access_rejects_fully_unmapped_ranges_without_partial_read() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let mut memory = allocate_memory(vec![range(page_size, page_size)]);

        for address in [GuestAddress::new(0), GuestAddress::new(2 * page_size)] {
            let access_range = range(address.raw_value(), 1);
            let mut destination = [0x55];

            assert_eq!(
                memory.write_slice(&[0xaa], address),
                Err(GuestMemoryAccessError::UnmappedRange {
                    range: access_range
                })
            );
            assert_eq!(
                memory.read_slice(&mut destination, address),
                Err(GuestMemoryAccessError::UnmappedRange {
                    range: access_range
                })
            );
            assert_eq!(destination, [0x55]);
        }
    }

    #[test]
    fn guest_memory_access_rejects_unmapped_hole_without_partial_write() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let mut memory =
            allocate_memory(vec![range(0, page_size), range(2 * page_size, page_size)]);
        let address = GuestAddress::new(page_size - 1);
        let access_range = range(page_size - 1, 2);

        assert_eq!(
            memory.write_slice(&[0xaa, 0xbb], address),
            Err(GuestMemoryAccessError::UnmappedRange {
                range: access_range
            })
        );

        let mut byte = [0xff];
        memory
            .read_slice(&mut byte, address)
            .expect("single-byte read before hole should still succeed");

        assert_eq!(byte, [0]);
    }

    #[test]
    fn guest_memory_access_rejects_unmapped_hole_without_partial_read() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let mut memory =
            allocate_memory(vec![range(0, page_size), range(2 * page_size, page_size)]);
        let address = GuestAddress::new(page_size - 1);
        let access_range = range(page_size - 1, 2);
        let mut destination = [0x55, 0x66];

        memory
            .write_slice(&[0xaa], address)
            .expect("single-byte write before hole should succeed");

        assert_eq!(
            memory.read_slice(&mut destination, address),
            Err(GuestMemoryAccessError::UnmappedRange {
                range: access_range
            })
        );
        assert_eq!(destination, [0x55, 0x66]);
    }

    #[test]
    fn guest_memory_access_spans_adjacent_ranges() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let mut memory = allocate_memory(vec![range(0, page_size), range(page_size, page_size)]);
        let source = [0x11, 0x22];
        let address = GuestAddress::new(page_size - 1);
        let mut destination = [0; 2];

        memory
            .write_slice(&source, address)
            .expect("guest memory write should cross adjacent ranges");
        memory
            .read_slice(&mut destination, address)
            .expect("guest memory read should cross adjacent ranges");

        assert_eq!(destination, source);
    }

    #[test]
    fn guest_memory_insert_region_preserves_sorted_order() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let mut memory = allocate_memory(vec![range(page_size * 2, page_size)]);

        memory
            .insert_region(range(0, page_size))
            .expect("inserting before existing region should succeed");
        memory
            .insert_region(range(page_size, page_size))
            .expect("inserting between existing regions should succeed");
        memory
            .insert_region(range(page_size * 3, page_size))
            .expect("inserting after existing regions should succeed");

        assert_eq!(
            memory_ranges(&memory),
            vec![
                range(0, page_size),
                range(page_size, page_size),
                range(page_size * 2, page_size),
                range(page_size * 3, page_size)
            ]
        );
        assert_eq!(memory.total_size(), page_size * 4);
    }

    #[test]
    fn guest_memory_insert_region_rejects_overlap_without_mutation() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let original_range = range(page_size, page_size * 2);
        let overlapping_before = range(0, page_size * 2);
        let overlapping_after = range(page_size * 2, page_size);
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut mapper = CountingMapper {
            maps: 0,
            drop_count: Arc::clone(&drop_count),
        };
        let mut memory = GuestMemory::allocate_with_mapper(
            &GuestMemoryLayout::new(vec![original_range]).expect("layout should be valid"),
            page_size,
            &mut mapper,
        )
        .expect("guest memory should allocate");

        let err = memory
            .insert_region_with_mapper(overlapping_before, page_size, &mut mapper)
            .expect_err("overlapping insert before existing range should fail");

        assert!(matches!(
            err,
            GuestMemoryAllocationError::InvalidLayout(GuestMemoryError::OverlappingRange {
                previous,
                next,
            }) if previous == overlapping_before && next == original_range
        ));

        let err = memory
            .insert_region_with_mapper(overlapping_after, page_size, &mut mapper)
            .expect_err("overlapping insert after existing range should fail");

        assert!(matches!(
            err,
            GuestMemoryAllocationError::InvalidLayout(GuestMemoryError::OverlappingRange {
                previous,
                next,
            }) if previous == original_range && next == overlapping_after
        ));
        assert_eq!(memory_ranges(&memory), vec![original_range]);
        assert_eq!(mapper.maps, 1);
        assert_eq!(drop_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn guest_memory_insert_region_rejects_unaligned_range_without_allocation() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let unaligned_range = range(page_size * 2, page_size - 1);
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut mapper = CountingMapper {
            maps: 0,
            drop_count: Arc::clone(&drop_count),
        };
        let mut memory = GuestMemory::allocate_with_mapper(
            &GuestMemoryLayout::new(vec![range(0, page_size)]).expect("layout should be valid"),
            page_size,
            &mut mapper,
        )
        .expect("guest memory should allocate");

        let err = memory
            .insert_region_with_mapper(unaligned_range, page_size, &mut mapper)
            .expect_err("unaligned insert should fail");

        assert!(matches!(
            err,
            GuestMemoryAllocationError::InvalidLayout(GuestMemoryError::UnalignedRange {
                range,
                alignment,
            }) if range == unaligned_range && alignment == page_size
        ));
        assert_eq!(memory_ranges(&memory), vec![range(0, page_size)]);
        assert_eq!(mapper.maps, 1);
        assert_eq!(drop_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn guest_memory_insert_region_access_spans_dynamic_adjacent_ranges() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let mut memory = allocate_memory(vec![range(0, page_size)]);
        let source = [0x33, 0x44];
        let address = GuestAddress::new(page_size - 1);
        let mut destination = [0; 2];

        memory
            .insert_region(range(page_size, page_size))
            .expect("dynamic adjacent insert should succeed");
        memory
            .write_slice(&source, address)
            .expect("write should cross dynamic adjacent range");
        memory
            .read_slice(&mut destination, address)
            .expect("read should cross dynamic adjacent range");

        assert_eq!(destination, source);
    }

    #[test]
    fn guest_memory_remove_region_drops_exact_range() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let removed_range = range(page_size, page_size);
        let remaining_range = range(0, page_size);
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut mapper = CountingMapper {
            maps: 0,
            drop_count: Arc::clone(&drop_count),
        };
        let mut memory = GuestMemory::allocate_with_mapper(
            &GuestMemoryLayout::new(vec![remaining_range, removed_range])
                .expect("layout should be valid"),
            page_size,
            &mut mapper,
        )
        .expect("guest memory should allocate");

        memory
            .remove_region(removed_range)
            .expect("exact region removal should succeed");

        assert_eq!(memory_ranges(&memory), vec![remaining_range]);
        assert_eq!(memory.total_size(), page_size);
        assert_eq!(drop_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn guest_memory_remove_region_allows_empty_memory_and_reinsert() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let original_range = range(0, page_size);
        let reinserted_range = range(page_size, page_size);
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut mapper = CountingMapper {
            maps: 0,
            drop_count: Arc::clone(&drop_count),
        };
        let mut memory = GuestMemory::allocate_with_mapper(
            &GuestMemoryLayout::new(vec![original_range]).expect("layout should be valid"),
            page_size,
            &mut mapper,
        )
        .expect("guest memory should allocate");

        memory
            .remove_region(original_range)
            .expect("removing the only region should succeed");
        memory
            .insert_region_with_mapper(reinserted_range, page_size, &mut mapper)
            .expect("reinserting into empty guest memory should succeed");

        assert_eq!(memory_ranges(&memory), vec![reinserted_range]);
        assert_eq!(memory.total_size(), page_size);
        assert_eq!(mapper.maps, 2);
        assert_eq!(drop_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn guest_memory_remove_region_rejects_missing_range_without_mutation() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let existing_range = range(0, page_size);
        let missing_range = range(page_size, page_size);
        let mut memory = allocate_memory(vec![existing_range]);

        assert_eq!(
            memory.remove_region(missing_range),
            Err(super::GuestMemoryRegionRemovalError::MissingRange {
                range: missing_range
            })
        );
        assert_eq!(
            super::GuestMemoryRegionRemovalError::MissingRange {
                range: missing_range
            }
            .to_string(),
            format!("guest memory region {missing_range} is not mapped")
        );
        assert_eq!(memory_ranges(&memory), vec![existing_range]);
    }

    #[test]
    fn guest_memory_insert_region_allocation_failure_does_not_mutate() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let existing_range = range(0, page_size);
        let inserted_range = range(page_size, page_size);
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut initial_mapper = CountingMapper {
            maps: 0,
            drop_count: Arc::clone(&drop_count),
        };
        let mut memory = GuestMemory::allocate_with_mapper(
            &GuestMemoryLayout::new(vec![existing_range]).expect("layout should be valid"),
            page_size,
            &mut initial_mapper,
        )
        .expect("guest memory should allocate");
        let mut failing_mapper = FailingMapper {
            maps: 0,
            fail_on: 1,
            drop_count: Arc::clone(&drop_count),
        };

        let err = memory
            .insert_region_with_mapper(inserted_range, page_size, &mut failing_mapper)
            .expect_err("insert allocation failure should fail");

        assert!(matches!(
            err,
            GuestMemoryAllocationError::AnonymousMmapFailed { size, .. }
                if size == usize::try_from(page_size).expect("page size should fit usize")
        ));
        assert_eq!(memory_ranges(&memory), vec![existing_range]);
        assert_eq!(failing_mapper.maps, 1);
        assert_eq!(drop_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn guest_memory_access_validation_rejects_aarch64_mmio64_gap() {
        let size_before_gap = aarch64::MMIO64_MEM_START - aarch64::DRAM_MEM_START;
        let layout = aarch64::dram_layout(size_before_gap + PAGE_SIZE)
            .expect("split aarch64 layout should be valid");
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut mapper = CountingMapper {
            maps: 0,
            drop_count: Arc::clone(&drop_count),
        };
        let memory = GuestMemory::allocate_with_mapper(&layout, PAGE_SIZE, &mut mapper)
            .expect("fake guest memory allocation should succeed");
        let access_range = range(aarch64::MMIO64_MEM_START - 1, 2);

        assert_eq!(
            memory.validate_mapped_range(access_range),
            Err(GuestMemoryAccessError::UnmappedRange {
                range: access_range
            })
        );
        assert_eq!(mapper.maps, 2);
        assert_eq!(drop_count.load(Ordering::Relaxed), 0);

        drop(memory);

        assert_eq!(drop_count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn guest_memory_rejects_unaligned_layout_before_allocation() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let unaligned_range = range(page_size, page_size - 1);
        let layout =
            GuestMemoryLayout::new(vec![unaligned_range]).expect("layout ordering should be valid");
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut mapper = CountingMapper {
            maps: 0,
            drop_count: Arc::clone(&drop_count),
        };

        let err = GuestMemory::allocate_with_mapper(&layout, page_size, &mut mapper)
            .expect_err("unaligned allocation should fail");

        assert!(matches!(
            err,
            GuestMemoryAllocationError::InvalidLayout(GuestMemoryError::UnalignedRange {
                range,
                alignment,
            }) if range == unaligned_range && alignment == page_size
        ));
        assert_eq!(mapper.maps, 0);
        assert_eq!(drop_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn guest_memory_validates_all_ranges_before_allocation() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let unaligned_range = range(page_size, page_size - 1);
        let layout = GuestMemoryLayout::new(vec![range(0, page_size), unaligned_range])
            .expect("layout ordering should be valid");
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut mapper = CountingMapper {
            maps: 0,
            drop_count: Arc::clone(&drop_count),
        };

        let err = GuestMemory::allocate_with_mapper(&layout, page_size, &mut mapper)
            .expect_err("unaligned second range should fail before allocation");

        assert!(matches!(
            err,
            GuestMemoryAllocationError::InvalidLayout(GuestMemoryError::UnalignedRange {
                range,
                alignment,
            }) if range == unaligned_range && alignment == page_size
        ));
        assert_eq!(mapper.maps, 0);
        assert_eq!(drop_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn guest_memory_rejects_invalid_host_page_size_before_allocation() {
        let layout = GuestMemoryLayout::new(vec![range(0, PAGE_SIZE)])
            .expect("page-aligned layout should be valid");

        for page_size in [0, 3] {
            let drop_count = Arc::new(AtomicUsize::new(0));
            let mut mapper = CountingMapper {
                maps: 0,
                drop_count: Arc::clone(&drop_count),
            };

            let err = GuestMemory::allocate_with_mapper(&layout, page_size, &mut mapper)
                .expect_err("invalid host page size should fail before allocation");

            assert!(matches!(
                err,
                GuestMemoryAllocationError::InvalidHostPageSize
            ));
            assert_eq!(mapper.maps, 0);
            assert_eq!(drop_count.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn guest_memory_allocations_are_independent() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let layout = GuestMemoryLayout::new(vec![range(0, page_size)])
            .expect("page-aligned layout should be valid");

        let first = GuestMemory::allocate(&layout).expect("first allocation should succeed");
        let second = GuestMemory::allocate(&layout).expect("second allocation should succeed");
        let first_region = first
            .regions()
            .first()
            .expect("first allocation should contain one region");
        let second_region = second
            .regions()
            .first()
            .expect("second allocation should contain one region");

        assert_eq!(first_region.range(), second_region.range());
        assert_ne!(first_region.host_address(), second_region.host_address());
    }

    #[test]
    fn anonymous_guest_memory_remains_the_default_and_has_no_descriptor_export() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let layout = GuestMemoryLayout::new(vec![range(0, page_size)])
            .expect("page-aligned layout should be valid");

        let memory = GuestMemory::allocate(&layout).expect("anonymous allocation should succeed");
        let region = memory
            .regions()
            .first()
            .expect("anonymous allocation should contain one region");

        assert_eq!(memory.backing(), GuestMemoryBacking::Anonymous);
        assert_eq!(region.backing(), GuestMemoryRegionBacking::Anonymous);
        assert!(
            !region
                .validate_shared_backing()
                .expect("anonymous backing validation should succeed")
        );
        assert!(
            region
                .try_clone_shared_backing()
                .expect("anonymous export query should succeed")
                .is_none()
        );
    }

    #[test]
    fn shared_guest_memory_is_exact_unlinked_cloexec_and_coherent() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let layout = GuestMemoryLayout::new(vec![range(0, page_size)])
            .expect("page-aligned layout should be valid");
        let mut memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("shared allocation should succeed");
        let region = memory
            .regions()
            .first()
            .expect("shared allocation should contain one region");
        assert_eq!(region.backing(), GuestMemoryRegionBacking::Shared);
        let region_debug = format!("{region:?}");
        let host_address = format!("{:p}", region.host_address());
        let export = region
            .try_clone_shared_backing()
            .expect("shared descriptor should clone")
            .expect("shared region should have an export");
        assert!(
            region
                .validate_shared_backing()
                .expect("shared backing validation should succeed")
        );
        let metadata = export
            .file
            .metadata()
            .expect("shared descriptor metadata should be readable");

        assert_eq!(memory.backing(), GuestMemoryBacking::Shared);
        assert_eq!(export.offset(), 0);
        assert_eq!(export.len(), page_size);
        assert!(!export.is_empty());
        assert_eq!(metadata.len(), page_size);
        assert_eq!(metadata.nlink(), 0);
        assert_eq!(metadata.mode() & 0o777, 0o600);
        // SAFETY: F_GETFD only inspects flags on this live borrowed descriptor.
        let descriptor_flags = unsafe { libc::fcntl(export.as_fd().as_raw_fd(), libc::F_GETFD) };
        assert!(descriptor_flags >= 0);
        assert_ne!(descriptor_flags & libc::FD_CLOEXEC, 0);

        let from_memory = [0x11, 0x22, 0x33, 0x44];
        memory
            .write_slice(&from_memory, GuestAddress::new(0))
            .expect("guest-memory write should succeed");
        let mut descriptor_read = [0_u8; 4];
        export
            .file
            .read_exact_at(&mut descriptor_read, 0)
            .expect("descriptor should observe guest-memory writes");
        assert_eq!(descriptor_read, from_memory);

        let from_descriptor = [0xaa, 0xbb, 0xcc, 0xdd];
        export
            .file
            .write_all_at(&from_descriptor, 8)
            .expect("descriptor write should succeed");
        let mut memory_read = [0_u8; 4];
        memory
            .read_slice(&mut memory_read, GuestAddress::new(8))
            .expect("guest memory should observe descriptor writes");
        assert_eq!(memory_read, from_descriptor);

        let export_debug = format!("{export:?}");
        assert!(!export_debug.contains("File"));
        assert!(!export_debug.contains("fd"));
        assert!(!export_debug.contains("bangbang-memory"));
        assert!(!region_debug.contains(&host_address));
    }

    #[test]
    fn shared_region_export_rejects_a_changed_descriptor_length() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let layout = GuestMemoryLayout::new(vec![range(0, page_size)])
            .expect("page-aligned layout should be valid");
        let memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("shared allocation should succeed");
        let region = memory
            .regions()
            .first()
            .expect("shared allocation should contain one region");
        let export = region
            .try_clone_shared_backing()
            .expect("initial shared descriptor should clone")
            .expect("shared region should have an export");
        let shortened = page_size / 2;
        export
            .file
            .set_len(shortened)
            .expect("test descriptor should be resizable");

        assert!(matches!(
            region
                .validate_shared_backing()
                .expect_err("changed descriptor length should fail validation"),
            GuestMemorySharedBackingError::UnexpectedLength { expected, actual }
                if expected == page_size && actual == shortened
        ));

        let error = region
            .try_clone_shared_backing()
            .expect_err("changed descriptor length should be rejected");
        assert!(matches!(
            error,
            GuestMemorySharedBackingError::UnexpectedLength { expected, actual }
                if expected == page_size && actual == shortened
        ));

        export
            .file
            .set_len(page_size)
            .expect("test descriptor length should be restored before unmapping");
    }

    #[test]
    fn shared_guest_memory_dynamic_regions_inherit_the_backing_profile() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let first_range = range(0, page_size);
        let second_range = range(page_size, page_size);
        let layout =
            GuestMemoryLayout::new(vec![first_range]).expect("page-aligned layout should be valid");
        let mut memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("shared allocation should succeed");

        memory
            .insert_region(second_range)
            .expect("shared dynamic region should allocate");
        assert!(memory.regions().iter().all(|region| {
            region.backing() == GuestMemoryRegionBacking::Shared
                && region
                    .try_clone_shared_backing()
                    .expect("shared descriptor should clone")
                    .is_some()
        }));

        let surviving_export = memory.regions()[1]
            .try_clone_shared_backing()
            .expect("dynamic shared descriptor should clone")
            .expect("dynamic region should have an export");
        memory
            .remove_region(second_range)
            .expect("dynamic shared region should be removable");
        assert_eq!(memory_ranges(&memory), vec![first_range]);
        assert_eq!(
            surviving_export
                .file
                .metadata()
                .expect("independent export should remain live")
                .len(),
            page_size
        );
    }

    #[test]
    fn shared_reservation_stays_out_of_active_guest_memory_and_exports_once() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let boot_range = range(0, page_size);
        let reservation_range = range(page_size * 2, page_size * 4);
        let online_range = range(page_size * 3, page_size);
        let layout =
            GuestMemoryLayout::new(vec![boot_range]).expect("page-aligned layout should be valid");
        let mut memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("shared allocation should succeed");

        memory
            .reserve_shared_region(reservation_range)
            .expect("shared reservation should succeed");

        let capture = memory
            .shared_reservation_capture_state(reservation_range)
            .expect("exact shared reservation should be capture-ready");
        assert_eq!(capture.range(), reservation_range);
        assert!(matches!(
            memory.shared_reservation_capture_state(online_range),
            Err(GuestMemorySharedReservationCaptureError::Missing { range })
                if range == online_range
        ));
        assert_eq!(
            format!("{capture:?}"),
            "GuestMemorySharedReservationCaptureState { state: \"<redacted>\" }"
        );

        assert_eq!(memory_ranges(&memory), vec![boot_range]);
        assert_eq!(memory.total_size(), page_size);
        assert_eq!(
            memory
                .shared_export_regions()
                .map(GuestMemoryRegion::range)
                .collect::<Vec<_>>(),
            vec![boot_range, reservation_range]
        );
        let mut offline_byte = [0xff];
        assert_eq!(
            memory.read_slice(&mut offline_byte, reservation_range.start()),
            Err(GuestMemoryAccessError::UnmappedRange {
                range: range(reservation_range.start().raw_value(), 1)
            })
        );
        assert_eq!(offline_byte, [0xff]);

        let reservation = memory
            .shared_reservations
            .first()
            .expect("reservation should remain owned");
        let reservation_address = reservation.host_address().as_ptr().addr();
        let reservation_export = reservation
            .try_clone_shared_backing()
            .expect("reservation export should clone")
            .expect("reservation should be shared");
        let reservation_metadata = reservation_export
            .file
            .metadata()
            .expect("reservation descriptor should be inspectable");

        let tracker = memory
            .enable_dirty_tracking()
            .expect("dirty tracking should enable for active RAM");
        memory
            .insert_region(online_range)
            .expect("online range should become a reservation view");

        assert_eq!(memory_ranges(&memory), vec![boot_range, online_range]);
        assert_eq!(memory.total_size(), page_size * 2);
        assert_eq!(
            memory
                .shared_export_regions()
                .map(GuestMemoryRegion::range)
                .collect::<Vec<_>>(),
            vec![boot_range, reservation_range]
        );
        let online = memory
            .regions()
            .iter()
            .find(|region| region.range() == online_range)
            .expect("online view should be active");
        assert_eq!(online.mapping_identity(), capture.mapping_identity());
        assert_eq!(
            online.host_address().as_ptr().addr(),
            reservation_address + usize::try_from(page_size).expect("page size should fit usize")
        );
        let online_export = online
            .try_clone_shared_backing()
            .expect("online view export should clone")
            .expect("online view should retain shared backing");
        let online_metadata = online_export
            .file
            .metadata()
            .expect("online descriptor should be inspectable");
        assert_eq!(online_export.offset(), page_size);
        assert_eq!(online_export.len(), page_size);
        assert_eq!(online_metadata.dev(), reservation_metadata.dev());
        assert_eq!(online_metadata.ino(), reservation_metadata.ino());
        let online_address = online.host_address();

        memory
            .write_slice(&[0x5a], online_range.start())
            .expect("online view should be writable");
        let mut descriptor_byte = [0];
        reservation_export
            .file
            .read_exact_at(&mut descriptor_byte, page_size)
            .expect("reservation descriptor should observe the view write");
        assert_eq!(descriptor_byte, [0x5a]);
        assert_eq!(
            tracker.dirty_pages().expect("dirty pages should query"),
            vec![online_range.start()]
        );

        memory
            .remove_region(online_range)
            .expect("online view should be removable");
        assert_eq!(memory_ranges(&memory), vec![boot_range]);
        assert_eq!(memory.total_size(), page_size);
        assert!(
            tracker
                .dirty_pages()
                .expect("dirty pages should query after removal")
                .is_empty()
        );
        memory
            .insert_region(online_range)
            .expect("removed range should reuse the retained reservation");
        assert_eq!(
            memory
                .regions()
                .iter()
                .find(|region| region.range() == online_range)
                .expect("reinserted view should be active")
                .host_address(),
            online_address
        );
    }

    #[test]
    fn shared_export_regions_merge_active_memory_and_reservations_by_guest_address() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let reservation_range = range(0, page_size * 2);
        let boot_range = range(page_size * 4, page_size);
        let layout =
            GuestMemoryLayout::new(vec![boot_range]).expect("page-aligned layout should be valid");
        let mut memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("shared allocation should succeed");

        memory
            .reserve_shared_region(reservation_range)
            .expect("lower reservation should succeed");

        assert_eq!(
            memory
                .shared_export_regions()
                .map(GuestMemoryRegion::range)
                .collect::<Vec<_>>(),
            vec![reservation_range, boot_range]
        );
    }

    #[test]
    fn shared_reservation_capture_identities_are_stable_and_not_reused() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let boot_range = range(0, page_size);
        let reservation_range = range(page_size * 2, page_size * 2);
        let layout =
            GuestMemoryLayout::new(vec![boot_range]).expect("page-aligned layout should be valid");
        let mut first = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("first shared memory should allocate");
        let mut second = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("second shared memory should allocate");
        first
            .reserve_shared_region(reservation_range)
            .expect("first reservation should succeed");
        second
            .reserve_shared_region(reservation_range)
            .expect("second reservation should succeed");

        let first_capture = first
            .shared_reservation_capture_state(reservation_range)
            .expect("first reservation should capture");
        let repeated_capture = first
            .shared_reservation_capture_state(reservation_range)
            .expect("repeated capture should succeed");
        let second_capture = second
            .shared_reservation_capture_state(reservation_range)
            .expect("second reservation should capture");

        assert_eq!(
            first_capture.mapping_identity(),
            repeated_capture.mapping_identity()
        );
        assert_ne!(
            first_capture.mapping_identity(),
            second_capture.mapping_identity()
        );
    }

    #[test]
    fn shared_reservation_accepts_any_profile_and_rejects_overlapping_ranges() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let boot_range = range(0, page_size);
        let reservation_range = range(page_size * 2, page_size * 2);
        let layout =
            GuestMemoryLayout::new(vec![boot_range]).expect("page-aligned layout should be valid");
        let mut anonymous =
            GuestMemory::allocate(&layout).expect("anonymous memory should allocate");

        anonymous
            .reserve_shared_region(reservation_range)
            .expect("explicit reservation should be independent of the dynamic profile");
        assert_eq!(anonymous.shared_export_regions().count(), 1);
        assert_eq!(anonymous.backing(), GuestMemoryBacking::Anonymous);

        let mut shared = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("shared memory should allocate");
        shared
            .reserve_shared_region(reservation_range)
            .expect("first reservation should succeed");
        assert!(matches!(
            shared
                .reserve_shared_region(range(page_size * 3, page_size * 2))
                .expect_err("overlapping reservation should fail"),
            GuestMemoryAllocationError::InvalidLayout(GuestMemoryError::OverlappingRange { .. })
        ));
        assert!(matches!(
            shared
                .insert_region(range(page_size, page_size * 2))
                .expect_err("partially overlapping online range should fail"),
            GuestMemoryAllocationError::InvalidLayout(GuestMemoryError::OverlappingRange { .. })
        ));
        assert_eq!(memory_ranges(&shared), vec![boot_range]);
        assert_eq!(shared.shared_reservations.len(), 1);
    }

    #[test]
    fn shared_reservation_discard_uses_the_view_backing_offset() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let layout = GuestMemoryLayout::new(vec![range(0, page_size)])
            .expect("page-aligned layout should be valid");
        let reservation_range = range(page_size * 2, page_size * 4);
        let online_range = range(page_size * 4, page_size);
        let mut memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("shared allocation should succeed");
        memory
            .reserve_shared_region(reservation_range)
            .expect("shared reservation should succeed");
        memory
            .insert_region(online_range)
            .expect("online range should become a reservation view");
        let mut adviser = TestSharedReclaimAdviser {
            page_size,
            calls: Vec::new(),
        };

        let outcome = memory.discard_range_with_adviser(online_range, &mut adviser);

        assert!(outcome.is_complete(), "discard outcome: {outcome:?}");
        assert_eq!(
            adviser.calls,
            vec![(
                usize::try_from(page_size * 2).expect("view offset should fit usize"),
                usize::try_from(page_size).expect("view size should fit usize")
            )]
        );
    }

    #[test]
    fn shared_guest_memory_writes_participate_in_dirty_tracking() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let layout = GuestMemoryLayout::new(vec![range(0, page_size)])
            .expect("page-aligned layout should be valid");
        let mut memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("shared allocation should succeed");
        let tracker = memory
            .enable_dirty_tracking()
            .expect("shared dirty tracking should enable");

        memory
            .write_slice(&[0x55], GuestAddress::new(1))
            .expect("tracked shared write should succeed");

        assert_eq!(
            tracker
                .dirty_pages()
                .expect("shared dirty pages should query"),
            vec![GuestAddress::new(0)]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn shared_guest_memory_supports_zero_safe_discard() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let layout = GuestMemoryLayout::new(vec![range(0, page_size)])
            .expect("page-aligned layout should be valid");
        let mut memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("shared allocation should succeed");
        memory
            .write_slice(
                &vec![0x7f; usize::try_from(page_size).expect("page size should fit")],
                GuestAddress::new(0),
            )
            .expect("shared page population should succeed");

        let outcome = memory.discard_range(range(0, page_size));

        assert!(outcome.is_complete(), "discard outcome: {outcome:?}");
        assert_eq!(outcome.advised_bytes(), page_size);
        let mut contents = vec![0xff; usize::try_from(page_size).expect("page size should fit")];
        memory
            .read_slice(&mut contents, GuestAddress::new(0))
            .expect("discarded shared page should remain readable");
        assert!(contents.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn shared_guest_memory_preflight_rejects_resource_limits() {
        let ranges = [range(0, PAGE_SIZE), range(PAGE_SIZE, PAGE_SIZE)];

        let error = preflight_shared_memory_resources_with_limits(
            ranges.len(),
            &ranges,
            SharedMemoryResourceLimits {
                file_size: PAGE_SIZE - 1,
                file_descriptors: libc::RLIM_INFINITY,
            },
        )
        .expect_err("low file-size limit should reject shared memory");
        assert!(matches!(
            error,
            GuestMemoryAllocationError::SharedFileSizeLimitExceeded { size }
                if size == PAGE_SIZE
        ));

        let error = preflight_shared_memory_resources_with_limits(
            ranges.len(),
            &ranges,
            SharedMemoryResourceLimits {
                file_size: libc::RLIM_INFINITY,
                file_descriptors: 1,
            },
        )
        .expect_err("low descriptor limit should reject shared memory");
        assert!(matches!(
            error,
            GuestMemoryAllocationError::SharedFileDescriptorLimitExceeded { regions: 2 }
        ));
    }

    #[test]
    fn shared_aperture_preflight_uses_complete_size_and_retained_table_count() {
        let aperture_size = 128 * 1024 * 1024;
        let error = preflight_shared_memory_resource_values_with_limits(
            3,
            aperture_size,
            SharedMemoryResourceLimits {
                file_size: aperture_size - 1,
                file_descriptors: libc::RLIM_INFINITY,
            },
        )
        .expect_err("full aperture must fit the file-size limit");
        assert!(matches!(
            error,
            GuestMemoryAllocationError::SharedFileSizeLimitExceeded { size }
                if size == aperture_size
        ));

        let error = preflight_shared_memory_resource_values_with_limits(
            3,
            aperture_size,
            SharedMemoryResourceLimits {
                file_size: libc::RLIM_INFINITY,
                file_descriptors: 2,
            },
        )
        .expect_err("boot regions plus aperture must fit the descriptor limit");
        assert!(matches!(
            error,
            GuestMemoryAllocationError::SharedFileDescriptorLimitExceeded { regions: 3 }
        ));
    }

    #[test]
    fn guest_memory_allocations_are_independent_across_threads() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let thread_count = 4;
        let start = Arc::new(Barrier::new(thread_count));
        let handles = (0..thread_count)
            .map(|_| {
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();

                    let layout = GuestMemoryLayout::new(vec![range(0, page_size)])
                        .expect("page-aligned layout should be valid");

                    GuestMemory::allocate(&layout).expect("guest memory allocation should succeed")
                })
            })
            .collect::<Vec<_>>();
        let memories = handles
            .into_iter()
            .map(|handle| handle.join().expect("allocation thread should not panic"))
            .collect::<Vec<_>>();
        let mut host_addresses = HashSet::new();

        for memory in &memories {
            let region = memory
                .regions()
                .first()
                .expect("guest memory should contain one region");
            assert_eq!(region.range(), range(0, page_size));
            assert!(host_addresses.insert(region.host_address()));
        }
    }

    #[test]
    fn guest_memory_drop_releases_all_regions() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let layout = GuestMemoryLayout::new(vec![range(0, page_size), range(page_size, page_size)])
            .expect("page-aligned layout should be valid");
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut mapper = CountingMapper {
            maps: 0,
            drop_count: Arc::clone(&drop_count),
        };

        let memory = GuestMemory::allocate_with_mapper(&layout, page_size, &mut mapper)
            .expect("guest memory allocation should succeed");

        assert_eq!(mapper.maps, 2);
        assert_eq!(drop_count.load(Ordering::Relaxed), 0);

        drop(memory);

        assert_eq!(drop_count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn guest_memory_drop_releases_regions_after_thread_move() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let layout = GuestMemoryLayout::new(vec![range(0, page_size), range(page_size, page_size)])
            .expect("page-aligned layout should be valid");
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut mapper = CountingMapper {
            maps: 0,
            drop_count: Arc::clone(&drop_count),
        };
        let memory = GuestMemory::allocate_with_mapper(&layout, page_size, &mut mapper)
            .expect("guest memory allocation should succeed");

        let handle = thread::spawn(move || drop(memory));

        handle.join().expect("drop thread should not panic");
        assert_eq!(drop_count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn guest_memory_partial_allocation_failure_drops_previous_regions() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let layout = GuestMemoryLayout::new(vec![range(0, page_size), range(page_size, page_size)])
            .expect("page-aligned layout should be valid");
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut mapper = FailingMapper {
            maps: 0,
            fail_on: 2,
            drop_count: Arc::clone(&drop_count),
        };

        let err = GuestMemory::allocate_with_mapper(&layout, page_size, &mut mapper)
            .expect_err("second region allocation should fail");

        assert!(matches!(
            err,
            GuestMemoryAllocationError::AnonymousMmapFailed { size, .. }
                if size == usize::try_from(page_size).expect("page size should fit usize")
        ));
        assert_eq!(mapper.maps, 2);
        assert_eq!(drop_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn private_file_memory_preflights_all_extents_and_drops_partial_mapping_failure() {
        let page_size = host_page_size().expect("host page size should be available for tests");
        let file_size = usize::try_from(page_size * 3).expect("test file size should fit usize");
        let file = Arc::new(
            super::create_shared_memory_file(file_size)
                .expect("test descriptor backing should create"),
        );
        let page_size_usize = usize::try_from(page_size).expect("test page size should fit usize");
        assert!(matches!(
            GuestMemoryMapping::map_private_file(page_size_usize - 1, Arc::clone(&file), page_size),
            Err(GuestMemoryAllocationError::PrivateFileMappingSizeInvalid)
        ));
        assert!(matches!(
            GuestMemoryMapping::map_private_file(page_size_usize, Arc::clone(&file), 1),
            Err(GuestMemoryAllocationError::PrivateFileOffsetUnaligned)
        ));
        let valid = [
            (range(0, page_size), page_size),
            (range(page_size, page_size), page_size * 2),
        ];
        let mut preflight_calls = 0;
        let unaligned = [valid[0], (valid[1].0, valid[1].1 + 1)];
        let error = GuestMemory::from_private_file_ranges_with_mapper(
            &unaligned,
            Arc::clone(&file),
            GuestMemoryBacking::Anonymous,
            &mut |size, _file, _offset| {
                preflight_calls += 1;
                Ok(GuestMemoryMapping::test_mapping(
                    size,
                    Arc::new(AtomicUsize::new(0)),
                ))
            },
        )
        .expect_err("a later unaligned extent should fail before every mmap");
        assert!(matches!(
            error,
            GuestMemoryAllocationError::PrivateFileOffsetUnaligned
        ));
        assert_eq!(preflight_calls, 0);

        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut maps = 0;
        let error = GuestMemory::from_private_file_ranges_with_mapper(
            &valid,
            Arc::clone(&file),
            GuestMemoryBacking::Anonymous,
            &mut |size, _file, _offset| {
                maps += 1;
                if maps == 2 {
                    Err(GuestMemoryAllocationError::PrivateFileMmapFailed {
                        size,
                        source: io::Error::other("injected private-file mmap failure"),
                    })
                } else {
                    Ok(GuestMemoryMapping::test_mapping(
                        size,
                        Arc::clone(&drop_count),
                    ))
                }
            },
        )
        .expect_err("second private-file mmap should fail");
        assert!(matches!(
            error,
            GuestMemoryAllocationError::PrivateFileMmapFailed { .. }
        ));
        assert_eq!(maps, 2);
        assert_eq!(drop_count.load(Ordering::Relaxed), 1);
        assert_eq!(Arc::strong_count(&file), 1);
    }

    #[derive(Debug)]
    struct CountingMapper {
        maps: usize,
        drop_count: Arc<AtomicUsize>,
    }

    impl GuestMemoryMapper for CountingMapper {
        fn map(&mut self, size: usize) -> Result<GuestMemoryMapping, GuestMemoryAllocationError> {
            self.maps += 1;
            Ok(GuestMemoryMapping::test_mapping(
                size,
                Arc::clone(&self.drop_count),
            ))
        }
    }

    #[derive(Debug)]
    struct FailingMapper {
        maps: usize,
        fail_on: usize,
        drop_count: Arc<AtomicUsize>,
    }

    impl GuestMemoryMapper for FailingMapper {
        fn map(&mut self, size: usize) -> Result<GuestMemoryMapping, GuestMemoryAllocationError> {
            self.maps += 1;

            if self.maps == self.fail_on {
                return Err(GuestMemoryAllocationError::AnonymousMmapFailed {
                    size,
                    source: io::Error::from_raw_os_error(libc::ENOMEM),
                });
            }

            Ok(GuestMemoryMapping::test_mapping(
                size,
                Arc::clone(&self.drop_count),
            ))
        }
    }
}
