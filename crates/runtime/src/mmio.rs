//! Backend-neutral MMIO region ownership and lookup.

use std::fmt;

use crate::memory::{GuestAddress, GuestMemoryError, GuestMemoryRange};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MmioRegionId(u64);

impl MmioRegionId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn raw_value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for MmioRegionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioRegion {
    id: MmioRegionId,
    range: GuestMemoryRange,
}

impl MmioRegion {
    pub fn new(id: MmioRegionId, start: GuestAddress, size: u64) -> Result<Self, GuestMemoryError> {
        Ok(Self {
            id,
            range: GuestMemoryRange::new(start, size)?,
        })
    }

    pub const fn id(self) -> MmioRegionId {
        self.id
    }

    pub const fn range(self) -> GuestMemoryRange {
        self.range
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioAccess {
    region: MmioRegion,
    range: GuestMemoryRange,
    offset: u64,
}

impl MmioAccess {
    pub const fn region(self) -> MmioRegion {
        self.region
    }

    pub const fn range(self) -> GuestMemoryRange {
        self.range
    }

    pub const fn offset(self) -> u64 {
        self.offset
    }

    pub const fn region_id(self) -> MmioRegionId {
        self.region.id()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmioBus {
    regions: Vec<MmioRegion>,
}

impl MmioBus {
    pub const fn new() -> Self {
        Self {
            regions: Vec::new(),
        }
    }

    pub fn regions(&self) -> &[MmioRegion] {
        &self.regions
    }

    pub fn insert(
        &mut self,
        id: MmioRegionId,
        start: GuestAddress,
        size: u64,
    ) -> Result<MmioRegion, MmioBusError> {
        let region = MmioRegion::new(id, start, size).map_err(|source| {
            MmioBusError::InvalidRegionRange {
                start,
                size,
                source,
            }
        })?;
        let insertion_index = self
            .regions
            .partition_point(|existing| existing.range().start() < region.range().start());

        if let Some(existing) = insertion_index
            .checked_sub(1)
            .and_then(|previous_index| self.regions.get(previous_index).copied())
        {
            reject_overlap(existing, region)?;
        }

        if let Some(existing) = self.regions.get(insertion_index).copied() {
            reject_overlap(existing, region)?;
        }

        self.regions.insert(insertion_index, region);

        Ok(region)
    }

    pub fn lookup(&self, start: GuestAddress, size: u64) -> Result<MmioAccess, MmioBusError> {
        let access_range = GuestMemoryRange::new(start, size).map_err(|source| {
            MmioBusError::InvalidAccessRange {
                start,
                size,
                source,
            }
        })?;
        let candidate_index = self
            .regions
            .partition_point(|region| region.range().start() <= access_range.start());
        let Some(candidate_index) = candidate_index.checked_sub(1) else {
            return Err(MmioBusError::UnownedAccess {
                range: access_range,
            });
        };
        let Some(region) = self.regions.get(candidate_index).copied() else {
            return Err(MmioBusError::UnownedAccess {
                range: access_range,
            });
        };

        if !range_contains_range(region.range(), access_range) {
            return Err(MmioBusError::UnownedAccess {
                range: access_range,
            });
        }

        Ok(MmioAccess {
            region,
            range: access_range,
            offset: access_range.start().raw_value() - region.range().start().raw_value(),
        })
    }
}

impl Default for MmioBus {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioBusError {
    InvalidRegionRange {
        start: GuestAddress,
        size: u64,
        source: GuestMemoryError,
    },
    InvalidAccessRange {
        start: GuestAddress,
        size: u64,
        source: GuestMemoryError,
    },
    OverlappingRegion {
        existing: MmioRegion,
        new: MmioRegion,
    },
    UnownedAccess {
        range: GuestMemoryRange,
    },
}

impl fmt::Display for MmioBusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRegionRange {
                start,
                size,
                source,
            } => {
                write!(
                    f,
                    "invalid MMIO region range at {start} with size {size}: {source}"
                )
            }
            Self::InvalidAccessRange {
                start,
                size,
                source,
            } => {
                write!(
                    f,
                    "invalid MMIO access range at {start} with size {size}: {source}"
                )
            }
            Self::OverlappingRegion { existing, new } => {
                write!(
                    f,
                    "MMIO region id={} range {} overlaps existing id={} range {}",
                    new.id(),
                    new.range(),
                    existing.id(),
                    existing.range()
                )
            }
            Self::UnownedAccess { range } => {
                write!(f, "MMIO access range {range} is not owned by any region")
            }
        }
    }
}

impl std::error::Error for MmioBusError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRegionRange { source, .. } | Self::InvalidAccessRange { source, .. } => {
                Some(source)
            }
            Self::OverlappingRegion { .. } | Self::UnownedAccess { .. } => None,
        }
    }
}

fn reject_overlap(existing: MmioRegion, new: MmioRegion) -> Result<(), MmioBusError> {
    if existing.range().overlaps(new.range()) {
        Err(MmioBusError::OverlappingRegion { existing, new })
    } else {
        Ok(())
    }
}

