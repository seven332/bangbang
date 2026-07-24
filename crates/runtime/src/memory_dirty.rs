//! Backend-neutral guest-memory dirty-page generations.

use std::collections::TryReserveError;
use std::fmt;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::memory::{GuestAddress, GuestMemoryRange};

/// Failure while creating or extending guest-memory dirty metadata.
#[derive(Debug)]
pub enum GuestMemoryDirtyTrackerError {
    ProtectedLazyMemory,
    InvalidPageSize {
        page_size: u64,
    },
    UnalignedRegion {
        range: GuestMemoryRange,
        page_size: u64,
    },
    EmptyRegions,
    UnorderedOrOverlappingRegion {
        range: GuestMemoryRange,
    },
    PageCountOverflow {
        range: GuestMemoryRange,
    },
    MetadataAllocationFailed {
        source: TryReserveError,
    },
}

impl fmt::Display for GuestMemoryDirtyTrackerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProtectedLazyMemory => {
                formatter.write_str("protected lazy guest memory cannot enable dirty tracking")
            }
            Self::InvalidPageSize { page_size } => {
                write!(formatter, "invalid dirty-page size {page_size}")
            }
            Self::UnalignedRegion { range, page_size } => write!(
                formatter,
                "dirty-page region {range} is not aligned to {page_size} bytes"
            ),
            Self::EmptyRegions => formatter.write_str("dirty-page tracker has no regions"),
            Self::UnorderedOrOverlappingRegion { range } => write!(
                formatter,
                "dirty-page region {range} is unordered or overlaps existing metadata"
            ),
            Self::PageCountOverflow { range } => {
                write!(
                    formatter,
                    "dirty-page count for region {range} exceeds this host"
                )
            }
            Self::MetadataAllocationFailed { .. } => {
                formatter.write_str("failed to allocate dirty-page metadata")
            }
        }
    }
}

impl std::error::Error for GuestMemoryDirtyTrackerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MetadataAllocationFailed { source } => Some(source),
            Self::ProtectedLazyMemory
            | Self::InvalidPageSize { .. }
            | Self::UnalignedRegion { .. }
            | Self::EmptyRegions
            | Self::UnorderedOrOverlappingRegion { .. }
            | Self::PageCountOverflow { .. } => None,
        }
    }
}

/// Failure while marking or querying dirty-page metadata.
#[derive(Debug)]
pub enum GuestMemoryDirtyTrackerAccessError {
    UntrackedRange { range: GuestMemoryRange },
    MetadataAllocationFailed { source: TryReserveError },
    InvalidState(&'static str),
}

impl fmt::Display for GuestMemoryDirtyTrackerAccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UntrackedRange { range } => {
                write!(formatter, "dirty-page range {range} is not fully tracked")
            }
            Self::MetadataAllocationFailed { .. } => {
                formatter.write_str("failed to allocate dirty-page query metadata")
            }
            Self::InvalidState(message) => {
                write!(formatter, "invalid dirty-page query state: {message}")
            }
        }
    }
}

impl std::error::Error for GuestMemoryDirtyTrackerAccessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MetadataAllocationFailed { source } => Some(source),
            Self::UntrackedRange { .. } | Self::InvalidState(_) => None,
        }
    }
}

/// One shared dirty-page generation for every process-owned guest-memory region.
///
/// Region topology changes take the small metadata lock. Steady-state marks
/// take a read lock and set atomic words, so independent VMM/device and vCPU
/// writers can publish into one generation without allocating.
pub struct GuestMemoryDirtyTracker {
    page_size: u64,
    epoch: AtomicU64,
    regions: RwLock<Vec<DirtyRegion>>,
}

impl GuestMemoryDirtyTracker {
    pub fn new(
        ranges: impl IntoIterator<Item = GuestMemoryRange>,
        page_size: u64,
    ) -> Result<Self, GuestMemoryDirtyTrackerError> {
        validate_page_size(page_size)?;
        let mut regions = Vec::new();
        for range in ranges {
            let index = validate_insert_position(&regions, range)?;
            if index != regions.len() {
                return Err(GuestMemoryDirtyTrackerError::UnorderedOrOverlappingRegion { range });
            }
            regions.try_reserve_exact(1).map_err(|source| {
                GuestMemoryDirtyTrackerError::MetadataAllocationFailed { source }
            })?;
            regions.push(DirtyRegion::new(range, page_size, false)?);
        }
        if regions.is_empty() {
            return Err(GuestMemoryDirtyTrackerError::EmptyRegions);
        }

        Ok(Self {
            page_size,
            epoch: AtomicU64::new(0),
            regions: RwLock::new(regions),
        })
    }

