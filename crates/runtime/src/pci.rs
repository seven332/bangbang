//! Backend-neutral PCI identity, configuration, ECAM, and resource ownership.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::memory::{GuestAddress, GuestMemoryError, GuestMemoryRange};
use crate::mmio::{
    MmioAccess, MmioAccessBytes, MmioHandler, MmioHandlerError, MmioRegionId, MmioRegionRequest,
    MmioRegistrationError, MmioRegistrationLease, MmioRegistrationOwner,
};

pub const PCI_SEGMENT_ZERO: u16 = 0;
pub const PCI_BUS_ZERO: u8 = 0;
pub const PCI_HOST_BRIDGE_DEVICE: u8 = 0;
pub const PCI_FIRST_ENDPOINT_DEVICE: u8 = 1;
pub const PCI_LAST_ENDPOINT_DEVICE: u8 = 31;
pub const PCI_FUNCTION_ZERO: u8 = 0;
pub const PCI_ECAM_FUNCTION_SIZE: u64 = 4096;
pub const PCI_ECAM_BUS_ZERO_SIZE: u64 = 1 << 20;
pub const PCI_ECAM_RESERVED_START: u64 = 0x7000_0000;
pub const PCI_ECAM_RESERVED_SIZE: u64 = 256 << 20;
pub const PCI_BAR32_START: u64 = 0x4000_3000;
pub const PCI_BAR32_END_EXCLUSIVE: u64 = PCI_ECAM_RESERVED_START;
pub const PCI_BAR64_START: u64 = 256 << 30;
pub const PCI_BAR64_SIZE: u64 = 256 << 30;
pub const PCI_HOST_BRIDGE_VENDOR_ID: u16 = 0x8086;
pub const PCI_HOST_BRIDGE_DEVICE_ID: u16 = 0x0d57;

const PCI_CONFIG_SPACE_SIZE: usize = 4096;
const PCI_CONFIG_REGISTER_SIZE: usize = 4;
const PCI_CONFIG_REGISTER_COUNT: usize = PCI_CONFIG_SPACE_SIZE / PCI_CONFIG_REGISTER_SIZE;
const PCI_BAR_REGISTER_FIRST: u16 = 4;
const PCI_BAR_REGISTER_COUNT: u8 = 6;
const PCI_BAR_MINIMUM_SIZE: u64 = 16;
const PCI_COMMAND_WRITABLE_MASK: u32 = 0x0000_ffff;
const PCI_CACHELINE_WRITABLE_MASK: u32 = 0x0000_00ff;
const PCI_INTERRUPT_LINE_WRITABLE_MASK: u32 = 0x0000_00ff;
const PCI_STATUS_CAPABILITY_LIST_MASK: u32 = 0x0010_0000;
const PCI_CAPABILITY_LIST_HEAD_OFFSET: usize = 0x34;
const PCI_FIRST_CAPABILITY_OFFSET: usize = 0x40;
const PCI_CAPABILITY_END_EXCLUSIVE: usize = 0xc0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PciSbdf {
    segment: u16,
    bus: u8,
    device: u8,
    function: u8,
}

impl PciSbdf {
    pub fn new(segment: u16, bus: u8, device: u8, function: u8) -> Result<Self, PciIdentityError> {
        if device > PCI_LAST_ENDPOINT_DEVICE {
            return Err(PciIdentityError::InvalidDevice { device });
        }
        if function > 7 {
            return Err(PciIdentityError::InvalidFunction { function });
        }
        Ok(Self {
            segment,
            bus,
            device,
            function,
        })
    }

    pub const fn segment(self) -> u16 {
        self.segment
    }

    pub const fn bus(self) -> u8 {
        self.bus
    }

    pub const fn device(self) -> u8 {
        self.device
    }

    pub const fn function(self) -> u8 {
        self.function
    }

    pub const fn ecam_offset(self) -> u32 {
        (self.bus as u32) << 20 | (self.device as u32) << 15 | (self.function as u32) << 12
    }
}

impl fmt::Display for PciSbdf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04x}:{:02x}:{:02x}.{}",
            self.segment, self.bus, self.device, self.function
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciIdentityError {
    InvalidDevice { device: u8 },
    InvalidFunction { function: u8 },
}

impl fmt::Display for PciIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDevice { device } => {
                write!(f, "PCI device number {device} exceeds 31")
            }
            Self::InvalidFunction { function } => {
                write!(f, "PCI function number {function} exceeds 7")
            }
        }
    }
}

impl std::error::Error for PciIdentityError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64PciAddressPlan {
    ecam: GuestMemoryRange,
    ecam_reservation: GuestMemoryRange,
    bar32: GuestMemoryRange,
    bar64: GuestMemoryRange,
}

impl Arm64PciAddressPlan {
    pub fn firecracker_v1_16() -> Result<Self, GuestMemoryError> {
        Ok(Self {
            ecam: GuestMemoryRange::new(
                GuestAddress::new(PCI_ECAM_RESERVED_START),
                PCI_ECAM_BUS_ZERO_SIZE,
            )?,
            ecam_reservation: GuestMemoryRange::new(
                GuestAddress::new(PCI_ECAM_RESERVED_START),
                PCI_ECAM_RESERVED_SIZE,
            )?,
            bar32: GuestMemoryRange::new(
                GuestAddress::new(PCI_BAR32_START),
                PCI_BAR32_END_EXCLUSIVE - PCI_BAR32_START,
            )?,
            bar64: GuestMemoryRange::new(GuestAddress::new(PCI_BAR64_START), PCI_BAR64_SIZE)?,
        })
    }

    pub const fn ecam(self) -> GuestMemoryRange {
        self.ecam
    }

    pub const fn ecam_reservation(self) -> GuestMemoryRange {
        self.ecam_reservation
    }

    pub const fn bar32(self) -> GuestMemoryRange {
        self.bar32
    }