const fn range_contains_range(container: GuestMemoryRange, candidate: GuestMemoryRange) -> bool {
    container.start().raw_value() <= candidate.start().raw_value()
        && candidate.end_exclusive().raw_value() <= container.end_exclusive().raw_value()
}

#[cfg(test)]
mod tests {
    use super::{MmioAccess, MmioBus, MmioBusError, MmioRegion, MmioRegionId};
    use crate::memory::{GuestAddress, GuestMemoryError, GuestMemoryRange};

    fn id(value: u64) -> MmioRegionId {
        MmioRegionId::new(value)
    }

    fn address(value: u64) -> GuestAddress {
        GuestAddress::new(value)
    }

    fn range(start: u64, size: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(address(start), size).expect("test range should be valid")
    }

    fn region(id_value: u64, start: u64, size: u64) -> MmioRegion {
        MmioRegion::new(id(id_value), address(start), size).expect("test region should be valid")
    }

    fn insert(bus: &mut MmioBus, id_value: u64, start: u64, size: u64) -> MmioRegion {
        bus.insert(id(id_value), address(start), size)
            .expect("test region insert should succeed")
    }

    fn lookup(bus: &MmioBus, start: u64, size: u64) -> MmioAccess {
        bus.lookup(address(start), size)
            .expect("test lookup should succeed")
    }

    #[test]
    fn region_creation_rejects_zero_size() {
        let err =
            MmioRegion::new(id(1), address(0x1000), 0).expect_err("zero-sized region should fail");

        assert_eq!(
            err,
            GuestMemoryError::EmptyRange {
                start: address(0x1000)
            }
        );
    }

    #[test]
    fn region_creation_rejects_overflow() {
        let err = MmioRegion::new(id(1), address(u64::MAX), 2)
            .expect_err("overflowing region should fail");

        assert_eq!(
            err,
            GuestMemoryError::AddressOverflow {
                start: address(u64::MAX),
                size: 2
            }
        );
    }

    #[test]
    fn insertion_keeps_regions_sorted() {
        let mut bus = MmioBus::new();

        let high = insert(&mut bus, 2, 0x3000, 0x100);
        let low = insert(&mut bus, 1, 0x1000, 0x100);
        let middle = insert(&mut bus, 3, 0x2000, 0x100);

        assert_eq!(bus.regions(), &[low, middle, high]);
    }

    #[test]
    fn insertion_accepts_adjacent_regions() {
        let mut bus = MmioBus::new();

        let first = insert(&mut bus, 1, 0x1000, 0x100);
        let second = insert(&mut bus, 2, 0x1100, 0x100);
        let before = insert(&mut bus, 3, 0x0f00, 0x100);

        assert_eq!(bus.regions(), &[before, first, second]);
    }

    #[test]
    fn insertion_accepts_same_owner_for_multiple_regions() {
        let mut bus = MmioBus::new();

        let first = insert(&mut bus, 1, 0x1000, 0x100);
        let second = insert(&mut bus, 1, 0x2000, 0x100);

        assert_eq!(bus.regions(), &[first, second]);
        assert_eq!(lookup(&bus, 0x1000, 4).region_id(), id(1));
        assert_eq!(lookup(&bus, 0x2000, 4).region_id(), id(1));
    }

    #[test]
    fn insertion_rejects_exact_duplicate() {
        let mut bus = MmioBus::new();
        let existing = insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .insert(id(2), address(0x1000), 0x100)
            .expect_err("duplicate region should fail");

        assert_eq!(
            err,
            MmioBusError::OverlappingRegion {
                existing,
                new: region(2, 0x1000, 0x100)
            }
        );
    }

    #[test]
    fn insertion_rejects_start_overlap() {
        let mut bus = MmioBus::new();
        let existing = insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .insert(id(2), address(0x1080), 0x100)
            .expect_err("start overlap should fail");

        assert_eq!(
            err,
            MmioBusError::OverlappingRegion {
                existing,
                new: region(2, 0x1080, 0x100)
            }
        );
    }

    #[test]
    fn insertion_rejects_end_overlap() {
        let mut bus = MmioBus::new();
        let existing = insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .insert(id(2), address(0x0f80), 0x100)
            .expect_err("end overlap should fail");

        assert_eq!(
            err,
            MmioBusError::OverlappingRegion {
                existing,
                new: region(2, 0x0f80, 0x100)
            }
        );
    }

    #[test]
    fn insertion_rejects_contained_region() {
        let mut bus = MmioBus::new();
        let existing = insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .insert(id(2), address(0x1040), 0x20)
            .expect_err("contained region should fail");

        assert_eq!(
            err,
            MmioBusError::OverlappingRegion {
                existing,
                new: region(2, 0x1040, 0x20)
            }
        );
    }