    /// Return the host-page granularity used by this tracker.
    pub const fn page_size(&self) -> u64 {
        self.page_size
    }

    /// Return the current generation number.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// Return whether one range is fully covered by current tracker metadata.
    pub fn contains_range(&self, range: GuestMemoryRange) -> bool {
        validate_tracked_range(&read_unpoisoned(&self.regions), range).is_ok()
    }

    /// Mark every host page intersecting one fully tracked guest range.
    pub fn mark_range(
        &self,
        range: GuestMemoryRange,
    ) -> Result<(), GuestMemoryDirtyTrackerAccessError> {
        let regions = read_unpoisoned(&self.regions);
        validate_tracked_range(&regions, range)?;
        for region in regions.iter() {
            region.mark_intersection(range, self.page_size);
        }
        Ok(())
    }

    /// Snapshot exact dirty page addresses in ascending region/page order.
    pub fn dirty_pages(&self) -> Result<Vec<GuestAddress>, GuestMemoryDirtyTrackerAccessError> {
        let regions = read_unpoisoned(&self.regions);
        let mut pages = Vec::new();
        for region in regions.iter() {
            region.append_dirty_pages(self.page_size, &mut pages)?;
        }
        Ok(pages)
    }

    pub(crate) fn insert_region(
        &self,
        range: GuestMemoryRange,
        initially_dirty: bool,
    ) -> Result<(), GuestMemoryDirtyTrackerError> {
        let mut regions = write_unpoisoned(&self.regions);
        let index = validate_insert_position(&regions, range)?;
        regions
            .try_reserve_exact(1)
            .map_err(|source| GuestMemoryDirtyTrackerError::MetadataAllocationFailed { source })?;
        let region = DirtyRegion::new(range, self.page_size, initially_dirty)?;
        regions.insert(index, region);
        Ok(())
    }

    pub(crate) fn remove_region(&self, range: GuestMemoryRange) -> bool {
        let mut regions = write_unpoisoned(&self.regions);
        let Some(index) = regions.iter().position(|region| region.range == range) else {
            return false;
        };
        regions.remove(index);
        true
    }

    /// Clear the current generation after external quiescence and advance it.
    ///
    /// This method allocates nothing and has no fallible operation. Callers
    /// must exclude all writers across the complete clear.
    pub fn clear_quiesced(&self) -> u64 {
        let regions = read_unpoisoned(&self.regions);
        for region in regions.iter() {
            region.clear();
        }
        drop(regions);
        self.epoch
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                Some(epoch.saturating_add(1))
            })
            .unwrap_or_else(|epoch| epoch)
            .saturating_add(1)
    }
}

impl fmt::Debug for GuestMemoryDirtyTracker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let regions = read_unpoisoned(&self.regions);
        formatter
            .debug_struct("GuestMemoryDirtyTracker")
            .field("page_size", &self.page_size)
            .field("epoch", &self.epoch())
            .field("region_count", &regions.len())
            .finish()
    }
}

struct DirtyRegion {
    range: GuestMemoryRange,
    words: Vec<AtomicU64>,
}

impl DirtyRegion {
    fn new(
        range: GuestMemoryRange,
        page_size: u64,
        initially_dirty: bool,
    ) -> Result<Self, GuestMemoryDirtyTrackerError> {
        range
            .validate_alignment(page_size)
            .map_err(|_| GuestMemoryDirtyTrackerError::UnalignedRegion { range, page_size })?;
        let page_count = usize::try_from(range.size() / page_size)
            .map_err(|_| GuestMemoryDirtyTrackerError::PageCountOverflow { range })?;
        let word_count = page_count
            .checked_add(63)
            .map(|count| count / 64)
            .ok_or(GuestMemoryDirtyTrackerError::PageCountOverflow { range })?;
        let mut words = Vec::new();
        words
            .try_reserve_exact(word_count)
            .map_err(|source| GuestMemoryDirtyTrackerError::MetadataAllocationFailed { source })?;
        for word_index in 0..word_count {
            let value = if initially_dirty {
                initial_word(page_count, word_index)
            } else {
                0
            };
            words.push(AtomicU64::new(value));
        }
        Ok(Self { range, words })
    }