    pub const fn bar64(self) -> GuestMemoryRange {
        self.bar64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciBarAddressSpace {
    Memory32,
    Memory64,
}

struct PciBarAllocatorProvenance;

pub struct PciBarLease {
    allocator: Arc<PciBarAllocatorProvenance>,
    generation: u64,
    address_space: PciBarAddressSpace,
    range: GuestMemoryRange,
}

impl PciBarLease {
    pub const fn address_space(&self) -> PciBarAddressSpace {
        self.address_space
    }

    pub const fn range(&self) -> GuestMemoryRange {
        self.range
    }
}

impl fmt::Debug for PciBarLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PciBarLease")
            .field("ownership", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PciBarAllocation {
    address_space: PciBarAddressSpace,
    range: GuestMemoryRange,
}

pub struct PciBarAllocator {
    provenance: Arc<PciBarAllocatorProvenance>,
    address_space: PciBarAddressSpace,
    capacity: GuestMemoryRange,
    free: Vec<GuestMemoryRange>,
    allocations: BTreeMap<u64, PciBarAllocation>,
    next_generation: u64,
}

impl PciBarAllocator {
    pub fn new(address_space: PciBarAddressSpace, capacity: GuestMemoryRange) -> Self {
        Self {
            provenance: Arc::new(PciBarAllocatorProvenance),
            address_space,
            capacity,
            free: vec![capacity],
            allocations: BTreeMap::new(),
            next_generation: 0,
        }
    }

    pub const fn address_space(&self) -> PciBarAddressSpace {
        self.address_space
    }

    pub const fn capacity(&self) -> GuestMemoryRange {
        self.capacity
    }

    pub fn allocate(&mut self, size: u64) -> Result<PciBarLease, PciBarAllocationError> {
        if size < PCI_BAR_MINIMUM_SIZE || !size.is_power_of_two() {
            return Err(PciBarAllocationError::InvalidSize { size });
        }
        let next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or(PciBarAllocationError::GenerationExhausted)?;

        let mut selected = None;
        for range in self.free.iter().copied() {
            let start = align_up(range.start().raw_value(), size)
                .ok_or(PciBarAllocationError::AddressOverflow)?;
            let end = start
                .checked_add(size)
                .ok_or(PciBarAllocationError::AddressOverflow)?;
            if end <= range.end_exclusive().raw_value() {
                selected = Some((
                    range,
                    GuestMemoryRange::new(GuestAddress::new(start), size)?,
                ));
                break;
            }
        }
        let Some((selected_free, allocation)) = selected else {
            return Err(PciBarAllocationError::Exhausted { size });
        };

        let mut next_free = Vec::new();
        next_free
            .try_reserve_exact(self.free.len().saturating_add(1))
            .map_err(|_| PciBarAllocationError::MetadataAllocation)?;
        for range in self.free.iter().copied() {
            if range != selected_free {
                next_free.push(range);
                continue;
            }
            if range.start() < allocation.start() {
                next_free.push(GuestMemoryRange::new(
                    range.start(),
                    allocation.start().raw_value() - range.start().raw_value(),
                )?);
            }
            if allocation.end_exclusive() < range.end_exclusive() {
                next_free.push(GuestMemoryRange::new(
                    allocation.end_exclusive(),
                    range.end_exclusive().raw_value() - allocation.end_exclusive().raw_value(),
                )?);
            }
        }
        next_free.sort_by_key(|range| range.start());

        let generation = self.next_generation;
        self.free = next_free;
        let replaced = self.allocations.insert(
            generation,
            PciBarAllocation {
                address_space: self.address_space,
                range: allocation,
            },
        );
        debug_assert!(replaced.is_none());
        self.next_generation = next_generation;
        Ok(PciBarLease {
            allocator: Arc::clone(&self.provenance),
            generation,
            address_space: self.address_space,
            range: allocation,
        })
    }

    pub fn release(&mut self, lease: &PciBarLease) -> Result<(), PciBarReleaseError> {
        if !Arc::ptr_eq(&self.provenance, &lease.allocator) {
            return Err(PciBarReleaseError::WrongAllocator);
        }
        let allocation = self
            .allocations
            .get(&lease.generation)
            .copied()
            .ok_or(PciBarReleaseError::StaleLease)?;
        if allocation.address_space != lease.address_space || allocation.range != lease.range {
            return Err(PciBarReleaseError::LeaseMismatch);
        }

        let mut ranges = self.free.clone();
        ranges.push(allocation.range);
        ranges.sort_by_key(|range| range.start());
        let free = coalesce_ranges(&ranges).ok_or(PciBarReleaseError::AllocatorStateMismatch)?;
        let removed = self.allocations.remove(&lease.generation);
        debug_assert!(removed.is_some());
        self.free = free;
        Ok(())
    }

    pub fn available_ranges(&self) -> &[GuestMemoryRange] {
        &self.free
    }
}

impl fmt::Debug for PciBarAllocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PciBarAllocator")
            .field("address_space", &self.address_space)
            .field("capacity", &"<redacted>")
            .field("free_ranges", &self.free.len())
            .field("allocations", &self.allocations.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PciBarAllocationError {
    InvalidSize { size: u64 },
    AddressOverflow,
    Exhausted { size: u64 },
    GenerationExhausted,
    MetadataAllocation,
    InvalidRange { source: GuestMemoryError },
}

impl From<GuestMemoryError> for PciBarAllocationError {
    fn from(source: GuestMemoryError) -> Self {
        Self::InvalidRange { source }
    }
}

impl fmt::Display for PciBarAllocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSize { size } => {
                write!(
                    f,
                    "PCI BAR size {size} is not a power of two of at least 16 bytes"
                )
            }
            Self::AddressOverflow => f.write_str("PCI BAR allocation address overflowed"),
            Self::Exhausted { size } => {
                write!(f, "PCI BAR address space cannot fit {size} bytes")
            }
            Self::GenerationExhausted => f.write_str("PCI BAR lease generation is exhausted"),
            Self::MetadataAllocation => f.write_str("PCI BAR allocator metadata allocation failed"),
            Self::InvalidRange { source } => write!(f, "invalid PCI BAR range: {source}"),
        }
    }
}

impl std::error::Error for PciBarAllocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRange { source } => Some(source),
            Self::InvalidSize { .. }
            | Self::AddressOverflow
            | Self::Exhausted { .. }
            | Self::GenerationExhausted
            | Self::MetadataAllocation => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciBarReleaseError {
    WrongAllocator,
    StaleLease,
    LeaseMismatch,
    AllocatorStateMismatch,
}

impl fmt::Display for PciBarReleaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongAllocator => f.write_str("PCI BAR lease belongs to another allocator"),
            Self::StaleLease => f.write_str("PCI BAR lease is stale"),
            Self::LeaseMismatch => f.write_str("PCI BAR lease does not match allocator state"),
            Self::AllocatorStateMismatch => {
                f.write_str("PCI BAR allocator free ranges overlap or are invalid")
            }
        }
    }
}

impl std::error::Error for PciBarReleaseError {}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    value
        .checked_add(alignment.checked_sub(1)?)
        .map(|candidate| candidate & !(alignment - 1))
}

fn coalesce_ranges(ranges: &[GuestMemoryRange]) -> Option<Vec<GuestMemoryRange>> {
    let mut coalesced: Vec<GuestMemoryRange> = Vec::new();
    coalesced.try_reserve_exact(ranges.len()).ok()?;
    for range in ranges.iter().copied() {
        let Some(previous) = coalesced.last_mut() else {
            coalesced.push(range);
            continue;
        };
        if previous.overlaps(range) {
            return None;
        }
        if previous.is_adjacent_to(range) {
            *previous = GuestMemoryRange::new(
                previous.start(),
                range.end_exclusive().raw_value() - previous.start().raw_value(),
            )
            .ok()?;
        } else {
            coalesced.push(range);
        }
    }
    Some(coalesced)
}

pub trait PciConfigFunction: fmt::Debug + Send {
    fn read_config(&mut self, offset: u16, data: &mut [u8]) -> Result<(), PciConfigAccessError>;

