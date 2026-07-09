//! Backend-neutral memory hotplug configuration model.

use std::fmt;

use crate::mmio::{MmioAccessBytes, MmioAccessBytesError, MmioHandlerError};
use crate::virtio_mmio::{
    VirtioMmioDeviceActivation, VirtioMmioDeviceActivationError, VirtioMmioDeviceActivationHandler,
    VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError, VirtioMmioDeviceConfigHandler,
    VirtioMmioQueueRegisterError, VirtioMmioQueueState, VirtioMmioRegisterHandler,
    VirtioMmioRegisterHandlerError,
};

pub const MEMORY_HOTPLUG_DEFAULT_BLOCK_SIZE_MIB: u64 = 2;
pub const MEMORY_HOTPLUG_DEFAULT_SLOT_SIZE_MIB: u64 = 128;
pub const VIRTIO_MEM_DEVICE_ID: u32 = 24;
pub const VIRTIO_MEM_QUEUE_COUNT: usize = 1;
pub const VIRTIO_MEM_QUEUE_SIZE: u16 = 256;
pub const VIRTIO_MEM_QUEUE_SIZES: [u16; VIRTIO_MEM_QUEUE_COUNT] = [VIRTIO_MEM_QUEUE_SIZE];
pub const VIRTIO_MEM_CONFIG_SPACE_SIZE: usize = 56;
pub const VIRTIO_MEM_F_UNPLUGGED_INACCESSIBLE: u32 = 1;
pub const VIRTIO_FEATURE_VERSION_1: u32 = 32;

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioMemDevice {
    active_queue: Option<VirtioMmioQueueState>,
}

impl VirtioMemDevice {
    pub const fn new() -> Self {
        Self { active_queue: None }
    }

    pub fn is_activated(&self) -> bool {
        self.active_queue.is_some()
    }

    pub const fn active_queue(&self) -> Option<VirtioMmioQueueState> {
        self.active_queue
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
        self.active_queue = Some(queue);

        Ok(())
    }

    pub fn reset(&mut self) {
        self.active_queue = None;
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::GuestAddress;
    use crate::mmio::{MmioAccess, MmioBus, MmioOperation, MmioRegionId};
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
        VIRTIO_DEVICE_STATUS_INIT, VIRTIO_MMIO_DEVICE_CONFIG_OFFSET,
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioAccess, VirtioMmioDeviceActivation,
        VirtioMmioDeviceRegisters, VirtioMmioQueueRegisters, VirtioMmioRegister,
        decode_virtio_mmio_access,
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
        assert_eq!(queue.max_size(), VIRTIO_MEM_QUEUE_SIZE);
        assert_eq!(queue.size(), VIRTIO_MEM_QUEUE_SIZE);
        assert!(queue.ready());
        assert_eq!(queue.descriptor_table(), TEST_QUEUE_DESCRIPTOR_TABLE);
        assert_eq!(queue.driver_ring(), TEST_QUEUE_DRIVER_RING);
        assert_eq!(queue.device_ring(), TEST_QUEUE_DEVICE_RING);

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

    fn virtio_mem_config_space() -> VirtioMemConfigSpace {
        VirtioMemConfigSpace::new(0x20_0000, 0x4000_0000, 0x8000_0000)
            .with_usable_region_size(0x8000_0000)
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

    fn guest_address_low(address: GuestAddress) -> u32 {
        u32::try_from(address.raw_value()).expect("test address should fit in queue low register")
    }
}
