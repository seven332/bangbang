//! Backend-neutral memory hotplug configuration model.

use std::fmt;

use crate::mmio::{MmioAccessBytes, MmioAccessBytesError, MmioHandlerError};
use crate::virtio_mmio::{
    VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError, VirtioMmioDeviceConfigHandler,
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
        VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioAccess,
        decode_virtio_mmio_access,
    };

    const TEST_VIRTIO_MEM_MMIO_BASE: u64 = 0x1000_0000;
    const TEST_VIRTIO_MEM_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(77);

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
}