    fn mark_intersection(&self, requested: GuestMemoryRange, page_size: u64) {
        let start = self
            .range
            .start()
            .raw_value()
            .max(requested.start().raw_value());
        let end = self
            .range
            .end_exclusive()
            .raw_value()
            .min(requested.end_exclusive().raw_value());
        if start >= end {
            return;
        }
        let region_start = self.range.start().raw_value();
        let first_page = ((start - region_start) / page_size) as usize;
        let last_page = ((end - 1 - region_start) / page_size) as usize;
        for page_index in first_page..=last_page {
            let (word_index, bit) = bitmap_location(page_index);
            if let Some(word) = self.words.get(word_index) {
                word.fetch_or(bit, Ordering::AcqRel);
            }
        }
    }

    fn append_dirty_pages(
        &self,
        page_size: u64,
        pages: &mut Vec<GuestAddress>,
    ) -> Result<(), GuestMemoryDirtyTrackerAccessError> {
        for (word_index, word) in self.words.iter().enumerate() {
            let mut remaining = word.load(Ordering::Acquire);
            while remaining != 0 {
                let bit_index = remaining.trailing_zeros() as usize;
                let page_index = word_index
                    .checked_mul(64)
                    .and_then(|start| start.checked_add(bit_index))
                    .ok_or(GuestMemoryDirtyTrackerAccessError::InvalidState(
                        "dirty-page index overflowed",
                    ))?;
                let offset = u64::try_from(page_index)
                    .ok()
                    .and_then(|page_index| page_index.checked_mul(page_size))
                    .ok_or(GuestMemoryDirtyTrackerAccessError::InvalidState(
                        "dirty-page offset overflowed",
                    ))?;
                let page = self.range.start().checked_add(offset).ok_or(
                    GuestMemoryDirtyTrackerAccessError::InvalidState(
                        "dirty-page address overflowed",
                    ),
                )?;
                if !self.range.contains(page) {
                    return Err(GuestMemoryDirtyTrackerAccessError::InvalidState(
                        "dirty-page metadata exceeded its region",
                    ));
                }
                pages.try_reserve(1).map_err(|source| {
                    GuestMemoryDirtyTrackerAccessError::MetadataAllocationFailed { source }
                })?;
                pages.push(page);
                remaining &= remaining - 1;
            }
        }
        Ok(())
    }

    fn clear(&self) {
        for word in &self.words {
            word.store(0, Ordering::Release);
        }
    }
}

fn validate_page_size(page_size: u64) -> Result<(), GuestMemoryDirtyTrackerError> {
    if page_size == 0 || !page_size.is_power_of_two() {
        Err(GuestMemoryDirtyTrackerError::InvalidPageSize { page_size })
    } else {
        Ok(())
    }
}

fn validate_insert_position(
    regions: &[DirtyRegion],
    range: GuestMemoryRange,
) -> Result<usize, GuestMemoryDirtyTrackerError> {
    for (index, region) in regions.iter().enumerate() {
        if region.range.overlaps(range) {
            return Err(GuestMemoryDirtyTrackerError::UnorderedOrOverlappingRegion { range });
        }
        if range.start() < region.range.start() {
            return Ok(index);
        }
    }
    Ok(regions.len())
}

fn validate_tracked_range(
    regions: &[DirtyRegion],
    range: GuestMemoryRange,
) -> Result<(), GuestMemoryDirtyTrackerAccessError> {
    let mut current = range.start().raw_value();
    for region in regions {
        if region.range.end_exclusive().raw_value() <= current {
            continue;
        }
        if region.range.start().raw_value() > current {
            return Err(GuestMemoryDirtyTrackerAccessError::UntrackedRange { range });
        }
        current = region
            .range
            .end_exclusive()
            .raw_value()
            .min(range.end_exclusive().raw_value());
        if current == range.end_exclusive().raw_value() {
            return Ok(());
        }
    }
    Err(GuestMemoryDirtyTrackerAccessError::UntrackedRange { range })
}