    fn write_config(&mut self, offset: u16, data: &[u8]) -> Result<(), PciConfigAccessError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciClassCode {
    Unclassified = 0x00,
    MassStorage = 0x01,
    Network = 0x02,
    Bridge = 0x06,
    Unassigned = 0xff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PciCapabilityId {
    VendorSpecific = 0x09,
    MsiX = 0x11,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciBarPrefetchable {
    No,
    Yes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PciBarRegister {
    encoded_address: u32,
    encoded_size: u32,
    probe_pending: bool,
}

pub struct PciType0Configuration {
    registers: Vec<u32>,
    writable_masks: Vec<u32>,
    bars: BTreeMap<u8, PciBarRegister>,
    last_capability: Option<(u8, u8)>,
}

impl fmt::Debug for PciType0Configuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PciType0Configuration")
            .field("configuration", &"<redacted>")
            .field("configured_bar_registers", &self.bars.len())
            .field("capability_count", &self.capability_count())
            .finish()
    }
}

impl PciType0Configuration {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vendor_id: u16,
        device_id: u16,
        revision_id: u8,
        class_code: PciClassCode,
        subclass: u8,
        programming_interface: u8,
        subsystem_vendor_id: u16,
        subsystem_id: u16,
    ) -> Self {
        let mut configuration = Self {
            registers: vec![0; PCI_CONFIG_REGISTER_COUNT],
            writable_masks: vec![0; PCI_CONFIG_REGISTER_COUNT],
            bars: BTreeMap::new(),
            last_capability: None,
        };
        configuration.set_register(0, (u32::from(device_id) << 16) | u32::from(vendor_id));
        configuration.set_register(
            2,
            (u32::from(class_code as u8) << 24)
                | (u32::from(subclass) << 16)
                | (u32::from(programming_interface) << 8)
                | u32::from(revision_id),
        );
        configuration.set_register(3, 0);
        configuration.set_register(
            11,
            (u32::from(subsystem_id) << 16) | u32::from(subsystem_vendor_id),
        );
        configuration.set_writable_mask(1, PCI_COMMAND_WRITABLE_MASK);
        configuration.set_writable_mask(3, PCI_CACHELINE_WRITABLE_MASK);
        configuration.set_writable_mask(15, PCI_INTERRUPT_LINE_WRITABLE_MASK);
        configuration
    }

    pub fn firecracker_host_bridge() -> Self {
        Self::new(
            PCI_HOST_BRIDGE_VENDOR_ID,
            PCI_HOST_BRIDGE_DEVICE_ID,
            0,
            PciClassCode::Bridge,
            0,
            0,
            0,
            0,
        )
    }

    pub fn install_bar(
        &mut self,
        index: u8,
        lease: &PciBarLease,
        prefetchable: PciBarPrefetchable,
    ) -> Result<(), PciBarConfigurationError> {
        if index >= PCI_BAR_REGISTER_COUNT {
            return Err(PciBarConfigurationError::InvalidIndex { index });
        }
        let range = lease.range();
        let type_bits = match lease.address_space() {
            PciBarAddressSpace::Memory32 => {
                if range.end_exclusive().raw_value() > 1_u64 << 32 {
                    return Err(PciBarConfigurationError::AddressExceeds32Bit { range });
                }
                0
            }
            PciBarAddressSpace::Memory64 => {
                if index >= PCI_BAR_REGISTER_COUNT - 1 {
                    return Err(PciBarConfigurationError::MissingHighRegister { index });
                }
                0b100
            }
        };
        let occupied_registers = if lease.address_space() == PciBarAddressSpace::Memory64 {
            2
        } else {
            1
        };
        for register in index..index + occupied_registers {
            if self.bars.contains_key(&register) {
                return Err(PciBarConfigurationError::DuplicateRegister { index: register });
            }
        }
        let prefetchable_bit = match prefetchable {
            PciBarPrefetchable::No => 0,
            PciBarPrefetchable::Yes => 0b1000,
        };
        let size_mask = !(range.size() - 1);
        self.bars.insert(
            index,
            PciBarRegister {
                encoded_address: (range.start().raw_value() as u32 & 0xffff_fff0)
                    | type_bits
                    | prefetchable_bit,
                encoded_size: size_mask as u32,
                probe_pending: false,
            },
        );
        if lease.address_space() == PciBarAddressSpace::Memory64 {
            self.bars.insert(
                index + 1,
                PciBarRegister {
                    encoded_address: (range.start().raw_value() >> 32) as u32,
                    encoded_size: (size_mask >> 32) as u32,
                    probe_pending: false,
                },
            );
        }
        Ok(())
    }

    /// Add one conventional PCI capability and return its byte offset.
    ///
    /// `body` excludes the standard capability ID/next-pointer header.
    /// `body_writable_mask` has the same length and controls guest-writable
    /// bits. Header identity and links are always immutable.
    pub fn add_capability(
        &mut self,
        id: PciCapabilityId,
        body: &[u8],
        body_writable_mask: &[u8],
    ) -> Result<u8, PciCapabilityError> {
        if body.len() != body_writable_mask.len() {
            return Err(PciCapabilityError::WritableMaskLength {
                body: body.len(),
                mask: body_writable_mask.len(),
            });
        }
        let total_len = body
            .len()
            .checked_add(2)
            .ok_or(PciCapabilityError::LengthOverflow)?;
        let total_len_u8 =
            u8::try_from(total_len).map_err(|_| PciCapabilityError::LengthOverflow)?;
        let (offset, previous_next_pointer) = match self.last_capability {
            Some((previous_offset, previous_len)) => {
                let next = usize::from(previous_offset)
                    .checked_add(usize::from(previous_len))
                    .and_then(|offset| offset.checked_add(3))
                    .map(|offset| offset & !3)
                    .ok_or(PciCapabilityError::LengthOverflow)?;
                (next, usize::from(previous_offset) + 1)
            }
            None => (PCI_FIRST_CAPABILITY_OFFSET, PCI_CAPABILITY_LIST_HEAD_OFFSET),
        };
        let end = offset
            .checked_add(total_len)
            .ok_or(PciCapabilityError::LengthOverflow)?;
        if end > PCI_CAPABILITY_END_EXCLUSIVE {
            return Err(PciCapabilityError::NoSpace {
                offset,
                length: total_len,
            });
        }
        let offset_u8 =
            u8::try_from(offset).map_err(|_| PciCapabilityError::InvalidOffset { offset })?;

        // All validation precedes mutation so a rejected capability cannot
        // damage the existing chain or writable masks.
        self.set_configuration_byte(previous_next_pointer, offset_u8);
        self.set_configuration_byte(offset, id as u8);
        self.set_configuration_byte(offset + 1, 0);
        self.set_writable_configuration_byte(offset, 0);
        self.set_writable_configuration_byte(offset + 1, 0);
        for (index, byte) in body.iter().copied().enumerate() {
            self.set_configuration_byte(offset + 2 + index, byte);
        }
        for (index, mask) in body_writable_mask.iter().copied().enumerate() {
            self.set_writable_configuration_byte(offset + 2 + index, mask);
        }
        let status =
            self.registers.get(1).copied().unwrap_or_default() | PCI_STATUS_CAPABILITY_LIST_MASK;
        self.set_register(1, status);
        self.last_capability = Some((offset_u8, total_len_u8));
        Ok(offset_u8)
    }

    pub fn capability_count(&self) -> usize {
        let mut count = 0;
        let mut offset = self.configuration_byte(PCI_CAPABILITY_LIST_HEAD_OFFSET);
        while offset != 0 && count < 64 {
            count += 1;
            offset = self.configuration_byte(usize::from(offset) + 1);
        }
        count
    }

    fn configuration_byte(&self, offset: usize) -> u8 {
        let register = self
            .registers
            .get(offset / PCI_CONFIG_REGISTER_SIZE)
            .copied()
            .unwrap_or(0);
        ((register >> ((offset % PCI_CONFIG_REGISTER_SIZE) * 8)) & 0xff) as u8
    }

    fn set_configuration_byte(&mut self, offset: usize, value: u8) {
        if let Some(register) = self.registers.get_mut(offset / PCI_CONFIG_REGISTER_SIZE) {
            let shift = (offset % PCI_CONFIG_REGISTER_SIZE) * 8;
            *register = (*register & !(0xff_u32 << shift)) | (u32::from(value) << shift);
        }
    }

    fn set_writable_configuration_byte(&mut self, offset: usize, value: u8) {
        if let Some(mask) = self
            .writable_masks
            .get_mut(offset / PCI_CONFIG_REGISTER_SIZE)
        {
            let shift = (offset % PCI_CONFIG_REGISTER_SIZE) * 8;
            *mask = (*mask & !(0xff_u32 << shift)) | (u32::from(value) << shift);
        }
    }

    fn set_register(&mut self, index: usize, value: u32) {
        if let Some(register) = self.registers.get_mut(index) {
            *register = value;
        }
    }

    fn set_writable_mask(&mut self, index: usize, value: u32) {
        if let Some(mask) = self.writable_masks.get_mut(index) {
            *mask = value;
        }
    }

    fn validate_access(offset: u16, len: usize) -> Result<(usize, usize), PciConfigAccessError> {
        if !matches!(len, 1 | 2 | 4) {
            return Err(PciConfigAccessError::InvalidWidth { len });
        }
        let byte_offset = usize::from(offset);
        let end = byte_offset
            .checked_add(len)
            .ok_or(PciConfigAccessError::OutsideConfigurationSpace { offset, len })?;
        if end > PCI_CONFIG_SPACE_SIZE {
            return Err(PciConfigAccessError::OutsideConfigurationSpace { offset, len });
        }
        let register_offset = byte_offset % PCI_CONFIG_REGISTER_SIZE;
        if register_offset + len > PCI_CONFIG_REGISTER_SIZE {
            return Err(PciConfigAccessError::CrossesRegister { offset, len });
        }
        Ok((byte_offset / PCI_CONFIG_REGISTER_SIZE, register_offset))
    }
}

impl PciConfigFunction for PciType0Configuration {
    fn read_config(&mut self, offset: u16, data: &mut [u8]) -> Result<(), PciConfigAccessError> {
        let (register_index, register_offset) = Self::validate_access(offset, data.len())?;
        let bar_index = register_index
            .checked_sub(usize::from(PCI_BAR_REGISTER_FIRST))
            .and_then(|index| u8::try_from(index).ok())
            .filter(|index| *index < PCI_BAR_REGISTER_COUNT);
        let value = if let Some(bar) = bar_index.and_then(|index| self.bars.get_mut(&index)) {
            if bar.probe_pending {
                bar.probe_pending = false;
                bar.encoded_size
            } else {
                bar.encoded_address
            }
        } else {
            self.registers
                .get(register_index)
                .copied()
                .unwrap_or(u32::MAX)
        };
        copy_register_bytes(value, register_offset, data);
        Ok(())
    }

    fn write_config(&mut self, offset: u16, data: &[u8]) -> Result<(), PciConfigAccessError> {
        let (register_index, register_offset) = Self::validate_access(offset, data.len())?;
        let bar_index = register_index
            .checked_sub(usize::from(PCI_BAR_REGISTER_FIRST))
            .and_then(|index| u8::try_from(index).ok())
            .filter(|index| *index < PCI_BAR_REGISTER_COUNT);
        if let Some(bar) = bar_index.and_then(|index| self.bars.get_mut(&index)) {
            bar.probe_pending = register_offset == 0
                && data.len() == PCI_CONFIG_REGISTER_SIZE
                && data.iter().all(|byte| *byte == u8::MAX);
            return Ok(());
        }

        let Some(register) = self.registers.get_mut(register_index) else {
            return Ok(());
        };
        let writable = self
            .writable_masks
            .get(register_index)
            .copied()
            .unwrap_or(0);
        let mut write_value = 0_u32;
        let mut byte_mask = 0_u32;
        for (index, byte) in data.iter().copied().enumerate() {
            let shift = (register_offset + index) * 8;
            write_value |= u32::from(byte) << shift;
            byte_mask |= 0xff_u32 << shift;
        }
        let mask = byte_mask & writable;
        *register = (*register & !mask) | (write_value & mask);
        Ok(())
    }
}

fn copy_register_bytes(value: u32, register_offset: usize, destination: &mut [u8]) {
    let bytes = value.to_le_bytes();
    for (index, destination_byte) in destination.iter_mut().enumerate() {
        if let Some(source) = bytes.get(register_offset + index) {
            *destination_byte = *source;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PciConfigAccessError {
    InvalidWidth { len: usize },
    OutsideConfigurationSpace { offset: u16, len: usize },
    CrossesRegister { offset: u16, len: usize },
    Handler { message: String },
}

impl fmt::Display for PciConfigAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidWidth { len } => write!(
                f,
                "PCI configuration access width {len} is not 1, 2, or 4 bytes"
            ),
            Self::OutsideConfigurationSpace { offset, len } => write!(
                f,
                "PCI configuration access offset 0x{offset:x} width {len} exceeds 4 KiB"
            ),
            Self::CrossesRegister { offset, len } => write!(
                f,
                "PCI configuration access offset 0x{offset:x} width {len} crosses a dword boundary"
            ),
            Self::Handler { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for PciConfigAccessError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PciCapabilityError {
    WritableMaskLength { body: usize, mask: usize },
    LengthOverflow,
    InvalidOffset { offset: usize },
    NoSpace { offset: usize, length: usize },
}

impl fmt::Display for PciCapabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WritableMaskLength { body, mask } => write!(
                f,
                "PCI capability body length {body} does not match writable mask length {mask}"
            ),
            Self::LengthOverflow => f.write_str("PCI capability length overflows"),
            Self::InvalidOffset { offset } => {
                write!(f, "PCI capability offset 0x{offset:x} is not representable")
            }
            Self::NoSpace { offset, length } => write!(
                f,
                "PCI capability at offset 0x{offset:x} with length {length} exceeds the conventional capability area"
            ),
        }
    }
}

impl std::error::Error for PciCapabilityError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciBarConfigurationError {
    InvalidIndex { index: u8 },
    MissingHighRegister { index: u8 },
    DuplicateRegister { index: u8 },
    AddressExceeds32Bit { range: GuestMemoryRange },
}

impl fmt::Display for PciBarConfigurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIndex { index } => write!(f, "PCI BAR index {index} exceeds 5"),
            Self::MissingHighRegister { index } => {
                write!(f, "64-bit PCI BAR at index {index} has no high register")
            }
            Self::DuplicateRegister { index } => {
                write!(f, "PCI BAR register {index} is already configured")
            }
            Self::AddressExceeds32Bit { range } => {
                write!(
                    f,
                    "32-bit PCI BAR range {range} exceeds the 32-bit address space"
                )
            }
        }
    }
}