    #[test]
    fn insertion_rejects_containing_region() {
        let mut bus = MmioBus::new();
        let existing = insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .insert(id(2), address(0x0f00), 0x300)
            .expect_err("containing region should fail");

        assert_eq!(
            err,
            MmioBusError::OverlappingRegion {
                existing,
                new: region(2, 0x0f00, 0x300)
            }
        );
    }

    #[test]
    fn insertion_reports_invalid_region_range() {
        let mut bus = MmioBus::new();

        let err = bus
            .insert(id(1), address(u64::MAX), 2)
            .expect_err("invalid insert range should fail");

        assert_eq!(
            err,
            MmioBusError::InvalidRegionRange {
                start: address(u64::MAX),
                size: 2,
                source: GuestMemoryError::AddressOverflow {
                    start: address(u64::MAX),
                    size: 2
                }
            }
        );
    }

    #[test]
    fn lookup_rejects_empty_bus() {
        let bus = MmioBus::new();

        let err = bus
            .lookup(address(0x1000), 4)
            .expect_err("empty bus lookup should fail");

        assert_eq!(
            err,
            MmioBusError::UnownedAccess {
                range: range(0x1000, 4)
            }
        );
    }

    #[test]
    fn lookup_returns_owner_and_offset() {
        let mut bus = MmioBus::new();
        let registered = insert(&mut bus, 7, 0x1000, 0x100);

        assert_eq!(
            lookup(&bus, 0x1040, 4),
            MmioAccess {
                region: registered,
                range: range(0x1040, 4),
                offset: 0x40
            }
        );
    }

    #[test]
    fn lookup_succeeds_for_first_byte() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let access = lookup(&bus, 0x1000, 1);

        assert_eq!(access.region_id(), id(1));
        assert_eq!(access.offset(), 0);
    }

    #[test]
    fn lookup_succeeds_for_last_byte() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let access = lookup(&bus, 0x10ff, 1);

        assert_eq!(access.region_id(), id(1));
        assert_eq!(access.offset(), 0xff);
    }

    #[test]
    fn lookup_succeeds_for_exact_end_contained_access() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let access = lookup(&bus, 0x10fc, 4);

        assert_eq!(access.range(), range(0x10fc, 4));
        assert_eq!(access.offset(), 0xfc);
    }

    #[test]
    fn lookup_rejects_hole_before_first_region() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .lookup(address(0x0fff), 1)
            .expect_err("hole before first region should fail");

        assert_eq!(
            err,
            MmioBusError::UnownedAccess {
                range: range(0x0fff, 1)
            }
        );
    }

    #[test]
    fn lookup_rejects_hole_between_regions() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);
        insert(&mut bus, 2, 0x1200, 0x100);

        let err = bus
            .lookup(address(0x1100), 1)
            .expect_err("hole between regions should fail");

        assert_eq!(
            err,
            MmioBusError::UnownedAccess {
                range: range(0x1100, 1)
            }
        );
    }

    #[test]
    fn lookup_rejects_hole_after_last_region() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .lookup(address(0x1100), 1)
            .expect_err("hole after last region should fail");

        assert_eq!(
            err,
            MmioBusError::UnownedAccess {
                range: range(0x1100, 1)
            }
        );
    }

    #[test]
    fn lookup_rejects_access_ending_one_byte_past_region() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .lookup(address(0x10fc), 5)
            .expect_err("access ending past region should fail");

        assert_eq!(
            err,
            MmioBusError::UnownedAccess {
                range: range(0x10fc, 5)
            }
        );
    }

    #[test]
    fn lookup_rejects_access_crossing_adjacent_regions() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);
        insert(&mut bus, 2, 0x1100, 0x100);

        let err = bus
            .lookup(address(0x10ff), 2)
            .expect_err("cross-region access should fail");

        assert_eq!(
            err,
            MmioBusError::UnownedAccess {
                range: range(0x10ff, 2)
            }
        );
    }

    #[test]
    fn lookup_rejects_zero_sized_access() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .lookup(address(0x1000), 0)
            .expect_err("zero-sized access should fail");

        assert_eq!(
            err,
            MmioBusError::InvalidAccessRange {
                start: address(0x1000),
                size: 0,
                source: GuestMemoryError::EmptyRange {
                    start: address(0x1000)
                }
            }
        );
    }

    #[test]
    fn lookup_rejects_access_overflow() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, u64::MAX - 4, 4);

        let err = bus
            .lookup(address(u64::MAX - 1), 4)
            .expect_err("overflowing access should fail");

        assert_eq!(
            err,
            MmioBusError::InvalidAccessRange {
                start: address(u64::MAX - 1),
                size: 4,
                source: GuestMemoryError::AddressOverflow {
                    start: address(u64::MAX - 1),
                    size: 4
                }
            }
        );
    }

    #[test]
    fn displays_mmio_bus_errors() {
        let err = MmioBusError::UnownedAccess {
            range: range(0x1000, 4),
        };

        assert_eq!(
            err.to_string(),
            "MMIO access range [0x1000..0x1004) (4 bytes) is not owned by any region"
        );
    }
}
