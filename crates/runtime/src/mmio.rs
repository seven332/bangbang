//! Backend-neutral MMIO region ownership, lookup, and operation metadata.

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

pub const MAX_MMIO_ACCESS_BYTES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioOperationKind {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioAccessBytes {
    bytes: [u8; MAX_MMIO_ACCESS_BYTES],
    len: usize,
}

impl MmioAccessBytes {
    pub fn new(bytes: &[u8]) -> Result<Self, MmioAccessBytesError> {
        validate_mmio_access_byte_len(bytes.len())?;

        let mut copied = [0; MAX_MMIO_ACCESS_BYTES];
        let (destination, _) = copied.split_at_mut(bytes.len());
        destination.copy_from_slice(bytes);

        Ok(Self {
            bytes: copied,
            len: bytes.len(),
        })
    }

    pub fn zeroed(len: usize) -> Result<Self, MmioAccessBytesError> {
        validate_mmio_access_byte_len(len)?;

        Ok(Self {
            bytes: [0; MAX_MMIO_ACCESS_BYTES],
            len,
        })
    }

    pub const fn len(self) -> usize {
        self.len
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        let (bytes, _) = self.bytes.split_at(self.len);
        bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioAccessBytesError {
    Empty,
    UnsupportedLength { len: usize },
    TooLarge { len: usize, max: usize },
}

impl fmt::Display for MmioAccessBytesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("MMIO access bytes cannot be empty"),
            Self::UnsupportedLength { len } => {
                write!(
                    f,
                    "unsupported MMIO access byte length {len}; supported lengths are 1, 2, 4, or 8"
                )
            }
            Self::TooLarge { len, max } => {
                write!(f, "MMIO access byte length {len} exceeds the maximum {max}")
            }
        }
    }
}

impl std::error::Error for MmioAccessBytesError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioOperation {
    Read {
        access: MmioAccess,
        data: MmioAccessBytes,
    },
    Write {
        access: MmioAccess,
        data: MmioAccessBytes,
    },
}

impl MmioOperation {
    pub fn read(access: MmioAccess) -> Result<Self, MmioOperationError> {
        let len = mmio_operation_len(access)?;

        Ok(Self::Read {
            access,
            data: MmioAccessBytes {
                bytes: [0; MAX_MMIO_ACCESS_BYTES],
                len,
            },
        })
    }

    pub fn write(access: MmioAccess, data: MmioAccessBytes) -> Result<Self, MmioOperationError> {
        let expected = mmio_operation_len(access)?;
        if data.len() != expected {
            return Err(MmioOperationError::DataLengthMismatch {
                access,
                expected,
                actual: data.len(),
            });
        }

        Ok(Self::Write { access, data })
    }

    pub const fn kind(self) -> MmioOperationKind {
        match self {
            Self::Read { .. } => MmioOperationKind::Read,
            Self::Write { .. } => MmioOperationKind::Write,
        }
    }

    pub const fn access(self) -> MmioAccess {
        match self {
            Self::Read { access, .. } | Self::Write { access, .. } => access,
        }
    }

