use std::collections::TryReserveError;
use std::ffi::c_void;
use std::fmt;
use std::io;
use std::ptr::{self, NonNull};

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

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

#[derive(Debug)]
pub struct GuestMemory {
    regions: Vec<GuestMemoryRegion>,
}

impl GuestMemory {
    pub fn allocate(layout: &GuestMemoryLayout) -> Result<Self, GuestMemoryAllocationError> {
        let page_size = host_page_size()?;
        let mut mapper = SystemAnonymousMapper;

        Self::allocate_with_mapper(layout, page_size, &mut mapper)
    }

    fn allocate_with_mapper(
        layout: &GuestMemoryLayout,
        page_size: u64,
        mapper: &mut impl AnonymousMapper,
    ) -> Result<Self, GuestMemoryAllocationError> {
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
                mapping: mapper.map(host_size)?,
            });
        }

        Ok(Self { regions })
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

    pub fn write_slice(
        &mut self,
        source: &[u8],
        guest_address: GuestAddress,
    ) -> Result<(), GuestMemoryAccessError> {
        let Some(range) = access_range(guest_address, source.len())? else {
            return Ok(());
        };

        self.validate_mapped_range(range)?;

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
                .mapping
                .address()
                .as_ptr()
                .cast::<u8>()
                .wrapping_add(segment.offset);

            // SAFETY: `validate_mapped_range` proved the whole requested guest
            // range is backed by live mappings. `access_segment` bounds this
            // segment to `region`, and the destination pointer is within that
            // mapping. The safe API provides no way to alias `source` with the
            // private anonymous mapping mutably.
            unsafe {
                ptr::copy_nonoverlapping(source_segment.as_ptr(), destination, segment.size);
            }

            remaining = next_remaining;
            current = advance_address(current, segment.size)?;
        }

        Ok(())
    }

    pub fn read_slice(
        &self,
        guest_address: GuestAddress,
        destination: &mut [u8],
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
                .mapping
                .address()
                .as_ptr()
                .cast::<u8>()
                .wrapping_add(segment.offset);

            // SAFETY: `validate_mapped_range` proved the whole requested guest
            // range is backed by live mappings. `access_segment` bounds this
            // segment to `region`, and the source pointer is within that
            // mapping. The destination is a caller-provided mutable slice.
            unsafe {
                ptr::copy_nonoverlapping(source, destination_segment.as_mut_ptr(), segment.size);
            }

            remaining = next_remaining;
            current = advance_address(current, segment.size)?;
        }

        Ok(())
    }

    fn validate_mapped_range(&self, range: GuestMemoryRange) -> Result<(), GuestMemoryAccessError> {
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
    mapping: AnonymousMapping,
}

impl GuestMemoryRegion {
    pub const fn range(&self) -> GuestMemoryRange {
        self.range
    }

    pub fn host_address(&self) -> NonNull<c_void> {
        self.mapping.address()
    }

    pub const fn host_size(&self) -> usize {
        self.mapping.size()
    }
}

impl fmt::Debug for GuestMemoryRegion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuestMemoryRegion")
            .field("range", &self.range)
            .field("host_size", &self.mapping.size())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum GuestMemoryAllocationError {
    InvalidLayout(GuestMemoryError),
    InvalidHostPageSize,
    SizeTooLarge { range: GuestMemoryRange },
    RegionMetadataAllocationFailed { source: TryReserveError },
    AnonymousMmapFailed { size: usize, source: io::Error },
    AnonymousMmapReturnedNull { size: usize },
}

impl fmt::Display for GuestMemoryAllocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLayout(source) => {
                write!(f, "invalid guest memory layout for allocation: {source}")
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
        }
    }
}

impl std::error::Error for GuestMemoryAllocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidLayout(source) => Some(source),
            Self::RegionMetadataAllocationFailed { source } => Some(source),
            Self::AnonymousMmapFailed { source, .. } => Some(source),
            Self::InvalidHostPageSize
            | Self::SizeTooLarge { .. }
            | Self::AnonymousMmapReturnedNull { .. } => None,
        }
    }
}

impl From<GuestMemoryError> for GuestMemoryAllocationError {
    fn from(source: GuestMemoryError) -> Self {
        Self::InvalidLayout(source)
    }
}

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