const fn bitmap_location(page_index: usize) -> (usize, u64) {
    (page_index / 64, 1u64 << (page_index % 64))
}

const fn initial_word(page_count: usize, word_index: usize) -> u64 {
    let word_start = word_index * 64;
    let remaining = page_count.saturating_sub(word_start);
    if remaining >= 64 {
        u64::MAX
    } else if remaining == 0 {
        0
    } else {
        (1u64 << remaining) - 1
    }
}

fn read_unpoisoned<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn write_unpoisoned<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    match lock.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::thread;

    use super::{
        GuestMemoryDirtyTracker, GuestMemoryDirtyTrackerAccessError, GuestMemoryDirtyTrackerError,
    };
    use crate::memory::{GuestAddress, GuestMemoryRange};

    const PAGE_SIZE: u64 = 0x1000;

    fn range(start: u64, pages: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(GuestAddress::new(start), pages * PAGE_SIZE)
            .expect("test dirty range should validate")
    }

    #[test]
    fn marks_exact_intersections_across_adjacent_regions_and_clears_epochs() {
        let tracker = GuestMemoryDirtyTracker::new([range(0x1000, 2), range(0x3000, 2)], PAGE_SIZE)
            .expect("tracker should build");
        tracker
            .mark_range(
                GuestMemoryRange::new(GuestAddress::new(0x1fff), 0x2002)
                    .expect("mark range should validate"),
            )
            .expect("adjacent tracked ranges should mark");
        assert_eq!(
            tracker
                .dirty_pages()
                .expect("dirty query should succeed")
                .into_iter()
                .collect::<BTreeSet<_>>(),
            [0x1000, 0x2000, 0x3000, 0x4000]
                .map(GuestAddress::new)
                .into_iter()
                .collect()
        );
        assert_eq!(tracker.clear_quiesced(), 1);
        assert_eq!(tracker.epoch(), 1);
        assert!(
            tracker
                .dirty_pages()
                .expect("query should succeed")
                .is_empty()
        );
        assert_eq!(tracker.clear_quiesced(), 2);
    }

    #[test]
    fn rejects_holes_and_tracks_inserted_regions_as_dirty() {
        let tracker = GuestMemoryDirtyTracker::new([range(0x1000, 1)], PAGE_SIZE)
            .expect("tracker should build");
        assert!(matches!(
            tracker.mark_range(range(0x1000, 2)),
            Err(GuestMemoryDirtyTrackerAccessError::UntrackedRange { .. })
        ));
        tracker
            .insert_region(range(0x2000, 2), true)
            .expect("adjacent region should insert");
        assert_eq!(
            tracker.dirty_pages().expect("query should succeed"),
            [GuestAddress::new(0x2000), GuestAddress::new(0x3000)]
        );
        assert!(tracker.remove_region(range(0x2000, 2)));
        assert!(!tracker.remove_region(range(0x2000, 2)));
    }

    #[test]
    fn rejects_unordered_or_overlapping_initial_regions() {
        assert!(matches!(
            GuestMemoryDirtyTracker::new([range(0x3000, 1), range(0x1000, 1)], PAGE_SIZE),
            Err(GuestMemoryDirtyTrackerError::UnorderedOrOverlappingRegion { .. })
        ));
        assert!(matches!(
            GuestMemoryDirtyTracker::new([range(0x1000, 2), range(0x2000, 1)], PAGE_SIZE),
            Err(GuestMemoryDirtyTrackerError::UnorderedOrOverlappingRegion { .. })
        ));
    }

    #[test]
    fn concurrent_repeated_marks_publish_one_shared_union() {
        let tracker = Arc::new(
            GuestMemoryDirtyTracker::new([range(0x1000, 64)], PAGE_SIZE)
                .expect("tracker should build"),
        );
        let mut workers = Vec::new();
        for index in 0..8u64 {
            let tracker = Arc::clone(&tracker);
            workers.push(thread::spawn(move || {
                for _ in 0..256 {
                    tracker
                        .mark_range(range(0x1000 + index * PAGE_SIZE, 1))
                        .expect("concurrent mark should succeed");
                }
            }));
        }
        for worker in workers {
            worker.join().expect("mark worker should join");
        }
        assert_eq!(
            tracker.dirty_pages().expect("query should succeed").len(),
            8
        );
    }
}
