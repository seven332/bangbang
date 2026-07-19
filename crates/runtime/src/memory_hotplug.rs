//! Backend-neutral memory hotplug configuration model.

use std::collections::TryReserveError;
use std::fmt;

use crate::memory::{
    GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryError, GuestMemoryRange, aarch64,
};
use crate::mmio::{
    MmioAccessBytes, MmioAccessBytesError, MmioBusError, MmioDispatchError, MmioDispatcher,
    MmioHandlerError, MmioHandlerLookupError, MmioRegion, MmioRegionId,
};
use crate::virtio::VirtioInterruptIntent;
use crate::virtio_mmio::{
    VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioDeviceActivation, VirtioMmioDeviceActivationError,
    VirtioMmioDeviceActivationHandler, VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError,
    VirtioMmioDeviceConfigHandler, VirtioMmioQueueRegisterError, VirtioMmioQueueState,
    VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError,
};
use crate::virtio_pci::{VirtioPciDeviceOperationError, VirtioPciEndpoint, VirtioPciEndpointError};
use crate::virtio_queue::{
    VirtqueueAvailableRing, VirtqueueAvailableRingError, VirtqueueDescriptor,
    VirtqueueDescriptorChain, VirtqueueNotificationSuppression, VirtqueueUsedRing,
    VirtqueueUsedRingError, VirtqueueUsedRingPublication,
};

const MIB: u64 = 1024 * 1024;

pub const MEMORY_HOTPLUG_DEFAULT_BLOCK_SIZE_MIB: u64 = 2;
pub const MEMORY_HOTPLUG_DEFAULT_SLOT_SIZE_MIB: u64 = 128;
pub const VIRTIO_MEM_DEFAULT_REGION_ADDRESS: GuestAddress =
    GuestAddress::new(aarch64::FIRST_ADDR_PAST_64BITS_MMIO);
const VIRTIO_MEM_REGION_ADDRESS_SPACE_END: u64 =
    aarch64::DRAM_MEM_START + aarch64::DRAM_MEM_MAX_SIZE;
pub const VIRTIO_MEM_DEVICE_ID: u32 = 24;
pub const VIRTIO_MEM_QUEUE_COUNT: usize = 1;
pub const VIRTIO_MEM_QUEUE_SIZE: u16 = 256;
pub const VIRTIO_MEM_QUEUE_SIZES: [u16; VIRTIO_MEM_QUEUE_COUNT] = [VIRTIO_MEM_QUEUE_SIZE];
pub const VIRTIO_MEM_CONFIG_SPACE_SIZE: usize = 56;
pub const VIRTIO_MEM_REQUEST_SIZE: usize = 24;
pub const VIRTIO_MEM_RESPONSE_SIZE: usize = 10;
pub const VIRTIO_MEM_F_UNPLUGGED_INACCESSIBLE: u32 = 1;
pub const VIRTIO_FEATURE_VERSION_1: u32 = 32;

const VIRTIO_MEM_REQ_PLUG: u16 = 0;
const VIRTIO_MEM_REQ_UNPLUG: u16 = 1;
const VIRTIO_MEM_REQ_UNPLUG_ALL: u16 = 2;
const VIRTIO_MEM_REQ_STATE: u16 = 3;
const VIRTIO_MEM_RESP_ACK: u16 = 0;
const VIRTIO_MEM_RESP_ERROR: u16 = 3;
const VIRTIO_MEM_STATE_PLUGGED: u16 = 0;
const VIRTIO_MEM_STATE_UNPLUGGED: u16 = 1;
const VIRTIO_MEM_STATE_MIXED: u16 = 2;
const VIRTIO_MEM_REQUEST_SIZE_U32: u32 = VIRTIO_MEM_REQUEST_SIZE as u32;
const VIRTIO_MEM_RESPONSE_SIZE_U32: u32 = VIRTIO_MEM_RESPONSE_SIZE as u32;

pub type VirtioMemMmioHandler = VirtioMmioRegisterHandler<VirtioMemConfigSpace, VirtioMemDevice>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryHotplugConfigInput {
    total_size_mib: u64,
    block_size_mib: u64,
    slot_size_mib: u64,
}

impl MemoryHotplugConfigInput {
    pub const fn new(total_size_mib: u64, block_size_mib: u64, slot_size_mib: u64) -> Self {
        Self {
            total_size_mib,
            block_size_mib,
            slot_size_mib,
        }
    }

    pub const fn total_size_mib(self) -> u64 {
        self.total_size_mib
    }

    pub const fn block_size_mib(self) -> u64 {
        self.block_size_mib
    }

    pub const fn slot_size_mib(self) -> u64 {
        self.slot_size_mib
    }