    pub const fn data(self) -> MmioAccessBytes {
        match self {
            Self::Read { data, .. } | Self::Write { data, .. } => data,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioOperationError {
    UnsupportedAccessSize {
        access: MmioAccess,
        size: u64,
    },
    DataLengthMismatch {
        access: MmioAccess,
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for MmioOperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAccessSize { access, size } => {
                write!(
                    f,
                    "unsupported MMIO operation size {size} for range {}; supported sizes are 1, 2, 4, or 8 bytes",
                    access.range()
                )
            }
            Self::DataLengthMismatch {
                access,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "MMIO operation data length {actual} does not match access size {expected} for range {}",
                    access.range()
                )
            }
        }
    }
}

impl std::error::Error for MmioOperationError {}

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

fn validate_mmio_access_byte_len(len: usize) -> Result<(), MmioAccessBytesError> {
    match len {
        0 => Err(MmioAccessBytesError::Empty),
        1 | 2 | 4 | MAX_MMIO_ACCESS_BYTES => Ok(()),
        len if len > MAX_MMIO_ACCESS_BYTES => Err(MmioAccessBytesError::TooLarge {
            len,
            max: MAX_MMIO_ACCESS_BYTES,
        }),
        len => Err(MmioAccessBytesError::UnsupportedLength { len }),
    }
}

fn mmio_operation_len(access: MmioAccess) -> Result<usize, MmioOperationError> {
    match access.range().size() {
        1 => Ok(1),
        2 => Ok(2),
        4 => Ok(4),
        8 => Ok(MAX_MMIO_ACCESS_BYTES),
        size => Err(MmioOperationError::UnsupportedAccessSize { access, size }),
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::{
        MAX_MMIO_ACCESS_BYTES, MmioAccess, MmioAccessBytes, MmioAccessBytesError, MmioBus,
        MmioBusError, MmioOperation, MmioOperationError, MmioOperationKind, MmioRegion,
        MmioRegionId,
    };
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

    fn lookup_access(start: u64, size: u64) -> MmioAccess {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, start, 0x100);
        lookup(&bus, start, size)
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
    fn region_creation_rejects_one_byte_at_max_address() {
        let err = MmioRegion::new(id(1), address(u64::MAX), 1)
            .expect_err("end-exclusive max-address range should fail");

        assert_eq!(
            err,
            GuestMemoryError::AddressOverflow {
                start: address(u64::MAX),
                size: 1
            }
        );
    }

    #[test]
    fn mmio_bus_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<MmioBus>();
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
    fn lookup_succeeds_for_whole_region() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let access = lookup(&bus, 0x1000, 0x100);

        assert_eq!(access.region_id(), id(1));
        assert_eq!(access.range(), range(0x1000, 0x100));
        assert_eq!(access.offset(), 0);
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
    fn access_bytes_copy_from_slice_preserves_data() {
        let mut source = [1, 2, 3, 4];
        let bytes = MmioAccessBytes::new(&source).expect("access bytes should be valid");

        source.fill(0xff);

        assert_eq!(bytes.len(), 4);
        assert!(!bytes.is_empty());
        assert_eq!(bytes.as_slice(), &[1, 2, 3, 4]);
    }

    #[test]
    fn access_bytes_zeroed_returns_requested_length() {
        let bytes =
            MmioAccessBytes::zeroed(MAX_MMIO_ACCESS_BYTES).expect("zeroed bytes should be valid");

        assert_eq!(bytes.len(), MAX_MMIO_ACCESS_BYTES);
        assert_eq!(bytes.as_slice(), &[0; MAX_MMIO_ACCESS_BYTES]);
    }

    #[test]
    fn access_bytes_reject_empty_data() {
        assert_eq!(
            MmioAccessBytes::new(&[]).expect_err("empty bytes should fail"),
            MmioAccessBytesError::Empty
        );
        assert_eq!(
            MmioAccessBytes::zeroed(0).expect_err("zeroed empty bytes should fail"),
            MmioAccessBytesError::Empty
        );
    }

    #[test]
    fn access_bytes_reject_unsupported_length() {
        assert_eq!(
            MmioAccessBytes::new(&[1, 2, 3]).expect_err("three-byte data should fail"),
            MmioAccessBytesError::UnsupportedLength { len: 3 }
        );
        assert_eq!(
            MmioAccessBytes::zeroed(3).expect_err("three-byte zeroed data should fail"),
            MmioAccessBytesError::UnsupportedLength { len: 3 }
        );
    }

    #[test]
    fn access_bytes_reject_oversized_data() {
        assert_eq!(
            MmioAccessBytes::new(&[0; MAX_MMIO_ACCESS_BYTES + 1])
                .expect_err("oversized bytes should fail"),
            MmioAccessBytesError::TooLarge {
                len: MAX_MMIO_ACCESS_BYTES + 1,
                max: MAX_MMIO_ACCESS_BYTES
            }
        );
        assert_eq!(
            MmioAccessBytes::zeroed(MAX_MMIO_ACCESS_BYTES + 1)
                .expect_err("oversized zeroed bytes should fail"),
            MmioAccessBytesError::TooLarge {
                len: MAX_MMIO_ACCESS_BYTES + 1,
                max: MAX_MMIO_ACCESS_BYTES
            }
        );
    }

    #[test]
    fn read_operation_creates_zeroed_data_for_access_size() {
        let access = lookup_access(0x2000, 4);
        let operation = MmioOperation::read(access).expect("read operation should be valid");

        assert_eq!(operation.kind(), MmioOperationKind::Read);
        assert_eq!(operation.access(), access);
        assert_eq!(operation.data().len(), 4);
        assert_eq!(operation.data().as_slice(), &[0, 0, 0, 0]);
    }

    #[test]
    fn write_operation_preserves_access_and_data() {
        let access = lookup_access(0x3000, 4);
        let data = MmioAccessBytes::new(&[1, 2, 3, 4]).expect("write data should be valid");
        let operation =
            MmioOperation::write(access, data).expect("write operation should be valid");

        assert_eq!(operation.kind(), MmioOperationKind::Write);
        assert_eq!(operation.access(), access);
        assert_eq!(operation.data(), data);
    }

    #[test]
    fn write_operation_rejects_data_length_mismatch() {
        let access = lookup_access(0x4000, 4);
        let data = MmioAccessBytes::new(&[1, 2]).expect("write data should be valid");
        let err =
            MmioOperation::write(access, data).expect_err("mismatched data length should fail");

        assert_eq!(
            err,
            MmioOperationError::DataLengthMismatch {
                access,
                expected: 4,
                actual: 2
            }
        );
    }

    #[test]
    fn operation_rejects_unsupported_access_size() {
        let access = lookup_access(0x5000, 3);
        let err = MmioOperation::read(access).expect_err("three-byte access should fail");

        assert_eq!(
            err,
            MmioOperationError::UnsupportedAccessSize { access, size: 3 }
        );
    }

    #[test]
    fn operation_rejects_access_larger_than_maximum() {
        let access = lookup_access(0x6000, 16);
        let err = MmioOperation::read(access).expect_err("oversized access should fail");

        assert_eq!(
            err,
            MmioOperationError::UnsupportedAccessSize { access, size: 16 }
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

    #[test]
    fn displays_access_bytes_errors() {
        let err = MmioAccessBytesError::TooLarge { len: 9, max: 8 };

        assert_eq!(
            err.to_string(),
            "MMIO access byte length 9 exceeds the maximum 8"
        );
        assert!(err.source().is_none());
    }

    #[test]
    fn displays_operation_errors() {
        let access = lookup_access(0x7000, 4);
        let err = MmioOperationError::DataLengthMismatch {
            access,
            expected: 4,
            actual: 2,
        };

        assert_eq!(
            err.to_string(),
            "MMIO operation data length 2 does not match access size 4 for range [0x7000..0x7004) (4 bytes)"
        );
        assert!(err.source().is_none());
    }
}
