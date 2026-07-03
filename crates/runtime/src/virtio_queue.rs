//! Backend-neutral virtqueue descriptor-chain and ring helpers.

use std::collections::TryReserveError;
use std::fmt;
use std::sync::atomic::{Ordering, fence};

use crate::memory::{GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryRange};

pub const VIRTQUEUE_DESCRIPTOR_SIZE: usize = 16;
pub const VIRTQUEUE_DESCRIPTOR_ALIGNMENT: u64 = 16;
pub const VIRTQUEUE_AVAILABLE_RING_ALIGNMENT: u64 = 2;
pub const VIRTQUEUE_USED_RING_ALIGNMENT: u64 = 4;
pub const VIRTQUEUE_DESC_F_NEXT: u16 = 0x1;
pub const VIRTQUEUE_DESC_F_WRITE: u16 = 0x2;
pub const VIRTQUEUE_DESC_F_INDIRECT: u16 = 0x4;

const VIRTQUEUE_DESCRIPTOR_SIZE_U64: u64 = 16;
const VIRTQUEUE_AVAILABLE_RING_HEADER_SIZE_U64: u64 = 4;
const VIRTQUEUE_AVAILABLE_RING_ENTRY_SIZE_U64: u64 = 2;
const VIRTQUEUE_AVAILABLE_RING_USED_EVENT_SIZE_U64: u64 = 2;
const VIRTQUEUE_AVAILABLE_RING_IDX_OFFSET: u64 = 2;
const VIRTQUEUE_AVAILABLE_RING_RING_OFFSET: u64 = 4;
const VIRTQUEUE_USED_RING_HEADER_SIZE_U64: u64 = 4;
const VIRTQUEUE_USED_RING_ELEMENT_SIZE_U64: u64 = 8;
const VIRTQUEUE_USED_RING_AVAIL_EVENT_SIZE_U64: u64 = 2;
const VIRTQUEUE_USED_RING_IDX_OFFSET: u64 = 2;
const VIRTQUEUE_USED_RING_RING_OFFSET: u64 = 4;
const DESCRIPTOR_ADDR_SIZE: usize = 8;
const DESCRIPTOR_LEN_SIZE: usize = 4;
const DESCRIPTOR_FLAGS_SIZE: usize = 2;
const U16_FIELD_SIZE: usize = 2;
const U32_FIELD_SIZE: usize = 4;

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
    validate_descriptor_table_range(memory, descriptor_table, queue_size)?;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtqueueAvailableRing {
    descriptor_table: GuestAddress,
    available_ring: GuestAddress,
    queue_size: u16,
    next_avail: u16,
}

impl VirtqueueAvailableRing {
    pub fn new(
        descriptor_table: GuestAddress,
        available_ring: GuestAddress,
        queue_size: u16,
    ) -> Result<Self, VirtqueueAvailableRingError> {
        Self::with_next_avail(descriptor_table, available_ring, queue_size, 0)
    }

    pub fn with_next_avail(
        descriptor_table: GuestAddress,
        available_ring: GuestAddress,
        queue_size: u16,
        next_avail: u16,
    ) -> Result<Self, VirtqueueAvailableRingError> {
        validate_available_ring_queue_size(queue_size)?;
        validate_available_ring_descriptor_table_alignment(descriptor_table)?;
        validate_available_ring_alignment(available_ring)?;
        available_ring_size(available_ring, queue_size)?;

        Ok(Self {
            descriptor_table,
            available_ring,
            queue_size,
            next_avail,
        })
    }

    pub const fn descriptor_table(&self) -> GuestAddress {
        self.descriptor_table
    }

    pub const fn available_ring(&self) -> GuestAddress {
        self.available_ring
    }

    pub const fn queue_size(&self) -> u16 {
        self.queue_size
    }

    pub const fn next_avail(&self) -> u16 {
        self.next_avail
    }

    pub fn used_event(&self, memory: &GuestMemory) -> Result<u16, VirtqueueAvailableRingError> {
        validate_available_ring_range(memory, self.available_ring, self.queue_size)?;

        let used_event_address =
            available_ring_used_event_address(self.available_ring, self.queue_size)?;
        read_available_ring_u16(
            memory,
            self.available_ring,
            self.queue_size,
            used_event_address,
        )
    }