impl std::error::Error for PciBarConfigurationError {}

struct PciSegmentProvenance;

pub struct PciFunctionLease {
    segment: Arc<PciSegmentProvenance>,
    generation: u64,
    sbdf: PciSbdf,
}

impl PciFunctionLease {
    pub const fn sbdf(&self) -> PciSbdf {
        self.sbdf
    }
}

impl fmt::Debug for PciFunctionLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PciFunctionLease")
            .field("ownership", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PciFunctionRecord {
    generation: u64,
}

pub struct PciSegment {
    provenance: Arc<PciSegmentProvenance>,
    functions: BTreeMap<u8, Box<dyn PciConfigFunction>>,
    suspended_functions: BTreeMap<u8, Box<dyn PciConfigFunction>>,
    records: BTreeMap<u8, PciFunctionRecord>,
    next_generation: u64,
}

impl PciSegment {
    pub fn new() -> Self {
        let mut functions: BTreeMap<u8, Box<dyn PciConfigFunction>> = BTreeMap::new();
        functions.insert(
            PCI_HOST_BRIDGE_DEVICE,
            Box::new(PciType0Configuration::firecracker_host_bridge()),
        );
        Self {
            provenance: Arc::new(PciSegmentProvenance),
            functions,
            suspended_functions: BTreeMap::new(),
            records: BTreeMap::new(),
            next_generation: 0,
        }
    }

    pub fn add_function(
        &mut self,
        function: impl PciConfigFunction + 'static,
    ) -> Result<PciFunctionLease, PciSegmentError> {
        let device = (PCI_FIRST_ENDPOINT_DEVICE..=PCI_LAST_ENDPOINT_DEVICE)
            .find(|device| !self.records.contains_key(device))
            .ok_or(PciSegmentError::NoDeviceSlot)?;
        let sbdf = PciSbdf::new(PCI_SEGMENT_ZERO, PCI_BUS_ZERO, device, PCI_FUNCTION_ZERO)
            .map_err(|source| PciSegmentError::InvalidIdentity { source })?;
        self.add_function_at(sbdf, function)
    }

    pub fn add_function_at(
        &mut self,
        sbdf: PciSbdf,
        function: impl PciConfigFunction + 'static,
    ) -> Result<PciFunctionLease, PciSegmentError> {
        if sbdf.segment() != PCI_SEGMENT_ZERO
            || sbdf.bus() != PCI_BUS_ZERO
            || sbdf.function() != PCI_FUNCTION_ZERO
            || !(PCI_FIRST_ENDPOINT_DEVICE..=PCI_LAST_ENDPOINT_DEVICE).contains(&sbdf.device())
        {
            return Err(PciSegmentError::UnsupportedIdentity { sbdf });
        }
        if self.functions.contains_key(&sbdf.device())
            || self.suspended_functions.contains_key(&sbdf.device())
            || self.records.contains_key(&sbdf.device())
        {
            return Err(PciSegmentError::DuplicateIdentity { sbdf });
        }
        let next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or(PciSegmentError::GenerationExhausted)?;
        let generation = self.next_generation;
        let previous = self.functions.insert(sbdf.device(), Box::new(function));
        debug_assert!(previous.is_none());
        let previous = self
            .records
            .insert(sbdf.device(), PciFunctionRecord { generation });
        debug_assert!(previous.is_none());
        self.next_generation = next_generation;
        Ok(PciFunctionLease {
            segment: Arc::clone(&self.provenance),
            generation,
            sbdf,
        })
    }

    pub fn remove_function(
        &mut self,
        lease: &PciFunctionLease,
    ) -> Result<(), PciFunctionReleaseError> {
        self.unpublish_function(lease)?;
        self.release_function_lease(lease)
    }

    /// Makes a leased function unreachable through ECAM while retaining its slot.
    ///
    /// The retained lease prevents a replacement function from reusing the
    /// same SBDF until device work and interrupt resources have drained.
    pub fn unpublish_function(
        &mut self,
        lease: &PciFunctionLease,
    ) -> Result<(), PciFunctionReleaseError> {
        if !Arc::ptr_eq(&self.provenance, &lease.segment) {
            return Err(PciFunctionReleaseError::WrongSegment);
        }
        let device = lease.sbdf.device();
        let record = self
            .records
            .get(&device)
            .copied()
            .ok_or(PciFunctionReleaseError::StaleLease { sbdf: lease.sbdf })?;
        if record.generation != lease.generation || !self.functions.contains_key(&device) {
            return Err(PciFunctionReleaseError::LeaseMismatch { sbdf: lease.sbdf });
        }
        let function = self
            .functions
            .remove(&device)
            .ok_or(PciFunctionReleaseError::LeaseMismatch { sbdf: lease.sbdf })?;
        let previous = self.suspended_functions.insert(device, function);
        debug_assert!(previous.is_none());
        Ok(())
    }

    /// Restores one exact leased function after a recoverable removal aborts.
    pub fn republish_function(
        &mut self,
        lease: &PciFunctionLease,
    ) -> Result<(), PciFunctionReleaseError> {
        if !Arc::ptr_eq(&self.provenance, &lease.segment) {
            return Err(PciFunctionReleaseError::WrongSegment);
        }
        let device = lease.sbdf.device();
        let record = self
            .records
            .get(&device)
            .copied()
            .ok_or(PciFunctionReleaseError::StaleLease { sbdf: lease.sbdf })?;
        if record.generation != lease.generation
            || self.functions.contains_key(&device)
            || !self.suspended_functions.contains_key(&device)
        {
            return Err(PciFunctionReleaseError::LeaseMismatch { sbdf: lease.sbdf });
        }
        let function = self
            .suspended_functions
            .remove(&device)
            .ok_or(PciFunctionReleaseError::LeaseMismatch { sbdf: lease.sbdf })?;
        let previous = self.functions.insert(device, function);
        debug_assert!(previous.is_none());
        Ok(())
    }

    /// Returns a previously unpublished function slot to the segment.
    pub fn release_function_lease(
        &mut self,
        lease: &PciFunctionLease,
    ) -> Result<(), PciFunctionReleaseError> {
        if !Arc::ptr_eq(&self.provenance, &lease.segment) {
            return Err(PciFunctionReleaseError::WrongSegment);
        }
        let device = lease.sbdf.device();
        let record = self
            .records
            .get(&device)
            .copied()
            .ok_or(PciFunctionReleaseError::StaleLease { sbdf: lease.sbdf })?;
        if record.generation != lease.generation
            || self.functions.contains_key(&device)
            || !self.suspended_functions.contains_key(&device)
        {
            return Err(PciFunctionReleaseError::LeaseMismatch { sbdf: lease.sbdf });
        }
        self.suspended_functions.remove(&device);
        self.records.remove(&device);
        Ok(())
    }

    pub fn function_count(&self) -> usize {
        self.functions.len()
    }

    /// Returns the exact number of endpoint slots not retained by a live lease.
    #[doc(hidden)]
    pub fn available_endpoint_slots(&self) -> usize {
        usize::from(PCI_LAST_ENDPOINT_DEVICE - PCI_FIRST_ENDPOINT_DEVICE + 1)
            .saturating_sub(self.records.len())
    }

    pub fn read_ecam(&mut self, offset: u64, data: &mut [u8]) -> Result<(), PciEcamAccessError> {
        let decoded = decode_ecam_access(offset, data.len())?;
        if decoded.bus != PCI_BUS_ZERO || decoded.function != PCI_FUNCTION_ZERO {
            data.fill(u8::MAX);
            return Ok(());
        }
        let Some(function) = self.functions.get_mut(&decoded.device) else {
            data.fill(u8::MAX);
            return Ok(());
        };
        function
            .read_config(decoded.register_offset, data)
            .map_err(|source| PciEcamAccessError::Configuration { source })
    }

    pub fn write_ecam(&mut self, offset: u64, data: &[u8]) -> Result<(), PciEcamAccessError> {
        let decoded = decode_ecam_access(offset, data.len())?;
        if decoded.bus != PCI_BUS_ZERO || decoded.function != PCI_FUNCTION_ZERO {
            return Ok(());
        }
        let Some(function) = self.functions.get_mut(&decoded.device) else {
            return Ok(());
        };
        function
            .write_config(decoded.register_offset, data)
            .map_err(|source| PciEcamAccessError::Configuration { source })
    }
}

impl Default for PciSegment {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for PciSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PciSegment")
            .field("identity", &"<redacted>")
            .field("function_count", &self.functions.len())
            .finish()
    }
}

