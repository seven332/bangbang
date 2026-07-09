//! Backend-neutral memory hotplug configuration model.

use std::fmt;

pub const MEMORY_HOTPLUG_DEFAULT_BLOCK_SIZE_MIB: u64 = 2;
pub const MEMORY_HOTPLUG_DEFAULT_SLOT_SIZE_MIB: u64 = 128;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
