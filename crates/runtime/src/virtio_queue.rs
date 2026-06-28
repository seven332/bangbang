//! Backend-neutral virtqueue descriptor-chain parsing.

use std::collections::TryReserveError;
use std::fmt;

use crate::memory::{GuestAddress, GuestMemory, GuestMemoryAccessError};

pub const VIRTQUEUE_DESCRIPTOR_SIZE: usize = 16;
pub const VIRTQUEUE_DESCRIPTOR_ALIGNMENT: u64 = 16;
pub const VIRTQUEUE_DESC_F_NEXT: u16 = 0x1;
pub const VIRTQUEUE_DESC_F_WRITE: u16 = 0x2;
pub const VIRTQUEUE_DESC_F_INDIRECT: u16 = 0x4;

const VIRTQUEUE_DESCRIPTOR_SIZE_U64: u64 = 16;
const DESCRIPTOR_ADDR_SIZE: usize = 8;
const DESCRIPTOR_LEN_SIZE: usize = 4;
const DESCRIPTOR_FLAGS_SIZE: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtqueueDescriptorFlags(u16);

impl VirtqueueDescriptorFlags {
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    pub const fn raw_value(self) -> u16 {
        self.0
    }

    pub const fn has_next(self) -> bool {
        self.0 & VIRTQUEUE_DESC_F_NEXT != 0
    }

    pub const fn is_write_only(self) -> bool {
        self.0 & VIRTQUEUE_DESC_F_WRITE != 0
    }

