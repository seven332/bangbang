use std::fmt;

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
}

fn validate_alignment(alignment: u64) -> Result<(), GuestMemoryError> {
    if alignment == 0 || !alignment.is_power_of_two() {
        Err(GuestMemoryError::InvalidAlignment { alignment })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{GuestAddress, GuestMemoryError, GuestMemoryLayout, GuestMemoryRange, aarch64};

    const PAGE_SIZE: u64 = 4096;

    fn range(start: u64, size: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(GuestAddress::new(start), size)
            .expect("range should be valid for test")
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
}