fn host_page_size() -> Result<u64, GuestMemoryAllocationError> {
    // SAFETY: `sysconf(_SC_PAGESIZE)` has no pointer arguments and does not
    // require process-local invariants from Rust.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size =
        u64::try_from(page_size).map_err(|_| GuestMemoryAllocationError::InvalidHostPageSize)?;

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

trait AnonymousMapper {
    fn map(&mut self, size: usize) -> Result<AnonymousMapping, GuestMemoryAllocationError>;
}

#[derive(Debug)]
struct SystemAnonymousMapper;

impl AnonymousMapper for SystemAnonymousMapper {
    fn map(&mut self, size: usize) -> Result<AnonymousMapping, GuestMemoryAllocationError> {
        AnonymousMapping::map(size)
    }
}

struct AnonymousMapping {
    address: NonNull<c_void>,
    size: usize,
    kind: AnonymousMappingKind,
}

// SAFETY: `AnonymousMapping` owns a process-local mmap region. Moving ownership
// to another thread does not invalidate the mapping, and `munmap` may run from
// any thread when the owner is dropped.
unsafe impl Send for AnonymousMapping {}

// SAFETY: Shared references expose only copyable metadata and a raw pointer.
// Safe Rust cannot mutate the mapped bytes through this type, and unsafe users
// must uphold the usual raw-pointer aliasing and lifetime requirements.
unsafe impl Sync for AnonymousMapping {}

impl AnonymousMapping {
    fn map(size: usize) -> Result<Self, GuestMemoryAllocationError> {
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
            kind: AnonymousMappingKind::System,
        })
    }

    #[cfg(test)]
    fn test_mapping(size: usize, drop_count: Arc<AtomicUsize>) -> Self {
        Self {
            address: NonNull::<u8>::dangling().cast(),
            size,
            kind: AnonymousMappingKind::Test { drop_count },
        }
    }

    const fn address(&self) -> NonNull<c_void> {
        self.address
    }

    const fn size(&self) -> usize {
        self.size
    }
}

impl fmt::Debug for AnonymousMapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnonymousMapping")
            .field("size", &self.size)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
enum AnonymousMappingKind {
    System,
    #[cfg(test)]
    Test {
        drop_count: Arc<AtomicUsize>,
    },
}

impl Drop for AnonymousMapping {
    fn drop(&mut self) {
        match &self.kind {
            AnonymousMappingKind::System => {
                // SAFETY: `AnonymousMapping::map` stores only successful mmap
                // results, and each `AnonymousMapping` owns exactly one mapping.
                unsafe {
                    let _ = libc::munmap(self.address.as_ptr(), self.size);
                }
            }
            #[cfg(test)]
            AnonymousMappingKind::Test { drop_count } => {
                drop_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::{
        AnonymousMapper, AnonymousMapping, GuestAddress, GuestMemory, GuestMemoryAccessError,
        GuestMemoryAllocationError, GuestMemoryError, GuestMemoryLayout, GuestMemoryRange, aarch64,
        host_page_size,
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
            .read_slice(address, &mut destination)
            .expect("guest memory read should succeed");

        assert_eq!(destination, source);
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
            .read_slice(address, &mut destination)
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
            .read_slice(GuestAddress::new(u64::MAX), &mut destination)
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
            .read_slice(address, &mut byte)
            .expect("single-byte read before hole should still succeed");

        assert_eq!(byte, [0]);
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
            .read_slice(address, &mut destination)
            .expect("guest memory read should cross adjacent ranges");

        assert_eq!(destination, source);
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

    #[derive(Debug)]
    struct CountingMapper {
        maps: usize,
        drop_count: Arc<AtomicUsize>,
    }

    impl AnonymousMapper for CountingMapper {
        fn map(&mut self, size: usize) -> Result<AnonymousMapping, GuestMemoryAllocationError> {
            self.maps += 1;
            Ok(AnonymousMapping::test_mapping(
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

    impl AnonymousMapper for FailingMapper {
        fn map(&mut self, size: usize) -> Result<AnonymousMapping, GuestMemoryAllocationError> {
            self.maps += 1;

            if self.maps == self.fail_on {
                return Err(GuestMemoryAllocationError::AnonymousMmapFailed {
                    size,
                    source: io::Error::from_raw_os_error(libc::ENOMEM),
                });
            }

            Ok(AnonymousMapping::test_mapping(
                size,
                Arc::clone(&self.drop_count),
            ))
        }
    }
}