    pub const fn is_indirect(self) -> bool {
        self.0 & VIRTQUEUE_DESC_F_INDIRECT != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtqueueDescriptor {
    index: u16,
    address: GuestAddress,
    len: u32,
    flags: VirtqueueDescriptorFlags,
    next_index: Option<u16>,
}

impl VirtqueueDescriptor {
    pub const fn index(self) -> u16 {
        self.index
    }

    pub const fn address(self) -> GuestAddress {
        self.address
    }

    pub const fn len(self) -> u32 {
        self.len
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub const fn flags(self) -> VirtqueueDescriptorFlags {
        self.flags
    }

    pub const fn has_next(self) -> bool {
        self.next_index.is_some()
    }

    pub const fn next_index(self) -> Option<u16> {
        self.next_index
    }

    pub const fn is_write_only(self) -> bool {
        self.flags.is_write_only()
    }

    pub const fn is_indirect(self) -> bool {
        self.flags.is_indirect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtqueueDescriptorChain {
    descriptors: Vec<VirtqueueDescriptor>,
}

impl VirtqueueDescriptorChain {
    pub fn descriptors(&self) -> &[VirtqueueDescriptor] {
        &self.descriptors
    }

    pub fn len(&self) -> usize {
        self.descriptors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.descriptors.is_empty()
    }
}

pub fn read_descriptor_chain(
    memory: &GuestMemory,
    descriptor_table: GuestAddress,
    queue_size: u16,
    head_index: u16,
) -> Result<VirtqueueDescriptorChain, VirtqueueDescriptorChainError> {
    validate_queue_size(queue_size)?;
    validate_descriptor_table_alignment(descriptor_table)?;
    validate_descriptor_table_range(descriptor_table, queue_size)?;
    validate_descriptor_index(head_index, queue_size).map_err(|_| {
        VirtqueueDescriptorChainError::InvalidHeadIndex {
            head_index,
            queue_size,
        }
    })?;

    let mut descriptors = Vec::new();
    descriptors
        .try_reserve_exact(usize::from(queue_size))
        .map_err(
            |source| VirtqueueDescriptorChainError::DescriptorChainAllocationFailed {
                queue_size,
                source,
            },
        )?;

    let mut current_index = head_index;
    for _ in 0..queue_size {
        let descriptor = read_descriptor(memory, descriptor_table, queue_size, current_index)?;
        if descriptor.is_indirect() {
            return Err(
                VirtqueueDescriptorChainError::UnsupportedIndirectDescriptor {
                    index: descriptor.index(),
                },
            );
        }

        let next_index = descriptor.next_index();
        descriptors.push(descriptor);

        match next_index {
            Some(index) => {
                validate_descriptor_index(index, queue_size).map_err(|_| {
                    VirtqueueDescriptorChainError::InvalidNextIndex {
                        index: current_index,
                        next_index: index,
                        queue_size,
                    }
                })?;
                current_index = index;
            }
            None => return Ok(VirtqueueDescriptorChain { descriptors }),
        }
    }

    Err(VirtqueueDescriptorChainError::DescriptorChainTooLong {
        head_index,
        queue_size,
    })
}

#[derive(Debug)]
pub enum VirtqueueDescriptorChainError {
    InvalidQueueSize {
        queue_size: u16,
    },
    UnalignedDescriptorTable {
        descriptor_table: GuestAddress,
        alignment: u64,
    },
    InvalidHeadIndex {
        head_index: u16,
        queue_size: u16,
    },
    DescriptorTableRangeOverflow {
        descriptor_table: GuestAddress,
        queue_size: u16,
    },
    DescriptorRead {
        index: u16,
        source: GuestMemoryAccessError,
    },
    InvalidNextIndex {
        index: u16,
        next_index: u16,
        queue_size: u16,
    },
    UnsupportedIndirectDescriptor {
        index: u16,
    },
    DescriptorChainTooLong {
        head_index: u16,
        queue_size: u16,
    },
    DescriptorChainAllocationFailed {
        queue_size: u16,
        source: TryReserveError,
    },
}

impl fmt::Display for VirtqueueDescriptorChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidQueueSize { queue_size } => {
                write!(
                    f,
                    "virtqueue size {queue_size} must be a nonzero power of two"
                )
            }
            Self::UnalignedDescriptorTable {
                descriptor_table,
                alignment,
            } => {
                write!(
                    f,
                    "virtqueue descriptor table address {descriptor_table} is not aligned to {alignment} bytes"
                )
            }
            Self::InvalidHeadIndex {
                head_index,
                queue_size,
            } => {
                write!(
                    f,
                    "virtqueue descriptor head index {head_index} is outside queue size {queue_size}"
                )
            }
            Self::DescriptorTableRangeOverflow {
                descriptor_table,
                queue_size,
            } => {
                write!(
                    f,
                    "virtqueue descriptor table address {descriptor_table} with queue size {queue_size} overflows address space"
                )
            }
            Self::DescriptorRead { index, source } => {
                write!(f, "failed to read virtqueue descriptor {index}: {source}")
            }
            Self::InvalidNextIndex {
                index,
                next_index,
                queue_size,
            } => {
                write!(
                    f,
                    "virtqueue descriptor {index} next index {next_index} is outside queue size {queue_size}"
                )
            }
            Self::UnsupportedIndirectDescriptor { index } => {
                write!(
                    f,
                    "virtqueue descriptor {index} uses unsupported indirect descriptors"
                )
            }
            Self::DescriptorChainTooLong {
                head_index,
                queue_size,
            } => {
                write!(
                    f,
                    "virtqueue descriptor chain starting at {head_index} did not terminate within queue size {queue_size}"
                )
            }
            Self::DescriptorChainAllocationFailed { queue_size, source } => {
                write!(
                    f,
                    "failed to reserve virtqueue descriptor chain storage for queue size {queue_size}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtqueueDescriptorChainError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DescriptorRead { source, .. } => Some(source),
            Self::DescriptorChainAllocationFailed { source, .. } => Some(source),
            Self::InvalidQueueSize { .. }
            | Self::UnalignedDescriptorTable { .. }
            | Self::InvalidHeadIndex { .. }
            | Self::DescriptorTableRangeOverflow { .. }
            | Self::InvalidNextIndex { .. }
            | Self::UnsupportedIndirectDescriptor { .. }
            | Self::DescriptorChainTooLong { .. } => None,
        }
    }
}

fn validate_queue_size(queue_size: u16) -> Result<(), VirtqueueDescriptorChainError> {
    if queue_size == 0 || !queue_size.is_power_of_two() {
        Err(VirtqueueDescriptorChainError::InvalidQueueSize { queue_size })
    } else {
        Ok(())
    }
}

fn validate_descriptor_table_alignment(
    descriptor_table: GuestAddress,
) -> Result<(), VirtqueueDescriptorChainError> {
    if descriptor_table
        .raw_value()
        .is_multiple_of(VIRTQUEUE_DESCRIPTOR_ALIGNMENT)
    {
        Ok(())
    } else {
        Err(VirtqueueDescriptorChainError::UnalignedDescriptorTable {
            descriptor_table,
            alignment: VIRTQUEUE_DESCRIPTOR_ALIGNMENT,
        })
    }
}

fn validate_descriptor_table_range(
    descriptor_table: GuestAddress,
    queue_size: u16,
) -> Result<(), VirtqueueDescriptorChainError> {
    let table_size = u64::from(queue_size) * VIRTQUEUE_DESCRIPTOR_SIZE_U64;
    descriptor_table.checked_add(table_size).ok_or(
        VirtqueueDescriptorChainError::DescriptorTableRangeOverflow {
            descriptor_table,
            queue_size,
        },
    )?;
    Ok(())
}

fn validate_descriptor_index(
    index: u16,
    queue_size: u16,
) -> Result<(), VirtqueueDescriptorChainError> {
    if index < queue_size {
        Ok(())
    } else {
        Err(VirtqueueDescriptorChainError::InvalidNextIndex {
            index,
            next_index: index,
            queue_size,
        })
    }
}

fn read_descriptor(
    memory: &GuestMemory,
    descriptor_table: GuestAddress,
    queue_size: u16,
    index: u16,
) -> Result<VirtqueueDescriptor, VirtqueueDescriptorChainError> {
    let descriptor_address = descriptor_address(descriptor_table, queue_size, index)?;
    let raw = read_raw_descriptor(memory, descriptor_address, index)?;
    let address = GuestAddress::new(raw.address);
    let len = raw.len;
    let flags = VirtqueueDescriptorFlags::new(raw.flags);
    let next_index = if flags.has_next() {
        Some(raw.next)
    } else {
        None
    };

    Ok(VirtqueueDescriptor {
        index,
        address,
        len,
        flags,
        next_index,
    })
}

fn descriptor_address(
    descriptor_table: GuestAddress,
    queue_size: u16,
    index: u16,
) -> Result<GuestAddress, VirtqueueDescriptorChainError> {
    let offset = u64::from(index) * VIRTQUEUE_DESCRIPTOR_SIZE_U64;
    descriptor_table.checked_add(offset).ok_or(
        VirtqueueDescriptorChainError::DescriptorTableRangeOverflow {
            descriptor_table,
            queue_size,
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RawVirtqueueDescriptor {
    address: u64,
    len: u32,
    flags: u16,
    next: u16,
}

fn read_raw_descriptor(
    memory: &GuestMemory,
    guest_address: GuestAddress,
    index: u16,
) -> Result<RawVirtqueueDescriptor, VirtqueueDescriptorChainError> {
    let mut bytes = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
    memory
        .read_slice(&mut bytes, guest_address)
        .map_err(|source| VirtqueueDescriptorChainError::DescriptorRead { index, source })?;
    Ok(parse_raw_descriptor(bytes))
}

fn parse_raw_descriptor(bytes: [u8; VIRTQUEUE_DESCRIPTOR_SIZE]) -> RawVirtqueueDescriptor {
    let (address_bytes, tail) = bytes.split_at(DESCRIPTOR_ADDR_SIZE);
    let (len_bytes, tail) = tail.split_at(DESCRIPTOR_LEN_SIZE);
    let (flags_bytes, next_bytes) = tail.split_at(DESCRIPTOR_FLAGS_SIZE);

    let mut address = [0; DESCRIPTOR_ADDR_SIZE];
    address.copy_from_slice(address_bytes);
    let mut len = [0; DESCRIPTOR_LEN_SIZE];
    len.copy_from_slice(len_bytes);
    let mut flags = [0; DESCRIPTOR_FLAGS_SIZE];
    flags.copy_from_slice(flags_bytes);
    let mut next = [0; DESCRIPTOR_FLAGS_SIZE];
    next.copy_from_slice(next_bytes);

    RawVirtqueueDescriptor {
        address: u64::from_le_bytes(address),
        len: u32::from_le_bytes(len),
        flags: u16::from_le_bytes(flags),
        next: u16::from_le_bytes(next),
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::{
        VIRTQUEUE_DESC_F_INDIRECT, VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE,
        VIRTQUEUE_DESCRIPTOR_ALIGNMENT, VIRTQUEUE_DESCRIPTOR_SIZE, VIRTQUEUE_DESCRIPTOR_SIZE_U64,
        VirtqueueDescriptorChainError, VirtqueueDescriptorFlags, read_descriptor_chain,
    };
    use crate::memory::{
        GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryLayout, GuestMemoryRange,
    };

    const TABLE: GuestAddress = GuestAddress::new(0x1000);

    fn guest_memory(size: u64) -> GuestMemory {
        let range = GuestMemoryRange::new(GuestAddress::new(0), size)
            .expect("test memory range should be valid");
        let layout =
            GuestMemoryLayout::new(vec![range]).expect("test memory layout should be valid");

        GuestMemory::allocate(&layout).expect("test memory should allocate")
    }

    fn write_descriptor(
        memory: &mut GuestMemory,
        table: GuestAddress,
        index: u16,
        address: u64,
        len: u32,
        flags: u16,
        next: u16,
    ) {
        let descriptor = table
            .checked_add(u64::from(index) * VIRTQUEUE_DESCRIPTOR_SIZE_U64)
            .expect("test descriptor address should not overflow");
        memory
            .write_slice(&address.to_le_bytes(), descriptor)
            .expect("descriptor address field should write");
        memory
            .write_slice(
                &len.to_le_bytes(),
                descriptor
                    .checked_add(8)
                    .expect("descriptor len address should not overflow"),
            )
            .expect("descriptor len field should write");
        memory
            .write_slice(
                &flags.to_le_bytes(),
                descriptor
                    .checked_add(12)
                    .expect("descriptor flags address should not overflow"),
            )
            .expect("descriptor flags field should write");
        memory
            .write_slice(
                &next.to_le_bytes(),
                descriptor
                    .checked_add(14)
                    .expect("descriptor next address should not overflow"),
            )
            .expect("descriptor next field should write");
    }

    #[test]
    fn exposes_virtqueue_descriptor_constants() {
        assert_eq!(VIRTQUEUE_DESCRIPTOR_SIZE, 16);
        assert_eq!(VIRTQUEUE_DESCRIPTOR_ALIGNMENT, 16);
        assert_eq!(VIRTQUEUE_DESC_F_NEXT, 0x1);
        assert_eq!(VIRTQUEUE_DESC_F_WRITE, 0x2);
        assert_eq!(VIRTQUEUE_DESC_F_INDIRECT, 0x4);
    }

    #[test]
    fn descriptor_flags_expose_known_bits() {
        let flags = VirtqueueDescriptorFlags::new(VIRTQUEUE_DESC_F_NEXT | VIRTQUEUE_DESC_F_WRITE);

        assert_eq!(
            flags.raw_value(),
            VIRTQUEUE_DESC_F_NEXT | VIRTQUEUE_DESC_F_WRITE
        );
        assert!(flags.has_next());
        assert!(flags.is_write_only());
        assert!(!flags.is_indirect());
    }

    #[test]
    fn reads_single_descriptor_chain() {
        let mut memory = guest_memory(0x4000);
        write_descriptor(
            &mut memory,
            TABLE,
            0,
            0x2000,
            0x40,
            VIRTQUEUE_DESC_F_WRITE,
            0,
        );

        let chain = read_descriptor_chain(&memory, TABLE, 8, 0)
            .expect("single descriptor chain should read");

        assert_eq!(chain.len(), 1);
        assert!(!chain.is_empty());
        let descriptor = chain.descriptors()[0];
        assert_eq!(descriptor.index(), 0);
        assert_eq!(descriptor.address(), GuestAddress::new(0x2000));
        assert_eq!(descriptor.len(), 0x40);
        assert!(!descriptor.is_empty());
        assert!(descriptor.is_write_only());
        assert!(!descriptor.has_next());
        assert_eq!(descriptor.next_index(), None);
    }

    #[test]
    fn reads_linked_descriptor_chain() {
        let mut memory = guest_memory(0x4000);
        write_descriptor(
            &mut memory,
            TABLE,
            0,
            0x2000,
            0x20,
            VIRTQUEUE_DESC_F_NEXT,
            1,
        );
        write_descriptor(&mut memory, TABLE, 1, 0x3000, 0, VIRTQUEUE_DESC_F_WRITE, 0);

        let chain = read_descriptor_chain(&memory, TABLE, 8, 0)
            .expect("linked descriptor chain should read");

        assert_eq!(chain.len(), 2);
        assert_eq!(chain.descriptors()[0].next_index(), Some(1));
        assert!(!chain.descriptors()[0].is_write_only());
        assert!(chain.descriptors()[1].is_write_only());
        assert!(chain.descriptors()[1].is_empty());
    }

    #[test]
    fn rejects_invalid_queue_sizes() {
        let memory = guest_memory(0x4000);

        assert!(matches!(
            read_descriptor_chain(&memory, TABLE, 0, 0),
            Err(VirtqueueDescriptorChainError::InvalidQueueSize { queue_size: 0 })
        ));
        assert!(matches!(
            read_descriptor_chain(&memory, TABLE, 3, 0),
            Err(VirtqueueDescriptorChainError::InvalidQueueSize { queue_size: 3 })
        ));
    }

    #[test]
    fn rejects_unaligned_descriptor_table() {
        let memory = guest_memory(0x4000);
        let table = GuestAddress::new(0x1001);

        let err = read_descriptor_chain(&memory, table, 8, 0)
            .expect_err("unaligned descriptor table should fail");

        assert!(matches!(
            err,
            VirtqueueDescriptorChainError::UnalignedDescriptorTable {
                descriptor_table,
                alignment: VIRTQUEUE_DESCRIPTOR_ALIGNMENT
            } if descriptor_table == table
        ));
    }

    #[test]
    fn rejects_head_index_outside_queue() {
        let memory = guest_memory(0x4000);

        let err = read_descriptor_chain(&memory, TABLE, 8, 8)
            .expect_err("out of range head index should fail");

        assert!(matches!(
            err,
            VirtqueueDescriptorChainError::InvalidHeadIndex {
                head_index: 8,
                queue_size: 8
            }
        ));
    }

    #[test]
    fn rejects_invalid_next_index() {
        let mut memory = guest_memory(0x4000);
        write_descriptor(
            &mut memory,
            TABLE,
            0,
            0x2000,
            0x20,
            VIRTQUEUE_DESC_F_NEXT,
            8,
        );

        let err = read_descriptor_chain(&memory, TABLE, 8, 0)
            .expect_err("out of range next index should fail");

        assert!(matches!(
            err,
            VirtqueueDescriptorChainError::InvalidNextIndex {
                index: 0,
                next_index: 8,
                queue_size: 8
            }
        ));
    }

    #[test]
    fn rejects_descriptor_table_range_overflow() {
        let memory = guest_memory(0x4000);
        let table = GuestAddress::new(u64::MAX - 15);

        let err = read_descriptor_chain(&memory, table, 2, 1)
            .expect_err("descriptor table range overflow should fail");

        assert!(matches!(
            err,
            VirtqueueDescriptorChainError::DescriptorTableRangeOverflow {
                descriptor_table,
                queue_size: 2
            } if descriptor_table == table
        ));
    }

    #[test]
    fn rejects_unmapped_descriptor_read() {
        let memory = guest_memory(0x4000);

        let err = read_descriptor_chain(&memory, GuestAddress::new(0x4000), 1, 0)
            .expect_err("descriptor table outside mapped memory should fail");

        match err {
            VirtqueueDescriptorChainError::DescriptorRead { index, source } => {
                assert_eq!(index, 0);
                assert!(matches!(
                    source,
                    GuestMemoryAccessError::UnmappedRange { .. }
                ));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn rejects_unsupported_indirect_descriptor() {
        let mut memory = guest_memory(0x4000);
        write_descriptor(
            &mut memory,
            TABLE,
            0,
            0x2000,
            0x40,
            VIRTQUEUE_DESC_F_INDIRECT,
            0,
        );

        let err = read_descriptor_chain(&memory, TABLE, 8, 0)
            .expect_err("indirect descriptor should fail");

        assert!(matches!(
            err,
            VirtqueueDescriptorChainError::UnsupportedIndirectDescriptor { index: 0 }
        ));
    }

    #[test]
    fn rejects_unsupported_indirect_descriptor_before_invalid_next_index() {
        let mut memory = guest_memory(0x4000);
        write_descriptor(
            &mut memory,
            TABLE,
            0,
            0x2000,
            0x40,
            VIRTQUEUE_DESC_F_INDIRECT | VIRTQUEUE_DESC_F_NEXT,
            8,
        );

        let err = read_descriptor_chain(&memory, TABLE, 8, 0)
            .expect_err("indirect descriptor should fail before next validation");

        assert!(matches!(
            err,
            VirtqueueDescriptorChainError::UnsupportedIndirectDescriptor { index: 0 }
        ));
    }

    #[test]
    fn rejects_descriptor_chain_cycle() {
        let mut memory = guest_memory(0x4000);
        write_descriptor(
            &mut memory,
            TABLE,
            0,
            0x2000,
            0x20,
            VIRTQUEUE_DESC_F_NEXT,
            1,
        );
        write_descriptor(
            &mut memory,
            TABLE,
            1,
            0x3000,
            0x20,
            VIRTQUEUE_DESC_F_NEXT,
            0,
        );

        let err = read_descriptor_chain(&memory, TABLE, 2, 0)
            .expect_err("cyclic descriptor chain should fail");

        assert!(matches!(
            err,
            VirtqueueDescriptorChainError::DescriptorChainTooLong {
                head_index: 0,
                queue_size: 2
            }
        ));
    }

    #[test]
    fn displays_errors_and_preserves_sources() {
        let err = VirtqueueDescriptorChainError::InvalidNextIndex {
            index: 1,
            next_index: 8,
            queue_size: 8,
        };
        assert_eq!(
            err.to_string(),
            "virtqueue descriptor 1 next index 8 is outside queue size 8"
        );
        assert!(err.source().is_none());

        let memory = guest_memory(0x4000);
        let read_err = read_descriptor_chain(&memory, GuestAddress::new(0x4000), 1, 0)
            .expect_err("unmapped descriptor should fail");
        assert!(read_err.source().is_some());
    }
}