#[derive(Debug)]
pub enum PciSegmentError {
    NoDeviceSlot,
    GenerationExhausted,
    InvalidIdentity { source: PciIdentityError },
    UnsupportedIdentity { sbdf: PciSbdf },
    DuplicateIdentity { sbdf: PciSbdf },
}

impl fmt::Display for PciSegmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDeviceSlot => f.write_str("PCI segment has no available device slot"),
            Self::GenerationExhausted => f.write_str("PCI function lease generation is exhausted"),
            Self::InvalidIdentity { source } => {
                write!(f, "invalid PCI function identity: {source}")
            }
            Self::UnsupportedIdentity { sbdf } => {
                write!(f, "PCI segment does not support function identity {sbdf}")
            }
            Self::DuplicateIdentity { sbdf } => {
                write!(f, "PCI function identity {sbdf} is already in use")
            }
        }
    }
}

impl std::error::Error for PciSegmentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidIdentity { source } => Some(source),
            Self::NoDeviceSlot
            | Self::GenerationExhausted
            | Self::UnsupportedIdentity { .. }
            | Self::DuplicateIdentity { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciFunctionReleaseError {
    WrongSegment,
    StaleLease { sbdf: PciSbdf },
    LeaseMismatch { sbdf: PciSbdf },
}

impl fmt::Display for PciFunctionReleaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongSegment => f.write_str("PCI function lease belongs to another segment"),
            Self::StaleLease { sbdf } => write!(f, "PCI function lease for {sbdf} is stale"),
            Self::LeaseMismatch { sbdf } => {
                write!(
                    f,
                    "PCI function lease for {sbdf} does not match segment state"
                )
            }
        }
    }
}

impl std::error::Error for PciFunctionReleaseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DecodedEcamAccess {
    bus: u8,
    device: u8,
    function: u8,
    register_offset: u16,
}

fn decode_ecam_access(offset: u64, len: usize) -> Result<DecodedEcamAccess, PciEcamAccessError> {
    if !matches!(len, 1 | 2 | 4) {
        return Err(PciEcamAccessError::InvalidWidth { len });
    }
    if offset >= PCI_ECAM_BUS_ZERO_SIZE {
        return Err(PciEcamAccessError::OutsideBusZero { offset });
    }
    let end = offset
        .checked_add(u64::try_from(len).map_err(|_| PciEcamAccessError::AddressOverflow)?)
        .ok_or(PciEcamAccessError::AddressOverflow)?;
    if end > PCI_ECAM_BUS_ZERO_SIZE {
        return Err(PciEcamAccessError::OutsideBusZero { offset });
    }
    let register_offset = (offset & (PCI_ECAM_FUNCTION_SIZE - 1)) as u16;
    if usize::from(register_offset % 4) + len > PCI_CONFIG_REGISTER_SIZE {
        return Err(PciEcamAccessError::CrossesRegister { offset, len });
    }
    Ok(DecodedEcamAccess {
        bus: ((offset >> 20) & 0xff) as u8,
        device: ((offset >> 15) & 0x1f) as u8,
        function: ((offset >> 12) & 0x7) as u8,
        register_offset,
    })
}

#[derive(Debug)]
pub enum PciEcamAccessError {
    InvalidWidth { len: usize },
    OutsideBusZero { offset: u64 },
    AddressOverflow,
    CrossesRegister { offset: u64, len: usize },
    Configuration { source: PciConfigAccessError },
}

impl fmt::Display for PciEcamAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidWidth { len } => {
                write!(f, "PCI ECAM access width {len} is not 1, 2, or 4 bytes")
            }
            Self::OutsideBusZero { offset } => {
                write!(f, "PCI ECAM offset 0x{offset:x} is outside bus 0")
            }
            Self::AddressOverflow => f.write_str("PCI ECAM access address overflowed"),
            Self::CrossesRegister { offset, len } => write!(
                f,
                "PCI ECAM access offset 0x{offset:x} width {len} crosses a dword boundary"
            ),
            Self::Configuration { source } => write!(f, "PCI ECAM configuration failed: {source}"),
        }
    }
}

impl std::error::Error for PciEcamAccessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Configuration { source } => Some(source),
            Self::InvalidWidth { .. }
            | Self::OutsideBusZero { .. }
            | Self::AddressOverflow
            | Self::CrossesRegister { .. } => None,
        }
    }
}

#[derive(Clone)]
pub struct SharedPciSegment {
    state: Arc<Mutex<PciSegment>>,
}

impl SharedPciSegment {
    pub fn new(segment: PciSegment) -> Self {
        Self {
            state: Arc::new(Mutex::new(segment)),
        }
    }

    pub fn with_segment<T>(
        &self,
        operation: impl FnOnce(&mut PciSegment) -> T,
    ) -> Result<T, PciSegmentLockError> {
        let mut segment = self
            .state
            .lock()
            .map_err(|_| PciSegmentLockError::Poisoned)?;
        Ok(operation(&mut segment))
    }
}

impl fmt::Debug for SharedPciSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedPciSegment")
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciSegmentLockError {
    Poisoned,
}

impl fmt::Display for PciSegmentLockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PCI segment state lock is poisoned")
    }
}

impl std::error::Error for PciSegmentLockError {}

#[derive(Debug)]
pub struct PciEcamHandler {
    segment: SharedPciSegment,
}

impl PciEcamHandler {
    pub fn new(segment: SharedPciSegment) -> Self {
        Self { segment }
    }
}

impl MmioHandler for PciEcamHandler {
    fn read(&mut self, access: MmioAccess) -> Result<MmioAccessBytes, MmioHandlerError> {
        let mut bytes = [0_u8; 4];
        let len = usize::try_from(access.range().size())
            .map_err(|_| MmioHandlerError::new("PCI ECAM read width is not representable"))?;
        let destination = bytes
            .get_mut(..len)
            .ok_or_else(|| MmioHandlerError::new("PCI ECAM read width exceeds four bytes"))?;
        let result = self
            .segment
            .with_segment(|segment| segment.read_ecam(access.offset(), destination))
            .map_err(|source| MmioHandlerError::new(source.to_string()))?;
        match result {
            Ok(()) => {}
            Err(PciEcamAccessError::CrossesRegister { .. }) => destination.fill(u8::MAX),
            Err(source) => return Err(MmioHandlerError::new(source.to_string())),
        }
        MmioAccessBytes::new(destination)
            .map_err(|source| MmioHandlerError::new(source.to_string()))
    }

    fn write(&mut self, access: MmioAccess, data: MmioAccessBytes) -> Result<(), MmioHandlerError> {
        let result = self
            .segment
            .with_segment(|segment| segment.write_ecam(access.offset(), data.as_slice()))
            .map_err(|source| MmioHandlerError::new(source.to_string()))?;
        match result {
            Ok(()) | Err(PciEcamAccessError::CrossesRegister { .. }) => Ok(()),
            Err(source) => Err(MmioHandlerError::new(source.to_string())),
        }
    }
}