    pub fn pop_descriptor_chain(
        &mut self,
        memory: &GuestMemory,
    ) -> Result<Option<VirtqueueDescriptorChain>, VirtqueueAvailableRingError> {
        validate_descriptor_table_range(memory, self.descriptor_table, self.queue_size)
            .map_err(|source| VirtqueueAvailableRingError::DescriptorTable { source })?;
        validate_available_ring_range(memory, self.available_ring, self.queue_size)?;

        let available_index_address = available_ring_offset_address(
            self.available_ring,
            self.queue_size,
            VIRTQUEUE_AVAILABLE_RING_IDX_OFFSET,
        )?;
        let available_index = read_available_ring_u16(
            memory,
            self.available_ring,
            self.queue_size,
            available_index_address,
        )?;
        let available_len = available_index.wrapping_sub(self.next_avail);

        if available_len > self.queue_size {
            return Err(VirtqueueAvailableRingError::AvailableRingLengthTooLarge {
                queue_size: self.queue_size,
                available_len,
            });
        }

        if available_len == 0 {
            return Ok(None);
        }

        // Match Firecracker's ordering point between the observed available
        // index and the selected ring-entry read.
        fence(Ordering::Acquire);

        let ring_index = self.next_avail % self.queue_size;
        let head_address =
            available_ring_entry_address(self.available_ring, self.queue_size, ring_index)?;
        let head_index =
            read_available_ring_u16(memory, self.available_ring, self.queue_size, head_address)?;
        let chain =
            read_descriptor_chain(memory, self.descriptor_table, self.queue_size, head_index)
                .map_err(|source| VirtqueueAvailableRingError::DescriptorChain {
                    head_index,
                    source,
                })?;

        self.next_avail = self.next_avail.wrapping_add(1);

        Ok(Some(chain))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtqueueNotificationSuppression {
    Disabled,
    EventIdx { used_event: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtqueueUsedRingPublication {
    needs_queue_interrupt: bool,
}

impl VirtqueueUsedRingPublication {
    pub const fn needs_queue_interrupt(self) -> bool {
        self.needs_queue_interrupt
    }
}

pub const fn virtqueue_event_idx_needs_notification(
    old_used: u16,
    new_used: u16,
    used_event: u16,
) -> bool {
    new_used.wrapping_sub(used_event).wrapping_sub(1) < new_used.wrapping_sub(old_used)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtqueueUsedRing {
    used_ring: GuestAddress,
    queue_size: u16,
    next_used: u16,
}

impl VirtqueueUsedRing {
    pub fn new(used_ring: GuestAddress, queue_size: u16) -> Result<Self, VirtqueueUsedRingError> {
        Self::with_next_used(used_ring, queue_size, 0)
    }

    pub fn with_next_used(
        used_ring: GuestAddress,
        queue_size: u16,
        next_used: u16,
    ) -> Result<Self, VirtqueueUsedRingError> {
        validate_used_ring_queue_size(queue_size)?;
        validate_used_ring_alignment(used_ring)?;
        used_ring_size(used_ring, queue_size)?;

        Ok(Self {
            used_ring,
            queue_size,
            next_used,
        })
    }

    pub const fn used_ring(&self) -> GuestAddress {
        self.used_ring
    }

    pub const fn queue_size(&self) -> u16 {
        self.queue_size
    }

    pub const fn next_used(&self) -> u16 {
        self.next_used
    }

    pub fn publish_used_element(
        &mut self,
        memory: &mut GuestMemory,
        descriptor_head: u16,
        len: u32,
    ) -> Result<(), VirtqueueUsedRingError> {
        self.publish_used_element_with_notification(
            memory,
            descriptor_head,
            len,
            VirtqueueNotificationSuppression::Disabled,
        )
        .map(|_| ())
    }

    pub fn publish_used_element_with_notification(
        &mut self,
        memory: &mut GuestMemory,
        descriptor_head: u16,
        len: u32,
        notification_suppression: VirtqueueNotificationSuppression,
    ) -> Result<VirtqueueUsedRingPublication, VirtqueueUsedRingError> {
        validate_used_ring_descriptor_head(descriptor_head, self.queue_size)?;
        validate_used_ring_range(memory, self.used_ring, self.queue_size)?;

        let old_used = self.next_used;
        let ring_index = self.next_used % self.queue_size;
        let entry_address = used_ring_entry_address(self.used_ring, self.queue_size, ring_index)?;
        let next_used = self.next_used.wrapping_add(1);

        write_used_ring_element(
            memory,
            self.used_ring,
            self.queue_size,
            entry_address,
            descriptor_head,
            len,
        )?;

        // Match Firecracker's ordering point before making a used entry
        // visible by advancing UsedRing.idx.
        fence(Ordering::Release);

        let index_address = used_ring_offset_address(
            self.used_ring,
            self.queue_size,
            VIRTQUEUE_USED_RING_IDX_OFFSET,
        )?;
        write_used_ring_u16(
            memory,
            self.used_ring,
            self.queue_size,
            index_address,
            next_used,
        )?;

        self.next_used = next_used;

        Ok(VirtqueueUsedRingPublication {
            needs_queue_interrupt: match notification_suppression {
                VirtqueueNotificationSuppression::Disabled => true,
                VirtqueueNotificationSuppression::EventIdx { used_event } => {
                    virtqueue_event_idx_needs_notification(old_used, next_used, used_event)
                }
            },
        })
    }
}

#[derive(Debug)]
pub enum VirtqueueUsedRingError {
    InvalidQueueSize {
        queue_size: u16,
    },
    UnalignedUsedRing {
        used_ring: GuestAddress,
        alignment: u64,
    },
    InvalidDescriptorHead {
        descriptor_head: u16,
        queue_size: u16,
    },
    UsedRingRangeOverflow {
        used_ring: GuestAddress,
        queue_size: u16,
    },
    UsedRingAccess {
        used_ring: GuestAddress,
        queue_size: u16,
        source: GuestMemoryAccessError,
    },
}

impl fmt::Display for VirtqueueUsedRingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidQueueSize { queue_size } => {
                write!(
                    f,
                    "virtqueue size {queue_size} must be a nonzero power of two"
                )
            }
            Self::UnalignedUsedRing {
                used_ring,
                alignment,
            } => {
                write!(
                    f,
                    "virtqueue used ring address {used_ring} is not aligned to {alignment} bytes"
                )
            }
            Self::InvalidDescriptorHead {
                descriptor_head,
                queue_size,
            } => {
                write!(
                    f,
                    "virtqueue used descriptor head {descriptor_head} is outside queue size {queue_size}"
                )
            }
            Self::UsedRingRangeOverflow {
                used_ring,
                queue_size,
            } => {
                write!(
                    f,
                    "virtqueue used ring address {used_ring} with queue size {queue_size} overflows address space"
                )
            }
            Self::UsedRingAccess {
                used_ring,
                queue_size,
                source,
            } => {
                write!(
                    f,
                    "virtqueue used ring address {used_ring} with queue size {queue_size} is not fully mapped: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtqueueUsedRingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UsedRingAccess { source, .. } => Some(source),
            Self::InvalidQueueSize { .. }
            | Self::UnalignedUsedRing { .. }
            | Self::InvalidDescriptorHead { .. }
            | Self::UsedRingRangeOverflow { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum VirtqueueAvailableRingError {
    InvalidQueueSize {
        queue_size: u16,
    },
    UnalignedDescriptorTable {
        descriptor_table: GuestAddress,
        alignment: u64,
    },
    UnalignedAvailableRing {
        available_ring: GuestAddress,
        alignment: u64,
    },
    AvailableRingRangeOverflow {
        available_ring: GuestAddress,
        queue_size: u16,
    },
    AvailableRingAccess {
        available_ring: GuestAddress,
        queue_size: u16,
        source: GuestMemoryAccessError,
    },
    AvailableRingLengthTooLarge {
        queue_size: u16,
        available_len: u16,
    },
    DescriptorTable {
        source: VirtqueueDescriptorChainError,
    },
    DescriptorChain {
        head_index: u16,
        source: VirtqueueDescriptorChainError,
    },
}

impl fmt::Display for VirtqueueAvailableRingError {
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
            Self::UnalignedAvailableRing {
                available_ring,
                alignment,
            } => {
                write!(
                    f,
                    "virtqueue available ring address {available_ring} is not aligned to {alignment} bytes"
                )
            }
            Self::AvailableRingRangeOverflow {
                available_ring,
                queue_size,
            } => {
                write!(
                    f,
                    "virtqueue available ring address {available_ring} with queue size {queue_size} overflows address space"
                )
            }
            Self::AvailableRingAccess {
                available_ring,
                queue_size,
                source,
            } => {
                write!(
                    f,
                    "virtqueue available ring address {available_ring} with queue size {queue_size} is not fully mapped: {source}"
                )
            }
            Self::AvailableRingLengthTooLarge {
                queue_size,
                available_len,
            } => {
                write!(
                    f,
                    "virtqueue available ring reports {available_len} descriptors, exceeding queue size {queue_size}"
                )
            }
            Self::DescriptorTable { source } => {
                write!(
                    f,
                    "virtqueue descriptor table is invalid before available-ring read: {source}"
                )
            }
            Self::DescriptorChain { head_index, source } => {
                write!(
                    f,
                    "failed to read virtqueue descriptor chain from available head {head_index}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtqueueAvailableRingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRingAccess { source, .. } => Some(source),
            Self::DescriptorTable { source } | Self::DescriptorChain { source, .. } => Some(source),
            Self::InvalidQueueSize { .. }
            | Self::UnalignedDescriptorTable { .. }
            | Self::UnalignedAvailableRing { .. }
            | Self::AvailableRingRangeOverflow { .. }
            | Self::AvailableRingLengthTooLarge { .. } => None,
        }
    }
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
    DescriptorTableAccess {
        descriptor_table: GuestAddress,
        queue_size: u16,
        source: GuestMemoryAccessError,
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
            Self::DescriptorTableAccess {
                descriptor_table,
                queue_size,
                source,
            } => {
                write!(
                    f,
                    "virtqueue descriptor table address {descriptor_table} with queue size {queue_size} is not fully mapped: {source}"
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
            Self::DescriptorTableAccess { source, .. } => Some(source),
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

fn is_valid_queue_size(queue_size: u16) -> bool {
    queue_size != 0 && queue_size.is_power_of_two()
}

fn validate_queue_size(queue_size: u16) -> Result<(), VirtqueueDescriptorChainError> {
    if is_valid_queue_size(queue_size) {
        Ok(())
    } else {
        Err(VirtqueueDescriptorChainError::InvalidQueueSize { queue_size })
    }
}

fn validate_available_ring_queue_size(queue_size: u16) -> Result<(), VirtqueueAvailableRingError> {
    if is_valid_queue_size(queue_size) {
        Ok(())
    } else {
        Err(VirtqueueAvailableRingError::InvalidQueueSize { queue_size })
    }
}

fn validate_used_ring_queue_size(queue_size: u16) -> Result<(), VirtqueueUsedRingError> {
    if is_valid_queue_size(queue_size) {
        Ok(())
    } else {
        Err(VirtqueueUsedRingError::InvalidQueueSize { queue_size })
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

fn validate_available_ring_descriptor_table_alignment(
    descriptor_table: GuestAddress,
) -> Result<(), VirtqueueAvailableRingError> {
    if descriptor_table
        .raw_value()
        .is_multiple_of(VIRTQUEUE_DESCRIPTOR_ALIGNMENT)
    {
        Ok(())
    } else {
        Err(VirtqueueAvailableRingError::UnalignedDescriptorTable {
            descriptor_table,
            alignment: VIRTQUEUE_DESCRIPTOR_ALIGNMENT,
        })
    }
}

fn validate_available_ring_alignment(
    available_ring: GuestAddress,
) -> Result<(), VirtqueueAvailableRingError> {
    if available_ring
        .raw_value()
        .is_multiple_of(VIRTQUEUE_AVAILABLE_RING_ALIGNMENT)
    {
        Ok(())
    } else {
        Err(VirtqueueAvailableRingError::UnalignedAvailableRing {
            available_ring,
            alignment: VIRTQUEUE_AVAILABLE_RING_ALIGNMENT,
        })
    }
}

fn validate_used_ring_alignment(used_ring: GuestAddress) -> Result<(), VirtqueueUsedRingError> {
    if used_ring
        .raw_value()
        .is_multiple_of(VIRTQUEUE_USED_RING_ALIGNMENT)
    {
        Ok(())
    } else {
        Err(VirtqueueUsedRingError::UnalignedUsedRing {
            used_ring,
            alignment: VIRTQUEUE_USED_RING_ALIGNMENT,
        })
    }
}

fn validate_descriptor_table_range(
    memory: &GuestMemory,
    descriptor_table: GuestAddress,
    queue_size: u16,
) -> Result<(), VirtqueueDescriptorChainError> {
    let table_size = u64::from(queue_size) * VIRTQUEUE_DESCRIPTOR_SIZE_U64;
    let table_range = GuestMemoryRange::new(descriptor_table, table_size).map_err(|_| {
        VirtqueueDescriptorChainError::DescriptorTableRangeOverflow {
            descriptor_table,
            queue_size,
        }
    })?;

    memory
        .validate_mapped_range(table_range)
        .map_err(
            |source| VirtqueueDescriptorChainError::DescriptorTableAccess {
                descriptor_table,
                queue_size,
                source,
            },
        )?;

    Ok(())
}

fn validate_available_ring_range(
    memory: &GuestMemory,
    available_ring: GuestAddress,
    queue_size: u16,
) -> Result<(), VirtqueueAvailableRingError> {
    let ring_size = available_ring_size(available_ring, queue_size)?;
    let ring_range = GuestMemoryRange::new(available_ring, ring_size).map_err(|_| {
        VirtqueueAvailableRingError::AvailableRingRangeOverflow {
            available_ring,
            queue_size,
        }
    })?;

    memory.validate_mapped_range(ring_range).map_err(|source| {
        VirtqueueAvailableRingError::AvailableRingAccess {
            available_ring,
            queue_size,
            source,
        }
    })?;

    Ok(())
}

fn validate_used_ring_range(
    memory: &GuestMemory,
    used_ring: GuestAddress,
    queue_size: u16,
) -> Result<(), VirtqueueUsedRingError> {
    let ring_size = used_ring_size(used_ring, queue_size)?;
    let ring_range = GuestMemoryRange::new(used_ring, ring_size).map_err(|_| {
        VirtqueueUsedRingError::UsedRingRangeOverflow {
            used_ring,
            queue_size,
        }
    })?;

    memory.validate_mapped_range(ring_range).map_err(|source| {
        VirtqueueUsedRingError::UsedRingAccess {
            used_ring,
            queue_size,
            source,
        }
    })?;

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

fn validate_used_ring_descriptor_head(
    descriptor_head: u16,
    queue_size: u16,
) -> Result<(), VirtqueueUsedRingError> {
    if descriptor_head < queue_size {
        Ok(())
    } else {
        Err(VirtqueueUsedRingError::InvalidDescriptorHead {
            descriptor_head,
            queue_size,
        })
    }
}

fn available_ring_size(
    available_ring: GuestAddress,
    queue_size: u16,
) -> Result<u64, VirtqueueAvailableRingError> {
    let entry_bytes = u64::from(queue_size)
        .checked_mul(VIRTQUEUE_AVAILABLE_RING_ENTRY_SIZE_U64)
        .ok_or(VirtqueueAvailableRingError::AvailableRingRangeOverflow {
            available_ring,
            queue_size,
        })?;
    let ring_size = VIRTQUEUE_AVAILABLE_RING_HEADER_SIZE_U64
        .checked_add(entry_bytes)
        .and_then(|size| size.checked_add(VIRTQUEUE_AVAILABLE_RING_USED_EVENT_SIZE_U64))
        .ok_or(VirtqueueAvailableRingError::AvailableRingRangeOverflow {
            available_ring,
            queue_size,
        })?;

    available_ring.checked_add(ring_size).ok_or(
        VirtqueueAvailableRingError::AvailableRingRangeOverflow {
            available_ring,
            queue_size,
        },
    )?;

    Ok(ring_size)
}

fn used_ring_size(used_ring: GuestAddress, queue_size: u16) -> Result<u64, VirtqueueUsedRingError> {
    let entry_bytes = u64::from(queue_size)
        .checked_mul(VIRTQUEUE_USED_RING_ELEMENT_SIZE_U64)
        .ok_or(VirtqueueUsedRingError::UsedRingRangeOverflow {
            used_ring,
            queue_size,
        })?;
    let ring_size = VIRTQUEUE_USED_RING_HEADER_SIZE_U64
        .checked_add(entry_bytes)
        .and_then(|size| size.checked_add(VIRTQUEUE_USED_RING_AVAIL_EVENT_SIZE_U64))
        .ok_or(VirtqueueUsedRingError::UsedRingRangeOverflow {
            used_ring,
            queue_size,
        })?;

    used_ring
        .checked_add(ring_size)
        .ok_or(VirtqueueUsedRingError::UsedRingRangeOverflow {
            used_ring,
            queue_size,
        })?;

    Ok(ring_size)
}

fn available_ring_offset_address(
    available_ring: GuestAddress,
    queue_size: u16,
    offset: u64,
) -> Result<GuestAddress, VirtqueueAvailableRingError> {
    available_ring.checked_add(offset).ok_or(
        VirtqueueAvailableRingError::AvailableRingRangeOverflow {
            available_ring,
            queue_size,
        },
    )
}

fn available_ring_entry_address(
    available_ring: GuestAddress,
    queue_size: u16,
    ring_index: u16,
) -> Result<GuestAddress, VirtqueueAvailableRingError> {
    let entry_offset = u64::from(ring_index)
        .checked_mul(VIRTQUEUE_AVAILABLE_RING_ENTRY_SIZE_U64)
        .and_then(|offset| offset.checked_add(VIRTQUEUE_AVAILABLE_RING_RING_OFFSET))
        .ok_or(VirtqueueAvailableRingError::AvailableRingRangeOverflow {
            available_ring,
            queue_size,
        })?;

    available_ring_offset_address(available_ring, queue_size, entry_offset)
}

fn available_ring_used_event_address(
    available_ring: GuestAddress,
    queue_size: u16,
) -> Result<GuestAddress, VirtqueueAvailableRingError> {
    let used_event_offset = u64::from(queue_size)
        .checked_mul(VIRTQUEUE_AVAILABLE_RING_ENTRY_SIZE_U64)
        .and_then(|offset| offset.checked_add(VIRTQUEUE_AVAILABLE_RING_RING_OFFSET))
        .ok_or(VirtqueueAvailableRingError::AvailableRingRangeOverflow {
            available_ring,
            queue_size,
        })?;

    available_ring_offset_address(available_ring, queue_size, used_event_offset)
}

fn used_ring_offset_address(
    used_ring: GuestAddress,
    queue_size: u16,
    offset: u64,
) -> Result<GuestAddress, VirtqueueUsedRingError> {
    used_ring
        .checked_add(offset)
        .ok_or(VirtqueueUsedRingError::UsedRingRangeOverflow {
            used_ring,
            queue_size,
        })
}

fn used_ring_entry_address(
    used_ring: GuestAddress,
    queue_size: u16,
    ring_index: u16,
) -> Result<GuestAddress, VirtqueueUsedRingError> {
    let entry_offset = u64::from(ring_index)
        .checked_mul(VIRTQUEUE_USED_RING_ELEMENT_SIZE_U64)
        .and_then(|offset| offset.checked_add(VIRTQUEUE_USED_RING_RING_OFFSET))
        .ok_or(VirtqueueUsedRingError::UsedRingRangeOverflow {
            used_ring,
            queue_size,
        })?;

    used_ring_offset_address(used_ring, queue_size, entry_offset)
}

fn read_available_ring_u16(
    memory: &GuestMemory,
    available_ring: GuestAddress,
    queue_size: u16,
    address: GuestAddress,
) -> Result<u16, VirtqueueAvailableRingError> {
    read_u16(memory, address).map_err(|source| VirtqueueAvailableRingError::AvailableRingAccess {
        available_ring,
        queue_size,
        source,
    })
}

fn write_used_ring_element(
    memory: &mut GuestMemory,
    used_ring: GuestAddress,
    queue_size: u16,
    address: GuestAddress,
    descriptor_head: u16,
    len: u32,
) -> Result<(), VirtqueueUsedRingError> {
    let mut element = [0; U32_FIELD_SIZE * 2];
    let (id_bytes, len_bytes) = element.split_at_mut(U32_FIELD_SIZE);
    id_bytes.copy_from_slice(&u32::from(descriptor_head).to_le_bytes());
    len_bytes.copy_from_slice(&len.to_le_bytes());

    memory
        .write_slice(&element, address)
        .map_err(|source| VirtqueueUsedRingError::UsedRingAccess {
            used_ring,
            queue_size,
            source,
        })
}

fn write_used_ring_u16(
    memory: &mut GuestMemory,
    used_ring: GuestAddress,
    queue_size: u16,
    address: GuestAddress,
    value: u16,
) -> Result<(), VirtqueueUsedRingError> {
    memory
        .write_slice(&value.to_le_bytes(), address)
        .map_err(|source| VirtqueueUsedRingError::UsedRingAccess {
            used_ring,
            queue_size,
            source,
        })
}

fn read_u16(memory: &GuestMemory, address: GuestAddress) -> Result<u16, GuestMemoryAccessError> {
    let mut bytes = [0; U16_FIELD_SIZE];
    memory.read_slice(&mut bytes, address)?;
    Ok(u16::from_le_bytes(bytes))
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
        VIRTQUEUE_AVAILABLE_RING_ALIGNMENT, VIRTQUEUE_AVAILABLE_RING_ENTRY_SIZE_U64,
        VIRTQUEUE_AVAILABLE_RING_IDX_OFFSET, VIRTQUEUE_AVAILABLE_RING_RING_OFFSET,
        VIRTQUEUE_DESC_F_INDIRECT, VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE,
        VIRTQUEUE_DESCRIPTOR_ALIGNMENT, VIRTQUEUE_DESCRIPTOR_SIZE, VIRTQUEUE_DESCRIPTOR_SIZE_U64,
        VIRTQUEUE_USED_RING_ALIGNMENT, VIRTQUEUE_USED_RING_ELEMENT_SIZE_U64,
        VIRTQUEUE_USED_RING_IDX_OFFSET, VIRTQUEUE_USED_RING_RING_OFFSET, VirtqueueAvailableRing,
        VirtqueueAvailableRingError, VirtqueueDescriptorChainError, VirtqueueDescriptorFlags,
        VirtqueueNotificationSuppression, VirtqueueUsedRing, VirtqueueUsedRingError,
        read_descriptor_chain, virtqueue_event_idx_needs_notification,
    };
    use crate::memory::{
        GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryLayout, GuestMemoryRange,
    };

    const TABLE: GuestAddress = GuestAddress::new(0x1000);
    const AVAIL: GuestAddress = GuestAddress::new(0x2000);
    const USED: GuestAddress = GuestAddress::new(0x2800);

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

    fn write_u16(memory: &mut GuestMemory, address: GuestAddress, value: u16) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("u16 field should write");
    }

    fn read_u16_field(memory: &GuestMemory, address: GuestAddress) -> u16 {
        let mut bytes = [0; 2];
        memory
            .read_slice(&mut bytes, address)
            .expect("u16 field should read");
        u16::from_le_bytes(bytes)
    }

    fn read_u32_field(memory: &GuestMemory, address: GuestAddress) -> u32 {
        let mut bytes = [0; 4];
        memory
            .read_slice(&mut bytes, address)
            .expect("u32 field should read");
        u32::from_le_bytes(bytes)
    }

    fn write_available_index(memory: &mut GuestMemory, available_ring: GuestAddress, index: u16) {
        write_u16(
            memory,
            available_ring
                .checked_add(VIRTQUEUE_AVAILABLE_RING_IDX_OFFSET)
                .expect("available index address should not overflow"),
            index,
        );
    }

    fn write_available_head(
        memory: &mut GuestMemory,
        available_ring: GuestAddress,
        ring_index: u16,
        head_index: u16,
    ) {
        let entry = available_ring
            .checked_add(
                VIRTQUEUE_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * VIRTQUEUE_AVAILABLE_RING_ENTRY_SIZE_U64,
            )
            .expect("available ring entry address should not overflow");
        write_u16(memory, entry, head_index);
    }

    fn available_ring_used_event_address(
        available_ring: GuestAddress,
        queue_size: u16,
    ) -> GuestAddress {
        available_ring
            .checked_add(
                VIRTQUEUE_AVAILABLE_RING_RING_OFFSET
                    + u64::from(queue_size) * VIRTQUEUE_AVAILABLE_RING_ENTRY_SIZE_U64,
            )
            .expect("available ring used_event address should not overflow")
    }

    fn write_available_used_event(
        memory: &mut GuestMemory,
        available_ring: GuestAddress,
        queue_size: u16,
        value: u16,
    ) {
        write_u16(
            memory,
            available_ring_used_event_address(available_ring, queue_size),
            value,
        );
    }

    fn used_ring_entry_address(used_ring: GuestAddress, ring_index: u16) -> GuestAddress {
        used_ring
            .checked_add(
                VIRTQUEUE_USED_RING_RING_OFFSET
                    + u64::from(ring_index) * VIRTQUEUE_USED_RING_ELEMENT_SIZE_U64,
            )
            .expect("used ring entry address should not overflow")
    }

    #[test]
    fn exposes_virtqueue_descriptor_constants() {
        assert_eq!(VIRTQUEUE_DESCRIPTOR_SIZE, 16);
        assert_eq!(VIRTQUEUE_DESCRIPTOR_ALIGNMENT, 16);
        assert_eq!(VIRTQUEUE_AVAILABLE_RING_ALIGNMENT, 2);
        assert_eq!(VIRTQUEUE_USED_RING_ALIGNMENT, 4);
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
    fn available_ring_accessors_expose_queue_state() {
        let queue = VirtqueueAvailableRing::with_next_avail(TABLE, AVAIL, 8, 3)
            .expect("available ring should be valid");

        assert_eq!(queue.descriptor_table(), TABLE);
        assert_eq!(queue.available_ring(), AVAIL);
        assert_eq!(queue.queue_size(), 8);
        assert_eq!(queue.next_avail(), 3);
    }

    #[test]
    fn used_ring_accessors_expose_queue_state() {
        let queue =
            VirtqueueUsedRing::with_next_used(USED, 8, 3).expect("used ring should be valid");

        assert_eq!(queue.used_ring(), USED);
        assert_eq!(queue.queue_size(), 8);
        assert_eq!(queue.next_used(), 3);
    }

    #[test]
    fn reads_available_ring_used_event_trailer() {
        let mut memory = guest_memory(0x4000);
        write_available_used_event(&mut memory, AVAIL, 8, 3);
        let queue =
            VirtqueueAvailableRing::new(TABLE, AVAIL, 8).expect("available ring should be valid");

        let used_event = queue
            .used_event(&memory)
            .expect("used_event trailer should read");

        assert_eq!(used_event, 3);
    }

    #[test]
    fn rejects_unmapped_available_ring_used_event_trailer() {
        let memory = guest_memory(0x4000);
        let available_ring = GuestAddress::new(0x3fec);
        let queue = VirtqueueAvailableRing::new(TABLE, available_ring, 8)
            .expect("available ring should be valid");

        let err = queue
            .used_event(&memory)
            .expect_err("partially unmapped used_event trailer should fail");

        assert!(matches!(
            err,
            VirtqueueAvailableRingError::AvailableRingAccess { .. }
        ));
    }

    #[test]
    fn event_idx_notification_formula_handles_boundaries() {
        assert!(!virtqueue_event_idx_needs_notification(0, 0, 0));
        assert!(virtqueue_event_idx_needs_notification(0, 1, 0));
        assert!(!virtqueue_event_idx_needs_notification(0, 1, 1));
        assert!(!virtqueue_event_idx_needs_notification(10, 11, 9));
        assert!(virtqueue_event_idx_needs_notification(10, 11, 10));
        assert!(virtqueue_event_idx_needs_notification(
            u16::MAX,
            0,
            u16::MAX
        ));
        assert!(!virtqueue_event_idx_needs_notification(u16::MAX, 0, 0));
    }

    #[test]
    fn publish_used_element_reports_event_idx_interrupt_need() {
        let mut memory = guest_memory(0x4000);
        let mut queue =
            VirtqueueUsedRing::new(USED, 8).expect("used ring should be valid for publishing");

        let first = queue
            .publish_used_element_with_notification(
                &mut memory,
                0,
                0,
                VirtqueueNotificationSuppression::EventIdx { used_event: 1 },
            )
            .expect("first used element should publish");
        let second = queue
            .publish_used_element_with_notification(
                &mut memory,
                1,
                0,
                VirtqueueNotificationSuppression::EventIdx { used_event: 1 },
            )
            .expect("second used element should publish");

        assert!(!first.needs_queue_interrupt());
        assert!(second.needs_queue_interrupt());
        assert_eq!(queue.next_used(), 2);
        assert_eq!(
            read_u16_field(&memory, USED.checked_add(2).expect("idx should fit")),
            2
        );
    }

    #[test]
    fn returns_none_for_empty_available_ring_without_advancing() {
        let mut memory = guest_memory(0x4000);
        write_available_index(&mut memory, AVAIL, 0);
        let mut queue =
            VirtqueueAvailableRing::new(TABLE, AVAIL, 8).expect("available ring should be valid");

        let chain = queue
            .pop_descriptor_chain(&memory)
            .expect("empty available ring should not fail");

        assert!(chain.is_none());
        assert_eq!(queue.next_avail(), 0);
    }

    #[test]
    fn pops_available_descriptor_chain_and_advances_next_avail() {
        let mut memory = guest_memory(0x4000);
        write_descriptor(&mut memory, TABLE, 2, 0x3000, 0x40, 0, 0);
        write_available_index(&mut memory, AVAIL, 1);
        write_available_head(&mut memory, AVAIL, 0, 2);
        let mut queue =
            VirtqueueAvailableRing::new(TABLE, AVAIL, 8).expect("available ring should be valid");

        let chain = queue
            .pop_descriptor_chain(&memory)
            .expect("available descriptor chain should pop")
            .expect("available ring should contain one head");

        assert_eq!(chain.len(), 1);
        assert_eq!(chain.descriptors()[0].index(), 2);
        assert_eq!(chain.descriptors()[0].address(), GuestAddress::new(0x3000));
        assert_eq!(queue.next_avail(), 1);
    }

    #[test]
    fn does_not_advance_next_avail_when_available_head_is_malformed() {
        let mut memory = guest_memory(0x4000);
        write_available_index(&mut memory, AVAIL, 1);
        write_available_head(&mut memory, AVAIL, 0, 8);
        let mut queue =
            VirtqueueAvailableRing::new(TABLE, AVAIL, 8).expect("available ring should be valid");

        let err = queue
            .pop_descriptor_chain(&memory)
            .expect_err("out-of-range available head should fail");

        match err {
            VirtqueueAvailableRingError::DescriptorChain { head_index, source } => {
                assert_eq!(head_index, 8);
                assert!(matches!(
                    source,
                    VirtqueueDescriptorChainError::InvalidHeadIndex {
                        head_index: 8,
                        queue_size: 8
                    }
                ));
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(queue.next_avail(), 0);
    }

    #[test]
    fn wraps_next_avail_when_popping_available_descriptor_chain() {
        let mut memory = guest_memory(0x4000);
        write_descriptor(&mut memory, TABLE, 1, 0x3000, 0x40, 0, 0);
        write_available_index(&mut memory, AVAIL, 0);
        write_available_head(&mut memory, AVAIL, 7, 1);
        let mut queue = VirtqueueAvailableRing::with_next_avail(TABLE, AVAIL, 8, u16::MAX)
            .expect("available ring should be valid");

        let chain = queue
            .pop_descriptor_chain(&memory)
            .expect("wrapped available descriptor chain should pop")
            .expect("available ring should contain one head");

        assert_eq!(chain.descriptors()[0].index(), 1);
        assert_eq!(queue.next_avail(), 0);
    }

    #[test]
    fn rejects_available_ring_length_greater_than_queue_size() {
        let mut memory = guest_memory(0x4000);
        write_available_index(&mut memory, AVAIL, 9);
        let mut queue =
            VirtqueueAvailableRing::new(TABLE, AVAIL, 8).expect("available ring should be valid");

        let err = queue
            .pop_descriptor_chain(&memory)
            .expect_err("overfull available ring should fail");

        assert!(matches!(
            err,
            VirtqueueAvailableRingError::AvailableRingLengthTooLarge {
                queue_size: 8,
                available_len: 9
            }
        ));
        assert_eq!(queue.next_avail(), 0);
    }

    #[test]
    fn rejects_invalid_available_ring_queue_sizes() {
        assert!(matches!(
            VirtqueueAvailableRing::new(TABLE, AVAIL, 0),
            Err(VirtqueueAvailableRingError::InvalidQueueSize { queue_size: 0 })
        ));
        assert!(matches!(
            VirtqueueAvailableRing::new(TABLE, AVAIL, 3),
            Err(VirtqueueAvailableRingError::InvalidQueueSize { queue_size: 3 })
        ));
    }

    #[test]
    fn rejects_unaligned_available_ring_descriptor_table() {
        let table = GuestAddress::new(0x1001);

        let err = VirtqueueAvailableRing::new(table, AVAIL, 8)
            .expect_err("unaligned descriptor table should fail");

        assert!(matches!(
            err,
            VirtqueueAvailableRingError::UnalignedDescriptorTable {
                descriptor_table,
                alignment: VIRTQUEUE_DESCRIPTOR_ALIGNMENT
            } if descriptor_table == table
        ));
    }

    #[test]
    fn rejects_unaligned_available_ring_address() {
        let available_ring = GuestAddress::new(0x2001);

        let err = VirtqueueAvailableRing::new(TABLE, available_ring, 8)
            .expect_err("unaligned available ring should fail");

        assert!(matches!(
            err,
            VirtqueueAvailableRingError::UnalignedAvailableRing {
                available_ring: ring,
                alignment: VIRTQUEUE_AVAILABLE_RING_ALIGNMENT
            } if ring == available_ring
        ));
    }

    #[test]
    fn rejects_available_ring_range_overflow() {
        let available_ring = GuestAddress::new(u64::MAX - 5);

        let err = VirtqueueAvailableRing::new(TABLE, available_ring, 1)
            .expect_err("available ring range overflow should fail");

        assert!(matches!(
            err,
            VirtqueueAvailableRingError::AvailableRingRangeOverflow {
                available_ring: ring,
                queue_size: 1
            } if ring == available_ring
        ));
    }

    #[test]
    fn rejects_unmapped_available_ring_before_reading_index() {
        let memory = guest_memory(0x4000);
        let available_ring = GuestAddress::new(0x4000);
        let mut queue = VirtqueueAvailableRing::new(TABLE, available_ring, 1)
            .expect("available ring metadata should be valid");

        let err = queue
            .pop_descriptor_chain(&memory)
            .expect_err("unmapped available ring should fail");

        assert!(matches!(
            err,
            VirtqueueAvailableRingError::AvailableRingAccess {
                available_ring: ring,
                queue_size: 1,
                source: GuestMemoryAccessError::UnmappedRange { .. }
            } if ring == available_ring
        ));
        assert_eq!(queue.next_avail(), 0);
    }

    #[test]
    fn rejects_unmapped_available_ring_descriptor_table_before_reading_index() {
        let mut memory = guest_memory(0x4000);
        let table = GuestAddress::new(0x4000);
        write_available_index(&mut memory, AVAIL, 2);
        let mut queue = VirtqueueAvailableRing::new(table, AVAIL, 1)
            .expect("available ring metadata should be valid");

        let err = queue
            .pop_descriptor_chain(&memory)
            .expect_err("unmapped descriptor table should fail before ring read");

        match err {
            VirtqueueAvailableRingError::DescriptorTable { source } => {
                assert!(matches!(
                    source,
                    VirtqueueDescriptorChainError::DescriptorTableAccess {
                        descriptor_table,
                        queue_size: 1,
                        source: GuestMemoryAccessError::UnmappedRange { .. }
                    } if descriptor_table == table
                ));
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(queue.next_avail(), 0);
    }

    #[test]
    fn rejects_partially_mapped_available_ring_before_reading_index() {
        let memory = guest_memory(0x4000);
        let available_ring = GuestAddress::new(0x3ffa);
        let mut queue = VirtqueueAvailableRing::new(TABLE, available_ring, 1)
            .expect("available ring metadata should be valid");

        let err = queue
            .pop_descriptor_chain(&memory)
            .expect_err("partially mapped available ring should fail");

        assert!(matches!(
            err,
            VirtqueueAvailableRingError::AvailableRingAccess {
                available_ring: ring,
                queue_size: 1,
                source: GuestMemoryAccessError::UnmappedRange { .. }
            } if ring == available_ring
        ));
    }

    #[test]
    fn accepts_available_ring_ending_at_memory_boundary() {
        let mut memory = guest_memory(0x4000);
        let available_ring = GuestAddress::new(0x3ff8);
        write_descriptor(&mut memory, TABLE, 0, 0x3000, 0x40, 0, 0);
        write_available_index(&mut memory, available_ring, 1);
        write_available_head(&mut memory, available_ring, 0, 0);
        let mut queue = VirtqueueAvailableRing::new(TABLE, available_ring, 1)
            .expect("available ring metadata should be valid");

        let chain = queue
            .pop_descriptor_chain(&memory)
            .expect("available ring ending at boundary should pop")
            .expect("available ring should contain one head");

        assert_eq!(chain.descriptors()[0].index(), 0);
        assert_eq!(queue.next_avail(), 1);
    }

    #[test]
    fn available_ring_errors_display_and_preserve_sources() {
        let err = VirtqueueAvailableRingError::AvailableRingLengthTooLarge {
            queue_size: 8,
            available_len: 9,
        };
        assert_eq!(
            err.to_string(),
            "virtqueue available ring reports 9 descriptors, exceeding queue size 8"
        );
        assert!(err.source().is_none());

        let memory = guest_memory(0x4000);
        let available_ring = GuestAddress::new(0x4000);
        let mut queue = VirtqueueAvailableRing::new(TABLE, available_ring, 1)
            .expect("available ring metadata should be valid");
        let access_err = queue
            .pop_descriptor_chain(&memory)
            .expect_err("unmapped available ring should fail");
        assert!(access_err.source().is_some());
    }

    #[test]
    fn publishes_used_element_and_advances_used_index() {
        let mut memory = guest_memory(0x4000);
        let mut queue = VirtqueueUsedRing::new(USED, 8).expect("used ring should be valid");

        queue
            .publish_used_element(&mut memory, 2, 0x1234)
            .expect("used element should publish");

        let entry = used_ring_entry_address(USED, 0);
        assert_eq!(read_u32_field(&memory, entry), 2);
        assert_eq!(
            read_u32_field(
                &memory,
                entry
                    .checked_add(4)
                    .expect("used element len address should not overflow")
            ),
            0x1234
        );
        assert_eq!(
            read_u16_field(
                &memory,
                USED.checked_add(VIRTQUEUE_USED_RING_IDX_OFFSET)
                    .expect("used index address should not overflow")
            ),
            1
        );
        assert_eq!(queue.next_used(), 1);
    }

    #[test]
    fn wraps_used_ring_slot_and_index() {
        let mut memory = guest_memory(0x4000);
        let mut queue = VirtqueueUsedRing::with_next_used(USED, 8, u16::MAX)
            .expect("used ring should be valid");

        queue
            .publish_used_element(&mut memory, 7, 0x55)
            .expect("wrapped used element should publish");

        let entry = used_ring_entry_address(USED, 7);
        assert_eq!(read_u32_field(&memory, entry), 7);
        assert_eq!(
            read_u32_field(
                &memory,
                entry
                    .checked_add(4)
                    .expect("used element len address should not overflow")
            ),
            0x55
        );
        assert_eq!(
            read_u16_field(
                &memory,
                USED.checked_add(VIRTQUEUE_USED_RING_IDX_OFFSET)
                    .expect("used index address should not overflow")
            ),
            0
        );
        assert_eq!(queue.next_used(), 0);
    }

    #[test]
    fn rejects_invalid_used_ring_queue_sizes() {
        assert!(matches!(
            VirtqueueUsedRing::new(USED, 0),
            Err(VirtqueueUsedRingError::InvalidQueueSize { queue_size: 0 })
        ));
        assert!(matches!(
            VirtqueueUsedRing::new(USED, 3),
            Err(VirtqueueUsedRingError::InvalidQueueSize { queue_size: 3 })
        ));
    }

    #[test]
    fn rejects_unaligned_used_ring_address() {
        let used_ring = GuestAddress::new(0x2802);

        let err =
            VirtqueueUsedRing::new(used_ring, 8).expect_err("unaligned used ring should fail");

        assert!(matches!(
            err,
            VirtqueueUsedRingError::UnalignedUsedRing {
                used_ring: ring,
                alignment: VIRTQUEUE_USED_RING_ALIGNMENT
            } if ring == used_ring
        ));
    }

    #[test]
    fn rejects_used_ring_range_overflow() {
        let used_ring = GuestAddress::new(u64::MAX - 11);

        let err = VirtqueueUsedRing::new(used_ring, 1).expect_err("used ring overflow should fail");

        assert!(matches!(
            err,
            VirtqueueUsedRingError::UsedRingRangeOverflow {
                used_ring: ring,
                queue_size: 1
            } if ring == used_ring
        ));
    }

    #[test]
    fn rejects_invalid_descriptor_head_without_advancing_or_writing() {
        let mut memory = guest_memory(0x4000);
        let mut queue =
            VirtqueueUsedRing::with_next_used(USED, 8, 3).expect("used ring should be valid");

        let err = queue
            .publish_used_element(&mut memory, 8, 0x1234)
            .expect_err("out-of-range descriptor head should fail");

        assert!(matches!(
            err,
            VirtqueueUsedRingError::InvalidDescriptorHead {
                descriptor_head: 8,
                queue_size: 8
            }
        ));
        assert_eq!(queue.next_used(), 3);
        assert_eq!(
            read_u16_field(
                &memory,
                USED.checked_add(VIRTQUEUE_USED_RING_IDX_OFFSET)
                    .expect("used index address should not overflow")
            ),
            0
        );

        let entry = used_ring_entry_address(USED, 3);
        assert_eq!(read_u32_field(&memory, entry), 0);
        assert_eq!(
            read_u32_field(
                &memory,
                entry
                    .checked_add(4)
                    .expect("used element len address should not overflow")
            ),
            0
        );
    }

    #[test]
    fn rejects_unmapped_used_ring_before_writing() {
        let mut memory = guest_memory(0x4000);
        let used_ring = GuestAddress::new(0x4000);
        let mut queue =
            VirtqueueUsedRing::new(used_ring, 1).expect("used ring metadata should be valid");

        let err = queue
            .publish_used_element(&mut memory, 0, 0x10)
            .expect_err("unmapped used ring should fail");

        assert!(matches!(
            err,
            VirtqueueUsedRingError::UsedRingAccess {
                used_ring: ring,
                queue_size: 1,
                source: GuestMemoryAccessError::UnmappedRange { .. }
            } if ring == used_ring
        ));
        assert_eq!(queue.next_used(), 0);
    }

    #[test]
    fn rejects_partially_mapped_used_ring_before_writing() {
        let mut memory = guest_memory(0x4000);
        let used_ring = GuestAddress::new(0x3ff4);
        let mut queue =
            VirtqueueUsedRing::new(used_ring, 1).expect("used ring metadata should be valid");

        let err = queue
            .publish_used_element(&mut memory, 0, 0x10)
            .expect_err("partially mapped used ring should fail");

        assert!(matches!(
            err,
            VirtqueueUsedRingError::UsedRingAccess {
                used_ring: ring,
                queue_size: 1,
                source: GuestMemoryAccessError::UnmappedRange { .. }
            } if ring == used_ring
        ));
        assert_eq!(queue.next_used(), 0);
    }

    #[test]
    fn accepts_used_ring_near_memory_boundary() {
        let mut memory = guest_memory(0x4000);
        let used_ring = GuestAddress::new(0x3ff0);
        let mut queue =
            VirtqueueUsedRing::new(used_ring, 1).expect("used ring metadata should be valid");

        queue
            .publish_used_element(&mut memory, 0, 0x20)
            .expect("used ring near boundary should publish");

        let entry = used_ring_entry_address(used_ring, 0);
        assert_eq!(read_u32_field(&memory, entry), 0);
        assert_eq!(
            read_u32_field(
                &memory,
                entry
                    .checked_add(4)
                    .expect("used element len address should not overflow")
            ),
            0x20
        );
        assert_eq!(queue.next_used(), 1);
    }

    #[test]
    fn used_ring_errors_display_and_preserve_sources() {
        let err = VirtqueueUsedRingError::InvalidDescriptorHead {
            descriptor_head: 8,
            queue_size: 8,
        };
        assert_eq!(
            err.to_string(),
            "virtqueue used descriptor head 8 is outside queue size 8"
        );
        assert!(err.source().is_none());

        let mut memory = guest_memory(0x4000);
        let used_ring = GuestAddress::new(0x4000);
        let mut queue =
            VirtqueueUsedRing::new(used_ring, 1).expect("used ring metadata should be valid");
        let access_err = queue
            .publish_used_element(&mut memory, 0, 0x10)
            .expect_err("unmapped used ring should fail");
        assert!(access_err.source().is_some());
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
    fn ignores_next_index_without_next_flag() {
        let mut memory = guest_memory(0x4000);
        write_descriptor(&mut memory, TABLE, 0, 0x2000, 0x20, 0, 8);

        let chain = read_descriptor_chain(&memory, TABLE, 8, 0)
            .expect("stale next field without next flag should be ignored");

        assert_eq!(chain.len(), 1);
        assert_eq!(chain.descriptors()[0].next_index(), None);
    }

    #[test]
    fn accepts_chain_with_queue_size_descriptors() {
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
        write_descriptor(&mut memory, TABLE, 1, 0x3000, 0x20, 0, 0);

        let chain = read_descriptor_chain(&memory, TABLE, 2, 0)
            .expect("chain with exactly queue_size descriptors should read");

        assert_eq!(chain.len(), 2);
        assert_eq!(chain.descriptors()[0].next_index(), Some(1));
        assert_eq!(chain.descriptors()[1].next_index(), None);
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
    fn rejects_unmapped_descriptor_table() {
        let memory = guest_memory(0x4000);

        let err = read_descriptor_chain(&memory, GuestAddress::new(0x4000), 1, 0)
            .expect_err("descriptor table outside mapped memory should fail");

        match err {
            VirtqueueDescriptorChainError::DescriptorTableAccess {
                descriptor_table,
                queue_size,
                source,
            } => {
                assert_eq!(descriptor_table, GuestAddress::new(0x4000));
                assert_eq!(queue_size, 1);
                assert!(matches!(
                    source,
                    GuestMemoryAccessError::UnmappedRange { .. }
                ));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn rejects_partially_mapped_descriptor_table_before_first_descriptor_read() {
        let mut memory = guest_memory(0x4000);
        let table = GuestAddress::new(0x3ff0);
        write_descriptor(&mut memory, table, 0, 0x2000, 0x20, 0, 0);

        let err = read_descriptor_chain(&memory, table, 2, 0)
            .expect_err("partially mapped descriptor table should fail");

        assert!(matches!(
            err,
            VirtqueueDescriptorChainError::DescriptorTableAccess {
                descriptor_table,
                queue_size: 2,
                source: GuestMemoryAccessError::UnmappedRange { .. }
            } if descriptor_table == table
        ));
    }

    #[test]
    fn accepts_descriptor_table_ending_at_memory_boundary() {
        let mut memory = guest_memory(0x4000);
        let table = GuestAddress::new(0x3ff0);
        write_descriptor(&mut memory, table, 0, 0x2000, 0x20, 0, 0);

        let chain = read_descriptor_chain(&memory, table, 1, 0)
            .expect("descriptor table ending at memory boundary should read");

        assert_eq!(chain.len(), 1);
        assert_eq!(chain.descriptors()[0].address(), GuestAddress::new(0x2000));
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