    pub fn validate(self) -> Result<(), MemoryHotplugConfigError> {
        if self.block_size_mib < MEMORY_HOTPLUG_DEFAULT_BLOCK_SIZE_MIB {
            return Err(MemoryHotplugConfigError::BlockSizeTooSmall {
                min_mib: MEMORY_HOTPLUG_DEFAULT_BLOCK_SIZE_MIB,
            });
        }
        if !self.block_size_mib.is_power_of_two() {
            return Err(MemoryHotplugConfigError::BlockSizeNotPowerOfTwo);
        }

        if self.slot_size_mib < MEMORY_HOTPLUG_DEFAULT_SLOT_SIZE_MIB {
            return Err(MemoryHotplugConfigError::SlotSizeTooSmall {
                min_mib: MEMORY_HOTPLUG_DEFAULT_SLOT_SIZE_MIB,
            });
        }
        if !self.slot_size_mib.is_multiple_of(self.block_size_mib) {
            return Err(MemoryHotplugConfigError::SlotSizeNotMultipleOfBlockSize {
                block_size_mib: self.block_size_mib,
            });
        }

        if self.total_size_mib < self.slot_size_mib {
            return Err(MemoryHotplugConfigError::TotalSizeTooSmall {
                slot_size_mib: self.slot_size_mib,
            });
        }
        if !self.total_size_mib.is_multiple_of(self.slot_size_mib) {
            return Err(MemoryHotplugConfigError::TotalSizeNotMultipleOfSlotSize {
                slot_size_mib: self.slot_size_mib,
            });
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryHotplugConfig {
    total_size_mib: u64,
    block_size_mib: u64,
    slot_size_mib: u64,
}

impl MemoryHotplugConfig {
    const fn new(total_size_mib: u64, block_size_mib: u64, slot_size_mib: u64) -> Self {
        Self {
            total_size_mib,
            block_size_mib,
            slot_size_mib,
        }
    }

    pub const fn total_size_mib(self) -> u64 {
        self.total_size_mib
    }

    pub const fn block_size_mib(self) -> u64 {
        self.block_size_mib
    }

    pub const fn slot_size_mib(self) -> u64 {
        self.slot_size_mib
    }

    pub const fn initial_status(self) -> MemoryHotplugStatus {
        MemoryHotplugStatus::new(self, 0, 0)
    }

    pub fn validate_size_update(
        self,
        input: MemoryHotplugSizeUpdateInput,
    ) -> Result<MemoryHotplugSizeUpdate, MemoryHotplugUpdateError> {
        let requested_size =
            memory_hotplug_mib_to_bytes(input.requested_size_mib(), "requested_size_mib")?;
        let block_size = memory_hotplug_mib_to_bytes(self.block_size_mib, "block_size_mib")?;
        let slot_size = memory_hotplug_mib_to_bytes(self.slot_size_mib, "slot_size_mib")?;
        let total_size = memory_hotplug_mib_to_bytes(self.total_size_mib, "total_size_mib")?;

        if !requested_size.is_multiple_of(block_size) {
            return Err(
                MemoryHotplugUpdateError::RequestedSizeNotMultipleOfBlockSize {
                    requested_size_mib: input.requested_size_mib(),
                    block_size_mib: self.block_size_mib,
                },
            );
        }

        if requested_size > total_size {
            return Err(MemoryHotplugUpdateError::RequestedSizeTooLarge {
                requested_size_mib: input.requested_size_mib(),
                total_size_mib: self.total_size_mib,
            });
        }

        Ok(MemoryHotplugSizeUpdate::new(
            input.requested_size_mib(),
            requested_size,
            slot_size,
        ))
    }
}

impl TryFrom<MemoryHotplugConfigInput> for MemoryHotplugConfig {
    type Error = MemoryHotplugConfigError;

    fn try_from(input: MemoryHotplugConfigInput) -> Result<Self, Self::Error> {
        input.validate()?;
        Ok(Self::new(
            input.total_size_mib(),
            input.block_size_mib(),
            input.slot_size_mib(),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryHotplugStatus {
    config: MemoryHotplugConfig,
    plugged_size_mib: u64,
    requested_size_mib: u64,
}

impl MemoryHotplugStatus {
    pub const fn new(
        config: MemoryHotplugConfig,
        plugged_size_mib: u64,
        requested_size_mib: u64,
    ) -> Self {
        Self {
            config,
            plugged_size_mib,
            requested_size_mib,
        }
    }

    pub fn try_from_plugged_size_bytes(
        config: MemoryHotplugConfig,
        plugged_size: u64,
        requested_size_mib: u64,
    ) -> Result<Self, MemoryHotplugStatusError> {
        if !plugged_size.is_multiple_of(MIB) {
            return Err(MemoryHotplugStatusError::PluggedSizeNotMibAligned { plugged_size });
        }

        Ok(Self::new(config, plugged_size / MIB, requested_size_mib))
    }

    pub const fn total_size_mib(self) -> u64 {
        self.config.total_size_mib()
    }

    pub const fn block_size_mib(self) -> u64 {
        self.config.block_size_mib()
    }

    pub const fn slot_size_mib(self) -> u64 {
        self.config.slot_size_mib()
    }

    pub const fn plugged_size_mib(self) -> u64 {
        self.plugged_size_mib
    }

    pub const fn requested_size_mib(self) -> u64 {
        self.requested_size_mib
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryHotplugSizeUpdateInput {
    requested_size_mib: u64,
}

impl MemoryHotplugSizeUpdateInput {
    pub const fn new(requested_size_mib: u64) -> Self {
        Self { requested_size_mib }
    }

    pub const fn requested_size_mib(self) -> u64 {
        self.requested_size_mib
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryHotplugSizeUpdate {
    requested_size_mib: u64,
    requested_size: u64,
    slot_size: u64,
}

impl MemoryHotplugSizeUpdate {
    const fn new(requested_size_mib: u64, requested_size: u64, slot_size: u64) -> Self {
        Self {
            requested_size_mib,
            requested_size,
            slot_size,
        }
    }

    pub const fn requested_size_mib(self) -> u64 {
        self.requested_size_mib
    }

    pub const fn requested_size(self) -> u64 {
        self.requested_size
    }

    pub const fn slot_size(self) -> u64 {
        self.slot_size
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryHotplugConfigError {
    BlockSizeTooSmall { min_mib: u64 },
    BlockSizeNotPowerOfTwo,
    SlotSizeTooSmall { min_mib: u64 },
    SlotSizeNotMultipleOfBlockSize { block_size_mib: u64 },
    TotalSizeTooSmall { slot_size_mib: u64 },
    TotalSizeNotMultipleOfSlotSize { slot_size_mib: u64 },
}

impl fmt::Display for MemoryHotplugConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlockSizeTooSmall { min_mib } => {
                write!(f, "Block size must not be lower than {min_mib} MiB")
            }
            Self::BlockSizeNotPowerOfTwo => f.write_str("Block size must be a power of 2"),
            Self::SlotSizeTooSmall { min_mib } => {
                write!(f, "Slot size must not be lower than {min_mib} MiB")
            }
            Self::SlotSizeNotMultipleOfBlockSize { block_size_mib } => write!(
                f,
                "Slot size must be a multiple of block size ({block_size_mib} MiB)"
            ),
            Self::TotalSizeTooSmall { slot_size_mib } => write!(
                f,
                "Total size must not be lower than slot size ({slot_size_mib} MiB)"
            ),
            Self::TotalSizeNotMultipleOfSlotSize { slot_size_mib } => write!(
                f,
                "Total size must be a multiple of slot size ({slot_size_mib} MiB)"
            ),
        }
    }
}

impl std::error::Error for MemoryHotplugConfigError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryHotplugStatusError {
    ActiveSessionUnavailable,
    ActiveSessionCommand { message: String },
    HandlerLookup(MmioHandlerLookupError),
    PluggedSizeNotMibAligned { plugged_size: u64 },
}

impl fmt::Display for MemoryHotplugStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveSessionUnavailable => {
                f.write_str("active memory hotplug device session is unavailable")
            }
            Self::ActiveSessionCommand { message } => {
                write!(
                    f,
                    "active memory hotplug device status query failed: {message}"
                )
            }
            Self::HandlerLookup(err) => write!(f, "{err}"),
            Self::PluggedSizeNotMibAligned { plugged_size } => {
                write!(
                    f,
                    "active memory hotplug plugged size ({plugged_size} bytes) is not MiB aligned"
                )
            }
        }
    }
}

impl std::error::Error for MemoryHotplugStatusError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HandlerLookup(err) => Some(err),
            Self::ActiveSessionUnavailable
            | Self::ActiveSessionCommand { .. }
            | Self::PluggedSizeNotMibAligned { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryHotplugUpdateError {
    SizeOverflow {
        field: &'static str,
        mib: u64,
    },
    RequestedSizeNotMultipleOfBlockSize {
        requested_size_mib: u64,
        block_size_mib: u64,
    },
    RequestedSizeTooLarge {
        requested_size_mib: u64,
        total_size_mib: u64,
    },
    UsableRegionSizeOverflow {
        requested_size: u64,
        slot_size: u64,
    },
    ActiveSessionUnavailable,
    ActiveSessionCommand {
        message: String,
    },
    HandlerLookup(MmioHandlerLookupError),
}

impl fmt::Display for MemoryHotplugUpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SizeOverflow { field, mib } => {
                write!(f, "memory hotplug {field} value {mib} MiB overflows bytes")
            }
            Self::RequestedSizeNotMultipleOfBlockSize {
                requested_size_mib,
                block_size_mib,
            } => write!(
                f,
                "Requested size ({requested_size_mib} MiB) must be a multiple of block size ({block_size_mib} MiB)"
            ),
            Self::RequestedSizeTooLarge {
                requested_size_mib,
                total_size_mib,
            } => write!(
                f,
                "Requested size ({requested_size_mib} MiB) must not exceed total memory hotplug size ({total_size_mib} MiB)"
            ),
            Self::UsableRegionSizeOverflow {
                requested_size,
                slot_size,
            } => write!(
                f,
                "memory hotplug requested size {requested_size} bytes cannot be rounded to slot size {slot_size} bytes"
            ),
            Self::ActiveSessionUnavailable => {
                f.write_str("active memory hotplug device session is unavailable")
            }
            Self::ActiveSessionCommand { message } => {
                write!(f, "active memory hotplug device update failed: {message}")
            }
            Self::HandlerLookup(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for MemoryHotplugUpdateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HandlerLookup(err) => Some(err),
            Self::SizeOverflow { .. }
            | Self::RequestedSizeNotMultipleOfBlockSize { .. }
            | Self::RequestedSizeTooLarge { .. }
            | Self::UsableRegionSizeOverflow { .. }
            | Self::ActiveSessionUnavailable
            | Self::ActiveSessionCommand { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedVirtioMemDevice {
    config_space: VirtioMemConfigSpace,
}

impl PreparedVirtioMemDevice {
    pub fn from_config(config: MemoryHotplugConfig) -> Result<Self, VirtioMemPrepareError> {
        Self::from_config_with_reserved_ranges(config, &[])
    }

    pub fn from_config_with_reserved_ranges(
        config: MemoryHotplugConfig,
        reserved_ranges: &[GuestMemoryRange],
    ) -> Result<Self, VirtioMemPrepareError> {
        mib_to_bytes(config.block_size_mib(), VirtioMemSizeField::Block)?;
        let region_size = mib_to_bytes(config.total_size_mib(), VirtioMemSizeField::Total)?;
        let alignment = mib_to_bytes(config.slot_size_mib(), VirtioMemSizeField::Slot)?;
        let address = allocate_virtio_mem_region(region_size, alignment, reserved_ranges)?;

        Self::from_config_at(config, address)
    }

    pub fn from_config_at(
        config: MemoryHotplugConfig,
        address: GuestAddress,
    ) -> Result<Self, VirtioMemPrepareError> {
        let block_size = mib_to_bytes(config.block_size_mib(), VirtioMemSizeField::Block)?;
        let region_size = mib_to_bytes(config.total_size_mib(), VirtioMemSizeField::Total)?;
        if address.checked_add(region_size).is_none() {
            return Err(VirtioMemPrepareError::RegionAddressOverflow {
                address,
                region_size,
            });
        }

        Ok(Self {
            config_space: VirtioMemConfigSpace::new(block_size, address.raw_value(), region_size),
        })
    }

    pub const fn config_space(self) -> VirtioMemConfigSpace {
        self.config_space
    }

    #[doc(hidden)]
    pub const fn into_parts(self) -> (VirtioMemConfigSpace, VirtioMemDevice) {
        (self.config_space, VirtioMemDevice::new())
    }

    pub fn register_mmio(
        self,
        layout: VirtioMemMmioLayout,
    ) -> Result<VirtioMemMmioDevice, VirtioMemMmioRegistrationError> {
        VirtioMemMmioDevice::from_prepared(self, layout)
    }

    pub fn register_mmio_with_dispatcher(
        self,
        layout: VirtioMemMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<VirtioMemMmioDevice, VirtioMemMmioRegistrationError> {
        VirtioMemMmioDevice::from_prepared_with_dispatcher(self, layout, dispatcher)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VirtioMemSizeField {
    Block,
    Slot,
    Total,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtioMemPrepareError {
    SizeOverflow {
        field: &'static str,
        mib: u64,
    },
    RegionAddressOverflow {
        address: GuestAddress,
        region_size: u64,
    },
    RegionAlignmentOverflow {
        address: GuestAddress,
        alignment: u64,
    },
    RegionUnavailable {
        region_size: u64,
        alignment: u64,
        address_space_end: GuestAddress,
    },
}

impl fmt::Display for VirtioMemPrepareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SizeOverflow { field, mib } => {
                write!(f, "virtio-mem {field} value {mib} MiB overflows bytes")
            }
            Self::RegionAddressOverflow {
                address,
                region_size,
            } => write!(
                f,
                "virtio-mem region at {address} with size {region_size} bytes overflows guest address space"
            ),
            Self::RegionAlignmentOverflow { address, alignment } => write!(
                f,
                "virtio-mem region address {address} cannot be aligned to {alignment} bytes"
            ),
            Self::RegionUnavailable {
                region_size,
                alignment,
                address_space_end,
            } => write!(
                f,
                "no {alignment}-byte-aligned virtio-mem region of {region_size} bytes fits below guest address-space end {address_space_end}"
            ),
        }
    }
}

impl std::error::Error for VirtioMemPrepareError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioMemMmioLayout {
    address: GuestAddress,
    region_id: MmioRegionId,
}

impl VirtioMemMmioLayout {
    pub const fn new(address: GuestAddress, region_id: MmioRegionId) -> Self {
        Self { address, region_id }
    }

    pub const fn address(self) -> GuestAddress {
        self.address
    }

    pub const fn region_id(self) -> MmioRegionId {
        self.region_id
    }

    fn region(self) -> Result<MmioRegion, VirtioMemMmioRegistrationError> {
        MmioRegion::new(self.region_id, self.address, VIRTIO_MMIO_DEVICE_WINDOW_SIZE).map_err(
            |source| VirtioMemMmioRegistrationError::InvalidRegion {
                region_id: self.region_id,
                address: self.address,
                source,
            },
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioMemMmioDeviceRegistration {
    region: MmioRegion,
}

impl VirtioMemMmioDeviceRegistration {
    pub const fn region(self) -> MmioRegion {
        self.region
    }

    pub const fn region_id(self) -> MmioRegionId {
        self.region.id()
    }

    pub const fn address(self) -> GuestAddress {
        self.region.range().start()
    }
}

#[derive(Debug)]
pub struct VirtioMemMmioDevice {
    dispatcher: MmioDispatcher,
    registration: VirtioMemMmioDeviceRegistration,
}

impl VirtioMemMmioDevice {
    pub fn from_prepared(
        prepared: PreparedVirtioMemDevice,
        layout: VirtioMemMmioLayout,
    ) -> Result<Self, VirtioMemMmioRegistrationError> {
        Self::from_prepared_with_dispatcher(prepared, layout, MmioDispatcher::new())
    }

    pub fn from_prepared_with_dispatcher(
        prepared: PreparedVirtioMemDevice,
        layout: VirtioMemMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<Self, VirtioMemMmioRegistrationError> {
        let region = layout.region()?;
        let handler = virtio_mem_mmio_handler_from_config_space(prepared.config_space()).map_err(
            |source| VirtioMemMmioRegistrationError::BuildHandler {
                region_id: layout.region_id(),
                source,
            },
        )?;
        let mut dispatcher = dispatcher;
        let inserted_region = dispatcher
            .insert_region(
                layout.region_id(),
                layout.address(),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .map_err(|source| VirtioMemMmioRegistrationError::InsertRegion {
                region_id: layout.region_id(),
                address: layout.address(),
                source,
            })?;
        dispatcher
            .register_handler(layout.region_id(), handler)
            .map_err(|source| VirtioMemMmioRegistrationError::RegisterHandler {
                region_id: layout.region_id(),
                source,
            })?;
        debug_assert_eq!(inserted_region, region);

        Ok(Self {
            dispatcher,
            registration: VirtioMemMmioDeviceRegistration { region },
        })
    }

    pub fn dispatcher(&self) -> &MmioDispatcher {
        &self.dispatcher
    }

    pub fn dispatcher_mut(&mut self) -> &mut MmioDispatcher {
        &mut self.dispatcher
    }

    pub const fn registration(&self) -> VirtioMemMmioDeviceRegistration {
        self.registration
    }

    pub fn into_parts(self) -> (MmioDispatcher, VirtioMemMmioDeviceRegistration) {
        (self.dispatcher, self.registration)
    }
}

#[derive(Debug)]
pub enum VirtioMemMmioRegistrationError {
    InvalidRegion {
        region_id: MmioRegionId,
        address: GuestAddress,
        source: GuestMemoryError,
    },
    BuildHandler {
        region_id: MmioRegionId,
        source: VirtioMmioRegisterHandlerError,
    },
    InsertRegion {
        region_id: MmioRegionId,
        address: GuestAddress,
        source: MmioBusError,
    },
    RegisterHandler {
        region_id: MmioRegionId,
        source: MmioDispatchError,
    },
}

impl fmt::Display for VirtioMemMmioRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRegion {
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "invalid virtio-mem MMIO region id={region_id} at {address}: {source}"
                )
            }
            Self::BuildHandler { region_id, source } => {
                write!(
                    f,
                    "failed to build virtio-mem MMIO handler for region id={region_id}: {source}"
                )
            }
            Self::InsertRegion {
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "failed to insert virtio-mem MMIO region id={region_id} at {address}: {source}"
                )
            }
            Self::RegisterHandler { region_id, source } => {
                write!(
                    f,
                    "failed to register virtio-mem MMIO handler for region id={region_id}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioMemMmioRegistrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRegion { source, .. } => Some(source),
            Self::BuildHandler { source, .. } => Some(source),
            Self::InsertRegion { source, .. } => Some(source),
            Self::RegisterHandler { source, .. } => Some(source),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioMemConfigSpace {
    block_size: u64,
    node_id: u16,
    addr: u64,
    region_size: u64,
    usable_region_size: u64,
    plugged_size: u64,
    requested_size: u64,
}

impl VirtioMemConfigSpace {
    pub const fn new(block_size: u64, addr: u64, region_size: u64) -> Self {
        Self {
            block_size,
            node_id: 0,
            addr,
            region_size,
            usable_region_size: 0,
            plugged_size: 0,
            requested_size: 0,
        }
    }

    pub const fn block_size(self) -> u64 {
        self.block_size
    }

    pub const fn node_id(self) -> u16 {
        self.node_id
    }

    pub const fn addr(self) -> u64 {
        self.addr
    }

    pub const fn region_size(self) -> u64 {
        self.region_size
    }

    pub const fn usable_region_size(self) -> u64 {
        self.usable_region_size
    }

    pub const fn plugged_size(self) -> u64 {
        self.plugged_size
    }

    pub const fn requested_size(self) -> u64 {
        self.requested_size
    }

    pub const fn with_node_id(mut self, node_id: u16) -> Self {
        self.node_id = node_id;
        self
    }

    pub const fn with_usable_region_size(mut self, usable_region_size: u64) -> Self {
        self.usable_region_size = usable_region_size;
        self
    }

    pub const fn with_plugged_size(mut self, plugged_size: u64) -> Self {
        self.plugged_size = plugged_size;
        self
    }

    pub const fn with_requested_size(mut self, requested_size: u64) -> Self {
        self.requested_size = requested_size;
        self
    }

    pub fn updated_requested_size(
        mut self,
        update: MemoryHotplugSizeUpdate,
    ) -> Result<Self, MemoryHotplugUpdateError> {
        if self.usable_region_size < update.requested_size() {
            self.usable_region_size =
                next_slot_multiple(update.requested_size(), update.slot_size())?
                    .min(self.region_size);
        }
        self.requested_size = update.requested_size();

        Ok(self)
    }

    pub const fn available_features(self) -> u64 {
        virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
            | virtio_feature_bit(VIRTIO_MEM_F_UNPLUGGED_INACCESSIBLE)
    }

    pub const fn from_le_bytes(bytes: [u8; VIRTIO_MEM_CONFIG_SPACE_SIZE]) -> Self {
        let [
            block0,
            block1,
            block2,
            block3,
            block4,
            block5,
            block6,
            block7,
            node0,
            node1,
            _pad0,
            _pad1,
            _pad2,
            _pad3,
            _pad4,
            _pad5,
            addr0,
            addr1,
            addr2,
            addr3,
            addr4,
            addr5,
            addr6,
            addr7,
            region0,
            region1,
            region2,
            region3,
            region4,
            region5,
            region6,
            region7,
            usable0,
            usable1,
            usable2,
            usable3,
            usable4,
            usable5,
            usable6,
            usable7,
            plugged0,
            plugged1,
            plugged2,
            plugged3,
            plugged4,
            plugged5,
            plugged6,
            plugged7,
            requested0,
            requested1,
            requested2,
            requested3,
            requested4,
            requested5,
            requested6,
            requested7,
        ] = bytes;

        Self {
            block_size: u64::from_le_bytes([
                block0, block1, block2, block3, block4, block5, block6, block7,
            ]),
            node_id: u16::from_le_bytes([node0, node1]),
            addr: u64::from_le_bytes([addr0, addr1, addr2, addr3, addr4, addr5, addr6, addr7]),
            region_size: u64::from_le_bytes([
                region0, region1, region2, region3, region4, region5, region6, region7,
            ]),
            usable_region_size: u64::from_le_bytes([
                usable0, usable1, usable2, usable3, usable4, usable5, usable6, usable7,
            ]),
            plugged_size: u64::from_le_bytes([
                plugged0, plugged1, plugged2, plugged3, plugged4, plugged5, plugged6, plugged7,
            ]),
            requested_size: u64::from_le_bytes([
                requested0, requested1, requested2, requested3, requested4, requested5, requested6,
                requested7,
            ]),
        }
    }

    pub const fn to_le_bytes(self) -> [u8; VIRTIO_MEM_CONFIG_SPACE_SIZE] {
        let [
            block0,
            block1,
            block2,
            block3,
            block4,
            block5,
            block6,
            block7,
        ] = self.block_size.to_le_bytes();
        let [node0, node1] = self.node_id.to_le_bytes();
        let [addr0, addr1, addr2, addr3, addr4, addr5, addr6, addr7] = self.addr.to_le_bytes();
        let [
            region0,
            region1,
            region2,
            region3,
            region4,
            region5,
            region6,
            region7,
        ] = self.region_size.to_le_bytes();
        let [
            usable0,
            usable1,
            usable2,
            usable3,
            usable4,
            usable5,
            usable6,
            usable7,
        ] = self.usable_region_size.to_le_bytes();
        let [
            plugged0,
            plugged1,
            plugged2,
            plugged3,
            plugged4,
            plugged5,
            plugged6,
            plugged7,
        ] = self.plugged_size.to_le_bytes();
        let [
            requested0,
            requested1,
            requested2,
            requested3,
            requested4,
            requested5,
            requested6,
            requested7,
        ] = self.requested_size.to_le_bytes();

        [
            block0, block1, block2, block3, block4, block5, block6, block7, node0, node1, 0, 0, 0,
            0, 0, 0, addr0, addr1, addr2, addr3, addr4, addr5, addr6, addr7, region0, region1,
            region2, region3, region4, region5, region6, region7, usable0, usable1, usable2,
            usable3, usable4, usable5, usable6, usable7, plugged0, plugged1, plugged2, plugged3,
            plugged4, plugged5, plugged6, plugged7, requested0, requested1, requested2, requested3,
            requested4, requested5, requested6, requested7,
        ]
    }
}

impl VirtioMmioDeviceConfigHandler for VirtioMemConfigSpace {
    fn read_device_config(
        &self,
        access: VirtioMmioDeviceConfigAccess,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
        let bytes = self.to_le_bytes();
        let bytes = read_virtio_mem_config_bytes(&bytes, access)?;
        MmioAccessBytes::new(bytes).map_err(config_bytes_error)
    }

    fn write_device_config(
        &mut self,
        access: VirtioMmioDeviceConfigAccess,
        _data: MmioAccessBytes,
    ) -> Result<(), VirtioMmioDeviceConfigError> {
        Err(VirtioMmioDeviceConfigError::UnsupportedWrite {
            offset: access.offset(),
            len: access.len(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioMemRequestedRange {
    address: GuestAddress,
    block_count: u16,
}

impl VirtioMemRequestedRange {
    pub const fn new(address: GuestAddress, block_count: u16) -> Self {
        Self {
            address,
            block_count,
        }
    }

    pub const fn address(self) -> GuestAddress {
        self.address
    }

    pub const fn block_count(self) -> u16 {
        self.block_count
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioMemRequestKind {
    Plug(VirtioMemRequestedRange),
    Unplug(VirtioMemRequestedRange),
    UnplugAll,
    State(VirtioMemRequestedRange),
    Unsupported { request_type: u16 },
}

impl VirtioMemRequestKind {
    pub const fn from_le_bytes(bytes: [u8; VIRTIO_MEM_REQUEST_SIZE]) -> Self {
        let [
            type0,
            type1,
            _pad0,
            _pad1,
            _pad2,
            _pad3,
            _pad4,
            _pad5,
            addr0,
            addr1,
            addr2,
            addr3,
            addr4,
            addr5,
            addr6,
            addr7,
            blocks0,
            blocks1,
            _payload_pad0,
            _payload_pad1,
            _payload_pad2,
            _payload_pad3,
            _payload_pad4,
            _payload_pad5,
        ] = bytes;
        let request_type = u16::from_le_bytes([type0, type1]);
        let range = VirtioMemRequestedRange::new(
            GuestAddress::new(u64::from_le_bytes([
                addr0, addr1, addr2, addr3, addr4, addr5, addr6, addr7,
            ])),
            u16::from_le_bytes([blocks0, blocks1]),
        );

        match request_type {
            VIRTIO_MEM_REQ_PLUG => Self::Plug(range),
            VIRTIO_MEM_REQ_UNPLUG => Self::Unplug(range),
            VIRTIO_MEM_REQ_UNPLUG_ALL => Self::UnplugAll,
            VIRTIO_MEM_REQ_STATE => Self::State(range),
            request_type => Self::Unsupported { request_type },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioMemResponse {
    Ack,
    AckPlugged,
    AckUnplugged,
    AckMixed,
    Error,
}

impl VirtioMemResponse {
    pub const fn to_le_bytes(self) -> [u8; VIRTIO_MEM_RESPONSE_SIZE] {
        let (response_type, state) = match self {
            Self::Ack => (VIRTIO_MEM_RESP_ACK, 0),
            Self::AckPlugged => (VIRTIO_MEM_RESP_ACK, VIRTIO_MEM_STATE_PLUGGED),
            Self::AckUnplugged => (VIRTIO_MEM_RESP_ACK, VIRTIO_MEM_STATE_UNPLUGGED),
            Self::AckMixed => (VIRTIO_MEM_RESP_ACK, VIRTIO_MEM_STATE_MIXED),
            Self::Error => (VIRTIO_MEM_RESP_ERROR, 0),
        };
        let [type0, type1] = response_type.to_le_bytes();
        let [state0, state1] = state.to_le_bytes();

        [type0, type1, 0, 0, 0, 0, 0, 0, state0, state1]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioMemBlockState {
    Plugged,
    Unplugged,
    Mixed,
}

impl VirtioMemBlockState {
    const fn response(self) -> VirtioMemResponse {
        match self {
            Self::Plugged => VirtioMemResponse::AckPlugged,
            Self::Unplugged => VirtioMemResponse::AckUnplugged,
            Self::Mixed => VirtioMemResponse::AckMixed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VirtioMemBlockRange {
    start: u64,
    end: u64,
}

impl VirtioMemBlockRange {
    fn new(start: u64, block_count: u64) -> Option<Self> {
        if block_count == 0 {
            return None;
        }
        let end = start.checked_add(block_count)?;
        Some(Self { start, end })
    }

    const fn start(self) -> u64 {
        self.start
    }

    const fn end(self) -> u64 {
        self.end
    }

    const fn block_count(self) -> u64 {
        self.end - self.start
    }

    fn len_bytes(self, block_size: u64) -> Option<u64> {
        self.block_count().checked_mul(block_size)
    }

    const fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    const fn merge(self, other: Self) -> Self {
        Self {
            start: if self.start < other.start {
                self.start
            } else {
                other.start
            },
            end: if self.end > other.end {
                self.end
            } else {
                other.end
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct VirtioMemPluggedBlocks {
    ranges: Vec<VirtioMemBlockRange>,
}

impl VirtioMemPluggedBlocks {
    fn range_state(&self, range: VirtioMemBlockRange) -> VirtioMemBlockState {
        let mut cursor = range.start();
        let mut saw_overlap = false;

        for plugged in &self.ranges {
            if plugged.end() <= range.start() {
                continue;
            }
            if plugged.start() >= range.end() {
                break;
            }
            if plugged.start() > cursor {
                return VirtioMemBlockState::Mixed;
            }

            saw_overlap = true;
            if plugged.end() > cursor {
                cursor = plugged.end().min(range.end());
            }
            if cursor == range.end() {
                return VirtioMemBlockState::Plugged;
            }
        }

        if saw_overlap {
            VirtioMemBlockState::Mixed
        } else {
            VirtioMemBlockState::Unplugged
        }
    }

    fn plugged_size(&self, block_size: u64) -> u64 {
        self.ranges
            .iter()
            .map(|range| range.block_count())
            .sum::<u64>()
            * block_size
    }

    fn plug(&mut self, range: VirtioMemBlockRange) {
        let mut pending = range;
        let mut inserted = false;
        let mut merged = Vec::with_capacity(self.ranges.len() + 1);

        for current in self.ranges.drain(..) {
            if current.end() < pending.start() {
                merged.push(current);
            } else if pending.end() < current.start() {
                if !inserted {
                    merged.push(pending);
                    inserted = true;
                }
                merged.push(current);
            } else {
                pending = pending.merge(current);
            }
        }

        if !inserted {
            merged.push(pending);
        }
        self.ranges = merged;
    }

    fn unplug(&mut self, range: VirtioMemBlockRange) {
        let mut remaining = Vec::with_capacity(self.ranges.len());

        for current in self.ranges.drain(..) {
            if !current.overlaps(range) {
                remaining.push(current);
                continue;
            }
            if current.start() < range.start() {
                remaining.push(VirtioMemBlockRange {
                    start: current.start(),
                    end: range.start(),
                });
            }
            if range.end() < current.end() {
                remaining.push(VirtioMemBlockRange {
                    start: range.end(),
                    end: current.end(),
                });
            }
        }

        self.ranges = remaining;
    }

    fn clear(&mut self) {
        self.ranges.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioMemResponseDescriptor {
    index: u16,
    address: GuestAddress,
}

impl VirtioMemResponseDescriptor {
    pub const fn index(self) -> u16 {
        self.index
    }

    pub const fn address(self) -> GuestAddress {
        self.address
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioMemRequest {
    descriptor_head: u16,
    kind: VirtioMemRequestKind,
    response: VirtioMemResponseDescriptor,
}

impl VirtioMemRequest {
    pub fn parse(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
    ) -> Result<Self, VirtioMemRequestError> {
        let request = descriptor_at(chain, 0, 2)?;
        validate_request_descriptor(request)?;
        let kind = read_virtio_mem_request(memory, request)?;
        let response = validate_response_descriptor(descriptor_at(chain, 1, 2)?)?;

        Ok(Self {
            descriptor_head: chain.head_index(),
            kind,
            response,
        })
    }

    pub const fn descriptor_head(&self) -> u16 {
        self.descriptor_head
    }

    pub const fn kind(&self) -> VirtioMemRequestKind {
        self.kind
    }

    pub const fn response(&self) -> VirtioMemResponseDescriptor {
        self.response
    }

    fn execute(
        &self,
        memory: &mut GuestMemory,
        config_space: VirtioMemConfigSpace,
        plugged_blocks: &VirtioMemPluggedBlocks,
        mutation_executor: &mut impl VirtioMemMutationExecutor,
    ) -> Result<VirtioMemRequestExecution, VirtioMemRequestExecutionError> {
        let prepared = response_for_request(self.kind, config_space, plugged_blocks);
        let mut response = prepared.response;
        let mut outcome = prepared.outcome;
        let mut commit_mutation = prepared.mutation;
        let mut applied_mutation = None;

        if let Some(mutation) = prepared.mutation {
            match mutation
                .to_executable_mutation(config_space, plugged_blocks)
                .and_then(|mutation| mutation_executor.apply(memory, mutation))
            {
                Ok(applied) => {
                    applied_mutation = Some(applied);
                }
                Err(source) => {
                    response = VirtioMemResponse::Error;
                    outcome = VirtioMemRequestExecutionOutcome::MutationFailed { source };
                    commit_mutation = None;
                }
            }
        }

        match memory.write_slice(&response.to_le_bytes(), self.response.address()) {
            Ok(()) => Ok(VirtioMemRequestExecution::new(
                VirtioMemRequestCompletion::new(self.descriptor_head, VIRTIO_MEM_RESPONSE_SIZE_U32),
                response,
                outcome,
                commit_mutation,
                applied_mutation,
            )),
            Err(source) => {
                if let Some(applied) = applied_mutation {
                    mutation_executor
                        .rollback(memory, applied)
                        .map_err(|rollback_source| {
                            VirtioMemRequestExecutionError::ResponseWriteRollback {
                                descriptor_head: self.descriptor_head,
                                address: self.response.address(),
                                response_source: source,
                                rollback_source,
                            }
                        })?;
                }

                Ok(VirtioMemRequestExecution::new(
                    VirtioMemRequestCompletion::new(self.descriptor_head, 0),
                    response,
                    VirtioMemRequestExecutionOutcome::ResponseWriteFailed {
                        address: self.response.address(),
                        source,
                    },
                    None,
                    None,
                ))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VirtioMemPreparedRequestExecution {
    response: VirtioMemResponse,
    outcome: VirtioMemRequestExecutionOutcome,
    mutation: Option<VirtioMemPendingMutation>,
}

impl VirtioMemPreparedRequestExecution {
    const fn new(
        response: VirtioMemResponse,
        outcome: VirtioMemRequestExecutionOutcome,
        mutation: Option<VirtioMemPendingMutation>,
    ) -> Self {
        Self {
            response,
            outcome,
            mutation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioMemRequestCompletion {
    descriptor_head: u16,
    bytes_written_to_guest: u32,
}

impl VirtioMemRequestCompletion {
    pub const fn new(descriptor_head: u16, bytes_written_to_guest: u32) -> Self {
        Self {
            descriptor_head,
            bytes_written_to_guest,
        }
    }

    pub const fn descriptor_head(self) -> u16 {
        self.descriptor_head
    }

    pub const fn bytes_written_to_guest(self) -> u32 {
        self.bytes_written_to_guest
    }
}

#[derive(Debug)]
pub struct VirtioMemRequestExecution {
    completion: VirtioMemRequestCompletion,
    response: VirtioMemResponse,
    outcome: VirtioMemRequestExecutionOutcome,
    mutation: Option<VirtioMemPendingMutation>,
    applied_mutation: Option<VirtioMemAppliedMutation>,
}

impl VirtioMemRequestExecution {
    const fn new(
        completion: VirtioMemRequestCompletion,
        response: VirtioMemResponse,
        outcome: VirtioMemRequestExecutionOutcome,
        mutation: Option<VirtioMemPendingMutation>,
        applied_mutation: Option<VirtioMemAppliedMutation>,
    ) -> Self {
        Self {
            completion,
            response,
            outcome,
            mutation,
            applied_mutation,
        }
    }

    pub const fn completion(&self) -> VirtioMemRequestCompletion {
        self.completion
    }

    pub const fn response(&self) -> VirtioMemResponse {
        self.response
    }

    pub const fn outcome(&self) -> &VirtioMemRequestExecutionOutcome {
        &self.outcome
    }

    const fn mutation(&self) -> Option<VirtioMemPendingMutation> {
        self.mutation
    }

    fn into_applied_mutation(self) -> Option<VirtioMemAppliedMutation> {
        self.applied_mutation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtioMemRequestExecutionOutcome {
    State {
        state: VirtioMemBlockState,
    },
    MutationAccepted,
    PolicyError,
    UnsupportedRequest {
        request_type: u16,
    },
    MutationFailed {
        source: VirtioMemMutationError,
    },
    ResponseWriteFailed {
        address: GuestAddress,
        source: GuestMemoryAccessError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtioMemRequestExecutionError {
    ResponseWriteRollback {
        descriptor_head: u16,
        address: GuestAddress,
        response_source: GuestMemoryAccessError,
        rollback_source: VirtioMemMutationRollbackError,
    },
}

impl fmt::Display for VirtioMemRequestExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResponseWriteRollback {
                descriptor_head,
                address,
                response_source,
                rollback_source,
            } => write!(
                f,
                "failed to write virtio-mem response for descriptor head {descriptor_head} at {address}: {response_source}; also failed to roll back applied mutation: {rollback_source}"
            ),
        }
    }
}

impl std::error::Error for VirtioMemRequestExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ResponseWriteRollback {
                rollback_source, ..
            } => Some(rollback_source),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioMemMutation {
    kind: VirtioMemMutationKind,
}

impl VirtioMemMutation {
    pub const fn new(kind: VirtioMemMutationKind) -> Self {
        Self { kind }
    }

    pub const fn kind(&self) -> &VirtioMemMutationKind {
        &self.kind
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtioMemMutationKind {
    Plug(Vec<GuestMemoryRange>),
    Unplug(Vec<GuestMemoryRange>),
    UnplugAll(Vec<GuestMemoryRange>),
}

#[derive(Debug, PartialEq, Eq)]
pub struct VirtioMemAppliedMutation {
    mutation: VirtioMemMutation,
}

impl VirtioMemAppliedMutation {
    pub const fn new(mutation: VirtioMemMutation) -> Self {
        Self { mutation }
    }

    pub const fn mutation(&self) -> &VirtioMemMutation {
        &self.mutation
    }
}

pub trait VirtioMemMutationExecutor {
    fn apply(
        &mut self,
        memory: &mut GuestMemory,
        mutation: VirtioMemMutation,
    ) -> Result<VirtioMemAppliedMutation, VirtioMemMutationError>;

    fn rollback(
        &mut self,
        memory: &mut GuestMemory,
        applied: VirtioMemAppliedMutation,
    ) -> Result<(), VirtioMemMutationRollbackError>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoopVirtioMemMutationExecutor;

impl VirtioMemMutationExecutor for NoopVirtioMemMutationExecutor {
    fn apply(
        &mut self,
        _memory: &mut GuestMemory,
        mutation: VirtioMemMutation,
    ) -> Result<VirtioMemAppliedMutation, VirtioMemMutationError> {
        Ok(VirtioMemAppliedMutation::new(mutation))
    }

    fn rollback(
        &mut self,
        _memory: &mut GuestMemory,
        _applied: VirtioMemAppliedMutation,
    ) -> Result<(), VirtioMemMutationRollbackError> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioMemMutationError {
    message: String,
}

impl VirtioMemMutationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for VirtioMemMutationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "virtio-mem mutation failed: {}", self.message)
    }
}

impl std::error::Error for VirtioMemMutationError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioMemMutationRollbackError {
    message: String,
}

impl VirtioMemMutationRollbackError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for VirtioMemMutationRollbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "virtio-mem mutation rollback failed: {}", self.message)
    }
}

impl std::error::Error for VirtioMemMutationRollbackError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VirtioMemPendingMutation {
    Plug(VirtioMemBlockRange),
    Unplug(VirtioMemBlockRange),
    UnplugAll,
}

impl VirtioMemPendingMutation {
    fn to_executable_mutation(
        self,
        config_space: VirtioMemConfigSpace,
        plugged_blocks: &VirtioMemPluggedBlocks,
    ) -> Result<VirtioMemMutation, VirtioMemMutationError> {
        match self {
            Self::Plug(range) => Ok(VirtioMemMutation::new(VirtioMemMutationKind::Plug(
                block_ranges_to_guest_ranges(&[range], config_space, "plug")?,
            ))),
            Self::Unplug(range) => Ok(VirtioMemMutation::new(VirtioMemMutationKind::Unplug(
                block_ranges_to_guest_ranges(&[range], config_space, "unplug")?,
            ))),
            Self::UnplugAll => Ok(VirtioMemMutation::new(VirtioMemMutationKind::UnplugAll(
                block_ranges_to_guest_ranges(&plugged_blocks.ranges, config_space, "unplug-all")?,
            ))),
        }
    }

    fn commit(
        self,
        config_space: &mut VirtioMemConfigSpace,
        plugged_blocks: &mut VirtioMemPluggedBlocks,
    ) {
        match self {
            Self::Plug(range) => plugged_blocks.plug(range),
            Self::Unplug(range) => plugged_blocks.unplug(range),
            Self::UnplugAll => {
                plugged_blocks.clear();
                config_space.usable_region_size = 0;
            }
        }
        config_space.plugged_size = plugged_blocks.plugged_size(config_space.block_size());
    }
}

impl VirtioMemMutationError {
    fn from_try_reserve_error(context: &str, source: TryReserveError) -> Self {
        Self::new(format!(
            "failed to allocate {context} mutation metadata: {source}"
        ))
    }
}

fn block_ranges_to_guest_ranges(
    block_ranges: &[VirtioMemBlockRange],
    config_space: VirtioMemConfigSpace,
    context: &str,
) -> Result<Vec<GuestMemoryRange>, VirtioMemMutationError> {
    let block_count = block_ranges.iter().try_fold(0_u64, |count, range| {
        count.checked_add(range.block_count()).ok_or_else(|| {
            VirtioMemMutationError::new(format!(
                "virtio-mem {context} block count overflows address space"
            ))
        })
    })?;
    let capacity = usize::try_from(block_count).map_err(|source| {
        VirtioMemMutationError::new(format!(
            "virtio-mem {context} block count {block_count} cannot be represented: {source}"
        ))
    })?;
    let mut guest_ranges = Vec::new();
    guest_ranges
        .try_reserve_exact(capacity)
        .map_err(|source| VirtioMemMutationError::from_try_reserve_error(context, source))?;

    let block_size = config_space.block_size();
    for range in block_ranges {
        for block in range.start()..range.end() {
            let Some(offset) = block.checked_mul(block_size) else {
                return Err(VirtioMemMutationError::new(format!(
                    "virtio-mem block {block} overflows block size {block_size}"
                )));
            };
            let Some(start) = config_space.addr().checked_add(offset) else {
                return Err(VirtioMemMutationError::new(format!(
                    "virtio-mem block offset {offset} overflows base address {}",
                    config_space.addr()
                )));
            };
            let guest_range =
                GuestMemoryRange::new(GuestAddress::new(start), block_size).map_err(|source| {
                    VirtioMemMutationError::new(format!(
                        "invalid virtio-mem mutation range: {source}"
                    ))
                })?;
            guest_ranges.push(guest_range);
        }
    }

    Ok(guest_ranges)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtioMemRequestError {
    DescriptorChainTooShort {
        expected: usize,
        actual: usize,
    },
    RequestDescriptorWriteOnly {
        index: u16,
    },
    RequestDescriptorInvalidLength {
        index: u16,
        len: u32,
        expected: u32,
    },
    ReadRequest {
        address: GuestAddress,
        source: GuestMemoryAccessError,
    },
    ResponseDescriptorReadOnly {
        index: u16,
    },
    ResponseDescriptorInvalidLength {
        index: u16,
        len: u32,
        expected: u32,
    },
}

impl fmt::Display for VirtioMemRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DescriptorChainTooShort { expected, actual } => write!(
                f,
                "virtio-mem descriptor chain has {actual} descriptor(s), expected at least {expected}"
            ),
            Self::RequestDescriptorWriteOnly { index } => {
                write!(f, "virtio-mem request descriptor {index} is write-only")
            }
            Self::RequestDescriptorInvalidLength {
                index,
                len,
                expected,
            } => write!(
                f,
                "virtio-mem request descriptor {index} has length {len}, expected at least {expected}"
            ),
            Self::ReadRequest { address, source } => {
                write!(
                    f,
                    "failed to read virtio-mem request at {address}: {source}"
                )
            }
            Self::ResponseDescriptorReadOnly { index } => {
                write!(f, "virtio-mem response descriptor {index} is not writable")
            }
            Self::ResponseDescriptorInvalidLength {
                index,
                len,
                expected,
            } => write!(
                f,
                "virtio-mem response descriptor {index} has length {len}, expected at least {expected}"
            ),
        }
    }
}

impl std::error::Error for VirtioMemRequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadRequest { source, .. } => Some(source),
            Self::DescriptorChainTooShort { .. }
            | Self::RequestDescriptorWriteOnly { .. }
            | Self::RequestDescriptorInvalidLength { .. }
            | Self::ResponseDescriptorReadOnly { .. }
            | Self::ResponseDescriptorInvalidLength { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioMemQueue {
    available: VirtqueueAvailableRing,
    used: VirtqueueUsedRing,
}

impl VirtioMemQueue {
    pub const fn new(available: VirtqueueAvailableRing, used: VirtqueueUsedRing) -> Self {
        Self { available, used }
    }

    pub fn from_mmio_queue_state(
        queue: &VirtioMmioQueueState,
    ) -> Result<Self, VirtioMemQueueBuildError> {
        if !queue.ready() {
            return Err(VirtioMemQueueBuildError::QueueNotReady);
        }

        let available = VirtqueueAvailableRing::new(
            queue.descriptor_table(),
            queue.driver_ring(),
            queue.size(),
        )
        .map_err(|source| VirtioMemQueueBuildError::AvailableRing { source })?;
        let used = VirtqueueUsedRing::new(queue.device_ring(), queue.size())
            .map_err(|source| VirtioMemQueueBuildError::UsedRing { source })?;

        Ok(Self { available, used })
    }

    pub const fn available_ring(&self) -> &VirtqueueAvailableRing {
        &self.available
    }

    pub const fn used_ring(&self) -> &VirtqueueUsedRing {
        &self.used
    }

    #[cfg(test)]
    fn dispatch(
        &mut self,
        memory: &mut GuestMemory,
        config_space: &mut VirtioMemConfigSpace,
        plugged_blocks: &mut VirtioMemPluggedBlocks,
    ) -> Result<VirtioMemQueueDispatch, VirtioMemQueueDispatchError> {
        let mut mutation_executor = NoopVirtioMemMutationExecutor;
        self.dispatch_with_executor(memory, config_space, plugged_blocks, &mut mutation_executor)
    }

    fn dispatch_with_executor(
        &mut self,
        memory: &mut GuestMemory,
        config_space: &mut VirtioMemConfigSpace,
        plugged_blocks: &mut VirtioMemPluggedBlocks,
        mutation_executor: &mut impl VirtioMemMutationExecutor,
    ) -> Result<VirtioMemQueueDispatch, VirtioMemQueueDispatchError> {
        let mut dispatch = VirtioMemQueueDispatch::default();
        while let Some(chain) = self
            .available
            .pop_descriptor_chain(memory)
            .map_err(|source| VirtioMemQueueDispatchError::AvailableRing {
                completed_dispatch: Box::new(dispatch.clone()),
                source,
            })?
        {
            let descriptor_head = chain.head_index();
            let (completion, outcome, mutation, applied_mutation) =
                match VirtioMemRequest::parse(memory, &chain) {
                    Ok(request) => {
                        let execution = request
                            .execute(memory, *config_space, plugged_blocks, mutation_executor)
                            .map_err(|source| VirtioMemQueueDispatchError::MutationRollback {
                                completed_dispatch: Box::new(dispatch.clone()),
                                source,
                            })?;
                        (
                            execution.completion(),
                            VirtioMemQueueDispatchOutcome::from_execution(&execution),
                            execution.mutation(),
                            execution.into_applied_mutation(),
                        )
                    }
                    Err(source) => (
                        VirtioMemRequestCompletion::new(descriptor_head, 0),
                        VirtioMemQueueDispatchOutcome::ParseError(source),
                        None,
                        None,
                    ),
                };

            let publication = self
                .used
                .publish_used_element_with_notification(
                    memory,
                    completion.descriptor_head(),
                    completion.bytes_written_to_guest(),
                    VirtqueueNotificationSuppression::Disabled,
                )
                .map_err(|source| VirtioMemQueueDispatchError::UsedRing {
                    completed_dispatch: Box::new(dispatch.clone()),
                    descriptor_head: completion.descriptor_head(),
                    bytes_written_to_guest: completion.bytes_written_to_guest(),
                    rollback_error: applied_mutation
                        .map(|applied| mutation_executor.rollback(memory, applied).err())
                        .unwrap_or(None),
                    source,
                })?;
            if let Some(mutation) = mutation {
                mutation.commit(config_space, plugged_blocks);
            }
            dispatch.record(outcome, publication);
        }

        Ok(dispatch)
    }
}

#[derive(Debug)]
pub enum VirtioMemQueueBuildError {
    QueueNotReady,
    AvailableRing { source: VirtqueueAvailableRingError },
    UsedRing { source: VirtqueueUsedRingError },
}

impl fmt::Display for VirtioMemQueueBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueNotReady => f.write_str("virtio-mem queue is not ready"),
            Self::AvailableRing { source } => {
                write!(f, "failed to build virtio-mem available ring: {source}")
            }
            Self::UsedRing { source } => {
                write!(f, "failed to build virtio-mem used ring: {source}")
            }
        }
    }
}

impl std::error::Error for VirtioMemQueueBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRing { source } => Some(source),
            Self::UsedRing { source } => Some(source),
            Self::QueueNotReady => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VirtioMemQueueDispatch {
    processed_requests: usize,
    state_requests: usize,
    policy_errors: usize,
    unsupported_requests: usize,
    mutation_failures: usize,
    parse_failures: usize,
    response_write_failures: usize,
    first_mutation_failure: Option<VirtioMemMutationError>,
    first_parse_failure: Option<VirtioMemRequestError>,
    needs_queue_interrupt: bool,
}

impl VirtioMemQueueDispatch {
    pub const fn processed_requests(&self) -> usize {
        self.processed_requests
    }

    pub const fn state_requests(&self) -> usize {
        self.state_requests
    }

    pub const fn policy_errors(&self) -> usize {
        self.policy_errors
    }

    pub const fn unsupported_requests(&self) -> usize {
        self.unsupported_requests
    }

    pub const fn mutation_failures(&self) -> usize {
        self.mutation_failures
    }

    pub const fn first_mutation_failure(&self) -> Option<&VirtioMemMutationError> {
        self.first_mutation_failure.as_ref()
    }

    pub const fn parse_failures(&self) -> usize {
        self.parse_failures
    }

    pub const fn first_parse_failure(&self) -> Option<&VirtioMemRequestError> {
        self.first_parse_failure.as_ref()
    }

    pub const fn response_write_failures(&self) -> usize {
        self.response_write_failures
    }

    pub const fn needs_queue_interrupt(&self) -> bool {
        self.needs_queue_interrupt
    }

    fn record(
        &mut self,
        outcome: VirtioMemQueueDispatchOutcome,
        publication: VirtqueueUsedRingPublication,
    ) {
        self.processed_requests += 1;
        self.needs_queue_interrupt |= publication.needs_queue_interrupt();
        match outcome {
            VirtioMemQueueDispatchOutcome::State { .. } => {
                self.state_requests += 1;
            }
            VirtioMemQueueDispatchOutcome::MutationAccepted => {}
            VirtioMemQueueDispatchOutcome::PolicyError => {
                self.policy_errors += 1;
            }
            VirtioMemQueueDispatchOutcome::UnsupportedRequest { .. } => {
                self.unsupported_requests += 1;
            }
            VirtioMemQueueDispatchOutcome::MutationFailed { source } => {
                self.mutation_failures += 1;
                if self.first_mutation_failure.is_none() {
                    self.first_mutation_failure = Some(source);
                }
            }
            VirtioMemQueueDispatchOutcome::ParseError(source) => {
                self.parse_failures += 1;
                if self.first_parse_failure.is_none() {
                    self.first_parse_failure = Some(source);
                }
            }
            VirtioMemQueueDispatchOutcome::ResponseWriteFailed => {
                self.response_write_failures += 1;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VirtioMemQueueDispatchOutcome {
    State { state: VirtioMemBlockState },
    MutationAccepted,
    PolicyError,
    UnsupportedRequest { request_type: u16 },
    MutationFailed { source: VirtioMemMutationError },
    ParseError(VirtioMemRequestError),
    ResponseWriteFailed,
}

impl VirtioMemQueueDispatchOutcome {
    fn from_execution(execution: &VirtioMemRequestExecution) -> Self {
        match execution.outcome() {
            VirtioMemRequestExecutionOutcome::State { state } => Self::State { state: *state },
            VirtioMemRequestExecutionOutcome::MutationAccepted => Self::MutationAccepted,
            VirtioMemRequestExecutionOutcome::PolicyError => Self::PolicyError,
            VirtioMemRequestExecutionOutcome::UnsupportedRequest { request_type } => {
                Self::UnsupportedRequest {
                    request_type: *request_type,
                }
            }
            VirtioMemRequestExecutionOutcome::MutationFailed { source } => Self::MutationFailed {
                source: source.clone(),
            },
            VirtioMemRequestExecutionOutcome::ResponseWriteFailed { .. } => {
                Self::ResponseWriteFailed
            }
        }
    }
}

#[derive(Debug)]
pub enum VirtioMemQueueDispatchError {
    AvailableRing {
        completed_dispatch: Box<VirtioMemQueueDispatch>,
        source: VirtqueueAvailableRingError,
    },
    UsedRing {
        completed_dispatch: Box<VirtioMemQueueDispatch>,
        descriptor_head: u16,
        bytes_written_to_guest: u32,
        rollback_error: Option<VirtioMemMutationRollbackError>,
        source: VirtqueueUsedRingError,
    },
    MutationRollback {
        completed_dispatch: Box<VirtioMemQueueDispatch>,
        source: VirtioMemRequestExecutionError,
    },
}

impl fmt::Display for VirtioMemQueueDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AvailableRing { source, .. } => {
                write!(
                    f,
                    "failed to pop virtio-mem available descriptor chain: {source}"
                )
            }
            Self::UsedRing {
                descriptor_head,
                bytes_written_to_guest,
                source,
                rollback_error,
                ..
            } => match rollback_error {
                Some(rollback_error) => write!(
                    f,
                    "failed to publish virtio-mem used descriptor head {descriptor_head} with length {bytes_written_to_guest}: {source}; also failed to roll back applied mutation: {rollback_error}"
                ),
                None => write!(
                    f,
                    "failed to publish virtio-mem used descriptor head {descriptor_head} with length {bytes_written_to_guest}: {source}"
                ),
            },
            Self::MutationRollback { source, .. } => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for VirtioMemQueueDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRing { source, .. } => Some(source),
            Self::UsedRing { source, .. } => Some(source),
            Self::MutationRollback { source, .. } => Some(source),
        }
    }
}

impl VirtioMemQueueDispatchError {
    pub const fn completed_dispatch(&self) -> &VirtioMemQueueDispatch {
        match self {
            Self::AvailableRing {
                completed_dispatch, ..
            }
            | Self::UsedRing {
                completed_dispatch, ..
            }
            | Self::MutationRollback {
                completed_dispatch, ..
            } => completed_dispatch,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioMemDeviceNotificationDispatch {
    drained_notifications: Vec<usize>,
    queue_dispatch: Option<VirtioMemQueueDispatch>,
}

impl VirtioMemDeviceNotificationDispatch {
    const fn new(
        drained_notifications: Vec<usize>,
        queue_dispatch: Option<VirtioMemQueueDispatch>,
    ) -> Self {
        Self {
            drained_notifications,
            queue_dispatch,
        }
    }

    pub fn drained_notifications(&self) -> &[usize] {
        &self.drained_notifications
    }

    pub const fn queue_dispatch(&self) -> Option<&VirtioMemQueueDispatch> {
        self.queue_dispatch.as_ref()
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.queue_dispatch
            .as_ref()
            .is_some_and(VirtioMemQueueDispatch::needs_queue_interrupt)
    }
}

#[derive(Debug)]
pub enum VirtioMemDeviceNotificationError {
    Inactive {
        drained_notifications: Vec<usize>,
    },
    UnsupportedQueue {
        drained_notifications: Vec<usize>,
        queue_index: usize,
    },
    QueueDispatch {
        drained_notifications: Vec<usize>,
        source: VirtioMemQueueDispatchError,
    },
}

impl VirtioMemDeviceNotificationError {
    pub fn drained_notifications(&self) -> &[usize] {
        match self {
            Self::Inactive {
                drained_notifications,
            }
            | Self::UnsupportedQueue {
                drained_notifications,
                ..
            }
            | Self::QueueDispatch {
                drained_notifications,
                ..
            } => drained_notifications,
        }
    }

    pub const fn completed_dispatch(&self) -> Option<&VirtioMemQueueDispatch> {
        match self {
            Self::QueueDispatch { source, .. } => Some(source.completed_dispatch()),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }
}

impl fmt::Display for VirtioMemDeviceNotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inactive { .. } => {
                f.write_str("virtio-mem queue notification received before device activation")
            }
            Self::UnsupportedQueue { queue_index, .. } => {
                write!(f, "unsupported virtio-mem queue notification {queue_index}")
            }
            Self::QueueDispatch { source, .. } => {
                write!(
                    f,
                    "failed to dispatch virtio-mem queue notification: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioMemDeviceNotificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueDispatch { source, .. } => Some(source),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct VirtioMemDevice {
    active_queue: Option<VirtioMemQueue>,
    plugged_blocks: VirtioMemPluggedBlocks,
}

impl VirtioMemDevice {
    pub const fn new() -> Self {
        Self {
            active_queue: None,
            plugged_blocks: VirtioMemPluggedBlocks { ranges: Vec::new() },
        }
    }

    pub fn is_activated(&self) -> bool {
        self.active_queue.is_some()
    }

    pub const fn active_queue(&self) -> Option<&VirtioMemQueue> {
        self.active_queue.as_ref()
    }

    pub fn active_queue_mut(&mut self) -> Option<&mut VirtioMemQueue> {
        self.active_queue.as_mut()
    }

    #[cfg(test)]
    fn dispatch_drained_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        config_space: &mut VirtioMemConfigSpace,
        drained_notifications: Vec<usize>,
    ) -> Result<VirtioMemDeviceNotificationDispatch, VirtioMemDeviceNotificationError> {
        let mut mutation_executor = NoopVirtioMemMutationExecutor;
        self.dispatch_drained_queue_notifications_with_executor(
            memory,
            config_space,
            drained_notifications,
            &mut mutation_executor,
        )
    }

    fn dispatch_drained_queue_notifications_with_executor(
        &mut self,
        memory: &mut GuestMemory,
        config_space: &mut VirtioMemConfigSpace,
        drained_notifications: Vec<usize>,
        mutation_executor: &mut impl VirtioMemMutationExecutor,
    ) -> Result<VirtioMemDeviceNotificationDispatch, VirtioMemDeviceNotificationError> {
        if drained_notifications.is_empty() {
            return Ok(VirtioMemDeviceNotificationDispatch::new(
                drained_notifications,
                None,
            ));
        }

        if let Some(queue_index) = drained_notifications
            .iter()
            .copied()
            .find(|queue_index| *queue_index != 0)
        {
            return Err(VirtioMemDeviceNotificationError::UnsupportedQueue {
                drained_notifications,
                queue_index,
            });
        }

        let Some(queue) = self.active_queue.as_mut() else {
            return Err(VirtioMemDeviceNotificationError::Inactive {
                drained_notifications,
            });
        };

        match queue.dispatch_with_executor(
            memory,
            config_space,
            &mut self.plugged_blocks,
            mutation_executor,
        ) {
            Ok(dispatch) => Ok(VirtioMemDeviceNotificationDispatch::new(
                drained_notifications,
                Some(dispatch),
            )),
            Err(source) => Err(VirtioMemDeviceNotificationError::QueueDispatch {
                drained_notifications,
                source,
            }),
        }
    }

    pub fn activate_mem(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioMemDeviceActivationError> {
        if self.active_queue.is_some() {
            return Err(VirtioMemDeviceActivationError::AlreadyActive);
        }

        let queue_count = activation.queue_count();
        if queue_count != VIRTIO_MEM_QUEUE_COUNT {
            return Err(VirtioMemDeviceActivationError::QueueCountMismatch {
                expected: VIRTIO_MEM_QUEUE_COUNT,
                actual: queue_count,
            });
        }

        let queue_index = 0;
        let queue = *activation.queue(queue_index).map_err(|source| {
            VirtioMemDeviceActivationError::QueueMetadata {
                queue_index,
                source,
            }
        })?;
        validate_virtio_mem_queue(queue_index, queue)?;
        self.active_queue = Some(VirtioMemQueue::from_mmio_queue_state(&queue).map_err(
            |source| VirtioMemDeviceActivationError::QueueBuild {
                queue_index,
                source,
            },
        )?);

        Ok(())
    }

    pub fn reset(&mut self) {
        self.active_queue = None;
    }
}

impl VirtioMmioRegisterHandler<VirtioMemConfigSpace, VirtioMemDevice> {
    pub fn dispatch_mem_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioMemDeviceNotificationDispatch, VirtioMemDeviceNotificationError> {
        let mut mutation_executor = NoopVirtioMemMutationExecutor;
        self.dispatch_mem_queue_notifications_with_executor(memory, &mut mutation_executor)
    }

    pub fn dispatch_mem_queue_notifications_with_executor(
        &mut self,
        memory: &mut GuestMemory,
        mutation_executor: &mut impl VirtioMemMutationExecutor,
    ) -> Result<VirtioMemDeviceNotificationDispatch, VirtioMemDeviceNotificationError> {
        let drained_notifications = self.take_pending_queue_notifications();
        let previous_config_space = *self.device_config_handler();
        let mut config_space = previous_config_space;
        let dispatch = self
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_executor(
                memory,
                &mut config_space,
                drained_notifications,
                mutation_executor,
            );
        if config_space != previous_config_space {
            *self.device_config_handler_mut() = config_space;
            self.increment_config_generation();
        }
        let needs_queue_interrupt = match &dispatch {
            Ok(dispatch) => dispatch.needs_queue_interrupt(),
            Err(error) => error
                .completed_dispatch()
                .is_some_and(VirtioMemQueueDispatch::needs_queue_interrupt),
        };
        if needs_queue_interrupt {
            self.mark_queue_interrupt_pending(0);
        }

        dispatch
    }

    pub fn update_mem_requested_size(
        &mut self,
        update: MemoryHotplugSizeUpdate,
    ) -> Result<(), MemoryHotplugUpdateError> {
        let config_space = self
            .device_config_handler()
            .updated_requested_size(update)?;
        *self.device_config_handler_mut() = config_space;
        self.increment_config_generation();
        self.mark_config_interrupt_pending();

        Ok(())
    }
}

impl VirtioPciEndpoint<VirtioMemConfigSpace, VirtioMemDevice> {
    pub fn dispatch_mem_queue_notifications_with_executor(
        &self,
        memory: &mut GuestMemory,
        mutation_executor: &mut impl VirtioMemMutationExecutor,
    ) -> Result<
        VirtioMemDeviceNotificationDispatch,
        VirtioPciDeviceOperationError<
            VirtioMemDeviceNotificationError,
            VirtioMemDeviceNotificationDispatch,
        >,
    > {
        let work = self
            .admit_device_work()
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let dispatch = work
            .with_core_mut(|core| {
                let drained_notifications =
                    core.queue_notifications.take_pending_queue_notifications();
                let previous_config_space = core.device_config;
                let dispatch = core
                    .activation
                    .dispatch_drained_queue_notifications_with_executor(
                        memory,
                        &mut core.device_config,
                        drained_notifications,
                        mutation_executor,
                    );
                if core.device_config != previous_config_space {
                    core.device.increment_config_generation();
                }
                let needs_queue_interrupt = match &dispatch {
                    Ok(dispatch) => dispatch.needs_queue_interrupt(),
                    Err(error) => error
                        .completed_dispatch()
                        .is_some_and(VirtioMemQueueDispatch::needs_queue_interrupt),
                };
                if needs_queue_interrupt {
                    core.record_interrupt_intent(VirtioInterruptIntent::Queue { queue_index: 0 });
                }
                dispatch
            })
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        VirtioPciDeviceOperationError::combine(dispatch, work.drain_interrupt_intents())
    }

    pub fn update_mem_requested_size(
        &self,
        update: MemoryHotplugSizeUpdate,
    ) -> Result<(), VirtioPciDeviceOperationError<MemoryHotplugUpdateError, ()>> {
        let work = self
            .admit_device_work()
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let result = work
            .with_core_mut(|core| {
                let config_space = core.device_config.updated_requested_size(update)?;
                core.device_config = config_space;
                core.device.increment_config_generation();
                core.record_interrupt_intent(VirtioInterruptIntent::Configuration);
                Ok(())
            })
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        VirtioPciDeviceOperationError::combine(result, work.drain_interrupt_intents())
    }

    pub fn plugged_size(&self) -> Result<u64, VirtioPciEndpointError> {
        let work = self.admit_device_work()?;
        work.with_core_mut(|core| core.device_config.plugged_size())
    }
}

impl VirtioMmioDeviceActivationHandler for VirtioMemDevice {
    fn activate(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioMmioDeviceActivationError> {
        self.activate_mem(activation).map_err(Into::into)
    }

    fn reset(&mut self) {
        VirtioMemDevice::reset(self);
    }
}

#[derive(Debug)]
pub enum VirtioMemDeviceActivationError {
    AlreadyActive,
    QueueCountMismatch {
        expected: usize,
        actual: usize,
    },
    QueueMetadata {
        queue_index: u32,
        source: VirtioMmioQueueRegisterError,
    },
    QueueBuild {
        queue_index: u32,
        source: VirtioMemQueueBuildError,
    },
    QueueMaxSizeMismatch {
        queue_index: u32,
        expected: u16,
        actual: u16,
    },
    QueueSizeZero {
        queue_index: u32,
    },
    QueueNotReady {
        queue_index: u32,
    },
}

impl PartialEq for VirtioMemDeviceActivationError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::AlreadyActive, Self::AlreadyActive) => true,
            (
                Self::QueueCountMismatch {
                    expected: left_expected,
                    actual: left_actual,
                },
                Self::QueueCountMismatch {
                    expected: right_expected,
                    actual: right_actual,
                },
            ) => left_expected == right_expected && left_actual == right_actual,
            (
                Self::QueueMetadata {
                    queue_index: left_index,
                    source: _,
                },
                Self::QueueMetadata {
                    queue_index: right_index,
                    source: _,
                },
            ) => left_index == right_index,
            (
                Self::QueueBuild {
                    queue_index: left_index,
                    source: _,
                },
                Self::QueueBuild {
                    queue_index: right_index,
                    source: _,
                },
            ) => left_index == right_index,
            (
                Self::QueueMaxSizeMismatch {
                    queue_index: left_index,
                    expected: left_expected,
                    actual: left_actual,
                },
                Self::QueueMaxSizeMismatch {
                    queue_index: right_index,
                    expected: right_expected,
                    actual: right_actual,
                },
            ) => {
                left_index == right_index
                    && left_expected == right_expected
                    && left_actual == right_actual
            }
            (
                Self::QueueSizeZero {
                    queue_index: left_index,
                },
                Self::QueueSizeZero {
                    queue_index: right_index,
                },
            )
            | (
                Self::QueueNotReady {
                    queue_index: left_index,
                },
                Self::QueueNotReady {
                    queue_index: right_index,
                },
            ) => left_index == right_index,
            _ => false,
        }
    }
}

impl Eq for VirtioMemDeviceActivationError {}

impl fmt::Display for VirtioMemDeviceActivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyActive => f.write_str("virtio-mem device is already active"),
            Self::QueueCountMismatch { expected, actual } => {
                write!(
                    f,
                    "virtio-mem device requires {expected} queue(s), got {actual}"
                )
            }
            Self::QueueMetadata {
                queue_index,
                source,
            } => write!(
                f,
                "failed to read virtio-mem queue {queue_index} activation metadata: {source}"
            ),
            Self::QueueBuild {
                queue_index,
                source,
            } => write!(
                f,
                "failed to build virtio-mem queue {queue_index}: {source}"
            ),
            Self::QueueMaxSizeMismatch {
                queue_index,
                expected,
                actual,
            } => write!(
                f,
                "virtio-mem queue {queue_index} max size must be {expected}, got {actual}"
            ),
            Self::QueueSizeZero { queue_index } => {
                write!(f, "virtio-mem queue {queue_index} size must be nonzero")
            }
            Self::QueueNotReady { queue_index } => {
                write!(f, "virtio-mem queue {queue_index} is not ready")
            }
        }
    }
}

impl std::error::Error for VirtioMemDeviceActivationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueMetadata { source, .. } => Some(source),
            Self::QueueBuild { source, .. } => Some(source),
            Self::AlreadyActive
            | Self::QueueCountMismatch { .. }
            | Self::QueueMaxSizeMismatch { .. }
            | Self::QueueSizeZero { .. }
            | Self::QueueNotReady { .. } => None,
        }
    }
}

impl From<VirtioMemDeviceActivationError> for VirtioMmioDeviceActivationError {
    fn from(source: VirtioMemDeviceActivationError) -> Self {
        MmioHandlerError::new(source.to_string()).into()
    }
}

pub fn virtio_mem_mmio_handler_from_config_space(
    config_space: VirtioMemConfigSpace,
) -> Result<VirtioMemMmioHandler, VirtioMmioRegisterHandlerError> {
    VirtioMmioRegisterHandler::with_device_config_and_activation(
        VIRTIO_MEM_DEVICE_ID,
        config_space.available_features(),
        &VIRTIO_MEM_QUEUE_SIZES,
        config_space,
        VirtioMemDevice::new(),
    )
}

fn response_for_request(
    request: VirtioMemRequestKind,
    config_space: VirtioMemConfigSpace,
    plugged_blocks: &VirtioMemPluggedBlocks,
) -> VirtioMemPreparedRequestExecution {
    match request {
        VirtioMemRequestKind::State(range) => match requested_block_range(range, config_space) {
            Some(range) => {
                let state = plugged_blocks.range_state(range);
                VirtioMemPreparedRequestExecution::new(
                    state.response(),
                    VirtioMemRequestExecutionOutcome::State { state },
                    None,
                )
            }
            None => policy_error_response(),
        },
        VirtioMemRequestKind::Plug(range) => {
            prepare_plug_request(range, config_space, plugged_blocks)
        }
        VirtioMemRequestKind::Unplug(range) => {
            prepare_unplug_request(range, config_space, plugged_blocks)
        }
        VirtioMemRequestKind::UnplugAll => VirtioMemPreparedRequestExecution::new(
            VirtioMemResponse::Ack,
            VirtioMemRequestExecutionOutcome::MutationAccepted,
            Some(VirtioMemPendingMutation::UnplugAll),
        ),
        VirtioMemRequestKind::Unsupported { request_type } => {
            VirtioMemPreparedRequestExecution::new(
                VirtioMemResponse::Error,
                VirtioMemRequestExecutionOutcome::UnsupportedRequest { request_type },
                None,
            )
        }
    }
}

fn requested_block_range(
    range: VirtioMemRequestedRange,
    config_space: VirtioMemConfigSpace,
) -> Option<VirtioMemBlockRange> {
    let block_size = config_space.block_size();
    if block_size == 0 {
        return None;
    }
    if range.block_count() == 0 {
        return None;
    }
    let start_offset = range
        .address()
        .raw_value()
        .checked_sub(config_space.addr())
        .filter(|start_offset| *start_offset < config_space.usable_region_size())
        .filter(|start_offset| *start_offset < config_space.region_size())?;
    if !start_offset.is_multiple_of(block_size) {
        return None;
    }
    let range_len = u64::from(range.block_count()).checked_mul(block_size)?;
    let end_offset = start_offset.checked_add(range_len)?;
    if end_offset > config_space.usable_region_size() {
        return None;
    }
    if end_offset > config_space.region_size() {
        return None;
    }
    let start_block = start_offset / block_size;

    VirtioMemBlockRange::new(start_block, u64::from(range.block_count()))
}

fn prepare_plug_request(
    range: VirtioMemRequestedRange,
    config_space: VirtioMemConfigSpace,
    plugged_blocks: &VirtioMemPluggedBlocks,
) -> VirtioMemPreparedRequestExecution {
    let Some(range) = requested_block_range(range, config_space) else {
        return policy_error_response();
    };
    let VirtioMemBlockState::Unplugged = plugged_blocks.range_state(range) else {
        return policy_error_response();
    };
    let Some(range_len) = range.len_bytes(config_space.block_size()) else {
        return policy_error_response();
    };
    let Some(plugged_size) = plugged_blocks
        .plugged_size(config_space.block_size())
        .checked_add(range_len)
    else {
        return policy_error_response();
    };
    if plugged_size > config_space.requested_size() {
        return policy_error_response();
    }

    VirtioMemPreparedRequestExecution::new(
        VirtioMemResponse::Ack,
        VirtioMemRequestExecutionOutcome::MutationAccepted,
        Some(VirtioMemPendingMutation::Plug(range)),
    )
}

fn prepare_unplug_request(
    range: VirtioMemRequestedRange,
    config_space: VirtioMemConfigSpace,
    plugged_blocks: &VirtioMemPluggedBlocks,
) -> VirtioMemPreparedRequestExecution {
    let Some(range) = requested_block_range(range, config_space) else {
        return policy_error_response();
    };
    let VirtioMemBlockState::Plugged = plugged_blocks.range_state(range) else {
        return policy_error_response();
    };

    VirtioMemPreparedRequestExecution::new(
        VirtioMemResponse::Ack,
        VirtioMemRequestExecutionOutcome::MutationAccepted,
        Some(VirtioMemPendingMutation::Unplug(range)),
    )
}

const fn policy_error_response() -> VirtioMemPreparedRequestExecution {
    VirtioMemPreparedRequestExecution::new(
        VirtioMemResponse::Error,
        VirtioMemRequestExecutionOutcome::PolicyError,
        None,
    )
}

fn descriptor_at(
    chain: &VirtqueueDescriptorChain,
    index: usize,
    expected: usize,
) -> Result<&VirtqueueDescriptor, VirtioMemRequestError> {
    chain
        .descriptors()
        .get(index)
        .ok_or(VirtioMemRequestError::DescriptorChainTooShort {
            expected,
            actual: chain.len(),
        })
}

fn validate_request_descriptor(
    descriptor: &VirtqueueDescriptor,
) -> Result<(), VirtioMemRequestError> {
    if descriptor.is_write_only() {
        return Err(VirtioMemRequestError::RequestDescriptorWriteOnly {
            index: descriptor.index(),
        });
    }
    if descriptor.len() < VIRTIO_MEM_REQUEST_SIZE_U32 {
        return Err(VirtioMemRequestError::RequestDescriptorInvalidLength {
            index: descriptor.index(),
            len: descriptor.len(),
            expected: VIRTIO_MEM_REQUEST_SIZE_U32,
        });
    }

    Ok(())
}

fn read_virtio_mem_request(
    memory: &GuestMemory,
    descriptor: &VirtqueueDescriptor,
) -> Result<VirtioMemRequestKind, VirtioMemRequestError> {
    let mut bytes = [0; VIRTIO_MEM_REQUEST_SIZE];
    memory
        .read_slice(&mut bytes, descriptor.address())
        .map_err(|source| VirtioMemRequestError::ReadRequest {
            address: descriptor.address(),
            source,
        })?;

    Ok(VirtioMemRequestKind::from_le_bytes(bytes))
}

fn validate_response_descriptor(
    descriptor: &VirtqueueDescriptor,
) -> Result<VirtioMemResponseDescriptor, VirtioMemRequestError> {
    if !descriptor.is_write_only() {
        return Err(VirtioMemRequestError::ResponseDescriptorReadOnly {
            index: descriptor.index(),
        });
    }
    if descriptor.len() < VIRTIO_MEM_RESPONSE_SIZE_U32 {
        return Err(VirtioMemRequestError::ResponseDescriptorInvalidLength {
            index: descriptor.index(),
            len: descriptor.len(),
            expected: VIRTIO_MEM_RESPONSE_SIZE_U32,
        });
    }

    Ok(VirtioMemResponseDescriptor {
        index: descriptor.index(),
        address: descriptor.address(),
    })
}

fn validate_virtio_mem_queue(
    queue_index: u32,
    queue: VirtioMmioQueueState,
) -> Result<(), VirtioMemDeviceActivationError> {
    if queue.max_size() != VIRTIO_MEM_QUEUE_SIZE {
        return Err(VirtioMemDeviceActivationError::QueueMaxSizeMismatch {
            queue_index,
            expected: VIRTIO_MEM_QUEUE_SIZE,
            actual: queue.max_size(),
        });
    }
    if !queue.ready() {
        return Err(VirtioMemDeviceActivationError::QueueNotReady { queue_index });
    }
    if queue.size() == 0 {
        return Err(VirtioMemDeviceActivationError::QueueSizeZero { queue_index });
    }

    Ok(())
}

const fn virtio_feature_bit(feature: u32) -> u64 {
    1_u64 << feature
}

fn read_virtio_mem_config_bytes(
    bytes: &[u8; VIRTIO_MEM_CONFIG_SPACE_SIZE],
    access: VirtioMmioDeviceConfigAccess,
) -> Result<&[u8], VirtioMmioDeviceConfigError> {
    let offset = usize::try_from(access.offset()).map_err(|_| {
        VirtioMmioDeviceConfigError::UnsupportedRead {
            offset: access.offset(),
            len: access.len(),
        }
    })?;
    let Some(end) = offset.checked_add(access.len()) else {
        return Err(VirtioMmioDeviceConfigError::UnsupportedRead {
            offset: access.offset(),
            len: access.len(),
        });
    };

    bytes
        .get(offset..end)
        .ok_or(VirtioMmioDeviceConfigError::UnsupportedRead {
            offset: access.offset(),
            len: access.len(),
        })
}

fn config_bytes_error(source: MmioAccessBytesError) -> VirtioMmioDeviceConfigError {
    VirtioMmioDeviceConfigError::Handler {
        source: MmioHandlerError::new(format!("virtio-mem config access bytes failed: {source}")),
    }
}

fn allocate_virtio_mem_region(
    region_size: u64,
    alignment: u64,
    reserved_ranges: &[GuestMemoryRange],
) -> Result<GuestAddress, VirtioMemPrepareError> {
    let mut address =
        align_virtio_mem_region_address(VIRTIO_MEM_DEFAULT_REGION_ADDRESS, alignment)?;

    loop {
        let Some(end) = address.checked_add(region_size) else {
            return Err(VirtioMemPrepareError::RegionAddressOverflow {
                address,
                region_size,
            });
        };
        if end.raw_value() > VIRTIO_MEM_REGION_ADDRESS_SPACE_END {
            return Err(VirtioMemPrepareError::RegionUnavailable {
                region_size,
                alignment,
                address_space_end: GuestAddress::new(VIRTIO_MEM_REGION_ADDRESS_SPACE_END),
            });
        }
        let candidate = GuestMemoryRange::new(address, region_size).map_err(|_| {
            VirtioMemPrepareError::RegionAddressOverflow {
                address,
                region_size,
            }
        })?;
        let Some(overlap) = reserved_ranges
            .iter()
            .copied()
            .filter(|reserved| candidate.overlaps(*reserved))
            .max_by_key(|reserved| reserved.end_exclusive().raw_value())
        else {
            return Ok(address);
        };

        address = align_virtio_mem_region_address(overlap.end_exclusive(), alignment)?;
    }
}

fn align_virtio_mem_region_address(
    address: GuestAddress,
    alignment: u64,
) -> Result<GuestAddress, VirtioMemPrepareError> {
    let remainder = address.raw_value() % alignment;
    if remainder == 0 {
        return Ok(address);
    }
    let offset = alignment - remainder;
    address
        .checked_add(offset)
        .ok_or(VirtioMemPrepareError::RegionAlignmentOverflow { address, alignment })
}

fn mib_to_bytes(mib: u64, field: VirtioMemSizeField) -> Result<u64, VirtioMemPrepareError> {
    mib.checked_mul(MIB)
        .ok_or(VirtioMemPrepareError::SizeOverflow {
            field: match field {
                VirtioMemSizeField::Block => "block_size_mib",
                VirtioMemSizeField::Slot => "slot_size_mib",
                VirtioMemSizeField::Total => "total_size_mib",
            },
            mib,
        })
}

fn memory_hotplug_mib_to_bytes(
    mib: u64,
    field: &'static str,
) -> Result<u64, MemoryHotplugUpdateError> {
    mib.checked_mul(MIB)
        .ok_or(MemoryHotplugUpdateError::SizeOverflow { field, mib })
}

fn next_slot_multiple(value: u64, slot_size: u64) -> Result<u64, MemoryHotplugUpdateError> {
    let remainder = value % slot_size;
    if remainder == 0 {
        return Ok(value);
    }

    value.checked_add(slot_size - remainder).ok_or(
        MemoryHotplugUpdateError::UsableRegionSizeOverflow {
            requested_size: value,
            slot_size,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::interrupt::DeviceInterruptKind;
    use crate::memory::{GuestAddress, GuestMemoryLayout, GuestMemoryRange};
    use crate::mmio::{MmioAccess, MmioBus, MmioOperation, MmioRegionId};
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
        VIRTIO_DEVICE_STATUS_INIT, VIRTIO_MMIO_DEVICE_CONFIG_OFFSET,
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioAccess, VirtioMmioDeviceActivation,
        VirtioMmioDeviceRegisters, VirtioMmioQueueRegisters, VirtioMmioRegister,
        decode_virtio_mmio_access,
    };
    use crate::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE, read_descriptor_chain,
    };

    const TEST_VIRTIO_MEM_MMIO_BASE: u64 = 0x1000_0000;
    const TEST_VIRTIO_MEM_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(77);
    const TEST_QUEUE_CONFIG_STATUS: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    const TEST_DRIVER_OK_STATUS: u32 = TEST_QUEUE_CONFIG_STATUS | VIRTIO_DEVICE_STATUS_DRIVER_OK;
    const TEST_QUEUE_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x1000);
    const TEST_QUEUE_DRIVER_RING: GuestAddress = GuestAddress::new(0x2000);
    const TEST_QUEUE_DEVICE_RING: GuestAddress = GuestAddress::new(0x3000);
    const TEST_QUEUE_SIZE: u16 = 8;
    const TEST_MEMORY_SIZE: u64 = 0x8000;
    const TEST_VIRTIO_MEM_REQUEST_ADDR: GuestAddress = GuestAddress::new(0x4000);
    const TEST_VIRTIO_MEM_RESPONSE_ADDR: GuestAddress = GuestAddress::new(0x5000);

    #[test]
    fn validates_default_sized_config() {
        let input = MemoryHotplugConfigInput::new(1024, 2, 128);

        assert_eq!(input.validate(), Ok(()));
    }

    #[test]
    fn validates_config_input_into_config() {
        let config = MemoryHotplugConfig::try_from(MemoryHotplugConfigInput::new(1024, 2, 128))
            .expect("valid memory hotplug config should convert");

        assert_eq!(config.total_size_mib(), 1024);
        assert_eq!(config.block_size_mib(), 2);
        assert_eq!(config.slot_size_mib(), 128);
    }

    #[test]
    fn exposes_initial_status() {
        let config = MemoryHotplugConfig::try_from(MemoryHotplugConfigInput::new(1024, 2, 128))
            .expect("valid memory hotplug config should convert");
        let status = config.initial_status();

        assert_eq!(status.total_size_mib(), 1024);
        assert_eq!(status.block_size_mib(), 2);
        assert_eq!(status.slot_size_mib(), 128);
        assert_eq!(status.plugged_size_mib(), 0);
        assert_eq!(status.requested_size_mib(), 0);
    }

    #[test]
    fn validates_memory_hotplug_requested_size_updates() {
        let config = MemoryHotplugConfig::try_from(MemoryHotplugConfigInput::new(1024, 2, 128))
            .expect("valid memory hotplug config should convert");

        let zero = config
            .validate_size_update(MemoryHotplugSizeUpdateInput::new(0))
            .expect("zero requested size should be valid");
        assert_eq!(zero.requested_size_mib(), 0);
        assert_eq!(zero.requested_size(), 0);
        assert_eq!(zero.slot_size(), 128 * MIB);

        let exact = config
            .validate_size_update(MemoryHotplugSizeUpdateInput::new(1024))
            .expect("total requested size should be valid");
        assert_eq!(exact.requested_size(), 1024 * MIB);
    }

    #[test]
    fn rejects_invalid_memory_hotplug_requested_size_updates() {
        let config = MemoryHotplugConfig::try_from(MemoryHotplugConfigInput::new(1024, 2, 128))
            .expect("valid memory hotplug config should convert");

        assert_eq!(
            config.validate_size_update(MemoryHotplugSizeUpdateInput::new(3)),
            Err(
                MemoryHotplugUpdateError::RequestedSizeNotMultipleOfBlockSize {
                    requested_size_mib: 3,
                    block_size_mib: 2,
                }
            )
        );
        assert_eq!(
            config.validate_size_update(MemoryHotplugSizeUpdateInput::new(1026)),
            Err(MemoryHotplugUpdateError::RequestedSizeTooLarge {
                requested_size_mib: 1026,
                total_size_mib: 1024,
            })
        );
        assert_eq!(
            config.validate_size_update(MemoryHotplugSizeUpdateInput::new(u64::MAX)),
            Err(MemoryHotplugUpdateError::SizeOverflow {
                field: "requested_size_mib",
                mib: u64::MAX,
            })
        );
    }

    #[test]
    fn prepared_virtio_mem_device_derives_config_space_from_config() {
        let config = MemoryHotplugConfig::try_from(MemoryHotplugConfigInput::new(1024, 2, 128))
            .expect("valid memory hotplug config should convert");
        let prepared =
            PreparedVirtioMemDevice::from_config(config).expect("virtio-mem device should prepare");
        let config_space = prepared.config_space();

        assert_eq!(config_space.block_size(), 2 * MIB);
        assert_eq!(
            config_space.addr(),
            VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value()
        );
        assert_eq!(config_space.region_size(), 1024 * MIB);
        assert_eq!(config_space.usable_region_size(), 0);
        assert_eq!(config_space.plugged_size(), 0);
        assert_eq!(config_space.requested_size(), 0);
    }

    #[test]
    fn prepared_virtio_mem_device_allocates_after_reserved_ranges_on_slot_boundary() {
        let config = MemoryHotplugConfig::try_from(MemoryHotplugConfigInput::new(128, 2, 128))
            .expect("valid memory hotplug config should convert");
        let high_dram = GuestMemoryRange::new(VIRTIO_MEM_DEFAULT_REGION_ADDRESS, 256 * MIB)
            .expect("high DRAM reservation should be valid");
        let pmem = GuestMemoryRange::new(high_dram.end_exclusive(), 2 * MIB)
            .expect("pmem reservation should be valid");

        let prepared =
            PreparedVirtioMemDevice::from_config_with_reserved_ranges(config, &[pmem, high_dram])
                .expect("virtio-mem region should skip reserved DRAM and pmem");

        assert_eq!(
            prepared.config_space().addr(),
            VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value() + 384 * MIB
        );
    }

    #[test]
    fn prepared_virtio_mem_device_reports_exhausted_40_bit_region() {
        let config = MemoryHotplugConfig::try_from(MemoryHotplugConfigInput::new(128, 2, 128))
            .expect("valid memory hotplug config should convert");
        let reserved = GuestMemoryRange::new(
            VIRTIO_MEM_DEFAULT_REGION_ADDRESS,
            VIRTIO_MEM_REGION_ADDRESS_SPACE_END - VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value(),
        )
        .expect("post-MMIO64 address space reservation should be valid");

        assert_eq!(
            PreparedVirtioMemDevice::from_config_with_reserved_ranges(config, &[reserved]),
            Err(VirtioMemPrepareError::RegionUnavailable {
                region_size: 128 * MIB,
                alignment: 128 * MIB,
                address_space_end: GuestAddress::new(VIRTIO_MEM_REGION_ADDRESS_SPACE_END),
            })
        );
    }

    #[test]
    fn prepared_virtio_mem_device_rejects_size_overflow() {
        let total_size_mib = u64::MAX - (u64::MAX % MEMORY_HOTPLUG_DEFAULT_SLOT_SIZE_MIB);
        let config = MemoryHotplugConfig::try_from(MemoryHotplugConfigInput::new(
            total_size_mib,
            MEMORY_HOTPLUG_DEFAULT_BLOCK_SIZE_MIB,
            MEMORY_HOTPLUG_DEFAULT_SLOT_SIZE_MIB,
        ))
        .expect("large but shape-valid memory hotplug config should convert");

        assert_eq!(
            PreparedVirtioMemDevice::from_config(config),
            Err(VirtioMemPrepareError::SizeOverflow {
                field: "total_size_mib",
                mib: total_size_mib,
            })
        );
    }

    #[test]
    fn prepared_virtio_mem_device_rejects_block_size_overflow() {
        let block_size_mib = 1_u64 << 63;
        let config = MemoryHotplugConfig::try_from(MemoryHotplugConfigInput::new(
            block_size_mib,
            block_size_mib,
            block_size_mib,
        ))
        .expect("large but shape-valid memory hotplug config should convert");

        assert_eq!(
            PreparedVirtioMemDevice::from_config(config),
            Err(VirtioMemPrepareError::SizeOverflow {
                field: "block_size_mib",
                mib: block_size_mib,
            })
        );
    }

    #[test]
    fn prepared_virtio_mem_device_rejects_region_address_overflow() {
        let config = MemoryHotplugConfig::try_from(MemoryHotplugConfigInput::new(1024, 2, 128))
            .expect("valid memory hotplug config should convert");

        assert_eq!(
            PreparedVirtioMemDevice::from_config_at(config, GuestAddress::new(u64::MAX)),
            Err(VirtioMemPrepareError::RegionAddressOverflow {
                address: GuestAddress::new(u64::MAX),
                region_size: 1024 * MIB,
            })
        );
    }

    #[test]
    fn virtio_mem_mmio_device_registers_handler() {
        let prepared = PreparedVirtioMemDevice::from_config(memory_hotplug_config())
            .expect("virtio-mem device should prepare");
        let mut device = prepared
            .register_mmio(VirtioMemMmioLayout::new(
                GuestAddress::new(TEST_VIRTIO_MEM_MMIO_BASE),
                TEST_VIRTIO_MEM_MMIO_REGION_ID,
            ))
            .expect("virtio-mem MMIO device should register");
        let registration = device.registration();

        assert_eq!(registration.region_id(), TEST_VIRTIO_MEM_MMIO_REGION_ID);
        assert_eq!(
            registration.address(),
            GuestAddress::new(TEST_VIRTIO_MEM_MMIO_BASE)
        );
        assert_eq!(
            registration.region().range().size(),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        );
        assert_eq!(device.dispatcher().regions(), &[registration.region()]);

        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioMemMmioHandler>(registration.region_id())
            .expect("virtio-mem handler should be registered");
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::DeviceId)
                .expect("device ID should read"),
            VIRTIO_MEM_DEVICE_ID
        );
    }

    #[test]
    fn virtio_mem_mmio_handler_updates_requested_size_and_interrupt_state() {
        let config = memory_hotplug_config();
        let update = config
            .validate_size_update(MemoryHotplugSizeUpdateInput::new(256))
            .expect("requested size update should validate");
        let mut handler = mem_mmio_handler(zero_usable_virtio_mem_config_space());

        handler
            .update_mem_requested_size(update)
            .expect("requested size should update active handler");

        let config_space = handler.device_config_handler();
        assert_eq!(config_space.requested_size(), 256 * MIB);
        assert_eq!(config_space.usable_region_size(), 256 * MIB);
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::ConfigGeneration)
                .expect("config generation should read"),
            1
        );
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status should read"),
            DeviceInterruptKind::Config.status().bits()
        );
    }

    #[test]
    fn virtio_mem_mmio_handler_grows_but_does_not_shrink_usable_region() {
        let config = memory_hotplug_config();
        let mut handler = mem_mmio_handler(zero_usable_virtio_mem_config_space());
        let grow = config
            .validate_size_update(MemoryHotplugSizeUpdateInput::new(130))
            .expect("requested size update should validate");
        let shrink = config
            .validate_size_update(MemoryHotplugSizeUpdateInput::new(64))
            .expect("smaller requested size update should validate");

        handler
            .update_mem_requested_size(grow)
            .expect("requested size should grow usable region");
        assert_eq!(handler.device_config_handler().requested_size(), 130 * MIB);
        assert_eq!(
            handler.device_config_handler().usable_region_size(),
            256 * MIB
        );

        handler
            .update_mem_requested_size(shrink)
            .expect("requested size should shrink without shrinking usable region");
        assert_eq!(handler.device_config_handler().requested_size(), 64 * MIB);
        assert_eq!(
            handler.device_config_handler().usable_region_size(),
            256 * MIB
        );
    }

    #[test]
    fn virtio_mem_mmio_device_rejects_overlapping_region() {
        let prepared = PreparedVirtioMemDevice::from_config(memory_hotplug_config())
            .expect("virtio-mem device should prepare");
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(
                MmioRegionId::new(1),
                GuestAddress::new(TEST_VIRTIO_MEM_MMIO_BASE),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("existing MMIO region should insert");

        let error = prepared
            .register_mmio_with_dispatcher(
                VirtioMemMmioLayout::new(
                    GuestAddress::new(TEST_VIRTIO_MEM_MMIO_BASE),
                    TEST_VIRTIO_MEM_MMIO_REGION_ID,
                ),
                dispatcher,
            )
            .expect_err("overlapping virtio-mem MMIO region should fail");

        assert!(matches!(
            error,
            VirtioMemMmioRegistrationError::InsertRegion {
                source: MmioBusError::OverlappingRegion { .. },
                ..
            }
        ));
    }

    #[test]
    fn virtio_mem_mmio_device_rejects_duplicate_handler() {
        let prepared = PreparedVirtioMemDevice::from_config(memory_hotplug_config())
            .expect("virtio-mem device should prepare");
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .register_handler(
                TEST_VIRTIO_MEM_MMIO_REGION_ID,
                virtio_mem_mmio_handler_from_config_space(virtio_mem_config_space())
                    .expect("existing handler should build"),
            )
            .expect("existing handler should register");

        let error = prepared
            .register_mmio_with_dispatcher(
                VirtioMemMmioLayout::new(
                    GuestAddress::new(TEST_VIRTIO_MEM_MMIO_BASE),
                    TEST_VIRTIO_MEM_MMIO_REGION_ID,
                ),
                dispatcher,
            )
            .expect_err("duplicate virtio-mem MMIO handler should fail");

        assert!(matches!(
            error,
            VirtioMemMmioRegistrationError::RegisterHandler {
                source: MmioDispatchError::DuplicateHandler {
                    region_id: TEST_VIRTIO_MEM_MMIO_REGION_ID,
                },
                ..
            }
        ));
    }

    #[test]
    fn rejects_block_size_below_minimum() {
        let input = MemoryHotplugConfigInput::new(1024, 1, 128);

        assert_eq!(
            input.validate(),
            Err(MemoryHotplugConfigError::BlockSizeTooSmall { min_mib: 2 })
        );
    }

    #[test]
    fn rejects_block_size_that_is_not_power_of_two() {
        let input = MemoryHotplugConfigInput::new(1024, 3, 128);

        assert_eq!(
            input.validate(),
            Err(MemoryHotplugConfigError::BlockSizeNotPowerOfTwo)
        );
    }

    #[test]
    fn rejects_slot_size_below_minimum() {
        let input = MemoryHotplugConfigInput::new(1024, 2, 64);

        assert_eq!(
            input.validate(),
            Err(MemoryHotplugConfigError::SlotSizeTooSmall { min_mib: 128 })
        );
    }

    #[test]
    fn rejects_slot_size_that_is_not_multiple_of_block_size() {
        let input = MemoryHotplugConfigInput::new(1024, 4, 130);

        assert_eq!(
            input.validate(),
            Err(MemoryHotplugConfigError::SlotSizeNotMultipleOfBlockSize { block_size_mib: 4 })
        );
    }

    #[test]
    fn rejects_total_size_below_slot_size() {
        let input = MemoryHotplugConfigInput::new(64, 2, 128);

        assert_eq!(
            input.validate(),
            Err(MemoryHotplugConfigError::TotalSizeTooSmall { slot_size_mib: 128 })
        );
    }

    #[test]
    fn rejects_total_size_that_is_not_multiple_of_slot_size() {
        let input = MemoryHotplugConfigInput::new(1000, 2, 128);

        assert_eq!(
            input.validate(),
            Err(MemoryHotplugConfigError::TotalSizeNotMultipleOfSlotSize { slot_size_mib: 128 })
        );
    }

    #[test]
    fn exposes_size_update_input() {
        let input = MemoryHotplugSizeUpdateInput::new(256);

        assert_eq!(input.requested_size_mib(), 256);
    }

    #[test]
    fn memory_hotplug_status_rejects_non_mib_aligned_plugged_bytes() {
        assert_eq!(
            MemoryHotplugStatus::try_from_plugged_size_bytes(memory_hotplug_config(), MIB + 1, 256),
            Err(MemoryHotplugStatusError::PluggedSizeNotMibAligned {
                plugged_size: MIB + 1,
            })
        );
    }

    #[test]
    fn virtio_mem_constants_match_firecracker_shape() {
        assert_eq!(VIRTIO_MEM_DEVICE_ID, 24);
        assert_eq!(VIRTIO_MEM_QUEUE_COUNT, 1);
        assert_eq!(VIRTIO_MEM_QUEUE_SIZE, 256);
        assert_eq!(VIRTIO_MEM_QUEUE_SIZES, [VIRTIO_MEM_QUEUE_SIZE]);
        assert_eq!(VIRTIO_MEM_CONFIG_SPACE_SIZE, 56);
        assert_eq!(VIRTIO_MEM_F_UNPLUGGED_INACCESSIBLE, 1);
    }

    #[test]
    fn virtio_mem_config_space_tracks_fields() {
        let config = VirtioMemConfigSpace::new(0x200000, 0x4000_0000, 0x8000_0000)
            .with_node_id(7)
            .with_usable_region_size(0x7000_0000)
            .with_plugged_size(0x2000_0000)
            .with_requested_size(0x3000_0000);

        assert_eq!(config.block_size(), 0x200000);
        assert_eq!(config.node_id(), 7);
        assert_eq!(config.addr(), 0x4000_0000);
        assert_eq!(config.region_size(), 0x8000_0000);
        assert_eq!(config.usable_region_size(), 0x7000_0000);
        assert_eq!(config.plugged_size(), 0x2000_0000);
        assert_eq!(config.requested_size(), 0x3000_0000);
    }

    #[test]
    fn virtio_mem_config_space_uses_firecracker_little_endian_layout() {
        let config = VirtioMemConfigSpace::new(
            0x0102_0304_0506_0708,
            0x2122_2324_2526_2728,
            0x3132_3334_3536_3738,
        )
        .with_node_id(0x1112)
        .with_usable_region_size(0x4142_4344_4546_4748)
        .with_plugged_size(0x5152_5354_5556_5758)
        .with_requested_size(0x6162_6364_6566_6768);
        let expected = [
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x12, 0x11, 0, 0, 0, 0, 0, 0, 0x28,
            0x27, 0x26, 0x25, 0x24, 0x23, 0x22, 0x21, 0x38, 0x37, 0x36, 0x35, 0x34, 0x33, 0x32,
            0x31, 0x48, 0x47, 0x46, 0x45, 0x44, 0x43, 0x42, 0x41, 0x58, 0x57, 0x56, 0x55, 0x54,
            0x53, 0x52, 0x51, 0x68, 0x67, 0x66, 0x65, 0x64, 0x63, 0x62, 0x61,
        ];

        assert_eq!(config.to_le_bytes(), expected);
        assert_eq!(VirtioMemConfigSpace::from_le_bytes(expected), config);
    }

    #[test]
    fn virtio_mem_config_space_preserves_boundaries() {
        let config = VirtioMemConfigSpace::new(u64::MAX, u64::MAX, u64::MAX)
            .with_node_id(u16::MAX)
            .with_usable_region_size(u64::MAX)
            .with_plugged_size(u64::MAX)
            .with_requested_size(u64::MAX);
        let mut expected = [0xff; VIRTIO_MEM_CONFIG_SPACE_SIZE];
        expected[10..16].fill(0);

        assert_eq!(config.to_le_bytes(), expected);
        assert_eq!(VirtioMemConfigSpace::from_le_bytes(expected), config);
    }

    #[test]
    fn virtio_mem_config_space_advertises_foundation_features() {
        let config = VirtioMemConfigSpace::new(0, 0, 0);

        assert_eq!(
            config.available_features(),
            (1_u64 << VIRTIO_FEATURE_VERSION_1) | (1_u64 << VIRTIO_MEM_F_UNPLUGGED_INACCESSIBLE)
        );
    }

    #[test]
    fn virtio_mem_config_space_reads_within_layout() {
        let config = VirtioMemConfigSpace::new(
            0x0102_0304_0506_0708,
            0x2122_2324_2526_2728,
            0x3132_3334_3536_3738,
        )
        .with_node_id(0x1112)
        .with_usable_region_size(0x4142_4344_4546_4748)
        .with_plugged_size(0x5152_5354_5556_5758)
        .with_requested_size(0x6162_6364_6566_6768);

        assert_eq!(
            read_mem_config(&config, 0, 8)
                .expect("block-size read should succeed")
                .as_slice(),
            &0x0102_0304_0506_0708_u64.to_le_bytes()
        );
        assert_eq!(
            read_mem_config(&config, 8, 2)
                .expect("node-id read should succeed")
                .as_slice(),
            &0x1112_u16.to_le_bytes()
        );
        assert_eq!(
            read_mem_config(&config, 10, 4)
                .expect("low padding read should succeed")
                .as_slice(),
            &[0, 0, 0, 0]
        );
        assert_eq!(
            read_mem_config(&config, 14, 2)
                .expect("high padding read should succeed")
                .as_slice(),
            &[0, 0]
        );
        assert_eq!(
            read_mem_config(&config, 16, 8)
                .expect("address read should succeed")
                .as_slice(),
            &0x2122_2324_2526_2728_u64.to_le_bytes()
        );
        assert_eq!(
            read_mem_config(&config, 48, 8)
                .expect("requested-size read should succeed")
                .as_slice(),
            &0x6162_6364_6566_6768_u64.to_le_bytes()
        );
        assert_eq!(
            read_mem_config(&config, 4, 4)
                .expect("partial read should succeed")
                .as_slice(),
            &[0x04, 0x03, 0x02, 0x01]
        );
        assert_eq!(
            read_mem_config(&config, 55, 1)
                .expect("last byte read should succeed")
                .as_slice(),
            &[0x61]
        );
    }

    #[test]
    fn virtio_mem_config_space_rejects_out_of_bounds_reads() {
        let config = VirtioMemConfigSpace::new(0, 0, 0);

        assert_eq!(
            read_mem_config(&config, 56, 1),
            Err(VirtioMmioDeviceConfigError::UnsupportedRead { offset: 56, len: 1 })
        );
        assert_eq!(
            read_mem_config(&config, 55, 2),
            Err(VirtioMmioDeviceConfigError::UnsupportedRead { offset: 55, len: 2 })
        );
    }

    #[test]
    fn virtio_mem_config_space_rejects_guest_writes() {
        let mut config = VirtioMemConfigSpace::new(0x200000, 0x4000_0000, 0x8000_0000);

        assert_eq!(
            write_mem_config(&mut config, 0, &[1, 2, 3, 4]),
            Err(VirtioMmioDeviceConfigError::UnsupportedWrite { offset: 0, len: 4 })
        );
        assert_eq!(
            config,
            VirtioMemConfigSpace::new(0x200000, 0x4000_0000, 0x8000_0000)
        );
    }

    #[test]
    fn virtio_mem_mmio_handler_exposes_firecracker_shape() {
        let config = virtio_mem_config_space();
        let handler = mem_mmio_handler(config);

        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::DeviceId)
                .expect("device ID should read"),
            VIRTIO_MEM_DEVICE_ID
        );
        assert_eq!(
            handler.queue_registers().queue_count(),
            VIRTIO_MEM_QUEUE_COUNT
        );
        assert_eq!(
            handler
                .queue_registers()
                .selected_queue()
                .expect("queue zero should exist")
                .max_size(),
            VIRTIO_MEM_QUEUE_SIZE
        );

        let features = handler.device_registers().device_features();
        assert_ne!(features & (1_u64 << VIRTIO_MEM_F_UNPLUGGED_INACCESSIBLE), 0);
        assert_ne!(features & (1_u64 << VIRTIO_FEATURE_VERSION_1), 0);
        assert_eq!(
            read_mem_handler_config(&handler, 0, 8)
                .expect("config read should route through handler")
                .as_slice(),
            &config.block_size().to_le_bytes()
        );
    }

    #[test]
    fn virtio_mem_request_and_response_layouts_match_firecracker_shape() {
        assert_eq!(VIRTIO_MEM_REQUEST_SIZE, 24);
        assert_eq!(VIRTIO_MEM_RESPONSE_SIZE, 10);

        let request = VirtioMemRequestKind::from_le_bytes(virtio_mem_request_bytes(
            VIRTIO_MEM_REQ_STATE,
            0x0102_0304_0506_0708,
            0x1112,
        ));
        assert_eq!(
            request,
            VirtioMemRequestKind::State(VirtioMemRequestedRange::new(
                GuestAddress::new(0x0102_0304_0506_0708),
                0x1112,
            ))
        );
        assert_eq!(
            VirtioMemResponse::AckUnplugged.to_le_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 1, 0]
        );
        assert_eq!(
            VirtioMemResponse::AckPlugged.to_le_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            VirtioMemResponse::AckMixed.to_le_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 2, 0]
        );
        assert_eq!(
            VirtioMemResponse::Ack.to_le_bytes(),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            VirtioMemResponse::Error.to_le_bytes(),
            [3, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn virtio_mem_request_parser_accepts_supported_and_unknown_types() {
        for (request_type, expected) in [
            (
                VIRTIO_MEM_REQ_PLUG,
                VirtioMemRequestKind::Plug(VirtioMemRequestedRange::new(
                    GuestAddress::new(0x4000_0000),
                    2,
                )),
            ),
            (
                VIRTIO_MEM_REQ_UNPLUG,
                VirtioMemRequestKind::Unplug(VirtioMemRequestedRange::new(
                    GuestAddress::new(0x4000_0000),
                    2,
                )),
            ),
            (VIRTIO_MEM_REQ_UNPLUG_ALL, VirtioMemRequestKind::UnplugAll),
            (
                VIRTIO_MEM_REQ_STATE,
                VirtioMemRequestKind::State(VirtioMemRequestedRange::new(
                    GuestAddress::new(0x4000_0000),
                    2,
                )),
            ),
            (99, VirtioMemRequestKind::Unsupported { request_type: 99 }),
        ] {
            let mut memory = request_memory();
            write_virtio_mem_chain(&mut memory, request_type, 0x4000_0000, 2);
            let chain = read_mem_descriptor_chain(&memory);

            let request = VirtioMemRequest::parse(&memory, &chain).expect("request should parse");

            assert_eq!(request.descriptor_head(), 0);
            assert_eq!(request.kind(), expected);
            assert_eq!(request.response().index(), 1);
            assert_eq!(request.response().address(), TEST_VIRTIO_MEM_RESPONSE_ADDR);
        }
    }

    #[test]
    fn virtio_mem_request_parser_rejects_malformed_descriptor_chains() {
        let cases = [
            (
                TestDescriptor::readable(
                    TEST_VIRTIO_MEM_REQUEST_ADDR,
                    VIRTIO_MEM_REQUEST_SIZE_U32 - 1,
                    Some(1),
                ),
                TestDescriptor::writable(
                    TEST_VIRTIO_MEM_RESPONSE_ADDR,
                    VIRTIO_MEM_RESPONSE_SIZE_U32,
                    None,
                ),
                VirtioMemRequestError::RequestDescriptorInvalidLength {
                    index: 0,
                    len: VIRTIO_MEM_REQUEST_SIZE_U32 - 1,
                    expected: VIRTIO_MEM_REQUEST_SIZE_U32,
                },
            ),
            (
                TestDescriptor::writable(
                    TEST_VIRTIO_MEM_REQUEST_ADDR,
                    VIRTIO_MEM_REQUEST_SIZE_U32,
                    Some(1),
                ),
                TestDescriptor::writable(
                    TEST_VIRTIO_MEM_RESPONSE_ADDR,
                    VIRTIO_MEM_RESPONSE_SIZE_U32,
                    None,
                ),
                VirtioMemRequestError::RequestDescriptorWriteOnly { index: 0 },
            ),
            (
                TestDescriptor::readable(
                    TEST_VIRTIO_MEM_REQUEST_ADDR,
                    VIRTIO_MEM_REQUEST_SIZE_U32,
                    Some(1),
                ),
                TestDescriptor::readable(
                    TEST_VIRTIO_MEM_RESPONSE_ADDR,
                    VIRTIO_MEM_RESPONSE_SIZE_U32,
                    None,
                ),
                VirtioMemRequestError::ResponseDescriptorReadOnly { index: 1 },
            ),
            (
                TestDescriptor::readable(
                    TEST_VIRTIO_MEM_REQUEST_ADDR,
                    VIRTIO_MEM_REQUEST_SIZE_U32,
                    Some(1),
                ),
                TestDescriptor::writable(
                    TEST_VIRTIO_MEM_RESPONSE_ADDR,
                    VIRTIO_MEM_RESPONSE_SIZE_U32 - 1,
                    None,
                ),
                VirtioMemRequestError::ResponseDescriptorInvalidLength {
                    index: 1,
                    len: VIRTIO_MEM_RESPONSE_SIZE_U32 - 1,
                    expected: VIRTIO_MEM_RESPONSE_SIZE_U32,
                },
            ),
        ];

        for (request_descriptor, response_descriptor, expected) in cases {
            let mut memory = request_memory();
            memory
                .write_slice(
                    &virtio_mem_request_bytes(VIRTIO_MEM_REQ_STATE, 0x4000_0000, 1),
                    TEST_VIRTIO_MEM_REQUEST_ADDR,
                )
                .expect("request should write");
            write_descriptor(&mut memory, 0, request_descriptor);
            write_descriptor(&mut memory, 1, response_descriptor);
            let chain = read_mem_descriptor_chain(&memory);

            assert_eq!(VirtioMemRequest::parse(&memory, &chain), Err(expected));
        }

        let mut memory = request_memory();
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VIRTIO_MEM_REQUEST_ADDR,
                VIRTIO_MEM_REQUEST_SIZE_U32,
                None,
            ),
        );
        let chain = read_mem_descriptor_chain(&memory);

        assert_eq!(
            VirtioMemRequest::parse(&memory, &chain),
            Err(VirtioMemRequestError::DescriptorChainTooShort {
                expected: 2,
                actual: 1,
            })
        );
    }

    #[test]
    fn virtio_mem_queue_dispatch_completes_valid_state_as_unplugged() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_STATE, 0x4000_0000, 1);
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space().with_usable_region_size(0x20_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();

        let dispatch = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("state request should dispatch");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.state_requests(), 1);
        assert_eq!(dispatch.policy_errors(), 0);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(
            read_response(&memory),
            VirtioMemResponse::AckUnplugged.to_le_bytes()
        );
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(
            read_used_element(&memory, 0),
            (0, VIRTIO_MEM_RESPONSE_SIZE_U32)
        );
    }

    #[test]
    fn virtio_mem_queue_dispatch_validates_request_offset_alignment() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_STATE, 0x4000_0001, 1);
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = VirtioMemConfigSpace::new(0x20_0000, 0x4000_0001, 0x8000_0000)
            .with_usable_region_size(0x20_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();

        let dispatch = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("state request should validate against the hotplug aperture offset");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.state_requests(), 1);
        assert_eq!(
            read_response(&memory),
            VirtioMemResponse::AckUnplugged.to_le_bytes()
        );
    }

    #[test]
    fn virtio_mem_queue_dispatch_errors_current_zero_usable_policy() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_STATE, 0x4000_0000, 1);
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = zero_usable_virtio_mem_config_space();
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();

        let dispatch = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("state request should dispatch with error response");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.state_requests(), 0);
        assert_eq!(dispatch.policy_errors(), 1);
        assert_eq!(
            read_response(&memory),
            VirtioMemResponse::Error.to_le_bytes()
        );
        assert_eq!(
            read_used_element(&memory, 0),
            (0, VIRTIO_MEM_RESPONSE_SIZE_U32)
        );
    }

    #[test]
    fn virtio_mem_queue_dispatch_accepts_request_ending_at_usable_limit() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4020_0000, 1);
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space()
            .with_usable_region_size(0x40_0000)
            .with_requested_size(0x40_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();

        let dispatch = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("exact end-exclusive usable bound should dispatch");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.policy_errors(), 0);
        assert_eq!(read_response(&memory), VirtioMemResponse::Ack.to_le_bytes());
        assert_eq!(config_space.plugged_size(), 0x20_0000);
    }

    #[test]
    fn virtio_mem_queue_dispatch_rejects_request_past_usable_limit() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4000_0000, 2);
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space()
            .with_usable_region_size(0x20_0000)
            .with_requested_size(0x40_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();

        let dispatch = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("out-of-usable request should dispatch with error response");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.policy_errors(), 1);
        assert_eq!(
            read_response(&memory),
            VirtioMemResponse::Error.to_le_bytes()
        );
        assert_eq!(config_space.plugged_size(), 0);
    }

    #[test]
    fn virtio_mem_queue_dispatch_rejects_request_past_region_limit() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4000_0000, 2);
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = VirtioMemConfigSpace::new(0x20_0000, 0x4000_0000, 0x20_0000)
            .with_usable_region_size(0x40_0000)
            .with_requested_size(0x40_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();

        let dispatch = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("out-of-region request should dispatch with error response");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.policy_errors(), 1);
        assert_eq!(
            read_response(&memory),
            VirtioMemResponse::Error.to_le_bytes()
        );
        assert_eq!(config_space.plugged_size(), 0);
    }

    #[test]
    fn virtio_mem_queue_dispatch_accepts_plug_unplug_and_unplug_all() {
        let mut memory = request_memory();
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space()
            .with_usable_region_size(0x80_0000)
            .with_requested_size(0x80_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();

        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4000_0000, 2);
        write_available_heads(&mut memory, &[0]);
        let dispatch = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("plug request should dispatch");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.policy_errors(), 0);
        assert_eq!(read_response(&memory), VirtioMemResponse::Ack.to_le_bytes());
        assert_eq!(config_space.plugged_size(), 0x40_0000);

        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_STATE, 0x4000_0000, 1);
        write_available_heads(&mut memory, &[0, 0]);
        let dispatch = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("plugged state request should dispatch");

        assert_eq!(dispatch.state_requests(), 1);
        assert_eq!(
            read_response(&memory),
            VirtioMemResponse::AckPlugged.to_le_bytes()
        );

        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_STATE, 0x4000_0000, 3);
        write_available_heads(&mut memory, &[0, 0, 0]);
        queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("mixed state request should dispatch");

        assert_eq!(
            read_response(&memory),
            VirtioMemResponse::AckMixed.to_le_bytes()
        );

        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_UNPLUG, 0x4000_0000, 2);
        write_available_heads(&mut memory, &[0, 0, 0, 0]);
        queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("unplug request should dispatch");

        assert_eq!(read_response(&memory), VirtioMemResponse::Ack.to_le_bytes());
        assert_eq!(config_space.plugged_size(), 0);

        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4040_0000, 1);
        write_available_heads(&mut memory, &[0, 0, 0, 0, 0]);
        queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("second plug request should dispatch");
        assert_eq!(config_space.plugged_size(), 0x20_0000);

        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_UNPLUG_ALL, 0, 0);
        write_available_heads(&mut memory, &[0, 0, 0, 0, 0, 0]);
        queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("unplug all request should dispatch");

        assert_eq!(read_response(&memory), VirtioMemResponse::Ack.to_le_bytes());
        assert_eq!(config_space.plugged_size(), 0);
        assert_eq!(config_space.usable_region_size(), 0);
    }

    #[test]
    fn virtio_mem_queue_dispatch_rejects_duplicate_plug_and_unplug() {
        let mut memory = request_memory();
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space()
            .with_usable_region_size(0x40_0000)
            .with_requested_size(0x40_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();

        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4000_0000, 1);
        write_available_heads(&mut memory, &[0]);
        queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("initial plug should dispatch");
        assert_eq!(config_space.plugged_size(), 0x20_0000);

        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4000_0000, 1);
        write_available_heads(&mut memory, &[0, 0]);
        let dispatch = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("duplicate plug should dispatch with error response");

        assert_eq!(dispatch.policy_errors(), 1);
        assert_eq!(
            read_response(&memory),
            VirtioMemResponse::Error.to_le_bytes()
        );
        assert_eq!(config_space.plugged_size(), 0x20_0000);

        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_UNPLUG, 0x4020_0000, 1);
        write_available_heads(&mut memory, &[0, 0, 0]);
        let dispatch = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("duplicate unplug should dispatch with error response");

        assert_eq!(dispatch.policy_errors(), 1);
        assert_eq!(
            read_response(&memory),
            VirtioMemResponse::Error.to_le_bytes()
        );
        assert_eq!(config_space.plugged_size(), 0x20_0000);
    }

    #[test]
    fn virtio_mem_queue_dispatch_completes_unknown_request_with_error() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, 99, 0, 0);
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space();
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();

        let dispatch = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("unsupported request should dispatch with error response");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.unsupported_requests(), 1);
        assert_eq!(dispatch.policy_errors(), 0);
        assert_eq!(
            read_response(&memory),
            VirtioMemResponse::Error.to_le_bytes()
        );
    }

    #[test]
    fn virtio_mem_queue_dispatch_publishes_zero_length_for_parse_errors() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_STATE, 0x4000_0000, 1);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VIRTIO_MEM_REQUEST_ADDR,
                VIRTIO_MEM_REQUEST_SIZE_U32,
                Some(1),
            ),
        );
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space();
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();

        let dispatch = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("parse failure should still publish used element");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.parse_failures(), 1);
        assert!(matches!(
            dispatch.first_parse_failure(),
            Some(VirtioMemRequestError::RequestDescriptorWriteOnly { index: 0 })
        ));
        assert_eq!(read_used_element(&memory, 0), (0, 0));
        assert!(dispatch.needs_queue_interrupt());
    }

    #[test]
    fn virtio_mem_queue_dispatch_records_response_write_failures() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_STATE, 0x4000_0000, 1);
        write_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(
                GuestAddress::new(TEST_MEMORY_SIZE),
                VIRTIO_MEM_RESPONSE_SIZE_U32,
                None,
            ),
        );
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space().with_usable_region_size(0x20_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();

        let dispatch = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("response write failure should still publish used element");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.response_write_failures(), 1);
        assert_eq!(read_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_mem_queue_dispatch_does_not_plug_after_response_write_failure() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4000_0000, 1);
        write_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(
                GuestAddress::new(TEST_MEMORY_SIZE),
                VIRTIO_MEM_RESPONSE_SIZE_U32,
                None,
            ),
        );
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space()
            .with_usable_region_size(0x20_0000)
            .with_requested_size(0x20_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();

        let dispatch = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("response write failure should still publish used element");

        assert_eq!(dispatch.response_write_failures(), 1);
        assert_eq!(config_space.plugged_size(), 0);
        assert_eq!(
            plugged_blocks.range_state(
                requested_block_range(
                    VirtioMemRequestedRange::new(GuestAddress::new(0x4000_0000), 1),
                    config_space,
                )
                .expect("test range should validate"),
            ),
            VirtioMemBlockState::Unplugged,
        );
    }

    #[test]
    fn virtio_mem_queue_dispatch_does_not_plug_after_used_ring_failure() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4000_0000, 1);
        write_available_heads(&mut memory, &[0]);
        let available = VirtqueueAvailableRing::new(
            TEST_QUEUE_DESCRIPTOR_TABLE,
            TEST_QUEUE_DRIVER_RING,
            TEST_QUEUE_SIZE,
        )
        .expect("available ring should build");
        let used = VirtqueueUsedRing::new(GuestAddress::new(TEST_MEMORY_SIZE), TEST_QUEUE_SIZE)
            .expect("used ring should build");
        let mut queue = VirtioMemQueue::new(available, used);
        let mut config_space = virtio_mem_config_space()
            .with_usable_region_size(0x20_0000)
            .with_requested_size(0x20_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();

        let error = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect_err("used ring publication should fail");

        assert!(matches!(
            error,
            VirtioMemQueueDispatchError::UsedRing { .. }
        ));
        assert_eq!(error.completed_dispatch().processed_requests(), 0);
        assert_eq!(config_space.plugged_size(), 0);
        assert_eq!(
            plugged_blocks.range_state(
                requested_block_range(
                    VirtioMemRequestedRange::new(GuestAddress::new(0x4000_0000), 1),
                    config_space,
                )
                .expect("test range should validate"),
            ),
            VirtioMemBlockState::Unplugged,
        );
    }

    #[test]
    fn virtio_mem_queue_dispatch_expands_multi_block_plug_and_partial_unplug() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4000_0000, 2);
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space()
            .with_usable_region_size(0x40_0000)
            .with_requested_size(0x40_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();
        let mut executor = RecordingMutationExecutor::default();

        queue
            .dispatch_with_executor(
                &mut memory,
                &mut config_space,
                &mut plugged_blocks,
                &mut executor,
            )
            .expect("plug should dispatch through executor");

        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_UNPLUG, 0x4020_0000, 1);
        write_available_heads(&mut memory, &[0, 0]);
        let dispatch = queue
            .dispatch_with_executor(
                &mut memory,
                &mut config_space,
                &mut plugged_blocks,
                &mut executor,
            )
            .expect("partial unplug should dispatch through executor");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.mutation_failures(), 0);
        assert_eq!(
            executor.apply_calls,
            [
                plug_blocks_mutation(0x4000_0000, 0x20_0000, 2),
                VirtioMemMutation::new(VirtioMemMutationKind::Unplug(vec![guest_range(
                    0x4020_0000,
                    0x20_0000,
                )])),
            ]
        );
        assert!(executor.rolled_back.is_empty());
        assert_eq!(config_space.plugged_size(), 0x20_0000);
        assert_eq!(read_response(&memory), VirtioMemResponse::Ack.to_le_bytes());
    }

    #[test]
    fn virtio_mem_queue_dispatch_unplug_all_executor_uses_plugged_range_snapshot() {
        let mut memory = request_memory();
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space()
            .with_usable_region_size(0x80_0000)
            .with_requested_size(0x80_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();

        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4000_0000, 1);
        write_available_heads(&mut memory, &[0]);
        queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("first plug should dispatch");
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4020_0000, 1);
        write_available_heads(&mut memory, &[0, 0]);
        queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect("second plug should dispatch");
        assert_eq!(plugged_blocks.ranges.len(), 1);
        assert_eq!(plugged_blocks.ranges[0].block_count(), 2);
        let mut executor = RecordingMutationExecutor::default();

        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_UNPLUG_ALL, 0, 0);
        write_available_heads(&mut memory, &[0, 0, 0]);
        let dispatch = queue
            .dispatch_with_executor(
                &mut memory,
                &mut config_space,
                &mut plugged_blocks,
                &mut executor,
            )
            .expect("unplug-all should dispatch through executor");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(
            executor.apply_calls,
            [VirtioMemMutation::new(VirtioMemMutationKind::UnplugAll(
                vec![
                    guest_range(0x4000_0000, 0x20_0000),
                    guest_range(0x4020_0000, 0x20_0000),
                ]
            ))]
        );
        assert_eq!(config_space.plugged_size(), 0);
        assert_eq!(config_space.usable_region_size(), 0);
    }

    #[test]
    fn virtio_mem_queue_dispatch_expands_request_across_conceptual_slot_boundary() {
        let mut memory = request_memory();
        let request_start = 0x47e0_0000;
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, request_start, 2);
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space()
            .with_usable_region_size(0x820_0000)
            .with_requested_size(0x820_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();
        let mut executor = RecordingMutationExecutor::default();

        queue
            .dispatch_with_executor(
                &mut memory,
                &mut config_space,
                &mut plugged_blocks,
                &mut executor,
            )
            .expect("boundary-crossing plug should dispatch");

        assert_eq!(
            executor.apply_calls,
            [plug_blocks_mutation(request_start, 0x20_0000, 2)]
        );
        assert_eq!(
            executor.apply_calls[0].kind(),
            &VirtioMemMutationKind::Plug(vec![
                guest_range(0x47e0_0000, 0x20_0000),
                guest_range(0x4800_0000, 0x20_0000),
            ])
        );
        assert_eq!(config_space.plugged_size(), 0x40_0000);
    }

    #[test]
    fn virtio_mem_mutation_expansion_accepts_maximum_request_block_count() {
        let block_size = 0x20_0000;
        let block_count = u64::from(u16::MAX);
        let region_size = block_count * block_size;
        let config_space = VirtioMemConfigSpace::new(block_size, 0x4000_0000, region_size)
            .with_usable_region_size(region_size)
            .with_requested_size(region_size);
        let mutation = VirtioMemPendingMutation::Plug(
            VirtioMemBlockRange::new(0, block_count).expect("maximum request should be nonempty"),
        )
        .to_executable_mutation(config_space, &VirtioMemPluggedBlocks::default())
        .expect("maximum request mutation metadata should be bounded");

        let VirtioMemMutationKind::Plug(ranges) = mutation.kind() else {
            panic!("expected plug mutation")
        };
        assert_eq!(ranges.len(), usize::from(u16::MAX));
        assert_eq!(ranges[0], guest_range(0x4000_0000, block_size));
        assert_eq!(
            ranges.last(),
            Some(&guest_range(
                0x4000_0000 + (block_count - 1) * block_size,
                block_size,
            ))
        );
    }

    #[test]
    fn virtio_mem_mutation_expansion_reports_metadata_capacity_failure() {
        let error = block_ranges_to_guest_ranges(
            &[VirtioMemBlockRange {
                start: 0,
                end: u64::MAX,
            }],
            VirtioMemConfigSpace::new(1, 0, u64::MAX),
            "plug",
        )
        .expect_err("unrepresentable mutation metadata should fail before range expansion");

        assert!(
            error
                .to_string()
                .contains("failed to allocate plug mutation metadata"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn virtio_mem_queue_dispatch_reports_mutation_executor_failure_without_commit() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4000_0000, 1);
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space()
            .with_usable_region_size(0x20_0000)
            .with_requested_size(0x20_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();
        let mut executor = RecordingMutationExecutor::default()
            .with_apply_error(VirtioMemMutationError::new("map failed"));

        let dispatch = queue
            .dispatch_with_executor(
                &mut memory,
                &mut config_space,
                &mut plugged_blocks,
                &mut executor,
            )
            .expect("executor failure should still publish error response");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.mutation_failures(), 1);
        assert_eq!(
            dispatch.first_mutation_failure().map(ToString::to_string),
            Some("virtio-mem mutation failed: map failed".to_string())
        );
        assert_eq!(
            read_response(&memory),
            VirtioMemResponse::Error.to_le_bytes()
        );
        assert_eq!(
            read_used_element(&memory, 0),
            (0, VIRTIO_MEM_RESPONSE_SIZE_U32)
        );
        assert_eq!(config_space.plugged_size(), 0);
        assert_eq!(
            plugged_blocks.range_state(
                requested_block_range(
                    VirtioMemRequestedRange::new(GuestAddress::new(0x4000_0000), 1),
                    config_space,
                )
                .expect("test range should validate"),
            ),
            VirtioMemBlockState::Unplugged,
        );
        assert_eq!(
            executor.apply_calls,
            [plug_mutation(0x4000_0000, 0x20_0000)]
        );
        assert!(executor.rolled_back.is_empty());
    }

    #[test]
    fn virtio_mem_queue_dispatch_reports_mutation_range_failure_without_executor_apply() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 1, 1);
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = VirtioMemConfigSpace::new(u64::MAX, 1, u64::MAX)
            .with_usable_region_size(u64::MAX)
            .with_requested_size(u64::MAX);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();
        let mut executor = RecordingMutationExecutor::default();

        let dispatch = queue
            .dispatch_with_executor(
                &mut memory,
                &mut config_space,
                &mut plugged_blocks,
                &mut executor,
            )
            .expect("range conversion failure should still publish error response");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.mutation_failures(), 1);
        let mutation_failure = dispatch
            .first_mutation_failure()
            .expect("mutation failure should be recorded")
            .to_string();
        assert!(mutation_failure.contains("invalid virtio-mem mutation range"));
        assert!(mutation_failure.contains("start=0x1"));
        assert!(mutation_failure.contains("size=18446744073709551615"));
        assert_eq!(
            read_response(&memory),
            VirtioMemResponse::Error.to_le_bytes()
        );
        assert!(executor.apply_calls.is_empty());
        assert!(executor.rolled_back.is_empty());
        assert_eq!(config_space.plugged_size(), 0);
        assert_eq!(
            plugged_blocks.range_state(
                requested_block_range(
                    VirtioMemRequestedRange::new(GuestAddress::new(1), 1),
                    config_space,
                )
                .expect("test range should validate before executable conversion"),
            ),
            VirtioMemBlockState::Unplugged,
        );
    }

    #[test]
    fn virtio_mem_queue_dispatch_rolls_back_applied_mutation_after_response_write_failure() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4000_0000, 2);
        write_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(
                GuestAddress::new(TEST_MEMORY_SIZE),
                VIRTIO_MEM_RESPONSE_SIZE_U32,
                None,
            ),
        );
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space()
            .with_usable_region_size(0x40_0000)
            .with_requested_size(0x40_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();
        let mut executor = RecordingMutationExecutor::default();

        let dispatch = queue
            .dispatch_with_executor(
                &mut memory,
                &mut config_space,
                &mut plugged_blocks,
                &mut executor,
            )
            .expect("response write failure should roll back and publish zero-length used entry");

        assert_eq!(dispatch.response_write_failures(), 1);
        assert_eq!(read_used_element(&memory, 0), (0, 0));
        assert_eq!(config_space.plugged_size(), 0);
        assert_eq!(
            executor.apply_calls,
            [plug_blocks_mutation(0x4000_0000, 0x20_0000, 2)]
        );
        assert_eq!(
            executor.rolled_back_mutations(),
            [plug_blocks_mutation(0x4000_0000, 0x20_0000, 2)]
        );
    }

    #[test]
    fn virtio_mem_queue_dispatch_rolls_back_applied_mutation_after_used_ring_failure() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4000_0000, 2);
        write_available_heads(&mut memory, &[0]);
        let available = VirtqueueAvailableRing::new(
            TEST_QUEUE_DESCRIPTOR_TABLE,
            TEST_QUEUE_DRIVER_RING,
            TEST_QUEUE_SIZE,
        )
        .expect("available ring should build");
        let used = VirtqueueUsedRing::new(GuestAddress::new(TEST_MEMORY_SIZE), TEST_QUEUE_SIZE)
            .expect("used ring should build");
        let mut queue = VirtioMemQueue::new(available, used);
        let mut config_space = virtio_mem_config_space()
            .with_usable_region_size(0x40_0000)
            .with_requested_size(0x40_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();
        let mut executor = RecordingMutationExecutor::default();

        let error = queue
            .dispatch_with_executor(
                &mut memory,
                &mut config_space,
                &mut plugged_blocks,
                &mut executor,
            )
            .expect_err("used ring publication should fail after rollback");

        match error {
            VirtioMemQueueDispatchError::UsedRing { rollback_error, .. } => {
                assert_eq!(rollback_error, None);
            }
            other => panic!("expected used-ring error, got {other:?}"),
        }
        assert_eq!(config_space.plugged_size(), 0);
        assert_eq!(
            executor.apply_calls,
            [plug_blocks_mutation(0x4000_0000, 0x20_0000, 2)]
        );
        assert_eq!(
            executor.rolled_back_mutations(),
            [plug_blocks_mutation(0x4000_0000, 0x20_0000, 2)]
        );
    }

    #[test]
    fn virtio_mem_queue_dispatch_surfaces_rollback_failure_after_response_write_failure() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4000_0000, 1);
        write_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(
                GuestAddress::new(TEST_MEMORY_SIZE),
                VIRTIO_MEM_RESPONSE_SIZE_U32,
                None,
            ),
        );
        write_available_heads(&mut memory, &[0]);
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space()
            .with_usable_region_size(0x20_0000)
            .with_requested_size(0x20_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();
        let mut executor = RecordingMutationExecutor::default()
            .with_rollback_error(VirtioMemMutationRollbackError::new("rollback failed"));

        let error = queue
            .dispatch_with_executor(
                &mut memory,
                &mut config_space,
                &mut plugged_blocks,
                &mut executor,
            )
            .expect_err("rollback failure should surface as dispatch error");

        assert!(matches!(
            error,
            VirtioMemQueueDispatchError::MutationRollback { .. }
        ));
        assert_eq!(error.completed_dispatch().processed_requests(), 0);
        assert!(error.to_string().contains("rollback failed"));
        assert_eq!(config_space.plugged_size(), 0);
        assert_eq!(
            executor.apply_calls,
            [plug_mutation(0x4000_0000, 0x20_0000)]
        );
        assert_eq!(
            executor.rolled_back_mutations(),
            [plug_mutation(0x4000_0000, 0x20_0000)]
        );
    }

    #[test]
    fn virtio_mem_queue_dispatch_surfaces_rollback_failure_after_used_ring_failure() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4000_0000, 1);
        write_available_heads(&mut memory, &[0]);
        let available = VirtqueueAvailableRing::new(
            TEST_QUEUE_DESCRIPTOR_TABLE,
            TEST_QUEUE_DRIVER_RING,
            TEST_QUEUE_SIZE,
        )
        .expect("available ring should build");
        let used = VirtqueueUsedRing::new(GuestAddress::new(TEST_MEMORY_SIZE), TEST_QUEUE_SIZE)
            .expect("used ring should build");
        let mut queue = VirtioMemQueue::new(available, used);
        let mut config_space = virtio_mem_config_space()
            .with_usable_region_size(0x20_0000)
            .with_requested_size(0x20_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();
        let mut executor = RecordingMutationExecutor::default()
            .with_rollback_error(VirtioMemMutationRollbackError::new("rollback failed"));

        let error = queue
            .dispatch_with_executor(
                &mut memory,
                &mut config_space,
                &mut plugged_blocks,
                &mut executor,
            )
            .expect_err("used-ring rollback failure should surface in used-ring error");

        match error {
            VirtioMemQueueDispatchError::UsedRing { rollback_error, .. } => {
                assert_eq!(
                    rollback_error.map(|source| source.to_string()),
                    Some("virtio-mem mutation rollback failed: rollback failed".to_string())
                );
            }
            other => panic!("expected used-ring error, got {other:?}"),
        }
        assert_eq!(config_space.plugged_size(), 0);
        assert_eq!(
            executor.apply_calls,
            [plug_mutation(0x4000_0000, 0x20_0000)]
        );
        assert_eq!(
            executor.rolled_back_mutations(),
            [plug_mutation(0x4000_0000, 0x20_0000)]
        );
    }

    #[test]
    fn virtio_mem_queue_dispatch_preserves_completed_dispatch_on_available_ring_error() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_STATE, 0x4000_0000, 1);
        write_available_heads(&mut memory, &[0, TEST_QUEUE_SIZE]);
        let mut queue = virtio_mem_queue();
        let mut config_space = virtio_mem_config_space().with_usable_region_size(0x20_0000);
        let mut plugged_blocks = VirtioMemPluggedBlocks::default();

        let error = queue
            .dispatch(&mut memory, &mut config_space, &mut plugged_blocks)
            .expect_err("invalid second available head should fail after first completion");

        assert!(matches!(
            error,
            VirtioMemQueueDispatchError::AvailableRing { .. }
        ));
        assert_eq!(error.completed_dispatch().processed_requests(), 1);
        assert_eq!(error.completed_dispatch().state_requests(), 1);
        assert!(error.completed_dispatch().needs_queue_interrupt());
        assert_eq!(read_used_index(&memory), 1);
    }

    #[test]
    fn virtio_mem_handler_dispatches_pending_queue_notifications() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_STATE, 0x4000_0000, 1);
        write_available_heads(&mut memory, &[0]);
        let mut handler =
            mem_mmio_handler(virtio_mem_config_space().with_usable_region_size(0x20_0000));

        configure_mem_mmio_handler_queue(&mut handler);
        handler
            .write_register(VirtioMmioRegister::Status, TEST_DRIVER_OK_STATUS)
            .expect("DRIVER_OK status should activate virtio-mem");
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 0)
            .expect("queue notification should write");

        let dispatch = handler
            .dispatch_mem_queue_notifications(&mut memory)
            .expect("pending queue notification should dispatch");

        assert_eq!(dispatch.drained_notifications(), &[0]);
        assert_eq!(
            dispatch
                .queue_dispatch()
                .expect("queue dispatch should be present")
                .state_requests(),
            1
        );
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status should read"),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn virtio_mem_handler_dispatch_updates_plugged_size_config() {
        let mut memory = request_memory();
        write_virtio_mem_chain(&mut memory, VIRTIO_MEM_REQ_PLUG, 0x4000_0000, 1);
        write_available_heads(&mut memory, &[0]);
        let mut handler = mem_mmio_handler(
            virtio_mem_config_space()
                .with_usable_region_size(0x20_0000)
                .with_requested_size(0x20_0000),
        );

        configure_mem_mmio_handler_queue(&mut handler);
        handler
            .write_register(VirtioMmioRegister::Status, TEST_DRIVER_OK_STATUS)
            .expect("DRIVER_OK status should activate virtio-mem");
        let initial_generation = handler
            .read_register(VirtioMmioRegister::ConfigGeneration)
            .expect("config generation should read");
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 0)
            .expect("queue notification should write");

        let dispatch = handler
            .dispatch_mem_queue_notifications(&mut memory)
            .expect("pending queue notification should dispatch");

        assert_eq!(
            dispatch
                .queue_dispatch()
                .expect("queue dispatch should be present")
                .policy_errors(),
            0
        );
        assert_eq!(handler.device_config_handler().plugged_size(), 0x20_0000);
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::ConfigGeneration)
                .expect("config generation should read"),
            initial_generation.wrapping_add(1)
        );
        assert_eq!(read_response(&memory), VirtioMemResponse::Ack.to_le_bytes());
    }

    #[test]
    fn virtio_mem_handler_rejects_inactive_queue_notifications() {
        let mut memory = request_memory();
        let mut handler = mem_mmio_handler(virtio_mem_config_space());

        advance_mem_mmio_handler_to_features_ok(&mut handler);
        handler
            .write_register(VirtioMmioRegister::Status, TEST_DRIVER_OK_STATUS)
            .expect_err("DRIVER_OK before queue ready should fail activation");
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 0)
            .expect("queue notification should record after failed DRIVER_OK");

        let error = handler
            .dispatch_mem_queue_notifications(&mut memory)
            .expect_err("inactive virtio-mem queue should reject notification dispatch");

        assert!(matches!(
            error,
            VirtioMemDeviceNotificationError::Inactive { .. }
        ));
        assert_eq!(error.drained_notifications(), [0]);
        assert!(error.completed_dispatch().is_none());
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status should read"),
            0
        );
        assert!(handler.pending_queue_notifications().is_empty());
        assert!(!handler.activation_handler().is_activated());
    }

    #[test]
    fn virtio_mem_device_notification_dispatch_rejects_unsupported_queue_without_dispatch() {
        let mut memory = request_memory();
        let mut device = VirtioMemDevice::new();
        let mut config_space = virtio_mem_config_space();

        let error = device
            .dispatch_drained_queue_notifications(&mut memory, &mut config_space, vec![0, 1])
            .expect_err("unsupported queue should prevent notification dispatch");

        match error {
            VirtioMemDeviceNotificationError::UnsupportedQueue {
                drained_notifications,
                queue_index,
            } => {
                assert_eq!(drained_notifications, vec![0, 1]);
                assert_eq!(queue_index, 1);
            }
            other => panic!("expected unsupported queue error, got {other:?}"),
        }
    }

    #[test]
    fn virtio_mem_mmio_handler_rejects_config_writes_before_driver_ok_without_mutating() {
        let config = virtio_mem_config_space();
        let mut handler = mem_mmio_handler(config);

        assert_eq!(
            write_mem_handler_config(&mut handler, 0, &[1, 2, 3, 4]),
            Err(VirtioMmioRegisterHandlerError::DeviceConfigWriteNotWritable { status: 0 })
        );
        assert_eq!(*handler.device_config_handler(), config);
    }

    #[test]
    fn virtio_mem_mmio_handler_rejects_config_writes_after_driver_ok_without_mutating() {
        let config = virtio_mem_config_space();
        let mut handler = mem_mmio_handler(config);

        configure_mem_mmio_handler_queue(&mut handler);
        handler
            .write_register(VirtioMmioRegister::Status, TEST_DRIVER_OK_STATUS)
            .expect("DRIVER_OK status should activate virtio-mem");

        assert!(handler.is_device_activated());
        assert!(handler.activation_handler().is_activated());
        assert_eq!(
            write_mem_handler_config(&mut handler, 0, &[1, 2, 3, 4]),
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigWrite { offset: 0, len: 4 })
        );
        assert_eq!(*handler.device_config_handler(), config);
    }

    #[test]
    fn virtio_mem_mmio_handler_activation_records_queue_metadata_and_resets() {
        let mut handler = mem_mmio_handler(virtio_mem_config_space());

        configure_mem_mmio_handler_queue(&mut handler);
        handler
            .write_register(VirtioMmioRegister::Status, TEST_DRIVER_OK_STATUS)
            .expect("DRIVER_OK status should activate virtio-mem");

        let queue = handler
            .activation_handler()
            .active_queue()
            .expect("active queue should be recorded");
        assert_eq!(
            queue.available_ring().descriptor_table(),
            TEST_QUEUE_DESCRIPTOR_TABLE
        );
        assert_eq!(
            queue.available_ring().available_ring(),
            TEST_QUEUE_DRIVER_RING
        );
        assert_eq!(queue.available_ring().queue_size(), VIRTIO_MEM_QUEUE_SIZE);
        assert_eq!(queue.used_ring().used_ring(), TEST_QUEUE_DEVICE_RING);
        assert_eq!(queue.used_ring().queue_size(), VIRTIO_MEM_QUEUE_SIZE);

        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_INIT)
            .expect("INIT status should reset virtio-mem activation");

        assert!(!handler.is_device_activated());
        assert!(!handler.activation_handler().is_activated());
        assert!(handler.activation_handler().active_queue().is_none());
    }

    #[test]
    fn virtio_mem_mmio_handler_rejects_driver_ok_before_queue_ready() {
        let mut handler = mem_mmio_handler(virtio_mem_config_space());

        advance_mem_mmio_handler_to_features_ok(&mut handler);
        let error = handler
            .write_register(VirtioMmioRegister::Status, TEST_DRIVER_OK_STATUS)
            .expect_err("DRIVER_OK before queue ready should fail activation");

        assert_eq!(
            error.to_string(),
            "virtio-mmio device activation failed while status is 0x4f: virtio-mmio device activation handler failed: virtio-mem queue 0 is not ready"
        );
        assert!(!handler.is_device_activated());
        assert!(!handler.activation_handler().is_activated());
    }

    #[test]
    fn virtio_mem_device_activation_rejects_duplicate_activation() {
        let queues = configured_mem_queue(&[VIRTIO_MEM_QUEUE_SIZE], VIRTIO_MEM_QUEUE_SIZE, true);
        let device_registers = mem_device_registers();
        let mut device = VirtioMemDevice::new();

        device
            .activate_mem(mem_device_activation(&device_registers, &queues))
            .expect("first virtio-mem activation should succeed");
        let error = device
            .activate_mem(mem_device_activation(&device_registers, &queues))
            .expect_err("second virtio-mem activation should fail");

        assert_eq!(error, VirtioMemDeviceActivationError::AlreadyActive);
        assert_eq!(error.to_string(), "virtio-mem device is already active");
    }

    #[test]
    fn virtio_mem_device_activation_rejects_unexpected_queue_count() {
        let queues = VirtioMmioQueueRegisters::new(&[VIRTIO_MEM_QUEUE_SIZE, VIRTIO_MEM_QUEUE_SIZE])
            .expect("queue table should build");
        let device_registers = mem_device_registers();
        let mut device = VirtioMemDevice::new();

        let error = device
            .activate_mem(mem_device_activation(&device_registers, &queues))
            .expect_err("extra queue should fail virtio-mem activation");

        assert_eq!(
            error,
            VirtioMemDeviceActivationError::QueueCountMismatch {
                expected: VIRTIO_MEM_QUEUE_COUNT,
                actual: 2,
            }
        );
    }

    #[test]
    fn virtio_mem_device_activation_rejects_wrong_queue_shape() {
        let queues = configured_mem_queue(&[8], 8, true);
        let device_registers = mem_device_registers();
        let mut device = VirtioMemDevice::new();

        let error = device
            .activate_mem(mem_device_activation(&device_registers, &queues))
            .expect_err("wrong queue max size should fail virtio-mem activation");

        assert_eq!(
            error,
            VirtioMemDeviceActivationError::QueueMaxSizeMismatch {
                queue_index: 0,
                expected: VIRTIO_MEM_QUEUE_SIZE,
                actual: 8,
            }
        );
    }

    #[test]
    fn virtio_mem_device_activation_rejects_zero_size_queue() {
        let mut queues = VirtioMmioQueueRegisters::new(&[VIRTIO_MEM_QUEUE_SIZE])
            .expect("queue table should build");
        queues
            .write_register(VirtioMmioRegister::QueueReady, 1, TEST_QUEUE_CONFIG_STATUS)
            .expect("queue ready should write");
        let device_registers = mem_device_registers();
        let mut device = VirtioMemDevice::new();

        let error = device
            .activate_mem(mem_device_activation(&device_registers, &queues))
            .expect_err("zero queue size should fail virtio-mem activation");

        assert_eq!(
            error,
            VirtioMemDeviceActivationError::QueueSizeZero { queue_index: 0 }
        );
    }

    #[test]
    fn virtio_mem_device_activation_rejects_unready_queue() {
        let queues = configured_mem_queue(&[VIRTIO_MEM_QUEUE_SIZE], VIRTIO_MEM_QUEUE_SIZE, false);
        let device_registers = mem_device_registers();
        let mut device = VirtioMemDevice::new();

        let error = device
            .activate_mem(mem_device_activation(&device_registers, &queues))
            .expect_err("unready queue should fail virtio-mem activation");

        assert_eq!(
            error,
            VirtioMemDeviceActivationError::QueueNotReady { queue_index: 0 }
        );
    }

    fn device_config_read_access(offset: u64, len: u64) -> VirtioMmioDeviceConfigAccess {
        let operation =
            MmioOperation::read(mmio_access(offset, len)).expect("read operation should build");
        decode_device_config_access(operation)
    }

    fn device_config_write_access(
        offset: u64,
        data: MmioAccessBytes,
    ) -> VirtioMmioDeviceConfigAccess {
        let len = u64::try_from(data.len()).expect("test write length should fit u64");
        let operation = MmioOperation::write(mmio_access(offset, len), data)
            .expect("write operation should build");
        decode_device_config_access(operation)
    }

    fn mmio_access(offset: u64, len: u64) -> MmioAccess {
        let mut bus = MmioBus::new();
        bus.insert(
            TEST_VIRTIO_MEM_MMIO_REGION_ID,
            GuestAddress::new(TEST_VIRTIO_MEM_MMIO_BASE),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("test MMIO region should insert");
        let start = TEST_VIRTIO_MEM_MMIO_BASE
            .checked_add(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + offset)
            .expect("test MMIO address should not overflow");
        bus.lookup(GuestAddress::new(start), len)
            .expect("test MMIO access should look up")
    }

    fn decode_device_config_access(operation: MmioOperation) -> VirtioMmioDeviceConfigAccess {
        match decode_virtio_mmio_access(&operation).expect("access should decode") {
            VirtioMmioAccess::DeviceConfig(access) => access,
            _ => panic!("test access should target device config"),
        }
    }

    fn read_mem_config(
        config: &VirtioMemConfigSpace,
        offset: u64,
        len: u64,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
        config.read_device_config(device_config_read_access(offset, len))
    }

    fn write_mem_config(
        config: &mut VirtioMemConfigSpace,
        offset: u64,
        data: &[u8],
    ) -> Result<(), VirtioMmioDeviceConfigError> {
        let data = MmioAccessBytes::new(data).expect("write bytes should be valid");
        config.write_device_config(device_config_write_access(offset, data), data)
    }

    fn memory_hotplug_config() -> MemoryHotplugConfig {
        MemoryHotplugConfig::try_from(MemoryHotplugConfigInput::new(1024, 2, 128))
            .expect("valid memory hotplug config should convert")
    }

    fn virtio_mem_config_space() -> VirtioMemConfigSpace {
        VirtioMemConfigSpace::new(0x20_0000, 0x4000_0000, 0x8000_0000)
            .with_usable_region_size(0x8000_0000)
    }

    fn zero_usable_virtio_mem_config_space() -> VirtioMemConfigSpace {
        VirtioMemConfigSpace::new(0x20_0000, 0x4000_0000, 0x8000_0000)
    }

    fn mem_mmio_handler(config_space: VirtioMemConfigSpace) -> VirtioMemMmioHandler {
        virtio_mem_mmio_handler_from_config_space(config_space)
            .expect("virtio-mem MMIO handler should build")
    }

    fn read_mem_handler_config(
        handler: &VirtioMemMmioHandler,
        offset: u64,
        len: u64,
    ) -> Result<MmioAccessBytes, VirtioMmioRegisterHandlerError> {
        handler.read_access(mmio_access(offset, len))
    }

    fn write_mem_handler_config(
        handler: &mut VirtioMemMmioHandler,
        offset: u64,
        data: &[u8],
    ) -> Result<(), VirtioMmioRegisterHandlerError> {
        let data = MmioAccessBytes::new(data).expect("write bytes should be valid");
        let len = u64::try_from(data.len()).expect("test write length should fit u64");
        handler.write_access(mmio_access(offset, len), data)
    }

    fn configure_mem_mmio_handler_queue(handler: &mut VirtioMemMmioHandler) {
        advance_mem_mmio_handler_to_features_ok(handler);
        handler
            .write_register(
                VirtioMmioRegister::QueueNum,
                u32::from(VIRTIO_MEM_QUEUE_SIZE),
            )
            .expect("queue size should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueDescLow,
                guest_address_low(TEST_QUEUE_DESCRIPTOR_TABLE),
            )
            .expect("queue descriptor table should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueDriverLow,
                guest_address_low(TEST_QUEUE_DRIVER_RING),
            )
            .expect("queue driver ring should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueDeviceLow,
                guest_address_low(TEST_QUEUE_DEVICE_RING),
            )
            .expect("queue device ring should write");
        handler
            .write_register(VirtioMmioRegister::QueueReady, 1)
            .expect("queue ready should write");
    }

    fn advance_mem_mmio_handler_to_features_ok(handler: &mut VirtioMemMmioHandler) {
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("ACKNOWLEDGE status should write");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("DRIVER status should write");
        handler
            .write_register(VirtioMmioRegister::Status, TEST_QUEUE_CONFIG_STATUS)
            .expect("FEATURES_OK status should write");
    }

    fn configured_mem_queue(
        queue_sizes: &[u16],
        selected_queue_size: u16,
        ready: bool,
    ) -> VirtioMmioQueueRegisters {
        let mut queues =
            VirtioMmioQueueRegisters::new(queue_sizes).expect("queue table should build");
        configure_selected_mem_queue_mut(&mut queues, selected_queue_size, ready);
        queues
    }

    fn configure_selected_mem_queue_mut(
        queues: &mut VirtioMmioQueueRegisters,
        queue_size: u16,
        ready: bool,
    ) {
        queues
            .write_register(
                VirtioMmioRegister::QueueNum,
                u32::from(queue_size),
                TEST_QUEUE_CONFIG_STATUS,
            )
            .expect("queue size should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDescLow,
                guest_address_low(TEST_QUEUE_DESCRIPTOR_TABLE),
                TEST_QUEUE_CONFIG_STATUS,
            )
            .expect("queue descriptor table should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDriverLow,
                guest_address_low(TEST_QUEUE_DRIVER_RING),
                TEST_QUEUE_CONFIG_STATUS,
            )
            .expect("queue driver ring should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDeviceLow,
                guest_address_low(TEST_QUEUE_DEVICE_RING),
                TEST_QUEUE_CONFIG_STATUS,
            )
            .expect("queue device ring should write");
        if ready {
            queues
                .write_register(VirtioMmioRegister::QueueReady, 1, TEST_QUEUE_CONFIG_STATUS)
                .expect("queue ready should write");
        }
    }

    fn mem_device_registers() -> VirtioMmioDeviceRegisters {
        VirtioMmioDeviceRegisters::new(
            VIRTIO_MEM_DEVICE_ID,
            virtio_mem_config_space().available_features(),
        )
    }

    fn mem_device_activation<'a>(
        device: &'a VirtioMmioDeviceRegisters,
        queues: &'a VirtioMmioQueueRegisters,
    ) -> VirtioMmioDeviceActivation<'a> {
        VirtioMmioDeviceActivation::new(device, queues)
    }

    #[derive(Debug, Default)]
    struct RecordingMutationExecutor {
        apply_results: VecDeque<Result<(), VirtioMemMutationError>>,
        rollback_results: VecDeque<Result<(), VirtioMemMutationRollbackError>>,
        apply_calls: Vec<VirtioMemMutation>,
        rolled_back: Vec<VirtioMemAppliedMutation>,
    }

    impl RecordingMutationExecutor {
        fn with_apply_error(mut self, source: VirtioMemMutationError) -> Self {
            self.apply_results.push_back(Err(source));
            self
        }

        fn with_rollback_error(mut self, source: VirtioMemMutationRollbackError) -> Self {
            self.rollback_results.push_back(Err(source));
            self
        }

        fn rolled_back_mutations(&self) -> Vec<VirtioMemMutation> {
            self.rolled_back
                .iter()
                .map(|applied| applied.mutation().clone())
                .collect()
        }
    }

    impl VirtioMemMutationExecutor for RecordingMutationExecutor {
        fn apply(
            &mut self,
            _memory: &mut GuestMemory,
            mutation: VirtioMemMutation,
        ) -> Result<VirtioMemAppliedMutation, VirtioMemMutationError> {
            self.apply_calls.push(mutation.clone());
            match self.apply_results.pop_front() {
                Some(Err(source)) => Err(source),
                Some(Ok(())) | None => Ok(VirtioMemAppliedMutation::new(mutation)),
            }
        }

        fn rollback(
            &mut self,
            _memory: &mut GuestMemory,
            applied: VirtioMemAppliedMutation,
        ) -> Result<(), VirtioMemMutationRollbackError> {
            self.rolled_back.push(applied);
            match self.rollback_results.pop_front() {
                Some(result) => result,
                None => Ok(()),
            }
        }
    }

    fn guest_range(start: u64, size: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(GuestAddress::new(start), size).expect("test range should be valid")
    }

    fn plug_mutation(start: u64, size: u64) -> VirtioMemMutation {
        plug_blocks_mutation(start, size, 1)
    }

    fn plug_blocks_mutation(start: u64, block_size: u64, block_count: u16) -> VirtioMemMutation {
        VirtioMemMutation::new(VirtioMemMutationKind::Plug(guest_block_ranges(
            start,
            block_size,
            block_count,
        )))
    }

    fn guest_block_ranges(start: u64, block_size: u64, block_count: u16) -> Vec<GuestMemoryRange> {
        (0..u64::from(block_count))
            .map(|block| guest_range(start + block * block_size, block_size))
            .collect()
    }

    #[derive(Clone, Copy)]
    struct TestDescriptor {
        address: GuestAddress,
        len: u32,
        writable: bool,
        next: Option<u16>,
    }

    impl TestDescriptor {
        const fn readable(address: GuestAddress, len: u32, next: Option<u16>) -> Self {
            Self {
                address,
                len,
                writable: false,
                next,
            }
        }

        const fn writable(address: GuestAddress, len: u32, next: Option<u16>) -> Self {
            Self {
                address,
                len,
                writable: true,
                next,
            }
        }
    }

    fn request_memory() -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), TEST_MEMORY_SIZE)
                .expect("test range should be valid"),
        ])
        .expect("test layout should be valid");
        GuestMemory::allocate(&layout).expect("test guest memory should allocate")
    }

    fn virtio_mem_queue() -> VirtioMemQueue {
        let available = VirtqueueAvailableRing::new(
            TEST_QUEUE_DESCRIPTOR_TABLE,
            TEST_QUEUE_DRIVER_RING,
            TEST_QUEUE_SIZE,
        )
        .expect("available ring should build");
        let used = VirtqueueUsedRing::new(TEST_QUEUE_DEVICE_RING, TEST_QUEUE_SIZE)
            .expect("used ring should build");

        VirtioMemQueue::new(available, used)
    }

    fn write_virtio_mem_chain(
        memory: &mut GuestMemory,
        request_type: u16,
        address: u64,
        block_count: u16,
    ) {
        memory
            .write_slice(
                &virtio_mem_request_bytes(request_type, address, block_count),
                TEST_VIRTIO_MEM_REQUEST_ADDR,
            )
            .expect("virtio-mem request should write");
        write_descriptor(
            memory,
            0,
            TestDescriptor::readable(
                TEST_VIRTIO_MEM_REQUEST_ADDR,
                VIRTIO_MEM_REQUEST_SIZE_U32,
                Some(1),
            ),
        );
        write_descriptor(
            memory,
            1,
            TestDescriptor::writable(
                TEST_VIRTIO_MEM_RESPONSE_ADDR,
                VIRTIO_MEM_RESPONSE_SIZE_U32,
                None,
            ),
        );
    }

    fn virtio_mem_request_bytes(
        request_type: u16,
        address: u64,
        block_count: u16,
    ) -> [u8; VIRTIO_MEM_REQUEST_SIZE] {
        let mut bytes = [0; VIRTIO_MEM_REQUEST_SIZE];
        bytes[0..2].copy_from_slice(&request_type.to_le_bytes());
        bytes[8..16].copy_from_slice(&address.to_le_bytes());
        bytes[16..18].copy_from_slice(&block_count.to_le_bytes());
        bytes
    }

    fn write_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        let descriptor_address = TEST_QUEUE_DESCRIPTOR_TABLE
            .checked_add(16 * u64::from(index))
            .expect("descriptor address should not overflow");
        let flags = descriptor_flags(descriptor);
        let next = descriptor.next.unwrap_or(0);
        let mut bytes = [0; 16];
        bytes[0..8].copy_from_slice(&descriptor.address.raw_value().to_le_bytes());
        bytes[8..12].copy_from_slice(&descriptor.len.to_le_bytes());
        bytes[12..14].copy_from_slice(&flags.to_le_bytes());
        bytes[14..16].copy_from_slice(&next.to_le_bytes());
        memory
            .write_slice(&bytes, descriptor_address)
            .expect("descriptor should write");
    }

    fn descriptor_flags(descriptor: TestDescriptor) -> u16 {
        let mut flags = 0;
        if descriptor.writable {
            flags |= VIRTQUEUE_DESC_F_WRITE;
        }
        if descriptor.next.is_some() {
            flags |= VIRTQUEUE_DESC_F_NEXT;
        }
        flags
    }

    fn write_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        write_guest_u16(
            memory,
            TEST_QUEUE_DRIVER_RING
                .checked_add(2)
                .expect("available idx address should not overflow"),
            u16::try_from(heads.len()).expect("test head count should fit u16"),
        );
        for (index, head) in heads.iter().copied().enumerate() {
            let offset = 4 + (u64::try_from(index).expect("index should fit u64") * 2);
            write_guest_u16(
                memory,
                TEST_QUEUE_DRIVER_RING
                    .checked_add(offset)
                    .expect("available ring address should not overflow"),
                head,
            );
        }
    }

    fn read_mem_descriptor_chain(memory: &GuestMemory) -> VirtqueueDescriptorChain {
        read_descriptor_chain(memory, TEST_QUEUE_DESCRIPTOR_TABLE, TEST_QUEUE_SIZE, 0)
            .expect("descriptor chain should read")
    }

    fn read_response(memory: &GuestMemory) -> [u8; VIRTIO_MEM_RESPONSE_SIZE] {
        let mut bytes = [0; VIRTIO_MEM_RESPONSE_SIZE];
        memory
            .read_slice(&mut bytes, TEST_VIRTIO_MEM_RESPONSE_ADDR)
            .expect("response should read");
        bytes
    }

    fn read_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_QUEUE_DEVICE_RING
                .checked_add(2)
                .expect("used idx address should not overflow"),
        )
    }

    fn read_used_element(memory: &GuestMemory, ring_index: u16) -> (u32, u32) {
        let entry = TEST_QUEUE_DEVICE_RING
            .checked_add(4 + (u64::from(ring_index) * 8))
            .expect("used ring entry address should not overflow");
        (
            read_guest_u32(memory, entry),
            read_guest_u32(
                memory,
                entry
                    .checked_add(4)
                    .expect("used ring len address should not overflow"),
            ),
        )
    }

    fn write_guest_u16(memory: &mut GuestMemory, address: GuestAddress, value: u16) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("guest u16 should write");
    }

    fn read_guest_u16(memory: &GuestMemory, address: GuestAddress) -> u16 {
        let mut bytes = [0; 2];
        memory
            .read_slice(&mut bytes, address)
            .expect("guest u16 should read");
        u16::from_le_bytes(bytes)
    }

    fn read_guest_u32(memory: &GuestMemory, address: GuestAddress) -> u32 {
        let mut bytes = [0; 4];
        memory
            .read_slice(&mut bytes, address)
            .expect("guest u32 should read");
        u32::from_le_bytes(bytes)
    }

    fn guest_address_low(address: GuestAddress) -> u32 {
        u32::try_from(address.raw_value()).expect("test address should fit in queue low register")
    }
}