pub fn register_ecam_handler(
    dispatcher: &mut crate::mmio::MmioDispatcher,
    owner: &MmioRegistrationOwner,
    region_id: MmioRegionId,
    plan: Arm64PciAddressPlan,
    segment: SharedPciSegment,
) -> Result<MmioRegistrationLease, MmioRegistrationError> {
    dispatcher.register_owned_handler(
        owner,
        region_id,
        &[MmioRegionRequest::new(
            plan.ecam().start(),
            plan.ecam().size(),
        )],
        PciEcamHandler::new(segment),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mmio::{
        MmioBusError, MmioDispatchOutcome, MmioDispatcher, MmioOperation,
        MmioRegistrationReleaseError,
    };

    fn range(start: u64, size: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(GuestAddress::new(start), size)
            .expect("test PCI range should be valid")
    }

    fn endpoint(vendor_id: u16, device_id: u16) -> PciType0Configuration {
        PciType0Configuration::new(
            vendor_id,
            device_id,
            1,
            PciClassCode::Unclassified,
            0,
            0,
            vendor_id,
            device_id,
        )
    }

    fn read_config_u32(function: &mut impl PciConfigFunction, offset: u16) -> u32 {
        let mut bytes = [0; 4];
        function
            .read_config(offset, &mut bytes)
            .expect("test PCI configuration read should succeed");
        u32::from_le_bytes(bytes)
    }

    fn read_ecam_u32(segment: &mut PciSegment, offset: u64) -> u32 {
        let mut bytes = [0; 4];
        segment
            .read_ecam(offset, &mut bytes)
            .expect("test PCI ECAM read should succeed");
        u32::from_le_bytes(bytes)
    }

    #[test]
    fn sbdf_validates_and_encodes_firecracker_identity() {
        let sbdf = PciSbdf::new(0, 0, 31, 7).expect("maximum PCI SBDF should be valid");

        assert_eq!(sbdf.to_string(), "0000:00:1f.7");
        assert_eq!(sbdf.ecam_offset(), 31 << 15 | 7 << 12);
        assert_eq!(
            PciSbdf::new(0, 0, 32, 0),
            Err(PciIdentityError::InvalidDevice { device: 32 })
        );
        assert_eq!(
            PciSbdf::new(0, 0, 0, 8),
            Err(PciIdentityError::InvalidFunction { function: 8 })
        );
    }

    #[test]
    fn arm64_address_plan_matches_pinned_firecracker_boundaries() {
        let plan = Arm64PciAddressPlan::firecracker_v1_16()
            .expect("pinned PCI address plan should be valid");

        assert_eq!(plan.ecam(), range(0x7000_0000, 1 << 20));
        assert_eq!(plan.ecam_reservation(), range(0x7000_0000, 256 << 20));
        assert_eq!(plan.bar32().start(), GuestAddress::new(0x4000_3000));
        assert_eq!(plan.bar32().end_exclusive(), GuestAddress::new(0x7000_0000));
        assert_eq!(plan.bar64(), range(256 << 30, 256 << 30));
        assert!(!plan.bar32().overlaps(plan.ecam_reservation()));
        assert!(!plan.ecam_reservation().overlaps(plan.bar64()));
    }

    #[test]
    fn bar_allocator_rejects_invalid_sizes_without_mutation() {
        let capacity = range(0x1000, 0x4000);
        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory32, capacity);

        for size in [0, 8, 24] {
            assert!(matches!(
                allocator.allocate(size),
                Err(PciBarAllocationError::InvalidSize { size: actual }) if actual == size
            ));
        }
        assert_eq!(allocator.available_ranges(), &[capacity]);
    }

    #[test]
    fn bar_allocator_exhaustively_reuses_fragmented_small_ranges() {
        for start_offset in 0..16 {
            for size in [16, 32, 64] {
                let capacity = range(0x1000 + start_offset, 0x180);
                let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory32, capacity);
                let mut leases = Vec::new();

                loop {
                    let expected = allocator.available_ranges().iter().find_map(|free| {
                        let start = align_up(free.start().raw_value(), size)?;
                        let end = start.checked_add(size)?;
                        (end <= free.end_exclusive().raw_value()).then(|| range(start, size))
                    });
                    let Some(expected) = expected else {
                        assert!(matches!(
                            allocator.allocate(size),
                            Err(PciBarAllocationError::Exhausted { size: exhausted })
                                if exhausted == size
                        ));
                        break;
                    };
                    let lease = allocator
                        .allocate(size)
                        .expect("reference-model PCI BAR allocation should fit");
                    assert_eq!(lease.range(), expected);
                    leases.push(Some(lease));
                }

                let released_count = leases.len().div_ceil(2);
                for lease in leases.iter_mut().step_by(2) {
                    allocator
                        .release(
                            lease
                                .take()
                                .as_ref()
                                .expect("fragmented PCI BAR lease should exist"),
                        )
                        .expect("fragmented PCI BAR release should succeed");
                }

                let mut replacements = Vec::new();
                for _ in 0..released_count {
                    let expected = allocator
                        .available_ranges()
                        .iter()
                        .find_map(|free| {
                            let start = align_up(free.start().raw_value(), size)?;
                            let end = start.checked_add(size)?;
                            (end <= free.end_exclusive().raw_value()).then(|| range(start, size))
                        })
                        .expect("each released aligned hole should be reusable");
                    let replacement = allocator
                        .allocate(size)
                        .expect("released PCI BAR hole should allocate");
                    assert_eq!(replacement.range(), expected);
                    replacements.push(replacement);
                }

                for lease in leases.into_iter().flatten().chain(replacements) {
                    allocator
                        .release(&lease)
                        .expect("remaining PCI BAR lease should release");
                }
                assert_eq!(allocator.available_ranges(), &[capacity]);
            }
        }
    }

    #[test]
    fn bar_allocator_overflow_generation_and_lease_mismatch_are_mutation_free() {
        let overflow_capacity = range(u64::MAX - 7, 7);
        let mut overflow = PciBarAllocator::new(PciBarAddressSpace::Memory64, overflow_capacity);
        assert!(matches!(
            overflow.allocate(16),
            Err(PciBarAllocationError::AddressOverflow)
        ));
        assert_eq!(overflow.available_ranges(), &[overflow_capacity]);

        let capacity = range(0x1000, 0x2000);
        let mut exhausted = PciBarAllocator::new(PciBarAddressSpace::Memory32, capacity);
        exhausted.next_generation = u64::MAX;
        assert!(matches!(
            exhausted.allocate(0x1000),
            Err(PciBarAllocationError::GenerationExhausted)
        ));
        assert_eq!(exhausted.available_ranges(), &[capacity]);
        assert!(exhausted.allocations.is_empty());

        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory32, capacity);
        let mut lease = allocator
            .allocate(0x1000)
            .expect("test PCI BAR should allocate");
        let original_range = lease.range;
        let free_before = allocator.available_ranges().to_vec();
        lease.range = range(0x4000, 0x1000);
        assert_eq!(
            allocator.release(&lease),
            Err(PciBarReleaseError::LeaseMismatch)
        );
        assert_eq!(allocator.available_ranges(), free_before);
        assert_eq!(allocator.allocations.len(), 1);
        lease.range = original_range;
        allocator
            .release(&lease)
            .expect("original exact PCI BAR lease should still release");
        assert_eq!(allocator.available_ranges(), &[capacity]);
    }

    #[test]
    fn bar_allocator_aligns_splits_releases_and_reuses_lowest_range() {
        let capacity = range(0x1003, 0x3ffd);
        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory32, capacity);
        let first = allocator
            .allocate(0x1000)
            .expect("first aligned PCI BAR should allocate");
        let second = allocator
            .allocate(0x1000)
            .expect("second aligned PCI BAR should allocate");

        assert_eq!(first.range(), range(0x2000, 0x1000));
        assert_eq!(second.range(), range(0x3000, 0x1000));
        allocator
            .release(&first)
            .expect("first PCI BAR release should succeed");
        let reused = allocator
            .allocate(0x1000)
            .expect("released PCI BAR should be reusable");
        assert_eq!(reused.range(), first.range());
        allocator
            .release(&second)
            .expect("second PCI BAR release should succeed");
        allocator
            .release(&reused)
            .expect("reused PCI BAR release should succeed");
        assert_eq!(allocator.available_ranges(), &[capacity]);
    }

    #[test]
    fn bar_allocator_rejects_foreign_and_stale_leases() {
        let mut first = PciBarAllocator::new(
            PciBarAddressSpace::Memory64,
            range(PCI_BAR64_START, 0x20_0000),
        );
        let mut second = PciBarAllocator::new(
            PciBarAddressSpace::Memory64,
            range(PCI_BAR64_START, 0x20_0000),
        );
        let lease = first
            .allocate(0x10_0000)
            .expect("test PCI BAR should allocate");

        assert_eq!(
            second.release(&lease),
            Err(PciBarReleaseError::WrongAllocator)
        );
        first
            .release(&lease)
            .expect("test PCI BAR should release once");
        assert_eq!(first.release(&lease), Err(PciBarReleaseError::StaleLease));
    }

    #[test]
    fn bar_allocator_reports_deterministic_exhaustion() {
        let mut allocator =
            PciBarAllocator::new(PciBarAddressSpace::Memory32, range(0x1000, 0x2000));
        let _first = allocator
            .allocate(0x1000)
            .expect("first PCI BAR should fit");
        let _second = allocator
            .allocate(0x1000)
            .expect("second PCI BAR should fit");

        assert!(matches!(
            allocator.allocate(0x1000),
            Err(PciBarAllocationError::Exhausted { size: 0x1000 })
        ));
    }

    #[test]
    fn type0_host_bridge_header_and_masks_match_firecracker() {
        let mut configuration = PciType0Configuration::firecracker_host_bridge();

        assert_eq!(read_config_u32(&mut configuration, 0), 0x0d57_8086);
        assert_eq!(read_config_u32(&mut configuration, 8), 0x0600_0000);
        configuration
            .write_config(0, &u32::MAX.to_le_bytes())
            .expect("identity write should be accepted and masked");
        assert_eq!(read_config_u32(&mut configuration, 0), 0x0d57_8086);
        configuration
            .write_config(4, &0x0000_0007_u32.to_le_bytes())
            .expect("command write should succeed");
        assert_eq!(read_config_u32(&mut configuration, 4) & 0xffff, 7);
    }

    #[test]
    fn type0_capability_chain_is_aligned_linked_and_header_immutable() {
        let mut configuration = endpoint(0x1af4, 0x1044);
        let first_body = [14, 1, 0, 0, 0, 0, 0, 0, 56, 0, 0, 0, 0, 0];
        let first = configuration
            .add_capability(PciCapabilityId::VendorSpecific, &first_body, &[0; 14])
            .expect("first capability should fit");
        let second_body = [0x01, 0x80, 0x00, 0x80, 0, 0, 0, 0x80, 4, 0];
        let mut second_mask = [0; 10];
        second_mask[1] = 0xc0;
        let second = configuration
            .add_capability(PciCapabilityId::MsiX, &second_body, &second_mask)
            .expect("second capability should fit");

        assert_eq!(first, 0x40);
        assert_eq!(second, 0x50);
        assert_eq!(configuration.capability_count(), 2);
        assert_ne!(
            read_config_u32(&mut configuration, 4) & PCI_STATUS_CAPABILITY_LIST_MASK,
            0
        );
        assert_eq!(read_config_u32(&mut configuration, 0x34) & 0xff, 0x40);
        assert_eq!(read_config_u32(&mut configuration, 0x40), 0x010e_5009);
        assert_eq!(read_config_u32(&mut configuration, 0x50), 0x8001_0011);

        configuration
            .write_config(0x40, &[0xff, 0xff, 0xff, 0xff])
            .expect("capability header write should be accepted as a no-op");
        assert_eq!(read_config_u32(&mut configuration, 0x40), 0x010e_5009);
        configuration
            .write_config(0x52, &0x4001_u16.to_le_bytes())
            .expect("MSI-X control write should be accepted");
        assert_eq!(read_config_u32(&mut configuration, 0x50), 0x4001_0011);
    }

    #[test]
    fn type0_capability_failures_leave_existing_chain_unchanged() {
        let mut configuration = endpoint(0x1af4, 0x1044);
        let first_body = vec![0; PCI_CAPABILITY_END_EXCLUSIVE - PCI_FIRST_CAPABILITY_OFFSET - 2];
        let first_mask = vec![0; first_body.len()];
        let first = configuration
            .add_capability(PciCapabilityId::VendorSpecific, &first_body, &first_mask)
            .expect("one maximum capability should fit");
        assert_eq!(first, 0x40);
        let before_head = read_config_u32(&mut configuration, 0x34);
        let before_first = read_config_u32(&mut configuration, 0x40);

        assert_eq!(
            configuration.add_capability(PciCapabilityId::MsiX, &[0; 10], &[0; 9]),
            Err(PciCapabilityError::WritableMaskLength { body: 10, mask: 9 })
        );
        assert!(matches!(
            configuration.add_capability(PciCapabilityId::MsiX, &[0; 10], &[0; 10]),
            Err(PciCapabilityError::NoSpace { .. })
        ));
        assert_eq!(read_config_u32(&mut configuration, 0x34), before_head);
        assert_eq!(read_config_u32(&mut configuration, 0x40), before_first);
        assert_eq!(configuration.capability_count(), 1);
    }

    #[test]
    fn type0_configuration_rejects_invalid_width_and_crossing() {
        let mut configuration = endpoint(0x1af4, 0x10ff);
        let mut three = [0; 3];
        let mut outside = [0xa5];

        assert_eq!(
            configuration.read_config(0, &mut three),
            Err(PciConfigAccessError::InvalidWidth { len: 3 })
        );
        assert_eq!(
            configuration.write_config(3, &[1, 2]),
            Err(PciConfigAccessError::CrossesRegister { offset: 3, len: 2 })
        );
        assert_eq!(
            configuration.read_config(PCI_CONFIG_SPACE_SIZE as u16, &mut outside),
            Err(PciConfigAccessError::OutsideConfigurationSpace {
                offset: PCI_CONFIG_SPACE_SIZE as u16,
                len: 1,
            })
        );
        assert_eq!(outside, [0xa5]);
        assert_eq!(
            configuration.write_config((PCI_CONFIG_SPACE_SIZE - 1) as u16, &[1, 2]),
            Err(PciConfigAccessError::OutsideConfigurationSpace {
                offset: (PCI_CONFIG_SPACE_SIZE - 1) as u16,
                len: 2,
            })
        );
    }

    #[test]
    fn type0_configuration_supports_partial_little_endian_accesses_and_masks() {
        let mut configuration = endpoint(0x1af4, 0x10ff);
        let mut identity_slice = [0; 2];
        configuration
            .read_config(1, &mut identity_slice)
            .expect("in-dword identity read should succeed");
        assert_eq!(identity_slice, [0x1a, 0xff]);

        configuration
            .write_config(0, &[0, 0])
            .expect("partial immutable identity write should be ignored");
        assert_eq!(read_config_u32(&mut configuration, 0), 0x10ff_1af4);

        configuration
            .write_config(4, &[0x07])
            .expect("command low-byte write should succeed");
        configuration
            .write_config(5, &[0x80])
            .expect("command high-byte write should succeed");
        assert_eq!(read_config_u32(&mut configuration, 4), 0x0000_8007);

        configuration
            .write_config(12, &[0x40, 0xff, 0xff, 0xff])
            .expect("cacheline/header partial mask write should succeed");
        assert_eq!(read_config_u32(&mut configuration, 12), 0x0000_0040);

        configuration
            .write_config(60, &[0x2a, 0xff])
            .expect("interrupt line/pin write should succeed");
        assert_eq!(read_config_u32(&mut configuration, 60), 0x0000_002a);
        assert_eq!(
            read_config_u32(&mut configuration, (PCI_CONFIG_SPACE_SIZE - 4) as u16),
            0
        );
    }

    #[test]
    fn type0_configuration_probes_and_preserves_32_bit_bar() {
        let mut allocator =
            PciBarAllocator::new(PciBarAddressSpace::Memory32, range(0x5000_0000, 0x20_0000));
        let lease = allocator
            .allocate(0x1000)
            .expect("test 32-bit BAR should allocate");
        let mut configuration = endpoint(0x1af4, 0x10ff);
        configuration
            .install_bar(0, &lease, PciBarPrefetchable::No)
            .expect("test 32-bit BAR should install");

        assert_eq!(read_config_u32(&mut configuration, 0x10), 0x5000_0000);
        configuration
            .write_config(0x10, &u32::MAX.to_le_bytes())
            .expect("BAR probe write should succeed");
        assert_eq!(read_config_u32(&mut configuration, 0x10), 0xffff_f000);
        assert_eq!(read_config_u32(&mut configuration, 0x10), 0x5000_0000);
        configuration
            .write_config(0x10, &0x6000_0000_u32.to_le_bytes())
            .expect("unsupported BAR relocation write should be ignored");
        assert_eq!(read_config_u32(&mut configuration, 0x10), 0x5000_0000);
    }

    #[test]
    fn type0_configuration_encodes_64_bit_prefetchable_bar() {
        let mut allocator = PciBarAllocator::new(
            PciBarAddressSpace::Memory64,
            range(PCI_BAR64_START, 0x20_0000),
        );
        let lease = allocator
            .allocate(0x80_000)
            .expect("test 64-bit BAR should allocate");
        let mut configuration = endpoint(0x1af4, 0x10ff);
        configuration
            .install_bar(0, &lease, PciBarPrefetchable::Yes)
            .expect("test 64-bit BAR should install");

        assert_eq!(read_config_u32(&mut configuration, 0x10), 0x0000_000c);
        assert_eq!(read_config_u32(&mut configuration, 0x14), 0x0000_0040);
        configuration
            .write_config(0x10, &u32::MAX.to_le_bytes())
            .expect("low BAR probe write should succeed");
        configuration
            .write_config(0x14, &u32::MAX.to_le_bytes())
            .expect("high BAR probe write should succeed");
        assert_eq!(read_config_u32(&mut configuration, 0x10), 0xfff8_0000);
        assert_eq!(read_config_u32(&mut configuration, 0x14), 0xffff_ffff);
    }

    #[test]
    fn segment_allocates_all_slots_then_reuses_released_slot() {
        let mut segment = PciSegment::new();
        let mut leases = Vec::new();
        for expected_device in PCI_FIRST_ENDPOINT_DEVICE..=PCI_LAST_ENDPOINT_DEVICE {
            let lease = segment
                .add_function(endpoint(0x1af4, 0x10ff))
                .expect("PCI endpoint slot should allocate");
            assert_eq!(lease.sbdf().device(), expected_device);
            leases.push(lease);
        }
        assert_eq!(segment.function_count(), 32);
        assert!(matches!(
            segment.add_function(endpoint(0x1af4, 0x10ff)),
            Err(PciSegmentError::NoDeviceSlot)
        ));

        let released = leases
            .get(7)
            .expect("test PCI lease should exist at slot 8");
        segment
            .remove_function(released)
            .expect("test PCI function should remove");
        let reused = segment
            .add_function(endpoint(0x1af4, 0x10ff))
            .expect("released PCI function slot should be reused");
        assert_eq!(reused.sbdf().device(), 8);
    }

    #[test]
    fn segment_unpublishes_before_releasing_a_reserved_slot() {
        let mut segment = PciSegment::new();
        let lease = segment
            .add_function(endpoint(0x1af4, 0x10ff))
            .expect("test endpoint should insert");
        let sbdf = lease.sbdf();

        segment
            .unpublish_function(&lease)
            .expect("test endpoint should become unreachable");
        assert_eq!(segment.function_count(), 1);
        assert_eq!(
            read_ecam_u32(&mut segment, u64::from(sbdf.ecam_offset())),
            u32::MAX
        );
        assert!(matches!(
            segment.add_function_at(sbdf, endpoint(0x1af4, 0x10ff)),
            Err(PciSegmentError::DuplicateIdentity { sbdf: duplicate }) if duplicate == sbdf
        ));
        let next = segment
            .add_function(endpoint(0x1af4, 0x10ff))
            .expect("reserved slot should be skipped by automatic allocation");
        assert_eq!(next.sbdf().device(), sbdf.device() + 1);
        segment
            .remove_function(&next)
            .expect("temporary next slot should release");
        assert_eq!(
            segment.remove_function(&lease),
            Err(PciFunctionReleaseError::LeaseMismatch { sbdf })
        );

        segment
            .republish_function(&lease)
            .expect("suspended function should republish with the same lease");
        assert_eq!(segment.function_count(), 2);
        assert_eq!(
            read_ecam_u32(&mut segment, u64::from(sbdf.ecam_offset())),
            0x10ff_1af4
        );
        segment
            .unpublish_function(&lease)
            .expect("republished function should suspend again");

        segment
            .release_function_lease(&lease)
            .expect("unpublished slot should release");
        let replacement = segment
            .add_function_at(sbdf, endpoint(0x1af4, 0x10ff))
            .expect("released slot should be reusable");
        assert_eq!(replacement.sbdf(), sbdf);
    }

    #[test]
    fn segment_rejects_duplicate_unsupported_foreign_and_stale_leases() {
        let mut first = PciSegment::new();
        let mut second = PciSegment::new();
        let sbdf = PciSbdf::new(0, 0, 1, 0).expect("test PCI SBDF should be valid");
        let lease = first
            .add_function_at(sbdf, endpoint(0x1af4, 0x10ff))
            .expect("test endpoint should insert");

        assert!(matches!(
            first.add_function_at(sbdf, endpoint(0x1af4, 0x10ff)),
            Err(PciSegmentError::DuplicateIdentity { sbdf: duplicate }) if duplicate == sbdf
        ));
        let unsupported = PciSbdf::new(1, 0, 2, 0).expect("test PCI SBDF should be valid");
        assert!(matches!(
            first.add_function_at(unsupported, endpoint(0x1af4, 0x10ff)),
            Err(PciSegmentError::UnsupportedIdentity { sbdf: actual }) if actual == unsupported
        ));
        assert_eq!(
            second.remove_function(&lease),
            Err(PciFunctionReleaseError::WrongSegment)
        );
        first
            .remove_function(&lease)
            .expect("test endpoint should remove once");
        assert_eq!(
            first.remove_function(&lease),
            Err(PciFunctionReleaseError::StaleLease { sbdf })
        );
    }

    #[test]
    fn segment_generation_and_lease_mismatch_fail_without_mutation() {
        let mut exhausted = PciSegment::new();
        exhausted.next_generation = u64::MAX;
        assert!(matches!(
            exhausted.add_function(endpoint(0x1af4, 0x10ff)),
            Err(PciSegmentError::GenerationExhausted)
        ));
        assert_eq!(exhausted.function_count(), 1);
        assert!(exhausted.records.is_empty());

        let mut segment = PciSegment::new();
        let mut lease = segment
            .add_function(endpoint(0x1af4, 0x10ff))
            .expect("test PCI endpoint should allocate");
        let original_generation = lease.generation;
        lease.generation = lease
            .generation
            .checked_add(1)
            .expect("test lease generation should increment");
        assert_eq!(
            segment.remove_function(&lease),
            Err(PciFunctionReleaseError::LeaseMismatch { sbdf: lease.sbdf() })
        );
        assert_eq!(segment.function_count(), 2);
        lease.generation = original_generation;
        segment
            .remove_function(&lease)
            .expect("original exact PCI function lease should still release");
        assert_eq!(segment.function_count(), 1);
    }

    #[test]
    fn ecam_exposes_host_endpoint_and_absent_functions() {
        let mut segment = PciSegment::new();
        let endpoint_lease = segment
            .add_function(endpoint(0x1af4, 0x10ff))
            .expect("test endpoint should insert");

        assert_eq!(read_ecam_u32(&mut segment, 0), 0x0d57_8086);
        assert_eq!(
            read_ecam_u32(&mut segment, u64::from(endpoint_lease.sbdf().ecam_offset())),
            0x10ff_1af4
        );
        assert_eq!(read_ecam_u32(&mut segment, 2 << 15), u32::MAX);
        assert_eq!(read_ecam_u32(&mut segment, 1 << 12), u32::MAX);
        assert_eq!(
            read_ecam_u32(
                &mut segment,
                u64::from(endpoint_lease.sbdf().ecam_offset()) + 0x100
            ),
            0
        );
        assert_eq!(read_ecam_u32(&mut segment, (2 << 15) + 0x100), u32::MAX);
    }

    #[test]
    fn ecam_rejects_invalid_width_crossing_and_bus_overflow() {
        let mut segment = PciSegment::new();
        let mut invalid = [0; 8];

        assert!(matches!(
            segment.read_ecam(0, &mut invalid),
            Err(PciEcamAccessError::InvalidWidth { len: 8 })
        ));
        assert!(matches!(
            segment.write_ecam(3, &[1, 2]),
            Err(PciEcamAccessError::CrossesRegister { offset: 3, len: 2 })
        ));
        assert!(matches!(
            segment.read_ecam(PCI_ECAM_BUS_ZERO_SIZE, &mut invalid[..4]),
            Err(PciEcamAccessError::OutsideBusZero { .. })
        ));
    }

    #[test]
    fn leased_ecam_handler_dispatches_and_removes_atomically() {
        let plan = Arm64PciAddressPlan::firecracker_v1_16()
            .expect("test PCI address plan should be valid");
        let shared = SharedPciSegment::new(PciSegment::new());
        let endpoint_lease = shared
            .with_segment(|segment| segment.add_function(endpoint(0x1af4, 0x10ff)))
            .expect("test PCI segment lock should succeed")
            .expect("test endpoint should insert");
        let owner = MmioRegistrationOwner::new();
        let mut dispatcher = MmioDispatcher::new();
        let lease = register_ecam_handler(
            &mut dispatcher,
            &owner,
            MmioRegionId::new(9000),
            plan,
            shared,
        )
        .expect("test ECAM handler should register");
        let address = plan
            .ecam()
            .start()
            .checked_add(u64::from(endpoint_lease.sbdf().ecam_offset()))
            .expect("test ECAM address should not overflow");
        let access = dispatcher
            .lookup(address, 4)
            .expect("test ECAM access should resolve");

        assert_eq!(
            dispatcher
                .dispatch(MmioOperation::read(access).expect("test ECAM read should build"))
                .expect("test ECAM read should dispatch"),
            MmioDispatchOutcome::Read {
                data: MmioAccessBytes::new(&0x10ff_1af4_u32.to_le_bytes())
                    .expect("test ECAM bytes should be valid")
            }
        );
        dispatcher
            .release_owned_handler(&owner, &lease)
            .expect("test ECAM lease should release");
        assert!(matches!(
            dispatcher.lookup(address, 4),
            Err(MmioBusError::UnownedAccess { .. })
        ));
        assert_eq!(
            dispatcher.release_owned_handler(&owner, &lease),
            Err(MmioRegistrationReleaseError::StaleLease {
                region_id: MmioRegionId::new(9000)
            })
        );
    }

    #[test]
    fn ecam_handler_returns_ones_and_ignores_writes_crossing_a_dword() {
        let plan = Arm64PciAddressPlan::firecracker_v1_16()
            .expect("test PCI address plan should be valid");
        let owner = MmioRegistrationOwner::new();
        let mut dispatcher = MmioDispatcher::new();
        let _lease = register_ecam_handler(
            &mut dispatcher,
            &owner,
            MmioRegionId::new(9001),
            plan,
            SharedPciSegment::new(PciSegment::new()),
        )
        .expect("test ECAM handler should register");
        let crossing_address = plan
            .ecam()
            .start()
            .checked_add(3)
            .expect("test crossing address should not overflow");
        let crossing_access = dispatcher
            .lookup(crossing_address, 2)
            .expect("test crossing ECAM access should resolve");

        assert_eq!(
            dispatcher
                .dispatch(MmioOperation::read(crossing_access).expect("test read should build"))
                .expect("crossing ECAM read should dispatch"),
            MmioDispatchOutcome::Read {
                data: MmioAccessBytes::new(&[u8::MAX; 2])
                    .expect("test all-ones bytes should be valid")
            }
        );
        assert_eq!(
            dispatcher
                .dispatch(
                    MmioOperation::write(
                        crossing_access,
                        MmioAccessBytes::new(&[1, 2]).expect("test write bytes should be valid"),
                    )
                    .expect("test write should build"),
                )
                .expect("crossing ECAM write should be ignored"),
            MmioDispatchOutcome::Write
        );
    }

    #[test]
    fn ownership_debug_output_is_redacted() {
        let mut allocator =
            PciBarAllocator::new(PciBarAddressSpace::Memory32, range(0x1000, 0x1000));
        let bar = allocator
            .allocate(0x1000)
            .expect("test PCI BAR should allocate");
        let mut segment = PciSegment::new();
        let function = segment
            .add_function(endpoint(0x1af4, 0x10ff))
            .expect("test PCI function should allocate");

        assert_eq!(
            format!("{bar:?}"),
            "PciBarLease { ownership: \"<redacted>\" }"
        );
        assert_eq!(
            format!("{function:?}"),
            "PciFunctionLease { ownership: \"<redacted>\" }"
        );

        let mut configuration = endpoint(0x1af4, 0x10ff);
        configuration
            .install_bar(0, &bar, PciBarPrefetchable::No)
            .expect("test PCI BAR should install");
        assert_eq!(
            format!("{configuration:?}"),
            "PciType0Configuration { configuration: \"<redacted>\", configured_bar_registers: 1, capability_count: 0 }"
        );
    }
}
