//! Backend-neutral virtio-mmio register access decoding.

use std::fmt;

use crate::interrupt::{DeviceInterruptKind, DeviceInterruptStatus, DeviceInterruptStatusError};
use crate::memory::GuestAddress;
use crate::mmio::{
    MmioAccess, MmioAccessBytes, MmioAccessBytesError, MmioHandler, MmioHandlerError,
    MmioOperation, MmioOperationError, MmioOperationKind,
};
use crate::virtio_queue::{
    VIRTQUEUE_AVAILABLE_RING_ALIGNMENT, VIRTQUEUE_DESCRIPTOR_ALIGNMENT,
    VIRTQUEUE_USED_RING_ALIGNMENT,
};

pub const VIRTIO_MMIO_DEVICE_WINDOW_SIZE: u64 = 0x1000;
pub const VIRTIO_MMIO_REGISTER_SPACE_SIZE: u64 = 0x100;
pub const VIRTIO_MMIO_DEVICE_CONFIG_OFFSET: u64 = 0x100;
pub const VIRTIO_MMIO_NOTIFY_OFFSET: u64 = 0x50;
pub const VIRTIO_MMIO_MAGIC_VALUE: u32 = 0x7472_6976;
pub const VIRTIO_MMIO_VERSION: u32 = 2;
pub const VIRTIO_MMIO_VENDOR_ID: u32 = 0;
pub const VIRTIO_MMIO_REGISTER_ACCESS_SIZE: usize = 4;
pub const VIRTIO_MMIO_FEATURE_VERSION_1: u32 = 32;
pub const VIRTIO_MMIO_VERSION_1_FEATURE: u64 = 1_u64 << VIRTIO_MMIO_FEATURE_VERSION_1;
pub const VIRTIO_DEVICE_STATUS_INIT: u32 = 0x00;
pub const VIRTIO_DEVICE_STATUS_ACKNOWLEDGE: u32 = 0x01;
pub const VIRTIO_DEVICE_STATUS_DRIVER: u32 = 0x02;
pub const VIRTIO_DEVICE_STATUS_DRIVER_OK: u32 = 0x04;
pub const VIRTIO_DEVICE_STATUS_FEATURES_OK: u32 = 0x08;
pub const VIRTIO_DEVICE_STATUS_DEVICE_NEEDS_RESET: u32 = 0x40;
pub const VIRTIO_DEVICE_STATUS_FAILED: u32 = 0x80;

const VIRTIO_MMIO_REGISTER_ACCESS_SIZE_U64: u64 = 4;
const VIRTIO_MMIO_FEATURE_SELECTOR_MAX: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioMmioDeviceRegisters {
    device_id: u32,
    vendor_id: u32,
    device_features: u64,
    config_generation: u32,
    device_features_select: u32,
    driver_features_select: u32,
    driver_features: u64,
    status: u32,
}

impl VirtioMmioDeviceRegisters {
    pub const fn new(device_id: u32, device_features: u64) -> Self {
        Self::with_vendor_id_and_config_generation(
            device_id,
            VIRTIO_MMIO_VENDOR_ID,
            device_features,
            0,
        )
    }

    pub const fn with_vendor_id_and_config_generation(
        device_id: u32,
        vendor_id: u32,
        device_features: u64,
        config_generation: u32,
    ) -> Self {
        Self {
            device_id,
            vendor_id,
            device_features: device_features | VIRTIO_MMIO_VERSION_1_FEATURE,
            config_generation,
            device_features_select: 0,
            driver_features_select: 0,
            driver_features: 0,
            status: VIRTIO_DEVICE_STATUS_INIT,
        }
    }

    pub const fn device_id(self) -> u32 {
        self.device_id
    }

    pub const fn vendor_id(self) -> u32 {
        self.vendor_id
    }

    pub const fn device_features(self) -> u64 {
        self.device_features
    }

    pub const fn config_generation(self) -> u32 {
        self.config_generation
    }

    pub const fn device_features_select(self) -> u32 {
        self.device_features_select
    }

    pub const fn driver_features_select(self) -> u32 {
        self.driver_features_select
    }

    pub const fn driver_features(self) -> u64 {
        self.driver_features
    }

    pub const fn status(self) -> u32 {
        self.status
    }

    pub fn read_register(
        &self,
        register: VirtioMmioRegister,
    ) -> Result<u32, VirtioMmioRegisterStateError> {
        match register {
            VirtioMmioRegister::MagicValue => Ok(VIRTIO_MMIO_MAGIC_VALUE),
            VirtioMmioRegister::Version => Ok(VIRTIO_MMIO_VERSION),
            VirtioMmioRegister::DeviceId => Ok(self.device_id),
            VirtioMmioRegister::VendorId => Ok(self.vendor_id),
            VirtioMmioRegister::DeviceFeatures => {
                feature_word(self.device_features, self.device_features_select)
            }
            VirtioMmioRegister::Status => Ok(self.status),
            VirtioMmioRegister::ConfigGeneration => Ok(self.config_generation),
            VirtioMmioRegister::DeviceFeaturesSel
            | VirtioMmioRegister::DriverFeatures
            | VirtioMmioRegister::DriverFeaturesSel
            | VirtioMmioRegister::QueueSel
            | VirtioMmioRegister::QueueNumMax
            | VirtioMmioRegister::QueueNum
            | VirtioMmioRegister::QueueReady
            | VirtioMmioRegister::QueueNotify
            | VirtioMmioRegister::InterruptStatus
            | VirtioMmioRegister::InterruptAck
            | VirtioMmioRegister::QueueDescLow
            | VirtioMmioRegister::QueueDescHigh
            | VirtioMmioRegister::QueueDriverLow
            | VirtioMmioRegister::QueueDriverHigh
            | VirtioMmioRegister::QueueDeviceLow
            | VirtioMmioRegister::QueueDeviceHigh => {
                Err(VirtioMmioRegisterStateError::UnsupportedRegisterRead { register })
            }
        }
    }

    pub fn write_register(
        &mut self,
        register: VirtioMmioRegister,
        value: u32,
    ) -> Result<(), VirtioMmioRegisterStateError> {
        match register {
            VirtioMmioRegister::DeviceFeaturesSel => {
                validate_feature_selector(value)?;
                self.device_features_select = value;
                Ok(())
            }
            VirtioMmioRegister::DriverFeaturesSel => {
                validate_feature_selector(value)?;
                self.driver_features_select = value;
                Ok(())
            }
            VirtioMmioRegister::DriverFeatures => self.write_driver_features(value),
            VirtioMmioRegister::Status => self.set_status(value),
            VirtioMmioRegister::MagicValue
            | VirtioMmioRegister::Version
            | VirtioMmioRegister::DeviceId
            | VirtioMmioRegister::VendorId
            | VirtioMmioRegister::DeviceFeatures
            | VirtioMmioRegister::QueueSel
            | VirtioMmioRegister::QueueNumMax
            | VirtioMmioRegister::QueueNum
            | VirtioMmioRegister::QueueReady
            | VirtioMmioRegister::QueueNotify
            | VirtioMmioRegister::InterruptStatus
            | VirtioMmioRegister::InterruptAck
            | VirtioMmioRegister::QueueDescLow
            | VirtioMmioRegister::QueueDescHigh
            | VirtioMmioRegister::QueueDriverLow
            | VirtioMmioRegister::QueueDriverHigh
            | VirtioMmioRegister::QueueDeviceLow
            | VirtioMmioRegister::QueueDeviceHigh
            | VirtioMmioRegister::ConfigGeneration => {
                Err(VirtioMmioRegisterStateError::UnsupportedRegisterWrite { register })
            }
        }
    }

    pub fn set_status(&mut self, status: u32) -> Result<(), VirtioMmioRegisterStateError> {
        if (status & VIRTIO_DEVICE_STATUS_FAILED) != 0 {
            self.status |= VIRTIO_DEVICE_STATUS_FAILED;
            return Ok(());
        }

        if status == VIRTIO_DEVICE_STATUS_INIT {
            self.reset();
            return Ok(());
        }

        if is_valid_status_transition(self.status, status) {
            self.status = status;
            Ok(())
        } else {
            Err(VirtioMmioRegisterStateError::InvalidStatusTransition {
                current: self.status,
                requested: status,
            })
        }
    }

    pub fn reset(&mut self) {
        self.device_features_select = 0;
        self.driver_features_select = 0;
        self.driver_features = 0;
        self.status = VIRTIO_DEVICE_STATUS_INIT;
    }

    fn write_driver_features(&mut self, value: u32) -> Result<(), VirtioMmioRegisterStateError> {
        if !self.can_write_driver_features() {
            return Err(VirtioMmioRegisterStateError::DriverFeaturesNotWritable {
                status: self.status,
            });
        }

        let supported = feature_word(self.device_features, self.driver_features_select)?;
        let unsupported = value & !supported;
        if unsupported != 0 {
            return Err(VirtioMmioRegisterStateError::UnsupportedDriverFeatures {
                selector: self.driver_features_select,
                requested: value,
                supported,
                unsupported,
            });
        }

        self.driver_features |= selected_feature_bits(self.driver_features_select, value)?;
        Ok(())
    }

    const fn can_write_driver_features(self) -> bool {
        self.status
            & (VIRTIO_DEVICE_STATUS_DRIVER
                | VIRTIO_DEVICE_STATUS_FEATURES_OK
                | VIRTIO_DEVICE_STATUS_FAILED
                | VIRTIO_DEVICE_STATUS_DEVICE_NEEDS_RESET)
            == VIRTIO_DEVICE_STATUS_DRIVER
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioMmioRegisterStateError {
    UnsupportedRegisterRead {
        register: VirtioMmioRegister,
    },
    UnsupportedRegisterWrite {
        register: VirtioMmioRegister,
    },
    UnsupportedFeaturePage {
        selector: u32,
    },
    DriverFeaturesNotWritable {
        status: u32,
    },
    UnsupportedDriverFeatures {
        selector: u32,
        requested: u32,
        supported: u32,
        unsupported: u32,
    },
    InvalidStatusTransition {
        current: u32,
        requested: u32,
    },
}

impl fmt::Display for VirtioMmioRegisterStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedRegisterRead { register } => {
                write!(f, "unsupported virtio-mmio state read from {register}")
            }
            Self::UnsupportedRegisterWrite { register } => {
                write!(f, "unsupported virtio-mmio state write to {register}")
            }
            Self::UnsupportedFeaturePage { selector } => {
                write!(
                    f,
                    "unsupported virtio-mmio feature selector page {selector}; supported pages are 0..={VIRTIO_MMIO_FEATURE_SELECTOR_MAX}"
                )
            }
            Self::DriverFeaturesNotWritable { status } => {
                write!(
                    f,
                    "virtio-mmio driver features cannot be written while status is 0x{status:x}"
                )
            }
            Self::UnsupportedDriverFeatures {
                selector,
                requested,
                supported,
                unsupported,
            } => {
                write!(
                    f,
                    "virtio-mmio driver feature page {selector} requested 0x{requested:x}, including unsupported bits 0x{unsupported:x}; supported bits are 0x{supported:x}"
                )
            }
            Self::InvalidStatusTransition { current, requested } => {
                write!(
                    f,
                    "invalid virtio-mmio device status transition: 0x{current:x} -> 0x{requested:x}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioMmioRegisterStateError {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioMmioInterruptRegisters {
    pending_status: DeviceInterruptStatus,
}

impl VirtioMmioInterruptRegisters {
    pub const fn new() -> Self {
        Self {
            pending_status: DeviceInterruptStatus::empty(),
        }
    }

    pub const fn pending_status(self) -> DeviceInterruptStatus {
        self.pending_status
    }

    pub fn mark_pending(&mut self, kind: DeviceInterruptKind) {
        self.pending_status.insert(kind);
    }

    pub fn read_register(
        &self,
        register: VirtioMmioRegister,
    ) -> Result<u32, VirtioMmioInterruptRegisterError> {
        match register {
            VirtioMmioRegister::InterruptStatus => Ok(self.pending_status.bits()),
            VirtioMmioRegister::MagicValue
            | VirtioMmioRegister::Version
            | VirtioMmioRegister::DeviceId
            | VirtioMmioRegister::VendorId
            | VirtioMmioRegister::DeviceFeatures
            | VirtioMmioRegister::DeviceFeaturesSel
            | VirtioMmioRegister::DriverFeatures
            | VirtioMmioRegister::DriverFeaturesSel
            | VirtioMmioRegister::QueueSel
            | VirtioMmioRegister::QueueNumMax
            | VirtioMmioRegister::QueueNum
            | VirtioMmioRegister::QueueReady
            | VirtioMmioRegister::QueueNotify
            | VirtioMmioRegister::InterruptAck
            | VirtioMmioRegister::Status
            | VirtioMmioRegister::QueueDescLow
            | VirtioMmioRegister::QueueDescHigh
            | VirtioMmioRegister::QueueDriverLow
            | VirtioMmioRegister::QueueDriverHigh
            | VirtioMmioRegister::QueueDeviceLow
            | VirtioMmioRegister::QueueDeviceHigh
            | VirtioMmioRegister::ConfigGeneration => {
                Err(VirtioMmioInterruptRegisterError::UnsupportedRegisterRead { register })
            }
        }
    }

    pub fn write_register(
        &mut self,
        register: VirtioMmioRegister,
        value: u32,
        status: u32,
    ) -> Result<(), VirtioMmioInterruptRegisterError> {
        match register {
            VirtioMmioRegister::InterruptAck => self.write_interrupt_ack(value, status),
            VirtioMmioRegister::MagicValue
            | VirtioMmioRegister::Version
            | VirtioMmioRegister::DeviceId
            | VirtioMmioRegister::VendorId
            | VirtioMmioRegister::DeviceFeatures
            | VirtioMmioRegister::DeviceFeaturesSel
            | VirtioMmioRegister::DriverFeatures
            | VirtioMmioRegister::DriverFeaturesSel
            | VirtioMmioRegister::QueueSel
            | VirtioMmioRegister::QueueNumMax
            | VirtioMmioRegister::QueueNum
            | VirtioMmioRegister::QueueReady
            | VirtioMmioRegister::QueueNotify
            | VirtioMmioRegister::InterruptStatus
            | VirtioMmioRegister::Status
            | VirtioMmioRegister::QueueDescLow
            | VirtioMmioRegister::QueueDescHigh
            | VirtioMmioRegister::QueueDriverLow
            | VirtioMmioRegister::QueueDriverHigh
            | VirtioMmioRegister::QueueDeviceLow
            | VirtioMmioRegister::QueueDeviceHigh
            | VirtioMmioRegister::ConfigGeneration => {
                Err(VirtioMmioInterruptRegisterError::UnsupportedRegisterWrite { register })
            }
        }
    }

    pub fn reset(&mut self) {
        self.pending_status = DeviceInterruptStatus::empty();
    }

    fn write_interrupt_ack(
        &mut self,
        value: u32,
        status: u32,
    ) -> Result<(), VirtioMmioInterruptRegisterError> {
        validate_interrupt_ack_status(status)?;

        let ack = DeviceInterruptStatus::from_bits(value).map_err(|source| {
            VirtioMmioInterruptRegisterError::InvalidInterruptAck { value, source }
        })?;

        self.pending_status.clear(ack);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioMmioInterruptRegisterError {
    UnsupportedRegisterRead {
        register: VirtioMmioRegister,
    },
    UnsupportedRegisterWrite {
        register: VirtioMmioRegister,
    },
    InterruptAckNotWritable {
        status: u32,
    },
    InvalidInterruptAck {
        value: u32,
        source: DeviceInterruptStatusError,
    },
}

impl fmt::Display for VirtioMmioInterruptRegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedRegisterRead { register } => {
                write!(
                    f,
                    "unsupported virtio-mmio interrupt state read from {register}"
                )
            }
            Self::UnsupportedRegisterWrite { register } => {
                write!(
                    f,
                    "unsupported virtio-mmio interrupt state write to {register}"
                )
            }
            Self::InterruptAckNotWritable { status } => {
                write!(
                    f,
                    "virtio-mmio interrupt acknowledgement cannot be written while status is 0x{status:x}"
                )
            }
            Self::InvalidInterruptAck { value, source } => {
                write!(
                    f,
                    "virtio-mmio interrupt acknowledgement value 0x{value:x} is invalid: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioMmioInterruptRegisterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidInterruptAck { source, .. } => Some(source),
            Self::UnsupportedRegisterRead { .. }
            | Self::UnsupportedRegisterWrite { .. }
            | Self::InterruptAckNotWritable { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioMmioQueueRegisters {
    queue_select: u32,
    queues: Vec<VirtioMmioQueueState>,
}

impl VirtioMmioQueueRegisters {
    pub fn new(queue_max_sizes: &[u16]) -> Result<Self, VirtioMmioQueueRegisterError> {
        if queue_max_sizes.is_empty() {
            return Err(VirtioMmioQueueRegisterError::EmptyQueueTable);
        }

        let mut queues = Vec::with_capacity(queue_max_sizes.len());
        for (queue_index, max_size) in queue_max_sizes.iter().copied().enumerate() {
            validate_queue_max_size(queue_index, max_size)?;
            queues.push(VirtioMmioQueueState::new(max_size));
        }

        Ok(Self {
            queue_select: 0,
            queues,
        })
    }

    pub const fn queue_select(&self) -> u32 {
        self.queue_select
    }

    pub fn queue_count(&self) -> usize {
        self.queues.len()
    }

    pub fn selected_queue(&self) -> Result<&VirtioMmioQueueState, VirtioMmioQueueRegisterError> {
        self.queue(self.queue_select)
    }

    pub fn queue(
        &self,
        queue_index: u32,
    ) -> Result<&VirtioMmioQueueState, VirtioMmioQueueRegisterError> {
        let index = self.queue_index(queue_index)?;
        self.queues
            .get(index)
            .ok_or_else(|| self.invalid_queue_index(queue_index))
    }

    pub fn read_register(
        &self,
        register: VirtioMmioRegister,
    ) -> Result<u32, VirtioMmioQueueRegisterError> {
        match register {
            VirtioMmioRegister::QueueNumMax => Ok(u32::from(self.selected_queue()?.max_size())),
            VirtioMmioRegister::QueueReady => Ok(queue_ready_value(self.selected_queue()?.ready())),
            VirtioMmioRegister::MagicValue
            | VirtioMmioRegister::Version
            | VirtioMmioRegister::DeviceId
            | VirtioMmioRegister::VendorId
            | VirtioMmioRegister::DeviceFeatures
            | VirtioMmioRegister::DeviceFeaturesSel
            | VirtioMmioRegister::DriverFeatures
            | VirtioMmioRegister::DriverFeaturesSel
            | VirtioMmioRegister::QueueSel
            | VirtioMmioRegister::QueueNum
            | VirtioMmioRegister::QueueNotify
            | VirtioMmioRegister::InterruptStatus
            | VirtioMmioRegister::InterruptAck
            | VirtioMmioRegister::Status
            | VirtioMmioRegister::QueueDescLow
            | VirtioMmioRegister::QueueDescHigh
            | VirtioMmioRegister::QueueDriverLow
            | VirtioMmioRegister::QueueDriverHigh
            | VirtioMmioRegister::QueueDeviceLow
            | VirtioMmioRegister::QueueDeviceHigh
            | VirtioMmioRegister::ConfigGeneration => {
                Err(VirtioMmioQueueRegisterError::UnsupportedRegisterRead { register })
            }
        }
    }

    pub fn write_register(
        &mut self,
        register: VirtioMmioRegister,
        value: u32,
        status: u32,
    ) -> Result<(), VirtioMmioQueueRegisterError> {
        match register {
            VirtioMmioRegister::QueueSel => self.select_queue(value),
            VirtioMmioRegister::QueueNum => self.write_queue_size(value, status),
            VirtioMmioRegister::QueueReady => self.write_queue_ready(value, status),
            VirtioMmioRegister::QueueDescLow => {
                self.write_queue_address_low(QueueAddressKind::DescriptorTable, value, status)
            }
            VirtioMmioRegister::QueueDescHigh => {
                self.write_queue_address_high(QueueAddressKind::DescriptorTable, value, status)
            }
            VirtioMmioRegister::QueueDriverLow => {
                self.write_queue_address_low(QueueAddressKind::DriverRing, value, status)
            }
            VirtioMmioRegister::QueueDriverHigh => {
                self.write_queue_address_high(QueueAddressKind::DriverRing, value, status)
            }
            VirtioMmioRegister::QueueDeviceLow => {
                self.write_queue_address_low(QueueAddressKind::DeviceRing, value, status)
            }
            VirtioMmioRegister::QueueDeviceHigh => {
                self.write_queue_address_high(QueueAddressKind::DeviceRing, value, status)
            }
            VirtioMmioRegister::MagicValue
            | VirtioMmioRegister::Version
            | VirtioMmioRegister::DeviceId
            | VirtioMmioRegister::VendorId
            | VirtioMmioRegister::DeviceFeatures
            | VirtioMmioRegister::DeviceFeaturesSel
            | VirtioMmioRegister::DriverFeatures
            | VirtioMmioRegister::DriverFeaturesSel
            | VirtioMmioRegister::QueueNumMax
            | VirtioMmioRegister::QueueNotify
            | VirtioMmioRegister::InterruptStatus
            | VirtioMmioRegister::InterruptAck
            | VirtioMmioRegister::Status
            | VirtioMmioRegister::ConfigGeneration => {
                Err(VirtioMmioQueueRegisterError::UnsupportedRegisterWrite { register })
            }
        }
    }

    pub fn reset(&mut self) {
        self.queue_select = 0;
        for queue in &mut self.queues {
            queue.reset();
        }
    }

    fn select_queue(&mut self, queue_index: u32) -> Result<(), VirtioMmioQueueRegisterError> {
        self.queue_index(queue_index)?;
        self.queue_select = queue_index;
        Ok(())
    }

    fn write_queue_size(
        &mut self,
        value: u32,
        status: u32,
    ) -> Result<(), VirtioMmioQueueRegisterError> {
        validate_queue_config_status(status)?;

        let queue = self.selected_queue()?;
        let max_size = queue.max_size();
        let queue_index = self.queue_select;
        let queue_size = validate_queue_size(queue_index, value, max_size)?;

        self.selected_queue_mut()?.size = queue_size;
        Ok(())
    }

    fn write_queue_ready(
        &mut self,
        value: u32,
        status: u32,
    ) -> Result<(), VirtioMmioQueueRegisterError> {
        validate_queue_config_status(status)?;

        let ready = validate_queue_ready_value(self.queue_select, value)?;
        self.selected_queue_mut()?.ready = ready;
        Ok(())
    }

    fn write_queue_address_low(
        &mut self,
        kind: QueueAddressKind,
        value: u32,
        status: u32,
    ) -> Result<(), VirtioMmioQueueRegisterError> {
        validate_queue_config_status(status)?;

        let current = self.selected_queue()?.address(kind);
        let candidate = replace_address_low(current, value);
        validate_queue_address(self.queue_select, kind, candidate)?;

        self.selected_queue_mut()?.set_address(kind, candidate);
        Ok(())
    }

    fn write_queue_address_high(
        &mut self,
        kind: QueueAddressKind,
        value: u32,
        status: u32,
    ) -> Result<(), VirtioMmioQueueRegisterError> {
        validate_queue_config_status(status)?;

        let current = self.selected_queue()?.address(kind);
        let candidate = replace_address_high(current, value);
        validate_queue_address(self.queue_select, kind, candidate)?;

        self.selected_queue_mut()?.set_address(kind, candidate);
        Ok(())
    }

    fn selected_queue_mut(
        &mut self,
    ) -> Result<&mut VirtioMmioQueueState, VirtioMmioQueueRegisterError> {
        let queue_index = self.queue_select;
        let index = self.queue_index(queue_index)?;
        let queue_count = self.queue_count();
        self.queues
            .get_mut(index)
            .ok_or(VirtioMmioQueueRegisterError::InvalidQueueIndex {
                queue_index,
                queue_count,
            })
    }

    fn queue_index(&self, queue_index: u32) -> Result<usize, VirtioMmioQueueRegisterError> {
        let index =
            usize::try_from(queue_index).map_err(|_| self.invalid_queue_index(queue_index))?;
        if index < self.queue_count() {
            Ok(index)
        } else {
            Err(self.invalid_queue_index(queue_index))
        }
    }

    fn invalid_queue_index(&self, queue_index: u32) -> VirtioMmioQueueRegisterError {
        VirtioMmioQueueRegisterError::InvalidQueueIndex {
            queue_index,
            queue_count: self.queue_count(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioMmioQueueState {
    max_size: u16,
    size: u16,
    ready: bool,
    descriptor_table: GuestAddress,
    driver_ring: GuestAddress,
    device_ring: GuestAddress,
}

impl VirtioMmioQueueState {
    const fn new(max_size: u16) -> Self {
        Self {
            max_size,
            size: 0,
            ready: false,
            descriptor_table: GuestAddress::new(0),
            driver_ring: GuestAddress::new(0),
            device_ring: GuestAddress::new(0),
        }
    }

    pub const fn max_size(self) -> u16 {
        self.max_size
    }

    pub const fn size(self) -> u16 {
        self.size
    }

    pub const fn ready(self) -> bool {
        self.ready
    }

    pub const fn descriptor_table(self) -> GuestAddress {
        self.descriptor_table
    }

    pub const fn driver_ring(self) -> GuestAddress {
        self.driver_ring
    }

    pub const fn device_ring(self) -> GuestAddress {
        self.device_ring
    }

    const fn address(self, kind: QueueAddressKind) -> GuestAddress {
        match kind {
            QueueAddressKind::DescriptorTable => self.descriptor_table,
            QueueAddressKind::DriverRing => self.driver_ring,
            QueueAddressKind::DeviceRing => self.device_ring,
        }
    }

    fn set_address(&mut self, kind: QueueAddressKind, address: GuestAddress) {
        match kind {
            QueueAddressKind::DescriptorTable => self.descriptor_table = address,
            QueueAddressKind::DriverRing => self.driver_ring = address,
            QueueAddressKind::DeviceRing => self.device_ring = address,
        }
    }

    fn reset(&mut self) {
        self.size = 0;
        self.ready = false;
        self.descriptor_table = GuestAddress::new(0);
        self.driver_ring = GuestAddress::new(0);
        self.device_ring = GuestAddress::new(0);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueAddressKind {
    DescriptorTable,
    DriverRing,
    DeviceRing,
}

impl QueueAddressKind {
    const fn register(self) -> VirtioMmioRegister {
        match self {
            Self::DescriptorTable => VirtioMmioRegister::QueueDescLow,
            Self::DriverRing => VirtioMmioRegister::QueueDriverLow,
            Self::DeviceRing => VirtioMmioRegister::QueueDeviceLow,
        }
    }

    const fn alignment(self) -> u64 {
        match self {
            Self::DescriptorTable => VIRTQUEUE_DESCRIPTOR_ALIGNMENT,
            Self::DriverRing => VIRTQUEUE_AVAILABLE_RING_ALIGNMENT,
            Self::DeviceRing => VIRTQUEUE_USED_RING_ALIGNMENT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioMmioQueueRegisterError {
    EmptyQueueTable,
    InvalidQueueMaxSize {
        queue_index: usize,
        max_size: u16,
    },
    InvalidQueueIndex {
        queue_index: u32,
        queue_count: usize,
    },
    UnsupportedRegisterRead {
        register: VirtioMmioRegister,
    },
    UnsupportedRegisterWrite {
        register: VirtioMmioRegister,
    },
    QueueConfigNotWritable {
        status: u32,
    },
    InvalidQueueSize {
        queue_index: u32,
        queue_size: u32,
        max_size: u16,
    },
    InvalidQueueReadyValue {
        queue_index: u32,
        value: u32,
    },
    UnalignedQueueAddress {
        queue_index: u32,
        register: VirtioMmioRegister,
        address: GuestAddress,
        alignment: u64,
    },
}

impl fmt::Display for VirtioMmioQueueRegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyQueueTable => f.write_str("virtio-mmio queue table cannot be empty"),
            Self::InvalidQueueMaxSize {
                queue_index,
                max_size,
            } => {
                write!(
                    f,
                    "virtio-mmio queue {queue_index} max size {max_size} must be a nonzero power of two"
                )
            }
            Self::InvalidQueueIndex {
                queue_index,
                queue_count,
            } => {
                write!(
                    f,
                    "virtio-mmio queue index {queue_index} is outside queue table size {queue_count}"
                )
            }
            Self::UnsupportedRegisterRead { register } => {
                write!(
                    f,
                    "unsupported virtio-mmio queue state read from {register}"
                )
            }
            Self::UnsupportedRegisterWrite { register } => {
                write!(f, "unsupported virtio-mmio queue state write to {register}")
            }
            Self::QueueConfigNotWritable { status } => {
                write!(
                    f,
                    "virtio-mmio queue configuration cannot be written while status is 0x{status:x}"
                )
            }
            Self::InvalidQueueSize {
                queue_index,
                queue_size,
                max_size,
            } => {
                write!(
                    f,
                    "virtio-mmio queue {queue_index} size {queue_size} must be a nonzero power of two not exceeding max size {max_size}"
                )
            }
            Self::InvalidQueueReadyValue { queue_index, value } => {
                write!(
                    f,
                    "virtio-mmio queue {queue_index} ready value {value} must be 0 or 1"
                )
            }
            Self::UnalignedQueueAddress {
                queue_index,
                register,
                address,
                alignment,
            } => {
                write!(
                    f,
                    "virtio-mmio queue {queue_index} {register} address {address} is not aligned to {alignment} bytes"
                )
            }
        }
    }
}

impl std::error::Error for VirtioMmioQueueRegisterError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioMmioQueueNotificationRegisters {
    pending_notifications: Vec<bool>,
}

impl VirtioMmioQueueNotificationRegisters {
    pub fn new(queue_count: usize) -> Result<Self, VirtioMmioQueueNotificationError> {
        if queue_count == 0 {
            return Err(VirtioMmioQueueNotificationError::EmptyQueueTable);
        }

        Ok(Self {
            pending_notifications: vec![false; queue_count],
        })
    }

    pub fn queue_count(&self) -> usize {
        self.pending_notifications.len()
    }

    pub fn is_queue_notification_pending(
        &self,
        queue_index: u32,
    ) -> Result<bool, VirtioMmioQueueNotificationError> {
        let index = self.queue_index(queue_index)?;
        self.pending_notifications
            .get(index)
            .copied()
            .ok_or_else(|| self.invalid_queue_index(queue_index))
    }

    pub fn pending_queue_notifications(&self) -> Vec<usize> {
        self.pending_notifications
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, is_pending)| *is_pending)
            .map(|(queue_index, _)| queue_index)
            .collect()
    }

    pub fn take_pending_queue_notifications(&mut self) -> Vec<usize> {
        let pending_notifications = self.pending_queue_notifications();
        self.reset();
        pending_notifications
    }

    pub fn read_register(
        &self,
        register: VirtioMmioRegister,
    ) -> Result<u32, VirtioMmioQueueNotificationError> {
        match register {
            VirtioMmioRegister::MagicValue
            | VirtioMmioRegister::Version
            | VirtioMmioRegister::DeviceId
            | VirtioMmioRegister::VendorId
            | VirtioMmioRegister::DeviceFeatures
            | VirtioMmioRegister::DeviceFeaturesSel
            | VirtioMmioRegister::DriverFeatures
            | VirtioMmioRegister::DriverFeaturesSel
            | VirtioMmioRegister::QueueSel
            | VirtioMmioRegister::QueueNumMax
            | VirtioMmioRegister::QueueNum
            | VirtioMmioRegister::QueueReady
            | VirtioMmioRegister::QueueNotify
            | VirtioMmioRegister::InterruptStatus
            | VirtioMmioRegister::InterruptAck
            | VirtioMmioRegister::Status
            | VirtioMmioRegister::QueueDescLow
            | VirtioMmioRegister::QueueDescHigh
            | VirtioMmioRegister::QueueDriverLow
            | VirtioMmioRegister::QueueDriverHigh
            | VirtioMmioRegister::QueueDeviceLow
            | VirtioMmioRegister::QueueDeviceHigh
            | VirtioMmioRegister::ConfigGeneration => {
                Err(VirtioMmioQueueNotificationError::UnsupportedRegisterRead { register })
            }
        }
    }

    pub fn write_register(
        &mut self,
        register: VirtioMmioRegister,
        value: u32,
        status: u32,
    ) -> Result<(), VirtioMmioQueueNotificationError> {
        match register {
            VirtioMmioRegister::QueueNotify => self.write_queue_notify(value, status),
            VirtioMmioRegister::MagicValue
            | VirtioMmioRegister::Version
            | VirtioMmioRegister::DeviceId
            | VirtioMmioRegister::VendorId
            | VirtioMmioRegister::DeviceFeatures
            | VirtioMmioRegister::DeviceFeaturesSel
            | VirtioMmioRegister::DriverFeatures
            | VirtioMmioRegister::DriverFeaturesSel
            | VirtioMmioRegister::QueueSel
            | VirtioMmioRegister::QueueNumMax
            | VirtioMmioRegister::QueueNum
            | VirtioMmioRegister::QueueReady
            | VirtioMmioRegister::InterruptStatus
            | VirtioMmioRegister::InterruptAck
            | VirtioMmioRegister::Status
            | VirtioMmioRegister::QueueDescLow
            | VirtioMmioRegister::QueueDescHigh
            | VirtioMmioRegister::QueueDriverLow
            | VirtioMmioRegister::QueueDriverHigh
            | VirtioMmioRegister::QueueDeviceLow
            | VirtioMmioRegister::QueueDeviceHigh
            | VirtioMmioRegister::ConfigGeneration => {
                Err(VirtioMmioQueueNotificationError::UnsupportedRegisterWrite { register })
            }
        }
    }

    pub fn reset(&mut self) {
        self.pending_notifications.fill(false);
    }

    fn write_queue_notify(
        &mut self,
        queue_index: u32,
        status: u32,
    ) -> Result<(), VirtioMmioQueueNotificationError> {
        validate_queue_notification_status(status)?;

        let index = self.queue_index(queue_index)?;
        let queue_count = self.queue_count();
        let pending = self.pending_notifications.get_mut(index).ok_or(
            VirtioMmioQueueNotificationError::InvalidQueueIndex {
                queue_index,
                queue_count,
            },
        )?;
        *pending = true;
        Ok(())
    }

    fn queue_index(&self, queue_index: u32) -> Result<usize, VirtioMmioQueueNotificationError> {
        let index =
            usize::try_from(queue_index).map_err(|_| self.invalid_queue_index(queue_index))?;
        if index < self.queue_count() {
            Ok(index)
        } else {
            Err(self.invalid_queue_index(queue_index))
        }
    }

    fn invalid_queue_index(&self, queue_index: u32) -> VirtioMmioQueueNotificationError {
        VirtioMmioQueueNotificationError::InvalidQueueIndex {
            queue_index,
            queue_count: self.queue_count(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioMmioQueueNotificationError {
    EmptyQueueTable,
    InvalidQueueIndex {
        queue_index: u32,
        queue_count: usize,
    },
    UnsupportedRegisterRead {
        register: VirtioMmioRegister,
    },
    UnsupportedRegisterWrite {
        register: VirtioMmioRegister,
    },
    QueueNotifyNotWritable {
        status: u32,
    },
}

impl fmt::Display for VirtioMmioQueueNotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyQueueTable => f.write_str("virtio-mmio queue table cannot be empty"),
            Self::InvalidQueueIndex {
                queue_index,
                queue_count,
            } => {
                write!(
                    f,
                    "virtio-mmio queue notification index {queue_index} is outside queue table size {queue_count}"
                )
            }
            Self::UnsupportedRegisterRead { register } => {
                write!(
                    f,
                    "unsupported virtio-mmio queue notification state read from {register}"
                )
            }
            Self::UnsupportedRegisterWrite { register } => {
                write!(
                    f,
                    "unsupported virtio-mmio queue notification state write to {register}"
                )
            }
            Self::QueueNotifyNotWritable { status } => {
                write!(
                    f,
                    "virtio-mmio queue notification cannot be written while status is 0x{status:x}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioMmioQueueNotificationError {}

pub trait VirtioMmioDeviceConfigHandler: fmt::Debug + Send {
    fn read_device_config(
        &self,
        access: VirtioMmioDeviceConfigAccess,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError>;

    fn write_device_config(
        &mut self,
        access: VirtioMmioDeviceConfigAccess,
        data: MmioAccessBytes,
    ) -> Result<(), VirtioMmioDeviceConfigError>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnsupportedVirtioMmioDeviceConfig;

impl VirtioMmioDeviceConfigHandler for UnsupportedVirtioMmioDeviceConfig {
    fn read_device_config(
        &self,
        access: VirtioMmioDeviceConfigAccess,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
        Err(VirtioMmioDeviceConfigError::UnsupportedRead {
            offset: access.offset(),
            len: access.len(),
        })
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtioMmioDeviceConfigError {
    UnsupportedRead { offset: u64, len: usize },
    UnsupportedWrite { offset: u64, len: usize },
    Handler { source: MmioHandlerError },
}

impl From<MmioHandlerError> for VirtioMmioDeviceConfigError {
    fn from(source: MmioHandlerError) -> Self {
        Self::Handler { source }
    }
}

impl fmt::Display for VirtioMmioDeviceConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedRead { offset, len } => {
                write!(
                    f,
                    "unsupported virtio-mmio device config read at offset 0x{offset:x} with length {len}"
                )
            }
            Self::UnsupportedWrite { offset, len } => {
                write!(
                    f,
                    "unsupported virtio-mmio device config write at offset 0x{offset:x} with length {len}"
                )
            }
            Self::Handler { source } => {
                write!(f, "virtio-mmio device config handler failed: {source}")
            }
        }
    }
}

impl std::error::Error for VirtioMmioDeviceConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Handler { source } => Some(source),
            Self::UnsupportedRead { .. } | Self::UnsupportedWrite { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioMmioRegisterHandler<C = UnsupportedVirtioMmioDeviceConfig> {
    device: VirtioMmioDeviceRegisters,
    queues: VirtioMmioQueueRegisters,
    queue_notifications: VirtioMmioQueueNotificationRegisters,
    interrupts: VirtioMmioInterruptRegisters,
    device_config: C,
    requires_device_config_write_status: bool,
}

impl VirtioMmioRegisterHandler<UnsupportedVirtioMmioDeviceConfig> {
    pub fn new(
        device_id: u32,
        device_features: u64,
        queue_max_sizes: &[u16],
    ) -> Result<Self, VirtioMmioRegisterHandlerError> {
        Self::with_vendor_id_and_config_generation(
            device_id,
            VIRTIO_MMIO_VENDOR_ID,
            device_features,
            0,
            queue_max_sizes,
        )
    }

    pub fn with_vendor_id_and_config_generation(
        device_id: u32,
        vendor_id: u32,
        device_features: u64,
        config_generation: u32,
        queue_max_sizes: &[u16],
    ) -> Result<Self, VirtioMmioRegisterHandlerError> {
        Self::with_vendor_id_and_config_generation_and_device_config_status_gate(
            device_id,
            vendor_id,
            device_features,
            config_generation,
            queue_max_sizes,
            UnsupportedVirtioMmioDeviceConfig,
            false,
        )
    }
}

impl<C: VirtioMmioDeviceConfigHandler> VirtioMmioRegisterHandler<C> {
    pub fn with_device_config(
        device_id: u32,
        device_features: u64,
        queue_max_sizes: &[u16],
        device_config: C,
    ) -> Result<Self, VirtioMmioRegisterHandlerError> {
        Self::with_vendor_id_and_config_generation_and_device_config_status_gate(
            device_id,
            VIRTIO_MMIO_VENDOR_ID,
            device_features,
            0,
            queue_max_sizes,
            device_config,
            true,
        )
    }

    pub fn with_vendor_id_and_config_generation_and_device_config(
        device_id: u32,
        vendor_id: u32,
        device_features: u64,
        config_generation: u32,
        queue_max_sizes: &[u16],
        device_config: C,
    ) -> Result<Self, VirtioMmioRegisterHandlerError> {
        Self::with_vendor_id_and_config_generation_and_device_config_status_gate(
            device_id,
            vendor_id,
            device_features,
            config_generation,
            queue_max_sizes,
            device_config,
            true,
        )
    }

    fn with_vendor_id_and_config_generation_and_device_config_status_gate(
        device_id: u32,
        vendor_id: u32,
        device_features: u64,
        config_generation: u32,
        queue_max_sizes: &[u16],
        device_config: C,
        requires_device_config_write_status: bool,
    ) -> Result<Self, VirtioMmioRegisterHandlerError> {
        let queues = VirtioMmioQueueRegisters::new(queue_max_sizes).map_err(|source| {
            VirtioMmioRegisterHandlerError::QueueRegisterInitialization { source }
        })?;
        let queue_count = queues.queue_count();
        let queue_notifications =
            VirtioMmioQueueNotificationRegisters::new(queue_count).map_err(|source| {
                VirtioMmioRegisterHandlerError::QueueNotificationInitialization { source }
            })?;

        Ok(Self {
            device: VirtioMmioDeviceRegisters::with_vendor_id_and_config_generation(
                device_id,
                vendor_id,
                device_features,
                config_generation,
            ),
            queues,
            queue_notifications,
            interrupts: VirtioMmioInterruptRegisters::new(),
            device_config,
            requires_device_config_write_status,
        })
    }

    pub const fn device_registers(&self) -> &VirtioMmioDeviceRegisters {
        &self.device
    }

    pub const fn queue_registers(&self) -> &VirtioMmioQueueRegisters {
        &self.queues
    }

    pub const fn queue_notification_registers(&self) -> &VirtioMmioQueueNotificationRegisters {
        &self.queue_notifications
    }

    pub fn is_queue_notification_pending(
        &self,
        queue_index: u32,
    ) -> Result<bool, VirtioMmioQueueNotificationError> {
        self.queue_notifications
            .is_queue_notification_pending(queue_index)
    }

    pub fn pending_queue_notifications(&self) -> Vec<usize> {
        self.queue_notifications.pending_queue_notifications()
    }

    pub fn take_pending_queue_notifications(&mut self) -> Vec<usize> {
        self.queue_notifications.take_pending_queue_notifications()
    }

    pub const fn interrupt_registers(&self) -> &VirtioMmioInterruptRegisters {
        &self.interrupts
    }

    pub const fn device_config_handler(&self) -> &C {
        &self.device_config
    }

    pub fn mark_interrupt_pending(&mut self, kind: DeviceInterruptKind) {
        self.interrupts.mark_pending(kind);
    }

    pub fn read_access(
        &self,
        access: MmioAccess,
    ) -> Result<MmioAccessBytes, VirtioMmioRegisterHandlerError> {
        let operation = MmioOperation::read(access)
            .map_err(|source| VirtioMmioRegisterHandlerError::InvalidOperation { source })?;
        match decode_virtio_mmio_access(&operation)
            .map_err(|source| VirtioMmioRegisterHandlerError::DecodeAccess { source })?
        {
            VirtioMmioAccess::Register(register_access) => {
                let value = self.read_register(register_access.register())?;
                register_read_data(value)
            }
            VirtioMmioAccess::DeviceConfig(config_access) => self.read_device_config(config_access),
        }
    }

    pub fn write_access(
        &mut self,
        access: MmioAccess,
        data: MmioAccessBytes,
    ) -> Result<(), VirtioMmioRegisterHandlerError> {
        let operation = MmioOperation::write(access, data)
            .map_err(|source| VirtioMmioRegisterHandlerError::InvalidOperation { source })?;
        match decode_virtio_mmio_access(&operation)
            .map_err(|source| VirtioMmioRegisterHandlerError::DecodeAccess { source })?
        {
            VirtioMmioAccess::Register(register_access) => {
                self.write_register(register_access.register(), register_write_value(data)?)
            }
            VirtioMmioAccess::DeviceConfig(config_access) => {
                self.write_device_config(config_access, data)
            }
        }
    }

    pub fn read_register(
        &self,
        register: VirtioMmioRegister,
    ) -> Result<u32, VirtioMmioRegisterHandlerError> {
        match register {
            VirtioMmioRegister::MagicValue
            | VirtioMmioRegister::Version
            | VirtioMmioRegister::DeviceId
            | VirtioMmioRegister::VendorId
            | VirtioMmioRegister::DeviceFeatures
            | VirtioMmioRegister::Status
            | VirtioMmioRegister::ConfigGeneration => {
                self.device.read_register(register).map_err(|source| {
                    VirtioMmioRegisterHandlerError::DeviceRegisterRead { register, source }
                })
            }
            VirtioMmioRegister::QueueNumMax | VirtioMmioRegister::QueueReady => {
                self.queues.read_register(register).map_err(|source| {
                    VirtioMmioRegisterHandlerError::QueueRegisterRead { register, source }
                })
            }
            VirtioMmioRegister::InterruptStatus => {
                self.interrupts.read_register(register).map_err(|source| {
                    VirtioMmioRegisterHandlerError::InterruptRegisterRead { register, source }
                })
            }
            VirtioMmioRegister::DeviceFeaturesSel
            | VirtioMmioRegister::DriverFeatures
            | VirtioMmioRegister::DriverFeaturesSel
            | VirtioMmioRegister::QueueSel
            | VirtioMmioRegister::QueueNum
            | VirtioMmioRegister::QueueNotify
            | VirtioMmioRegister::InterruptAck
            | VirtioMmioRegister::QueueDescLow
            | VirtioMmioRegister::QueueDescHigh
            | VirtioMmioRegister::QueueDriverLow
            | VirtioMmioRegister::QueueDriverHigh
            | VirtioMmioRegister::QueueDeviceLow
            | VirtioMmioRegister::QueueDeviceHigh => {
                Err(VirtioMmioRegisterHandlerError::UnsupportedRegisterRead { register })
            }
        }
    }

    pub fn write_register(
        &mut self,
        register: VirtioMmioRegister,
        value: u32,
    ) -> Result<(), VirtioMmioRegisterHandlerError> {
        match register {
            VirtioMmioRegister::DeviceFeaturesSel
            | VirtioMmioRegister::DriverFeatures
            | VirtioMmioRegister::DriverFeaturesSel
            | VirtioMmioRegister::Status => self.write_device_register(register, value),
            VirtioMmioRegister::QueueSel
            | VirtioMmioRegister::QueueNum
            | VirtioMmioRegister::QueueReady
            | VirtioMmioRegister::QueueDescLow
            | VirtioMmioRegister::QueueDescHigh
            | VirtioMmioRegister::QueueDriverLow
            | VirtioMmioRegister::QueueDriverHigh
            | VirtioMmioRegister::QueueDeviceLow
            | VirtioMmioRegister::QueueDeviceHigh => self
                .queues
                .write_register(register, value, self.device.status())
                .map_err(
                    |source| VirtioMmioRegisterHandlerError::QueueRegisterWrite {
                        register,
                        source,
                    },
                ),
            VirtioMmioRegister::QueueNotify => self
                .queue_notifications
                .write_register(register, value, self.device.status())
                .map_err(
                    |source| VirtioMmioRegisterHandlerError::QueueNotificationWrite {
                        register,
                        source,
                    },
                ),
            VirtioMmioRegister::InterruptAck => self
                .interrupts
                .write_register(register, value, self.device.status())
                .map_err(
                    |source| VirtioMmioRegisterHandlerError::InterruptRegisterWrite {
                        register,
                        source,
                    },
                ),
            VirtioMmioRegister::MagicValue
            | VirtioMmioRegister::Version
            | VirtioMmioRegister::DeviceId
            | VirtioMmioRegister::VendorId
            | VirtioMmioRegister::DeviceFeatures
            | VirtioMmioRegister::QueueNumMax
            | VirtioMmioRegister::InterruptStatus
            | VirtioMmioRegister::ConfigGeneration => {
                Err(VirtioMmioRegisterHandlerError::UnsupportedRegisterWrite { register })
            }
        }
    }

    pub fn reset(&mut self) {
        self.device.reset();
        self.queues.reset();
        self.queue_notifications.reset();
        self.interrupts.reset();
    }

    fn write_device_register(
        &mut self,
        register: VirtioMmioRegister,
        value: u32,
    ) -> Result<(), VirtioMmioRegisterHandlerError> {
        self.device
            .write_register(register, value)
            .map_err(
                |source| VirtioMmioRegisterHandlerError::DeviceRegisterWrite { register, source },
            )?;

        if register == VirtioMmioRegister::Status && value == VIRTIO_DEVICE_STATUS_INIT {
            self.queues.reset();
            self.queue_notifications.reset();
            self.interrupts.reset();
        }

        Ok(())
    }

    fn read_device_config(
        &self,
        access: VirtioMmioDeviceConfigAccess,
    ) -> Result<MmioAccessBytes, VirtioMmioRegisterHandlerError> {
        let data = self
            .device_config
            .read_device_config(access)
            .map_err(|source| map_device_config_read_error(access, source))?;
        if data.len() == access.len() {
            Ok(data)
        } else {
            Err(VirtioMmioRegisterHandlerError::DeviceConfigReadDataLength {
                offset: access.offset(),
                expected: access.len(),
                actual: data.len(),
            })
        }
    }

    fn write_device_config(
        &mut self,
        access: VirtioMmioDeviceConfigAccess,
        data: MmioAccessBytes,
    ) -> Result<(), VirtioMmioRegisterHandlerError> {
        if self.requires_device_config_write_status {
            validate_device_config_write_status(self.device.status())?;
        }

        self.device_config
            .write_device_config(access, data)
            .map_err(|source| map_device_config_write_error(access, source))
    }
}

impl<C: VirtioMmioDeviceConfigHandler> MmioHandler for VirtioMmioRegisterHandler<C> {
    fn read(&mut self, access: MmioAccess) -> Result<MmioAccessBytes, MmioHandlerError> {
        self.read_access(access).map_err(MmioHandlerError::from)
    }

    fn write(&mut self, access: MmioAccess, data: MmioAccessBytes) -> Result<(), MmioHandlerError> {
        self.write_access(access, data)
            .map_err(MmioHandlerError::from)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtioMmioRegisterHandlerError {
    QueueRegisterInitialization {
        source: VirtioMmioQueueRegisterError,
    },
    QueueNotificationInitialization {
        source: VirtioMmioQueueNotificationError,
    },
    InvalidOperation {
        source: MmioOperationError,
    },
    DecodeAccess {
        source: VirtioMmioAccessError,
    },
    RegisterReadData {
        source: MmioAccessBytesError,
    },
    RegisterWriteDataLength {
        len: usize,
    },
    UnsupportedDeviceConfigRead {
        offset: u64,
        len: usize,
    },
    UnsupportedDeviceConfigWrite {
        offset: u64,
        len: usize,
    },
    DeviceConfigRead {
        offset: u64,
        len: usize,
        source: VirtioMmioDeviceConfigError,
    },
    DeviceConfigWrite {
        offset: u64,
        len: usize,
        source: VirtioMmioDeviceConfigError,
    },
    DeviceConfigReadDataLength {
        offset: u64,
        expected: usize,
        actual: usize,
    },
    DeviceConfigWriteNotWritable {
        status: u32,
    },
    UnsupportedRegisterRead {
        register: VirtioMmioRegister,
    },
    UnsupportedRegisterWrite {
        register: VirtioMmioRegister,
    },
    DeviceRegisterRead {
        register: VirtioMmioRegister,
        source: VirtioMmioRegisterStateError,
    },
    DeviceRegisterWrite {
        register: VirtioMmioRegister,
        source: VirtioMmioRegisterStateError,
    },
    QueueRegisterRead {
        register: VirtioMmioRegister,
        source: VirtioMmioQueueRegisterError,
    },
    QueueRegisterWrite {
        register: VirtioMmioRegister,
        source: VirtioMmioQueueRegisterError,
    },
    QueueNotificationWrite {
        register: VirtioMmioRegister,
        source: VirtioMmioQueueNotificationError,
    },
    InterruptRegisterRead {
        register: VirtioMmioRegister,
        source: VirtioMmioInterruptRegisterError,
    },
    InterruptRegisterWrite {
        register: VirtioMmioRegister,
        source: VirtioMmioInterruptRegisterError,
    },
}

impl fmt::Display for VirtioMmioRegisterHandlerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueRegisterInitialization { source } => {
                write!(
                    f,
                    "failed to initialize virtio-mmio queue registers: {source}"
                )
            }
            Self::QueueNotificationInitialization { source } => {
                write!(
                    f,
                    "failed to initialize virtio-mmio queue notification registers: {source}"
                )
            }
            Self::InvalidOperation { source } => {
                write!(f, "invalid virtio-mmio MMIO operation: {source}")
            }
            Self::DecodeAccess { source } => {
                write!(f, "failed to decode virtio-mmio MMIO access: {source}")
            }
            Self::RegisterReadData { source } => {
                write!(
                    f,
                    "failed to build virtio-mmio register read data: {source}"
                )
            }
            Self::RegisterWriteDataLength { len } => {
                write!(
                    f,
                    "virtio-mmio register write data length {len} cannot be decoded as a 4-byte value"
                )
            }
            Self::UnsupportedDeviceConfigRead { offset, len } => {
                write!(
                    f,
                    "unsupported virtio-mmio device config read at offset 0x{offset:x} with length {len}"
                )
            }
            Self::UnsupportedDeviceConfigWrite { offset, len } => {
                write!(
                    f,
                    "unsupported virtio-mmio device config write at offset 0x{offset:x} with length {len}"
                )
            }
            Self::DeviceConfigRead {
                offset,
                len,
                source,
            } => {
                write!(
                    f,
                    "virtio-mmio device config read at offset 0x{offset:x} with length {len} failed: {source}"
                )
            }
            Self::DeviceConfigWrite {
                offset,
                len,
                source,
            } => {
                write!(
                    f,
                    "virtio-mmio device config write at offset 0x{offset:x} with length {len} failed: {source}"
                )
            }
            Self::DeviceConfigReadDataLength {
                offset,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "virtio-mmio device config read at offset 0x{offset:x} returned {actual} bytes; expected {expected}"
                )
            }
            Self::DeviceConfigWriteNotWritable { status } => {
                write!(
                    f,
                    "virtio-mmio device config cannot be written while status is 0x{status:x}"
                )
            }
            Self::UnsupportedRegisterRead { register } => {
                write!(
                    f,
                    "unsupported virtio-mmio register handler read from {register}"
                )
            }
            Self::UnsupportedRegisterWrite { register } => {
                write!(
                    f,
                    "unsupported virtio-mmio register handler write to {register}"
                )
            }
            Self::DeviceRegisterRead { register, source } => {
                write!(
                    f,
                    "virtio-mmio device register {register} read failed: {source}"
                )
            }
            Self::DeviceRegisterWrite { register, source } => {
                write!(
                    f,
                    "virtio-mmio device register {register} write failed: {source}"
                )
            }
            Self::QueueRegisterRead { register, source } => {
                write!(
                    f,
                    "virtio-mmio queue register {register} read failed: {source}"
                )
            }
            Self::QueueRegisterWrite { register, source } => {
                write!(
                    f,
                    "virtio-mmio queue register {register} write failed: {source}"
                )
            }
            Self::QueueNotificationWrite { register, source } => {
                write!(
                    f,
                    "virtio-mmio queue notification register {register} write failed: {source}"
                )
            }
            Self::InterruptRegisterRead { register, source } => {
                write!(
                    f,
                    "virtio-mmio interrupt register {register} read failed: {source}"
                )
            }
            Self::InterruptRegisterWrite { register, source } => {
                write!(
                    f,
                    "virtio-mmio interrupt register {register} write failed: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioMmioRegisterHandlerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueRegisterInitialization { source }
            | Self::QueueRegisterRead { source, .. }
            | Self::QueueRegisterWrite { source, .. } => Some(source),
            Self::QueueNotificationInitialization { source }
            | Self::QueueNotificationWrite { source, .. } => Some(source),
            Self::InvalidOperation { source } => Some(source),
            Self::DecodeAccess { source } => Some(source),
            Self::RegisterReadData { source } => Some(source),
            Self::DeviceRegisterRead { source, .. } | Self::DeviceRegisterWrite { source, .. } => {
                Some(source)
            }
            Self::DeviceConfigRead { source, .. } | Self::DeviceConfigWrite { source, .. } => {
                Some(source)
            }
            Self::InterruptRegisterRead { source, .. }
            | Self::InterruptRegisterWrite { source, .. } => Some(source),
            Self::RegisterWriteDataLength { .. }
            | Self::UnsupportedDeviceConfigRead { .. }
            | Self::UnsupportedDeviceConfigWrite { .. }
            | Self::DeviceConfigReadDataLength { .. }
            | Self::DeviceConfigWriteNotWritable { .. }
            | Self::UnsupportedRegisterRead { .. }
            | Self::UnsupportedRegisterWrite { .. } => None,
        }
    }
}

impl From<VirtioMmioRegisterHandlerError> for MmioHandlerError {
    fn from(error: VirtioMmioRegisterHandlerError) -> Self {
        Self::new(error.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioMmioRegister {
    MagicValue,
    Version,
    DeviceId,
    VendorId,
    DeviceFeatures,
    DeviceFeaturesSel,
    DriverFeatures,
    DriverFeaturesSel,
    QueueSel,
    QueueNumMax,
    QueueNum,
    QueueReady,
    QueueNotify,
    InterruptStatus,
    InterruptAck,
    Status,
    QueueDescLow,
    QueueDescHigh,
    QueueDriverLow,
    QueueDriverHigh,
    QueueDeviceLow,
    QueueDeviceHigh,
    ConfigGeneration,
}

impl VirtioMmioRegister {
    pub const fn offset(self) -> u64 {
        match self {
            Self::MagicValue => 0x00,
            Self::Version => 0x04,
            Self::DeviceId => 0x08,
            Self::VendorId => 0x0c,
            Self::DeviceFeatures => 0x10,
            Self::DeviceFeaturesSel => 0x14,
            Self::DriverFeatures => 0x20,
            Self::DriverFeaturesSel => 0x24,
            Self::QueueSel => 0x30,
            Self::QueueNumMax => 0x34,
            Self::QueueNum => 0x38,
            Self::QueueReady => 0x44,
            Self::QueueNotify => VIRTIO_MMIO_NOTIFY_OFFSET,
            Self::InterruptStatus => 0x60,
            Self::InterruptAck => 0x64,
            Self::Status => 0x70,
            Self::QueueDescLow => 0x80,
            Self::QueueDescHigh => 0x84,
            Self::QueueDriverLow => 0x90,
            Self::QueueDriverHigh => 0x94,
            Self::QueueDeviceLow => 0xa0,
            Self::QueueDeviceHigh => 0xa4,
            Self::ConfigGeneration => 0xfc,
        }
    }

    pub const fn is_readable(self) -> bool {
        match self {
            Self::MagicValue
            | Self::Version
            | Self::DeviceId
            | Self::VendorId
            | Self::DeviceFeatures
            | Self::QueueNumMax
            | Self::QueueReady
            | Self::InterruptStatus
            | Self::Status
            | Self::ConfigGeneration => true,
            Self::DeviceFeaturesSel
            | Self::DriverFeatures
            | Self::DriverFeaturesSel
            | Self::QueueSel
            | Self::QueueNum
            | Self::QueueNotify
            | Self::InterruptAck
            | Self::QueueDescLow
            | Self::QueueDescHigh
            | Self::QueueDriverLow
            | Self::QueueDriverHigh
            | Self::QueueDeviceLow
            | Self::QueueDeviceHigh => false,
        }
    }

    pub const fn is_writable(self) -> bool {
        match self {
            Self::DeviceFeaturesSel
            | Self::DriverFeatures
            | Self::DriverFeaturesSel
            | Self::QueueSel
            | Self::QueueNum
            | Self::QueueReady
            | Self::QueueNotify
            | Self::InterruptAck
            | Self::Status
            | Self::QueueDescLow
            | Self::QueueDescHigh
            | Self::QueueDriverLow
            | Self::QueueDriverHigh
            | Self::QueueDeviceLow
            | Self::QueueDeviceHigh => true,
            Self::MagicValue
            | Self::Version
            | Self::DeviceId
            | Self::VendorId
            | Self::DeviceFeatures
            | Self::QueueNumMax
            | Self::InterruptStatus
            | Self::ConfigGeneration => false,
        }
    }

    pub const fn read_at_offset(offset: u64) -> Option<Self> {
        match offset {
            0x00 => Some(Self::MagicValue),
            0x04 => Some(Self::Version),
            0x08 => Some(Self::DeviceId),
            0x0c => Some(Self::VendorId),
            0x10 => Some(Self::DeviceFeatures),
            0x34 => Some(Self::QueueNumMax),
            0x44 => Some(Self::QueueReady),
            0x60 => Some(Self::InterruptStatus),
            0x70 => Some(Self::Status),
            0xfc => Some(Self::ConfigGeneration),
            _ => None,
        }
    }

    pub const fn write_at_offset(offset: u64) -> Option<Self> {
        match offset {
            0x14 => Some(Self::DeviceFeaturesSel),
            0x20 => Some(Self::DriverFeatures),
            0x24 => Some(Self::DriverFeaturesSel),
            0x30 => Some(Self::QueueSel),
            0x38 => Some(Self::QueueNum),
            0x44 => Some(Self::QueueReady),
            0x50 => Some(Self::QueueNotify),
            0x64 => Some(Self::InterruptAck),
            0x70 => Some(Self::Status),
            0x80 => Some(Self::QueueDescLow),
            0x84 => Some(Self::QueueDescHigh),
            0x90 => Some(Self::QueueDriverLow),
            0x94 => Some(Self::QueueDriverHigh),
            0xa0 => Some(Self::QueueDeviceLow),
            0xa4 => Some(Self::QueueDeviceHigh),
            _ => None,
        }
    }
}

impl fmt::Display for VirtioMmioRegister {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MagicValue => f.write_str("MagicValue"),
            Self::Version => f.write_str("Version"),
            Self::DeviceId => f.write_str("DeviceId"),
            Self::VendorId => f.write_str("VendorId"),
            Self::DeviceFeatures => f.write_str("DeviceFeatures"),
            Self::DeviceFeaturesSel => f.write_str("DeviceFeaturesSel"),
            Self::DriverFeatures => f.write_str("DriverFeatures"),
            Self::DriverFeaturesSel => f.write_str("DriverFeaturesSel"),
            Self::QueueSel => f.write_str("QueueSel"),
            Self::QueueNumMax => f.write_str("QueueNumMax"),
            Self::QueueNum => f.write_str("QueueNum"),
            Self::QueueReady => f.write_str("QueueReady"),
            Self::QueueNotify => f.write_str("QueueNotify"),
            Self::InterruptStatus => f.write_str("InterruptStatus"),
            Self::InterruptAck => f.write_str("InterruptAck"),
            Self::Status => f.write_str("Status"),
            Self::QueueDescLow => f.write_str("QueueDescLow"),
            Self::QueueDescHigh => f.write_str("QueueDescHigh"),
            Self::QueueDriverLow => f.write_str("QueueDriverLow"),
            Self::QueueDriverHigh => f.write_str("QueueDriverHigh"),
            Self::QueueDeviceLow => f.write_str("QueueDeviceLow"),
            Self::QueueDeviceHigh => f.write_str("QueueDeviceHigh"),
            Self::ConfigGeneration => f.write_str("ConfigGeneration"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioMmioAccess {
    Register(VirtioMmioRegisterAccess),
    DeviceConfig(VirtioMmioDeviceConfigAccess),
}

impl VirtioMmioAccess {
    pub const fn kind(self) -> MmioOperationKind {
        match self {
            Self::Register(access) => access.kind(),
            Self::DeviceConfig(access) => access.kind(),
        }
    }

    pub const fn len(self) -> usize {
        match self {
            Self::Register(_) => VIRTIO_MMIO_REGISTER_ACCESS_SIZE,
            Self::DeviceConfig(access) => access.len(),
        }
    }

    pub const fn is_empty(self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioMmioRegisterAccess {
    kind: MmioOperationKind,
    register: VirtioMmioRegister,
}

impl VirtioMmioRegisterAccess {
    pub const fn kind(self) -> MmioOperationKind {
        self.kind
    }

    pub const fn register(self) -> VirtioMmioRegister {
        self.register
    }

    pub const fn offset(self) -> u64 {
        self.register.offset()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioMmioDeviceConfigAccess {
    kind: MmioOperationKind,
    offset: u64,
    len: usize,
}

impl VirtioMmioDeviceConfigAccess {
    pub const fn kind(self) -> MmioOperationKind {
        self.kind
    }

    pub const fn offset(self) -> u64 {
        self.offset
    }

    pub const fn absolute_offset(self) -> u64 {
        self.offset + VIRTIO_MMIO_DEVICE_CONFIG_OFFSET
    }

    pub const fn len(self) -> usize {
        self.len
    }

    pub const fn is_empty(self) -> bool {
        false
    }
}

pub fn decode_virtio_mmio_access(
    operation: &MmioOperation,
) -> Result<VirtioMmioAccess, VirtioMmioAccessError> {
    let kind = operation.kind();
    let offset = operation.access().offset();
    let len = operation.data().len();
    let len_u64 =
        u64::try_from(len).map_err(|_| VirtioMmioAccessError::AccessLengthTooLarge { len })?;
    let end = offset
        .checked_add(len_u64)
        .ok_or(VirtioMmioAccessError::AccessRangeOverflow { kind, offset, len })?;

    if end > VIRTIO_MMIO_DEVICE_WINDOW_SIZE {
        return Err(VirtioMmioAccessError::AccessOutsideDeviceWindow {
            kind,
            offset,
            len,
            window_size: VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        });
    }

    if offset >= VIRTIO_MMIO_DEVICE_CONFIG_OFFSET {
        return Ok(VirtioMmioAccess::DeviceConfig(
            VirtioMmioDeviceConfigAccess {
                kind,
                offset: offset - VIRTIO_MMIO_DEVICE_CONFIG_OFFSET,
                len,
            },
        ));
    }

    decode_virtio_mmio_register_access(kind, offset, len, end)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioMmioAccessError {
    AccessLengthTooLarge {
        len: usize,
    },
    AccessRangeOverflow {
        kind: MmioOperationKind,
        offset: u64,
        len: usize,
    },
    AccessOutsideDeviceWindow {
        kind: MmioOperationKind,
        offset: u64,
        len: usize,
        window_size: u64,
    },
    RegisterAccessCrossesBoundary {
        kind: MmioOperationKind,
        offset: u64,
        len: usize,
        register_offset: u64,
        register_size: usize,
    },
    UnsupportedRegisterAccessSize {
        kind: MmioOperationKind,
        offset: u64,
        len: usize,
        expected: usize,
    },
    UnsupportedRegisterOffset {
        kind: MmioOperationKind,
        offset: u64,
    },
}

impl fmt::Display for VirtioMmioAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccessLengthTooLarge { len } => {
                write!(f, "virtio-mmio access length {len} cannot fit in u64")
            }
            Self::AccessRangeOverflow { kind, offset, len } => {
                write!(
                    f,
                    "virtio-mmio {kind} access at offset 0x{offset:x} with length {len} overflows"
                )
            }
            Self::AccessOutsideDeviceWindow {
                kind,
                offset,
                len,
                window_size,
            } => {
                write!(
                    f,
                    "virtio-mmio {kind} access at offset 0x{offset:x} with length {len} exceeds device window size 0x{window_size:x}"
                )
            }
            Self::RegisterAccessCrossesBoundary {
                kind,
                offset,
                len,
                register_offset,
                register_size,
            } => {
                write!(
                    f,
                    "virtio-mmio {kind} access at offset 0x{offset:x} with length {len} crosses {register_size}-byte register boundary at 0x{register_offset:x}"
                )
            }
            Self::UnsupportedRegisterAccessSize {
                kind,
                offset,
                len,
                expected,
            } => {
                write!(
                    f,
                    "unsupported virtio-mmio {kind} register access size {len} at offset 0x{offset:x}; expected {expected} bytes"
                )
            }
            Self::UnsupportedRegisterOffset { kind, offset } => {
                write!(
                    f,
                    "unsupported virtio-mmio {kind} register offset 0x{offset:x}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioMmioAccessError {}

fn decode_virtio_mmio_register_access(
    kind: MmioOperationKind,
    offset: u64,
    len: usize,
    end: u64,
) -> Result<VirtioMmioAccess, VirtioMmioAccessError> {
    let register_offset = register_slot_offset(offset);
    let register_end = register_offset + VIRTIO_MMIO_REGISTER_ACCESS_SIZE_U64;

    if end > register_end {
        return Err(VirtioMmioAccessError::RegisterAccessCrossesBoundary {
            kind,
            offset,
            len,
            register_offset,
            register_size: VIRTIO_MMIO_REGISTER_ACCESS_SIZE,
        });
    }

    if len != VIRTIO_MMIO_REGISTER_ACCESS_SIZE {
        return Err(VirtioMmioAccessError::UnsupportedRegisterAccessSize {
            kind,
            offset,
            len,
            expected: VIRTIO_MMIO_REGISTER_ACCESS_SIZE,
        });
    }

    let register = register_for_kind(kind, offset)
        .ok_or(VirtioMmioAccessError::UnsupportedRegisterOffset { kind, offset })?;

    Ok(VirtioMmioAccess::Register(VirtioMmioRegisterAccess {
        kind,
        register,
    }))
}

const fn register_for_kind(kind: MmioOperationKind, offset: u64) -> Option<VirtioMmioRegister> {
    match kind {
        MmioOperationKind::Read => VirtioMmioRegister::read_at_offset(offset),
        MmioOperationKind::Write => VirtioMmioRegister::write_at_offset(offset),
    }
}

const fn register_slot_offset(offset: u64) -> u64 {
    offset / VIRTIO_MMIO_REGISTER_ACCESS_SIZE_U64 * VIRTIO_MMIO_REGISTER_ACCESS_SIZE_U64
}

fn map_device_config_read_error(
    access: VirtioMmioDeviceConfigAccess,
    source: VirtioMmioDeviceConfigError,
) -> VirtioMmioRegisterHandlerError {
    match source {
        VirtioMmioDeviceConfigError::UnsupportedRead { .. } => {
            VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead {
                offset: access.offset(),
                len: access.len(),
            }
        }
        source => VirtioMmioRegisterHandlerError::DeviceConfigRead {
            offset: access.offset(),
            len: access.len(),
            source,
        },
    }
}

fn map_device_config_write_error(
    access: VirtioMmioDeviceConfigAccess,
    source: VirtioMmioDeviceConfigError,
) -> VirtioMmioRegisterHandlerError {
    match source {
        VirtioMmioDeviceConfigError::UnsupportedWrite { .. } => {
            VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigWrite {
                offset: access.offset(),
                len: access.len(),
            }
        }
        source => VirtioMmioRegisterHandlerError::DeviceConfigWrite {
            offset: access.offset(),
            len: access.len(),
            source,
        },
    }
}

fn register_read_data(value: u32) -> Result<MmioAccessBytes, VirtioMmioRegisterHandlerError> {
    MmioAccessBytes::new(&value.to_le_bytes())
        .map_err(|source| VirtioMmioRegisterHandlerError::RegisterReadData { source })
}

fn register_write_value(data: MmioAccessBytes) -> Result<u32, VirtioMmioRegisterHandlerError> {
    let bytes: [u8; VIRTIO_MMIO_REGISTER_ACCESS_SIZE] = data
        .as_slice()
        .try_into()
        .map_err(|_| VirtioMmioRegisterHandlerError::RegisterWriteDataLength { len: data.len() })?;
    Ok(u32::from_le_bytes(bytes))
}

fn feature_word(features: u64, selector: u32) -> Result<u32, VirtioMmioRegisterStateError> {
    match selector {
        0 => Ok((features & u64::from(u32::MAX)) as u32),
        1 => Ok((features >> 32) as u32),
        _ => Err(VirtioMmioRegisterStateError::UnsupportedFeaturePage { selector }),
    }
}

fn selected_feature_bits(selector: u32, value: u32) -> Result<u64, VirtioMmioRegisterStateError> {
    match selector {
        0 => Ok(u64::from(value)),
        1 => Ok(u64::from(value) << 32),
        _ => Err(VirtioMmioRegisterStateError::UnsupportedFeaturePage { selector }),
    }
}

fn validate_feature_selector(selector: u32) -> Result<(), VirtioMmioRegisterStateError> {
    if selector <= VIRTIO_MMIO_FEATURE_SELECTOR_MAX {
        Ok(())
    } else {
        Err(VirtioMmioRegisterStateError::UnsupportedFeaturePage { selector })
    }
}

const fn is_valid_status_transition(current: u32, requested: u32) -> bool {
    match current {
        VIRTIO_DEVICE_STATUS_INIT => requested == VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE => {
            requested == VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER
        }
        status if status == VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER => {
            requested
                == VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                    | VIRTIO_DEVICE_STATUS_DRIVER
                    | VIRTIO_DEVICE_STATUS_FEATURES_OK
        }
        status
            if status
                == VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                    | VIRTIO_DEVICE_STATUS_DRIVER
                    | VIRTIO_DEVICE_STATUS_FEATURES_OK =>
        {
            requested
                == VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                    | VIRTIO_DEVICE_STATUS_DRIVER
                    | VIRTIO_DEVICE_STATUS_FEATURES_OK
                    | VIRTIO_DEVICE_STATUS_DRIVER_OK
        }
        _ => false,
    }
}

fn validate_queue_max_size(
    queue_index: usize,
    max_size: u16,
) -> Result<(), VirtioMmioQueueRegisterError> {
    if max_size != 0 && max_size.is_power_of_two() {
        Ok(())
    } else {
        Err(VirtioMmioQueueRegisterError::InvalidQueueMaxSize {
            queue_index,
            max_size,
        })
    }
}

fn validate_queue_config_status(status: u32) -> Result<(), VirtioMmioQueueRegisterError> {
    if status
        == VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
            | VIRTIO_DEVICE_STATUS_DRIVER
            | VIRTIO_DEVICE_STATUS_FEATURES_OK
    {
        Ok(())
    } else {
        Err(VirtioMmioQueueRegisterError::QueueConfigNotWritable { status })
    }
}

fn validate_interrupt_ack_status(status: u32) -> Result<(), VirtioMmioInterruptRegisterError> {
    if (status & VIRTIO_DEVICE_STATUS_DRIVER_OK) == VIRTIO_DEVICE_STATUS_DRIVER_OK {
        Ok(())
    } else {
        Err(VirtioMmioInterruptRegisterError::InterruptAckNotWritable { status })
    }
}

fn validate_device_config_write_status(status: u32) -> Result<(), VirtioMmioRegisterHandlerError> {
    if (status & VIRTIO_DEVICE_STATUS_DRIVER) == VIRTIO_DEVICE_STATUS_DRIVER
        && (status & (VIRTIO_DEVICE_STATUS_FAILED | VIRTIO_DEVICE_STATUS_DEVICE_NEEDS_RESET)) == 0
    {
        Ok(())
    } else {
        Err(VirtioMmioRegisterHandlerError::DeviceConfigWriteNotWritable { status })
    }
}

fn validate_queue_notification_status(status: u32) -> Result<(), VirtioMmioQueueNotificationError> {
    if (status & VIRTIO_DEVICE_STATUS_DRIVER_OK) == VIRTIO_DEVICE_STATUS_DRIVER_OK {
        Ok(())
    } else {
        Err(VirtioMmioQueueNotificationError::QueueNotifyNotWritable { status })
    }
}

fn validate_queue_size(
    queue_index: u32,
    value: u32,
    max_size: u16,
) -> Result<u16, VirtioMmioQueueRegisterError> {
    let queue_size =
        u16::try_from(value).map_err(|_| VirtioMmioQueueRegisterError::InvalidQueueSize {
            queue_index,
            queue_size: value,
            max_size,
        })?;

    if queue_size != 0 && queue_size.is_power_of_two() && queue_size <= max_size {
        Ok(queue_size)
    } else {
        Err(VirtioMmioQueueRegisterError::InvalidQueueSize {
            queue_index,
            queue_size: value,
            max_size,
        })
    }
}

fn validate_queue_ready_value(
    queue_index: u32,
    value: u32,
) -> Result<bool, VirtioMmioQueueRegisterError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(VirtioMmioQueueRegisterError::InvalidQueueReadyValue { queue_index, value }),
    }
}

fn queue_ready_value(ready: bool) -> u32 {
    if ready { 1 } else { 0 }
}

fn replace_address_low(current: GuestAddress, value: u32) -> GuestAddress {
    GuestAddress::new((current.raw_value() & !u64::from(u32::MAX)) | u64::from(value))
}

fn replace_address_high(current: GuestAddress, value: u32) -> GuestAddress {
    GuestAddress::new((current.raw_value() & u64::from(u32::MAX)) | (u64::from(value) << 32))
}

fn validate_queue_address(
    queue_index: u32,
    kind: QueueAddressKind,
    address: GuestAddress,
) -> Result<(), VirtioMmioQueueRegisterError> {
    let alignment = kind.alignment();
    let is_aligned = address.is_aligned(alignment).unwrap_or(false);
    if is_aligned {
        Ok(())
    } else {
        Err(VirtioMmioQueueRegisterError::UnalignedQueueAddress {
            queue_index,
            register: kind.register(),
            address,
            alignment,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FAILED,
        VIRTIO_DEVICE_STATUS_FEATURES_OK, VIRTIO_DEVICE_STATUS_INIT,
        VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        VIRTIO_MMIO_FEATURE_VERSION_1, VIRTIO_MMIO_MAGIC_VALUE, VIRTIO_MMIO_NOTIFY_OFFSET,
        VIRTIO_MMIO_REGISTER_ACCESS_SIZE, VIRTIO_MMIO_REGISTER_SPACE_SIZE, VIRTIO_MMIO_VENDOR_ID,
        VIRTIO_MMIO_VERSION, VIRTIO_MMIO_VERSION_1_FEATURE, VirtioMmioAccess,
        VirtioMmioAccessError, VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError,
        VirtioMmioDeviceConfigHandler, VirtioMmioDeviceRegisters, VirtioMmioInterruptRegisterError,
        VirtioMmioInterruptRegisters, VirtioMmioQueueNotificationError,
        VirtioMmioQueueNotificationRegisters, VirtioMmioQueueRegisterError,
        VirtioMmioQueueRegisters, VirtioMmioRegister, VirtioMmioRegisterHandler,
        VirtioMmioRegisterHandlerError, VirtioMmioRegisterStateError, decode_virtio_mmio_access,
    };
    use crate::interrupt::{DeviceInterruptKind, DeviceInterruptStatusError};
    use crate::memory::GuestAddress;
    use crate::mmio::{
        MmioAccessBytes, MmioBus, MmioDispatchOutcome, MmioDispatcher, MmioHandlerError,
        MmioOperation, MmioOperationKind, MmioRegionId,
    };

    const BASE: u64 = 0x1000_0000;
    const QUEUE_CONFIG_STATUS: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    const DRIVER_OK_STATUS: u32 = QUEUE_CONFIG_STATUS | VIRTIO_DEVICE_STATUS_DRIVER_OK;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestDeviceConfig {
        bytes: Vec<u8>,
        writes: Vec<(VirtioMmioDeviceConfigAccess, MmioAccessBytes)>,
        read_error: Option<MmioHandlerError>,
        write_error: Option<MmioHandlerError>,
        short_read: bool,
    }

    impl TestDeviceConfig {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes,
                writes: Vec::new(),
                read_error: None,
                write_error: None,
                short_read: false,
            }
        }
    }

    impl VirtioMmioDeviceConfigHandler for TestDeviceConfig {
        fn read_device_config(
            &self,
            access: VirtioMmioDeviceConfigAccess,
        ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
            if let Some(source) = &self.read_error {
                return Err(source.clone().into());
            }

            let start = usize::try_from(access.offset())
                .map_err(|_| MmioHandlerError::new("test config offset does not fit usize"))?;
            let end = start
                .checked_add(access.len())
                .ok_or_else(|| MmioHandlerError::new("test config read range overflows"))?;
            let bytes = self
                .bytes
                .get(start..end)
                .ok_or_else(|| MmioHandlerError::new("test config read is outside data"))?;
            let returned_len = if self.short_read {
                bytes.len().saturating_sub(1)
            } else {
                bytes.len()
            };
            MmioAccessBytes::new(&bytes[..returned_len]).map_err(|source| {
                MmioHandlerError::new(format!("test config read bytes failed: {source}")).into()
            })
        }

        fn write_device_config(
            &mut self,
            access: VirtioMmioDeviceConfigAccess,
            data: MmioAccessBytes,
        ) -> Result<(), VirtioMmioDeviceConfigError> {
            if let Some(source) = &self.write_error {
                return Err(source.clone().into());
            }

            self.writes.push((access, data));
            Ok(())
        }
    }

    fn read_operation(offset: u64, len: u64) -> MmioOperation {
        let access = access(offset, len);
        MmioOperation::read(access).expect("read operation should be valid")
    }

    fn write_operation(offset: u64, bytes: &[u8]) -> MmioOperation {
        let access = access(
            offset,
            u64::try_from(bytes.len()).expect("test byte length should fit"),
        );
        let data = MmioAccessBytes::new(bytes).expect("write bytes should be valid");
        MmioOperation::write(access, data).expect("write operation should be valid")
    }

    fn read_register_u32<C: VirtioMmioDeviceConfigHandler>(
        handler: &VirtioMmioRegisterHandler<C>,
        offset: u64,
    ) -> Result<u32, VirtioMmioRegisterHandlerError> {
        let data = handler.read_access(access(offset, 4))?;
        let bytes: [u8; VIRTIO_MMIO_REGISTER_ACCESS_SIZE] = data
            .as_slice()
            .try_into()
            .expect("register read should return four bytes");
        Ok(u32::from_le_bytes(bytes))
    }

    fn write_register_u32<C: VirtioMmioDeviceConfigHandler>(
        handler: &mut VirtioMmioRegisterHandler<C>,
        offset: u64,
        value: u32,
    ) -> Result<(), VirtioMmioRegisterHandlerError> {
        let data =
            MmioAccessBytes::new(&value.to_le_bytes()).expect("register bytes should be valid");
        handler.write_access(access(offset, 4), data)
    }

    fn advance_handler_to_features_ok<C: VirtioMmioDeviceConfigHandler>(
        handler: &mut VirtioMmioRegisterHandler<C>,
    ) -> Result<(), VirtioMmioRegisterHandlerError> {
        advance_handler_to_driver(handler)?;
        write_register_u32(
            handler,
            VirtioMmioRegister::Status.offset(),
            QUEUE_CONFIG_STATUS,
        )
    }

    fn advance_handler_to_driver<C: VirtioMmioDeviceConfigHandler>(
        handler: &mut VirtioMmioRegisterHandler<C>,
    ) -> Result<(), VirtioMmioRegisterHandlerError> {
        write_register_u32(
            handler,
            VirtioMmioRegister::Status.offset(),
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        )?;
        write_register_u32(
            handler,
            VirtioMmioRegister::Status.offset(),
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        )
    }

    fn advance_handler_to_driver_ok<C: VirtioMmioDeviceConfigHandler>(
        handler: &mut VirtioMmioRegisterHandler<C>,
    ) -> Result<(), VirtioMmioRegisterHandlerError> {
        advance_handler_to_features_ok(handler)?;
        write_register_u32(
            handler,
            VirtioMmioRegister::Status.offset(),
            DRIVER_OK_STATUS,
        )
    }

    fn access(offset: u64, len: u64) -> crate::mmio::MmioAccess {
        let mut bus = MmioBus::new();
        bus.insert(
            MmioRegionId::new(7),
            GuestAddress::new(BASE),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE * 2,
        )
        .expect("test region should insert");
        bus.lookup(GuestAddress::new(BASE + offset), len)
            .expect("test access should resolve")
    }

    fn decode(operation: &MmioOperation) -> VirtioMmioAccess {
        decode_virtio_mmio_access(operation).expect("virtio-mmio access should decode")
    }

    fn advance_to_driver_status(registers: &mut VirtioMmioDeviceRegisters) {
        registers
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("ACKNOWLEDGE status transition should succeed");
        registers
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("DRIVER status transition should succeed");
    }

    fn advance_to_features_ok_status(registers: &mut VirtioMmioDeviceRegisters) {
        advance_to_driver_status(registers);
        registers
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                    | VIRTIO_DEVICE_STATUS_DRIVER
                    | VIRTIO_DEVICE_STATUS_FEATURES_OK,
            )
            .expect("FEATURES_OK status transition should succeed");
    }

    #[test]
    fn exposes_firecracker_compatible_constants() {
        assert_eq!(VIRTIO_MMIO_DEVICE_WINDOW_SIZE, 0x1000);
        assert_eq!(VIRTIO_MMIO_REGISTER_SPACE_SIZE, 0x100);
        assert_eq!(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, 0x100);
        assert_eq!(VIRTIO_MMIO_NOTIFY_OFFSET, 0x50);
        assert_eq!(VIRTIO_MMIO_MAGIC_VALUE, 0x7472_6976);
        assert_eq!(VIRTIO_MMIO_VERSION, 2);
        assert_eq!(VIRTIO_MMIO_VENDOR_ID, 0);
        assert_eq!(VIRTIO_MMIO_REGISTER_ACCESS_SIZE, 4);
        assert_eq!(VIRTIO_MMIO_FEATURE_VERSION_1, 32);
        assert_eq!(VIRTIO_MMIO_VERSION_1_FEATURE, 1_u64 << 32);
    }

    #[test]
    fn device_registers_read_identity_and_initial_state() {
        let registers =
            VirtioMmioDeviceRegisters::with_vendor_id_and_config_generation(7, 0x1234, 0x2a, 9);

        assert_eq!(registers.device_id(), 7);
        assert_eq!(registers.vendor_id(), 0x1234);
        assert_eq!(
            registers.device_features(),
            VIRTIO_MMIO_VERSION_1_FEATURE | 0x2a
        );
        assert_eq!(registers.config_generation(), 9);
        assert_eq!(registers.device_features_select(), 0);
        assert_eq!(registers.driver_features_select(), 0);
        assert_eq!(registers.driver_features(), 0);
        assert_eq!(registers.status(), VIRTIO_DEVICE_STATUS_INIT);
        assert_eq!(
            registers.read_register(VirtioMmioRegister::MagicValue),
            Ok(VIRTIO_MMIO_MAGIC_VALUE)
        );
        assert_eq!(
            registers.read_register(VirtioMmioRegister::Version),
            Ok(VIRTIO_MMIO_VERSION)
        );
        assert_eq!(registers.read_register(VirtioMmioRegister::DeviceId), Ok(7));
        assert_eq!(
            registers.read_register(VirtioMmioRegister::VendorId),
            Ok(0x1234)
        );
        assert_eq!(
            registers.read_register(VirtioMmioRegister::DeviceFeatures),
            Ok(0x2a)
        );
        assert_eq!(
            registers.read_register(VirtioMmioRegister::Status),
            Ok(VIRTIO_DEVICE_STATUS_INIT)
        );
        assert_eq!(
            registers.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(9)
        );
    }

    #[test]
    fn device_registers_select_and_read_feature_pages() {
        let mut registers = VirtioMmioDeviceRegisters::new(7, 0x0000_0004_0000_002a);

        assert_eq!(
            registers.read_register(VirtioMmioRegister::DeviceFeatures),
            Ok(0x2a)
        );

        registers
            .write_register(VirtioMmioRegister::DeviceFeaturesSel, 1)
            .expect("feature selector page 1 should be valid");

        assert_eq!(registers.device_features_select(), 1);
        assert_eq!(
            registers.read_register(VirtioMmioRegister::DeviceFeatures),
            Ok(0x5)
        );

        let err = registers
            .write_register(VirtioMmioRegister::DeviceFeaturesSel, 2)
            .expect_err("unsupported device feature selector should fail");
        assert_eq!(
            err,
            VirtioMmioRegisterStateError::UnsupportedFeaturePage { selector: 2 }
        );
        assert_eq!(registers.device_features_select(), 1);
    }

    #[test]
    fn device_registers_accept_supported_driver_features_in_driver_state() {
        let mut registers = VirtioMmioDeviceRegisters::new(7, 0x0000_0004_0000_002a);
        advance_to_driver_status(&mut registers);

        registers
            .write_register(VirtioMmioRegister::DriverFeatures, 0x2a)
            .expect("supported page 0 driver features should be accepted");
        assert_eq!(registers.driver_features(), 0x2a);

        registers
            .write_register(VirtioMmioRegister::DriverFeaturesSel, 1)
            .expect("driver feature selector page 1 should be valid");
        registers
            .write_register(VirtioMmioRegister::DriverFeatures, 0x5)
            .expect("supported page 1 driver features should be accepted");

        assert_eq!(registers.driver_features(), 0x2a | u64::from(0x5_u32) << 32);
    }

    #[test]
    fn device_registers_reject_driver_features_outside_driver_state() {
        let mut registers = VirtioMmioDeviceRegisters::new(7, 0x2a);

        let err = registers
            .write_register(VirtioMmioRegister::DriverFeatures, 0x2a)
            .expect_err("driver features should not be writable before DRIVER status");
        assert_eq!(
            err,
            VirtioMmioRegisterStateError::DriverFeaturesNotWritable {
                status: VIRTIO_DEVICE_STATUS_INIT,
            }
        );
        assert_eq!(registers.driver_features(), 0);

        advance_to_features_ok_status(&mut registers);
        let err = registers
            .write_register(VirtioMmioRegister::DriverFeatures, 0x2a)
            .expect_err("driver features should not be writable after FEATURES_OK");
        assert_eq!(
            err,
            VirtioMmioRegisterStateError::DriverFeaturesNotWritable {
                status: VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                    | VIRTIO_DEVICE_STATUS_DRIVER
                    | VIRTIO_DEVICE_STATUS_FEATURES_OK,
            }
        );
    }

    #[test]
    fn device_registers_reject_unsupported_driver_feature_pages_and_bits() {
        let mut registers = VirtioMmioDeviceRegisters::new(7, 0x2a);
        advance_to_driver_status(&mut registers);

        let err = registers
            .write_register(VirtioMmioRegister::DriverFeaturesSel, 3)
            .expect_err("unsupported driver feature selector should fail");
        assert_eq!(
            err,
            VirtioMmioRegisterStateError::UnsupportedFeaturePage { selector: 3 }
        );
        assert_eq!(registers.driver_features_select(), 0);

        let err = registers
            .write_register(VirtioMmioRegister::DriverFeatures, 0x80)
            .expect_err("unsupported driver feature bits should fail");
        assert_eq!(
            err,
            VirtioMmioRegisterStateError::UnsupportedDriverFeatures {
                selector: 0,
                requested: 0x80,
                supported: 0x2a,
                unsupported: 0x80,
            }
        );
        assert_eq!(registers.driver_features(), 0);
    }

    #[test]
    fn device_registers_follow_status_state_machine() {
        let mut registers = VirtioMmioDeviceRegisters::new(7, 0);

        registers
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("ACKNOWLEDGE transition should succeed");
        assert_eq!(registers.status(), VIRTIO_DEVICE_STATUS_ACKNOWLEDGE);

        registers
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("DRIVER transition should succeed");
        assert_eq!(
            registers.status(),
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER
        );

        registers
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                    | VIRTIO_DEVICE_STATUS_DRIVER
                    | VIRTIO_DEVICE_STATUS_FEATURES_OK,
            )
            .expect("FEATURES_OK transition should succeed");
        assert_eq!(
            registers.status(),
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                | VIRTIO_DEVICE_STATUS_DRIVER
                | VIRTIO_DEVICE_STATUS_FEATURES_OK
        );

        registers
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                    | VIRTIO_DEVICE_STATUS_DRIVER
                    | VIRTIO_DEVICE_STATUS_FEATURES_OK
                    | VIRTIO_DEVICE_STATUS_DRIVER_OK,
            )
            .expect("DRIVER_OK transition should succeed");
        assert_eq!(
            registers.status(),
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                | VIRTIO_DEVICE_STATUS_DRIVER
                | VIRTIO_DEVICE_STATUS_FEATURES_OK
                | VIRTIO_DEVICE_STATUS_DRIVER_OK
        );
    }

    #[test]
    fn device_registers_reject_invalid_status_transitions() {
        let mut registers = VirtioMmioDeviceRegisters::new(7, 0);

        let err = registers
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect_err("skipping ACKNOWLEDGE should fail");
        assert_eq!(
            err,
            VirtioMmioRegisterStateError::InvalidStatusTransition {
                current: VIRTIO_DEVICE_STATUS_INIT,
                requested: VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            }
        );
        assert_eq!(registers.status(), VIRTIO_DEVICE_STATUS_INIT);

        advance_to_features_ok_status(&mut registers);
        let err = registers
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect_err("clearing FEATURES_OK without reset should fail");
        assert_eq!(
            err,
            VirtioMmioRegisterStateError::InvalidStatusTransition {
                current: VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                    | VIRTIO_DEVICE_STATUS_DRIVER
                    | VIRTIO_DEVICE_STATUS_FEATURES_OK,
                requested: VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            }
        );
    }

    #[test]
    fn device_registers_or_failed_status_and_reset_to_init() {
        let mut registers =
            VirtioMmioDeviceRegisters::with_vendor_id_and_config_generation(7, 0x1234, 0x2a, 3);
        advance_to_driver_status(&mut registers);
        registers
            .write_register(VirtioMmioRegister::DriverFeatures, 0x2a)
            .expect("driver feature write should succeed before reset");
        registers
            .write_register(VirtioMmioRegister::DeviceFeaturesSel, 1)
            .expect("device feature selector should update before reset");

        registers
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_FAILED)
            .expect("FAILED should be ORed into status");
        assert_eq!(
            registers.status(),
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                | VIRTIO_DEVICE_STATUS_DRIVER
                | VIRTIO_DEVICE_STATUS_FAILED
        );

        registers
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_INIT)
            .expect("writing INIT should reset common transport state");
        assert_eq!(registers.status(), VIRTIO_DEVICE_STATUS_INIT);
        assert_eq!(registers.driver_features(), 0);
        assert_eq!(registers.device_features_select(), 0);
        assert_eq!(registers.driver_features_select(), 0);
        assert_eq!(registers.config_generation(), 3);
        assert_eq!(registers.vendor_id(), 0x1234);
    }

    #[test]
    fn device_registers_reject_out_of_scope_register_accesses() {
        let mut registers = VirtioMmioDeviceRegisters::new(7, 0);

        assert_eq!(
            registers.read_register(VirtioMmioRegister::QueueReady),
            Err(VirtioMmioRegisterStateError::UnsupportedRegisterRead {
                register: VirtioMmioRegister::QueueReady,
            })
        );
        assert_eq!(
            registers.write_register(VirtioMmioRegister::QueueNotify, 0),
            Err(VirtioMmioRegisterStateError::UnsupportedRegisterWrite {
                register: VirtioMmioRegister::QueueNotify,
            })
        );
        assert_eq!(
            registers.write_register(VirtioMmioRegister::MagicValue, 0),
            Err(VirtioMmioRegisterStateError::UnsupportedRegisterWrite {
                register: VirtioMmioRegister::MagicValue,
            })
        );
    }

    #[test]
    fn device_register_state_errors_display_and_preserve_sources() {
        let err = VirtioMmioRegisterStateError::UnsupportedFeaturePage { selector: 2 };
        assert_eq!(
            err.to_string(),
            "unsupported virtio-mmio feature selector page 2; supported pages are 0..=1"
        );
        assert!(err.source().is_none());

        let err = VirtioMmioRegisterStateError::InvalidStatusTransition {
            current: VIRTIO_DEVICE_STATUS_INIT,
            requested: VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        };
        assert_eq!(
            err.to_string(),
            "invalid virtio-mmio device status transition: 0x0 -> 0x3"
        );
    }

    #[test]
    fn interrupt_registers_read_empty_status() {
        let registers = VirtioMmioInterruptRegisters::new();

        assert!(registers.pending_status().is_empty());
        assert_eq!(
            registers
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status read should succeed"),
            0
        );
    }

    #[test]
    fn interrupt_registers_read_queue_and_config_pending_bits() {
        let mut registers = VirtioMmioInterruptRegisters::new();
        let mut expected = DeviceInterruptKind::Queue.status();
        expected.insert(DeviceInterruptKind::Config);

        registers.mark_pending(DeviceInterruptKind::Queue);
        registers.mark_pending(DeviceInterruptKind::Config);

        assert_eq!(registers.pending_status(), expected);
        assert_eq!(
            registers
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status read should succeed"),
            expected.bits()
        );
    }

    #[test]
    fn interrupt_ack_clears_selected_pending_bits() {
        let mut registers = VirtioMmioInterruptRegisters::new();

        registers.mark_pending(DeviceInterruptKind::Queue);
        registers.mark_pending(DeviceInterruptKind::Config);
        registers
            .write_register(
                VirtioMmioRegister::InterruptAck,
                DeviceInterruptKind::Queue.status().bits(),
                DRIVER_OK_STATUS,
            )
            .expect("queue interrupt ack should succeed");

        assert_eq!(
            registers.pending_status(),
            DeviceInterruptKind::Config.status()
        );

        registers
            .write_register(
                VirtioMmioRegister::InterruptAck,
                DeviceInterruptKind::Config.status().bits(),
                DRIVER_OK_STATUS,
            )
            .expect("config interrupt ack should succeed");

        assert!(registers.pending_status().is_empty());
    }

    #[test]
    fn interrupt_ack_allows_status_with_driver_ok_bit() {
        let mut registers = VirtioMmioInterruptRegisters::new();
        let failed_driver_ok_status = DRIVER_OK_STATUS | VIRTIO_DEVICE_STATUS_FAILED;

        registers.mark_pending(DeviceInterruptKind::Queue);
        registers
            .write_register(
                VirtioMmioRegister::InterruptAck,
                DeviceInterruptKind::Queue.status().bits(),
                failed_driver_ok_status,
            )
            .expect("interrupt ack should require only the DRIVER_OK bit");

        assert!(registers.pending_status().is_empty());
    }

    #[test]
    fn interrupt_ack_empty_mask_is_noop() {
        let mut registers = VirtioMmioInterruptRegisters::new();

        registers.mark_pending(DeviceInterruptKind::Config);
        registers
            .write_register(VirtioMmioRegister::InterruptAck, 0, DRIVER_OK_STATUS)
            .expect("empty interrupt ack should succeed");

        assert_eq!(
            registers.pending_status(),
            DeviceInterruptKind::Config.status()
        );
    }

    #[test]
    fn interrupt_ack_requires_driver_ok_status() {
        let mut registers = VirtioMmioInterruptRegisters::new();

        registers.mark_pending(DeviceInterruptKind::Queue);
        let err = registers
            .write_register(
                VirtioMmioRegister::InterruptAck,
                DeviceInterruptKind::Queue.status().bits(),
                QUEUE_CONFIG_STATUS,
            )
            .expect_err("interrupt ack before DRIVER_OK should fail");

        assert_eq!(
            err,
            VirtioMmioInterruptRegisterError::InterruptAckNotWritable {
                status: QUEUE_CONFIG_STATUS,
            }
        );
        assert_eq!(
            registers.pending_status(),
            DeviceInterruptKind::Queue.status()
        );
    }

    #[test]
    fn interrupt_ack_checks_status_before_ack_bits() {
        let mut registers = VirtioMmioInterruptRegisters::new();

        registers.mark_pending(DeviceInterruptKind::Queue);
        let err = registers
            .write_register(VirtioMmioRegister::InterruptAck, 0x5, QUEUE_CONFIG_STATUS)
            .expect_err("interrupt ack before DRIVER_OK should fail before parsing bits");

        assert_eq!(
            err,
            VirtioMmioInterruptRegisterError::InterruptAckNotWritable {
                status: QUEUE_CONFIG_STATUS,
            }
        );
        assert_eq!(
            registers.pending_status(),
            DeviceInterruptKind::Queue.status()
        );
    }

    #[test]
    fn interrupt_ack_rejects_unknown_bits_without_mutating_state() {
        let mut registers = VirtioMmioInterruptRegisters::new();
        let mut expected = DeviceInterruptKind::Queue.status();
        expected.insert(DeviceInterruptKind::Config);

        registers.mark_pending(DeviceInterruptKind::Queue);
        registers.mark_pending(DeviceInterruptKind::Config);
        let err = registers
            .write_register(VirtioMmioRegister::InterruptAck, 0x5, DRIVER_OK_STATUS)
            .expect_err("unknown interrupt ack bits should fail");

        assert_eq!(
            err,
            VirtioMmioInterruptRegisterError::InvalidInterruptAck {
                value: 0x5,
                source: DeviceInterruptStatusError::UnknownBits { bits: 0x4 },
            }
        );
        assert_eq!(registers.pending_status(), expected);
    }

    #[test]
    fn interrupt_registers_reset_pending_status() {
        let mut registers = VirtioMmioInterruptRegisters::new();

        registers.mark_pending(DeviceInterruptKind::Queue);
        registers.mark_pending(DeviceInterruptKind::Config);
        registers.reset();

        assert!(registers.pending_status().is_empty());
        assert_eq!(
            registers
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status read should succeed after reset"),
            0
        );
    }

    #[test]
    fn interrupt_registers_reject_unsupported_register_accesses() {
        let mut registers = VirtioMmioInterruptRegisters::new();

        assert_eq!(
            registers.read_register(VirtioMmioRegister::Status),
            Err(VirtioMmioInterruptRegisterError::UnsupportedRegisterRead {
                register: VirtioMmioRegister::Status,
            })
        );
        assert_eq!(
            registers.write_register(VirtioMmioRegister::InterruptStatus, 0, DRIVER_OK_STATUS),
            Err(VirtioMmioInterruptRegisterError::UnsupportedRegisterWrite {
                register: VirtioMmioRegister::InterruptStatus,
            })
        );
    }

    #[test]
    fn interrupt_register_errors_display_and_preserve_sources() {
        let err = VirtioMmioInterruptRegisterError::InterruptAckNotWritable {
            status: QUEUE_CONFIG_STATUS,
        };
        assert_eq!(
            err.to_string(),
            "virtio-mmio interrupt acknowledgement cannot be written while status is 0xb"
        );
        assert!(err.source().is_none());

        let err = VirtioMmioInterruptRegisterError::InvalidInterruptAck {
            value: 0x5,
            source: DeviceInterruptStatusError::UnknownBits { bits: 0x4 },
        };
        assert_eq!(
            err.to_string(),
            "virtio-mmio interrupt acknowledgement value 0x5 is invalid: unknown device interrupt status bits 0x4"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("unknown device interrupt status bits 0x4".to_string())
        );
    }

    #[test]
    fn queue_registers_initialize_and_validate_queue_table() {
        assert_eq!(
            VirtioMmioQueueRegisters::new(&[]),
            Err(VirtioMmioQueueRegisterError::EmptyQueueTable)
        );
        assert_eq!(
            VirtioMmioQueueRegisters::new(&[0]),
            Err(VirtioMmioQueueRegisterError::InvalidQueueMaxSize {
                queue_index: 0,
                max_size: 0,
            })
        );
        assert_eq!(
            VirtioMmioQueueRegisters::new(&[8, 3]),
            Err(VirtioMmioQueueRegisterError::InvalidQueueMaxSize {
                queue_index: 1,
                max_size: 3,
            })
        );

        let queues = VirtioMmioQueueRegisters::new(&[8, 16]).expect("queue table should build");

        assert_eq!(queues.queue_count(), 2);
        assert_eq!(queues.queue_select(), 0);
        let selected = queues
            .selected_queue()
            .expect("selected queue should exist");
        assert_eq!(selected.max_size(), 8);
        assert_eq!(selected.size(), 0);
        assert!(!selected.ready());
        assert_eq!(selected.descriptor_table(), GuestAddress::new(0));
        assert_eq!(selected.driver_ring(), GuestAddress::new(0));
        assert_eq!(selected.device_ring(), GuestAddress::new(0));
    }

    #[test]
    fn queue_registers_select_and_read_selected_queue() {
        let mut queues = VirtioMmioQueueRegisters::new(&[8, 16]).expect("queue table should build");

        assert_eq!(queues.read_register(VirtioMmioRegister::QueueNumMax), Ok(8));
        assert_eq!(queues.read_register(VirtioMmioRegister::QueueReady), Ok(0));

        queues
            .write_register(VirtioMmioRegister::QueueSel, 1, VIRTIO_DEVICE_STATUS_INIT)
            .expect("queue 1 should select");
        assert_eq!(queues.queue_select(), 1);
        assert_eq!(
            queues.read_register(VirtioMmioRegister::QueueNumMax),
            Ok(16)
        );

        let err = queues
            .write_register(VirtioMmioRegister::QueueSel, 2, VIRTIO_DEVICE_STATUS_INIT)
            .expect_err("out-of-range queue select should fail");
        assert_eq!(
            err,
            VirtioMmioQueueRegisterError::InvalidQueueIndex {
                queue_index: 2,
                queue_count: 2,
            }
        );
        assert_eq!(queues.queue_select(), 1);
    }

    #[test]
    fn queue_registers_gate_configuration_writes_on_status() {
        let mut queues = VirtioMmioQueueRegisters::new(&[8]).expect("queue table should build");

        let err = queues
            .write_register(VirtioMmioRegister::QueueNum, 8, VIRTIO_DEVICE_STATUS_INIT)
            .expect_err("queue size should not write before FEATURES_OK");
        assert_eq!(
            err,
            VirtioMmioQueueRegisterError::QueueConfigNotWritable {
                status: VIRTIO_DEVICE_STATUS_INIT,
            }
        );
        assert_eq!(
            queues.selected_queue().expect("queue should exist").size(),
            0
        );

        let driver_ok_status = QUEUE_CONFIG_STATUS | VIRTIO_DEVICE_STATUS_DRIVER_OK;
        let err = queues
            .write_register(VirtioMmioRegister::QueueReady, 1, driver_ok_status)
            .expect_err("queue ready should not write after DRIVER_OK");
        assert_eq!(
            err,
            VirtioMmioQueueRegisterError::QueueConfigNotWritable {
                status: driver_ok_status,
            }
        );
        assert!(!queues.selected_queue().expect("queue should exist").ready());
    }

    #[test]
    fn queue_registers_validate_queue_size_without_partial_mutation() {
        let mut queues = VirtioMmioQueueRegisters::new(&[8]).expect("queue table should build");

        queues
            .write_register(VirtioMmioRegister::QueueNum, 4, QUEUE_CONFIG_STATUS)
            .expect("valid queue size should write");
        assert_eq!(
            queues.selected_queue().expect("queue should exist").size(),
            4
        );

        for invalid_size in [0, 3, 16, 65_536] {
            let err = queues
                .write_register(
                    VirtioMmioRegister::QueueNum,
                    invalid_size,
                    QUEUE_CONFIG_STATUS,
                )
                .expect_err("invalid queue size should fail");
            assert_eq!(
                err,
                VirtioMmioQueueRegisterError::InvalidQueueSize {
                    queue_index: 0,
                    queue_size: invalid_size,
                    max_size: 8,
                }
            );
            assert_eq!(
                queues.selected_queue().expect("queue should exist").size(),
                4
            );
        }
    }

    #[test]
    fn queue_registers_validate_ready_values_without_partial_mutation() {
        let mut queues = VirtioMmioQueueRegisters::new(&[8]).expect("queue table should build");

        queues
            .write_register(VirtioMmioRegister::QueueReady, 1, QUEUE_CONFIG_STATUS)
            .expect("ready value 1 should write");
        assert!(queues.selected_queue().expect("queue should exist").ready());
        assert_eq!(queues.read_register(VirtioMmioRegister::QueueReady), Ok(1));

        let err = queues
            .write_register(VirtioMmioRegister::QueueReady, 2, QUEUE_CONFIG_STATUS)
            .expect_err("ready value outside 0/1 should fail");
        assert_eq!(
            err,
            VirtioMmioQueueRegisterError::InvalidQueueReadyValue {
                queue_index: 0,
                value: 2,
            }
        );
        assert!(queues.selected_queue().expect("queue should exist").ready());

        queues
            .write_register(VirtioMmioRegister::QueueReady, 0, QUEUE_CONFIG_STATUS)
            .expect("ready value 0 should write");
        assert!(!queues.selected_queue().expect("queue should exist").ready());
    }

    #[test]
    fn queue_registers_compose_address_halves_and_validate_alignment() {
        let mut queues = VirtioMmioQueueRegisters::new(&[8]).expect("queue table should build");

        queues
            .write_register(
                VirtioMmioRegister::QueueDescLow,
                0x1000,
                QUEUE_CONFIG_STATUS,
            )
            .expect("aligned descriptor table low address should write");
        queues
            .write_register(VirtioMmioRegister::QueueDescHigh, 1, QUEUE_CONFIG_STATUS)
            .expect("descriptor table high address should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDriverLow,
                0x2002,
                QUEUE_CONFIG_STATUS,
            )
            .expect("aligned driver ring address should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDeviceLow,
                0x3004,
                QUEUE_CONFIG_STATUS,
            )
            .expect("aligned device ring address should write");

        let queue = queues.selected_queue().expect("queue should exist");
        assert_eq!(queue.descriptor_table(), GuestAddress::new(0x1_0000_1000));
        assert_eq!(queue.driver_ring(), GuestAddress::new(0x2002));
        assert_eq!(queue.device_ring(), GuestAddress::new(0x3004));

        let err = queues
            .write_register(
                VirtioMmioRegister::QueueDescLow,
                0x1001,
                QUEUE_CONFIG_STATUS,
            )
            .expect_err("unaligned descriptor table should fail");
        assert_eq!(
            err,
            VirtioMmioQueueRegisterError::UnalignedQueueAddress {
                queue_index: 0,
                register: VirtioMmioRegister::QueueDescLow,
                address: GuestAddress::new(0x1_0000_1001),
                alignment: 16,
            }
        );
        assert_eq!(
            queues
                .selected_queue()
                .expect("queue should exist")
                .descriptor_table(),
            GuestAddress::new(0x1_0000_1000)
        );

        let err = queues
            .write_register(
                VirtioMmioRegister::QueueDriverLow,
                0x2001,
                QUEUE_CONFIG_STATUS,
            )
            .expect_err("unaligned driver ring should fail");
        assert_eq!(
            err,
            VirtioMmioQueueRegisterError::UnalignedQueueAddress {
                queue_index: 0,
                register: VirtioMmioRegister::QueueDriverLow,
                address: GuestAddress::new(0x2001),
                alignment: 2,
            }
        );
        assert_eq!(
            queues
                .selected_queue()
                .expect("queue should exist")
                .driver_ring(),
            GuestAddress::new(0x2002)
        );

        let err = queues
            .write_register(
                VirtioMmioRegister::QueueDeviceLow,
                0x3002,
                QUEUE_CONFIG_STATUS,
            )
            .expect_err("unaligned device ring should fail");
        assert_eq!(
            err,
            VirtioMmioQueueRegisterError::UnalignedQueueAddress {
                queue_index: 0,
                register: VirtioMmioRegister::QueueDeviceLow,
                address: GuestAddress::new(0x3002),
                alignment: 4,
            }
        );
        assert_eq!(
            queues
                .selected_queue()
                .expect("queue should exist")
                .device_ring(),
            GuestAddress::new(0x3004)
        );
    }

    #[test]
    fn queue_registers_reset_selected_queue_and_preserve_max_sizes() {
        let mut queues = VirtioMmioQueueRegisters::new(&[8, 16]).expect("queue table should build");
        queues
            .write_register(VirtioMmioRegister::QueueSel, 1, VIRTIO_DEVICE_STATUS_INIT)
            .expect("queue 1 should select");
        queues
            .write_register(VirtioMmioRegister::QueueNum, 16, QUEUE_CONFIG_STATUS)
            .expect("queue size should write");
        queues
            .write_register(VirtioMmioRegister::QueueReady, 1, QUEUE_CONFIG_STATUS)
            .expect("queue ready should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDeviceLow,
                0x4000,
                QUEUE_CONFIG_STATUS,
            )
            .expect("device ring address should write");

        queues.reset();

        assert_eq!(queues.queue_select(), 0);
        assert_eq!(queues.queue(0).expect("queue 0 should exist").max_size(), 8);
        let queue_1 = queues.queue(1).expect("queue 1 should exist");
        assert_eq!(queue_1.max_size(), 16);
        assert_eq!(queue_1.size(), 0);
        assert!(!queue_1.ready());
        assert_eq!(queue_1.device_ring(), GuestAddress::new(0));
    }

    #[test]
    fn queue_registers_reject_unsupported_register_accesses() {
        let mut queues = VirtioMmioQueueRegisters::new(&[8]).expect("queue table should build");

        assert_eq!(
            queues.read_register(VirtioMmioRegister::Status),
            Err(VirtioMmioQueueRegisterError::UnsupportedRegisterRead {
                register: VirtioMmioRegister::Status,
            })
        );
        assert_eq!(
            queues.write_register(
                VirtioMmioRegister::QueueNotify,
                0,
                VIRTIO_DEVICE_STATUS_INIT,
            ),
            Err(VirtioMmioQueueRegisterError::UnsupportedRegisterWrite {
                register: VirtioMmioRegister::QueueNotify,
            })
        );
        assert_eq!(
            queues.write_register(
                VirtioMmioRegister::QueueNumMax,
                0,
                VIRTIO_DEVICE_STATUS_INIT,
            ),
            Err(VirtioMmioQueueRegisterError::UnsupportedRegisterWrite {
                register: VirtioMmioRegister::QueueNumMax,
            })
        );
    }

    #[test]
    fn queue_register_errors_display_and_preserve_sources() {
        let err = VirtioMmioQueueRegisterError::InvalidQueueSize {
            queue_index: 1,
            queue_size: 12,
            max_size: 8,
        };
        assert_eq!(
            err.to_string(),
            "virtio-mmio queue 1 size 12 must be a nonzero power of two not exceeding max size 8"
        );
        assert!(err.source().is_none());

        let err = VirtioMmioQueueRegisterError::UnalignedQueueAddress {
            queue_index: 0,
            register: VirtioMmioRegister::QueueDriverLow,
            address: GuestAddress::new(0x1001),
            alignment: 2,
        };
        assert_eq!(
            err.to_string(),
            "virtio-mmio queue 0 QueueDriverLow address 0x1001 is not aligned to 2 bytes"
        );
    }

    #[test]
    fn queue_notification_registers_initialize_and_validate_queue_count() {
        assert_eq!(
            VirtioMmioQueueNotificationRegisters::new(0),
            Err(VirtioMmioQueueNotificationError::EmptyQueueTable)
        );

        let notifications =
            VirtioMmioQueueNotificationRegisters::new(2).expect("notifications should build");
        assert_eq!(notifications.queue_count(), 2);
        assert!(notifications.pending_queue_notifications().is_empty());
        assert_eq!(notifications.is_queue_notification_pending(0), Ok(false));
        assert_eq!(notifications.is_queue_notification_pending(1), Ok(false));
        assert_eq!(
            notifications.is_queue_notification_pending(2),
            Err(VirtioMmioQueueNotificationError::InvalidQueueIndex {
                queue_index: 2,
                queue_count: 2,
            })
        );
    }

    #[test]
    fn queue_notify_records_and_drains_pending_notifications() {
        let mut notifications =
            VirtioMmioQueueNotificationRegisters::new(3).expect("notifications should build");

        notifications
            .write_register(VirtioMmioRegister::QueueNotify, 2, DRIVER_OK_STATUS)
            .expect("queue notification should write after DRIVER_OK");
        notifications
            .write_register(VirtioMmioRegister::QueueNotify, 0, DRIVER_OK_STATUS)
            .expect("queue notification should write after DRIVER_OK");

        assert_eq!(notifications.is_queue_notification_pending(0), Ok(true));
        assert_eq!(notifications.is_queue_notification_pending(1), Ok(false));
        assert_eq!(notifications.is_queue_notification_pending(2), Ok(true));
        assert_eq!(notifications.pending_queue_notifications(), vec![0, 2]);
        assert_eq!(notifications.take_pending_queue_notifications(), vec![0, 2]);
        assert!(notifications.pending_queue_notifications().is_empty());
    }

    #[test]
    fn queue_notify_coalesces_duplicate_pending_notifications() {
        let mut notifications =
            VirtioMmioQueueNotificationRegisters::new(1).expect("notifications should build");

        notifications
            .write_register(VirtioMmioRegister::QueueNotify, 0, DRIVER_OK_STATUS)
            .expect("first queue notification should write");
        notifications
            .write_register(VirtioMmioRegister::QueueNotify, 0, DRIVER_OK_STATUS)
            .expect("duplicate queue notification should write");

        assert_eq!(notifications.pending_queue_notifications(), vec![0]);
    }

    #[test]
    fn queue_notify_allows_status_with_driver_ok_bit() {
        let mut notifications =
            VirtioMmioQueueNotificationRegisters::new(1).expect("notifications should build");
        let failed_driver_ok_status = DRIVER_OK_STATUS | VIRTIO_DEVICE_STATUS_FAILED;

        notifications
            .write_register(VirtioMmioRegister::QueueNotify, 0, failed_driver_ok_status)
            .expect("queue notification should require only the DRIVER_OK bit");

        assert_eq!(notifications.pending_queue_notifications(), vec![0]);
    }

    #[test]
    fn queue_notify_requires_driver_ok_status_before_queue_index_check() {
        let mut notifications =
            VirtioMmioQueueNotificationRegisters::new(1).expect("notifications should build");

        let err = notifications
            .write_register(VirtioMmioRegister::QueueNotify, 2, QUEUE_CONFIG_STATUS)
            .expect_err("queue notification before DRIVER_OK should fail before index check");

        assert_eq!(
            err,
            VirtioMmioQueueNotificationError::QueueNotifyNotWritable {
                status: QUEUE_CONFIG_STATUS,
            }
        );
        assert!(notifications.pending_queue_notifications().is_empty());
    }

    #[test]
    fn queue_notify_rejects_invalid_queue_index_without_mutation() {
        let mut notifications =
            VirtioMmioQueueNotificationRegisters::new(2).expect("notifications should build");
        notifications
            .write_register(VirtioMmioRegister::QueueNotify, 1, DRIVER_OK_STATUS)
            .expect("valid queue notification should write");

        let err = notifications
            .write_register(VirtioMmioRegister::QueueNotify, 2, DRIVER_OK_STATUS)
            .expect_err("out-of-range queue notification should fail");

        assert_eq!(
            err,
            VirtioMmioQueueNotificationError::InvalidQueueIndex {
                queue_index: 2,
                queue_count: 2,
            }
        );
        assert_eq!(notifications.pending_queue_notifications(), vec![1]);
    }

    #[test]
    fn queue_notification_registers_reset_pending_notifications() {
        let mut notifications =
            VirtioMmioQueueNotificationRegisters::new(2).expect("notifications should build");
        notifications
            .write_register(VirtioMmioRegister::QueueNotify, 0, DRIVER_OK_STATUS)
            .expect("queue notification should write");
        notifications
            .write_register(VirtioMmioRegister::QueueNotify, 1, DRIVER_OK_STATUS)
            .expect("queue notification should write");

        notifications.reset();

        assert!(notifications.pending_queue_notifications().is_empty());
        assert_eq!(notifications.is_queue_notification_pending(0), Ok(false));
    }

    #[test]
    fn queue_notification_registers_reject_unsupported_register_accesses() {
        let mut notifications =
            VirtioMmioQueueNotificationRegisters::new(1).expect("notifications should build");

        assert_eq!(
            notifications.read_register(VirtioMmioRegister::QueueNotify),
            Err(VirtioMmioQueueNotificationError::UnsupportedRegisterRead {
                register: VirtioMmioRegister::QueueNotify,
            })
        );
        assert_eq!(
            notifications.write_register(VirtioMmioRegister::Status, 0, DRIVER_OK_STATUS),
            Err(VirtioMmioQueueNotificationError::UnsupportedRegisterWrite {
                register: VirtioMmioRegister::Status,
            })
        );
    }

    #[test]
    fn queue_notification_errors_display_and_preserve_sources() {
        let err = VirtioMmioQueueNotificationError::InvalidQueueIndex {
            queue_index: 4,
            queue_count: 2,
        };
        assert_eq!(
            err.to_string(),
            "virtio-mmio queue notification index 4 is outside queue table size 2"
        );
        assert!(err.source().is_none());

        let err = VirtioMmioQueueNotificationError::QueueNotifyNotWritable {
            status: QUEUE_CONFIG_STATUS,
        };
        assert_eq!(
            err.to_string(),
            "virtio-mmio queue notification cannot be written while status is 0xb"
        );
    }

    #[test]
    fn register_handler_reads_common_queue_and_interrupt_registers() {
        let mut handler =
            VirtioMmioRegisterHandler::new(7, 0x2a, &[8]).expect("handler should build");

        assert_eq!(
            read_register_u32(&handler, VirtioMmioRegister::MagicValue.offset()),
            Ok(VIRTIO_MMIO_MAGIC_VALUE)
        );
        assert_eq!(
            read_register_u32(&handler, VirtioMmioRegister::Version.offset()),
            Ok(VIRTIO_MMIO_VERSION)
        );
        assert_eq!(
            read_register_u32(&handler, VirtioMmioRegister::DeviceId.offset()),
            Ok(7)
        );
        assert_eq!(
            read_register_u32(&handler, VirtioMmioRegister::QueueNumMax.offset()),
            Ok(8)
        );

        handler.mark_interrupt_pending(DeviceInterruptKind::Queue);
        assert_eq!(
            read_register_u32(&handler, VirtioMmioRegister::InterruptStatus.offset()),
            Ok(DeviceInterruptKind::Queue.status().bits())
        );
    }

    #[test]
    fn register_handler_routes_common_and_queue_writes() {
        let mut handler =
            VirtioMmioRegisterHandler::new(7, 0x2a, &[8]).expect("handler should build");

        write_register_u32(
            &mut handler,
            VirtioMmioRegister::Status.offset(),
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        )
        .expect("ACKNOWLEDGE status should write");
        write_register_u32(
            &mut handler,
            VirtioMmioRegister::Status.offset(),
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        )
        .expect("DRIVER status should write");
        write_register_u32(
            &mut handler,
            VirtioMmioRegister::DriverFeatures.offset(),
            0x2a,
        )
        .expect("driver features should write");
        write_register_u32(
            &mut handler,
            VirtioMmioRegister::Status.offset(),
            QUEUE_CONFIG_STATUS,
        )
        .expect("FEATURES_OK status should write");
        write_register_u32(&mut handler, VirtioMmioRegister::QueueNum.offset(), 8)
            .expect("queue size should write");
        write_register_u32(&mut handler, VirtioMmioRegister::QueueReady.offset(), 1)
            .expect("queue ready should write");

        assert_eq!(handler.device_registers().driver_features(), 0x2a);
        let queue = handler
            .queue_registers()
            .selected_queue()
            .expect("selected queue should exist");
        assert_eq!(queue.size(), 8);
        assert!(queue.ready());
        assert_eq!(
            read_register_u32(&handler, VirtioMmioRegister::QueueReady.offset()),
            Ok(1)
        );
    }

    #[test]
    fn register_handler_routes_queue_notifications_and_interrupt_acks() {
        let mut handler =
            VirtioMmioRegisterHandler::new(7, 0x2a, &[8]).expect("handler should build");
        advance_handler_to_driver_ok(&mut handler).expect("handler should reach DRIVER_OK");

        write_register_u32(&mut handler, VirtioMmioRegister::QueueNotify.offset(), 0)
            .expect("queue notify should write");
        assert_eq!(
            handler
                .queue_notification_registers()
                .pending_queue_notifications(),
            vec![0]
        );

        handler.mark_interrupt_pending(DeviceInterruptKind::Queue);
        assert_eq!(
            read_register_u32(&handler, VirtioMmioRegister::InterruptStatus.offset()),
            Ok(DeviceInterruptKind::Queue.status().bits())
        );
        write_register_u32(
            &mut handler,
            VirtioMmioRegister::InterruptAck.offset(),
            DeviceInterruptKind::Queue.status().bits(),
        )
        .expect("interrupt ack should write");
        assert_eq!(
            read_register_u32(&handler, VirtioMmioRegister::InterruptStatus.offset()),
            Ok(0)
        );
    }

    #[test]
    fn register_handler_drains_queue_notifications() {
        let mut handler =
            VirtioMmioRegisterHandler::new(7, 0x2a, &[8, 16]).expect("handler should build");
        advance_handler_to_driver_ok(&mut handler).expect("handler should reach DRIVER_OK");

        assert_eq!(handler.is_queue_notification_pending(1), Ok(false));
        assert!(handler.pending_queue_notifications().is_empty());

        write_register_u32(&mut handler, VirtioMmioRegister::QueueNotify.offset(), 1)
            .expect("queue notify should write");

        assert_eq!(handler.is_queue_notification_pending(1), Ok(true));
        assert_eq!(handler.pending_queue_notifications(), vec![1]);
        assert_eq!(handler.take_pending_queue_notifications(), vec![1]);
        assert_eq!(handler.is_queue_notification_pending(1), Ok(false));
        assert!(handler.take_pending_queue_notifications().is_empty());
    }

    #[test]
    fn register_handler_queue_notification_drain_rejects_invalid_queue_index() {
        let handler =
            VirtioMmioRegisterHandler::new(7, 0x2a, &[8, 16]).expect("handler should build");

        assert_eq!(
            handler.is_queue_notification_pending(2),
            Err(VirtioMmioQueueNotificationError::InvalidQueueIndex {
                queue_index: 2,
                queue_count: 2,
            })
        );
    }

    #[test]
    fn register_handler_status_reset_clears_composed_substates() {
        let mut handler =
            VirtioMmioRegisterHandler::new(7, 0x2a, &[8]).expect("handler should build");
        advance_handler_to_features_ok(&mut handler).expect("handler should reach FEATURES_OK");
        write_register_u32(&mut handler, VirtioMmioRegister::QueueNum.offset(), 8)
            .expect("queue size should write");
        write_register_u32(&mut handler, VirtioMmioRegister::QueueReady.offset(), 1)
            .expect("queue ready should write");
        write_register_u32(
            &mut handler,
            VirtioMmioRegister::Status.offset(),
            DRIVER_OK_STATUS,
        )
        .expect("handler should reach DRIVER_OK");
        write_register_u32(&mut handler, VirtioMmioRegister::QueueNotify.offset(), 0)
            .expect("queue notify should write");
        handler.mark_interrupt_pending(DeviceInterruptKind::Queue);

        write_register_u32(
            &mut handler,
            VirtioMmioRegister::Status.offset(),
            VIRTIO_DEVICE_STATUS_INIT,
        )
        .expect("INIT status should reset composed state");

        assert_eq!(
            handler.device_registers().status(),
            VIRTIO_DEVICE_STATUS_INIT
        );
        let queue = handler
            .queue_registers()
            .selected_queue()
            .expect("selected queue should exist after reset");
        assert_eq!(queue.size(), 0);
        assert!(!queue.ready());
        assert!(handler.pending_queue_notifications().is_empty());
        assert!(handler.interrupt_registers().pending_status().is_empty());
    }

    #[test]
    fn register_handler_explicit_reset_clears_pending_queue_notifications() {
        let mut handler =
            VirtioMmioRegisterHandler::new(7, 0x2a, &[8]).expect("handler should build");
        advance_handler_to_driver_ok(&mut handler).expect("handler should reach DRIVER_OK");
        write_register_u32(&mut handler, VirtioMmioRegister::QueueNotify.offset(), 0)
            .expect("queue notify should write");

        handler.reset();

        assert!(handler.pending_queue_notifications().is_empty());
        assert_eq!(handler.is_queue_notification_pending(0), Ok(false));
    }

    #[test]
    fn register_handler_delegates_device_config_reads_and_writes() {
        let config = TestDeviceConfig::new(vec![0x11, 0x22, 0x33, 0x44, 0x55]);
        let mut handler = VirtioMmioRegisterHandler::with_device_config(7, 0x2a, &[8], config)
            .expect("handler should build");

        let read = handler
            .read_access(access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + 1, 2))
            .expect("device config read should delegate");
        assert_eq!(read.as_slice(), &[0x22, 0x33]);

        advance_handler_to_features_ok(&mut handler).expect("handler should reach FEATURES_OK");
        let write_data = MmioAccessBytes::new(&[0xaa, 0xbb]).expect("write bytes should build");
        handler
            .write_access(access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + 3, 2), write_data)
            .expect("device config write should delegate");

        let writes = &handler.device_config_handler().writes;
        assert_eq!(writes.len(), 1);
        let (write_access, written_data) = writes.first().expect("write should be recorded");
        assert_eq!(write_access.kind(), MmioOperationKind::Write);
        assert_eq!(write_access.offset(), 3);
        assert_eq!(
            write_access.absolute_offset(),
            VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + 3
        );
        assert_eq!(write_access.len(), 2);
        assert_eq!(*written_data, write_data);
    }

    #[test]
    fn register_handler_rejects_device_config_write_before_driver_status() {
        let config = TestDeviceConfig::new(vec![0; 4]);
        let mut handler = VirtioMmioRegisterHandler::with_device_config(7, 0x2a, &[8], config)
            .expect("handler should build");
        let write_data = MmioAccessBytes::new(&[0xaa]).expect("write bytes should build");

        assert_eq!(
            handler.write_access(access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, 1), write_data),
            Err(
                VirtioMmioRegisterHandlerError::DeviceConfigWriteNotWritable {
                    status: VIRTIO_DEVICE_STATUS_INIT,
                }
            )
        );
        assert!(handler.device_config_handler().writes.is_empty());
    }

    #[test]
    fn register_handler_allows_device_config_write_after_driver_status() {
        let config = TestDeviceConfig::new(vec![0; 4]);
        let mut handler = VirtioMmioRegisterHandler::with_device_config(7, 0x2a, &[8], config)
            .expect("handler should build");
        advance_handler_to_driver(&mut handler).expect("handler should reach DRIVER");
        let write_data = MmioAccessBytes::new(&[0xaa]).expect("write bytes should build");

        handler
            .write_access(access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, 1), write_data)
            .expect("device config write should delegate after DRIVER");

        assert_eq!(handler.device_config_handler().writes.len(), 1);
    }

    #[test]
    fn register_handler_rejects_device_config_write_after_failed_status() {
        let config = TestDeviceConfig::new(vec![0; 4]);
        let mut handler = VirtioMmioRegisterHandler::with_device_config(7, 0x2a, &[8], config)
            .expect("handler should build");
        advance_handler_to_driver(&mut handler).expect("handler should reach DRIVER");
        write_register_u32(
            &mut handler,
            VirtioMmioRegister::Status.offset(),
            VIRTIO_DEVICE_STATUS_FAILED,
        )
        .expect("FAILED status should write");
        let write_data = MmioAccessBytes::new(&[0xaa]).expect("write bytes should build");

        assert_eq!(
            handler.write_access(access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, 1), write_data),
            Err(
                VirtioMmioRegisterHandlerError::DeviceConfigWriteNotWritable {
                    status: VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                        | VIRTIO_DEVICE_STATUS_DRIVER
                        | VIRTIO_DEVICE_STATUS_FAILED,
                }
            )
        );
        assert!(handler.device_config_handler().writes.is_empty());
    }

    #[test]
    fn register_handler_propagates_device_config_handler_errors() {
        let read_source = MmioHandlerError::new("read failed");
        let write_source = MmioHandlerError::new("write failed");
        let mut config = TestDeviceConfig::new(vec![0; 8]);
        config.read_error = Some(read_source.clone());
        config.write_error = Some(write_source.clone());
        let mut handler = VirtioMmioRegisterHandler::with_device_config(7, 0x2a, &[8], config)
            .expect("handler should build");

        let read_err = handler
            .read_access(access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + 2, 2))
            .expect_err("device config read failure should propagate");
        assert_eq!(
            read_err,
            VirtioMmioRegisterHandlerError::DeviceConfigRead {
                offset: 2,
                len: 2,
                source: VirtioMmioDeviceConfigError::Handler {
                    source: read_source.clone(),
                },
            }
        );
        assert_eq!(
            read_err.source().map(ToString::to_string),
            Some("virtio-mmio device config handler failed: read failed".to_string())
        );
        assert_eq!(
            read_err
                .source()
                .and_then(std::error::Error::source)
                .map(ToString::to_string),
            Some(read_source.to_string())
        );

        advance_handler_to_features_ok(&mut handler).expect("handler should reach FEATURES_OK");
        let write_data = MmioAccessBytes::new(&[0xaa]).expect("write bytes should build");
        assert_eq!(
            handler.write_access(access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + 4, 1), write_data),
            Err(VirtioMmioRegisterHandlerError::DeviceConfigWrite {
                offset: 4,
                len: 1,
                source: VirtioMmioDeviceConfigError::Handler {
                    source: write_source,
                },
            })
        );
    }

    #[test]
    fn register_handler_rejects_mismatched_device_config_read_length() {
        let mut config = TestDeviceConfig::new(vec![0x11, 0x22]);
        config.short_read = true;
        let handler = VirtioMmioRegisterHandler::with_device_config(7, 0x2a, &[8], config)
            .expect("handler should build");

        assert_eq!(
            handler.read_access(access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, 2)),
            Err(VirtioMmioRegisterHandlerError::DeviceConfigReadDataLength {
                offset: 0,
                expected: 2,
                actual: 1,
            })
        );
    }

    #[test]
    fn register_handler_rejects_device_config_and_unsupported_register_accesses() {
        let mut handler =
            VirtioMmioRegisterHandler::new(7, 0x2a, &[8]).expect("handler should build");

        assert_eq!(
            handler.read_access(access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, 4)),
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 0, len: 4 })
        );
        assert_eq!(
            write_register_u32(&mut handler, VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, 0),
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigWrite { offset: 0, len: 4 })
        );
        assert_eq!(
            handler.read_register(VirtioMmioRegister::DriverFeatures),
            Err(VirtioMmioRegisterHandlerError::UnsupportedRegisterRead {
                register: VirtioMmioRegister::DriverFeatures,
            })
        );
        assert_eq!(
            handler.write_register(VirtioMmioRegister::MagicValue, 0),
            Err(VirtioMmioRegisterHandlerError::UnsupportedRegisterWrite {
                register: VirtioMmioRegister::MagicValue,
            })
        );
    }

    #[test]
    fn register_handler_invalid_write_preserves_substate() {
        let mut handler =
            VirtioMmioRegisterHandler::new(7, 0x2a, &[8]).expect("handler should build");
        advance_handler_to_features_ok(&mut handler).expect("handler should reach FEATURES_OK");
        write_register_u32(&mut handler, VirtioMmioRegister::QueueNum.offset(), 4)
            .expect("valid queue size should write");

        let err = write_register_u32(&mut handler, VirtioMmioRegister::QueueNum.offset(), 16)
            .expect_err("oversized queue should fail");

        assert_eq!(
            err,
            VirtioMmioRegisterHandlerError::QueueRegisterWrite {
                register: VirtioMmioRegister::QueueNum,
                source: VirtioMmioQueueRegisterError::InvalidQueueSize {
                    queue_index: 0,
                    queue_size: 16,
                    max_size: 8,
                },
            }
        );
        assert_eq!(
            handler
                .queue_registers()
                .selected_queue()
                .expect("queue should exist")
                .size(),
            4
        );
    }

    #[test]
    fn register_handler_implements_mmio_handler_for_dispatcher() {
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(
                MmioRegionId::new(3),
                GuestAddress::new(BASE),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("virtio-mmio region should insert");
        dispatcher
            .register_handler(
                MmioRegionId::new(3),
                VirtioMmioRegisterHandler::new(7, 0x2a, &[8]).expect("handler should build"),
            )
            .expect("handler should register");
        let access = dispatcher
            .lookup(
                GuestAddress::new(BASE + VirtioMmioRegister::MagicValue.offset()),
                4,
            )
            .expect("register access should resolve");

        let outcome = dispatcher
            .dispatch(MmioOperation::read(access).expect("read operation should be valid"))
            .expect("dispatcher should route read to handler");

        assert_eq!(
            outcome,
            MmioDispatchOutcome::Read {
                data: MmioAccessBytes::new(&VIRTIO_MMIO_MAGIC_VALUE.to_le_bytes())
                    .expect("magic read bytes should be valid"),
            }
        );
    }

    #[test]
    fn register_handler_errors_display_and_preserve_sources() {
        let handler = VirtioMmioRegisterHandler::new(7, 0x2a, &[8]).expect("handler should build");
        let err = handler
            .read_access(access(0x18, 4))
            .expect_err("reserved read offset should fail");

        assert_eq!(
            err.to_string(),
            "failed to decode virtio-mmio MMIO access: unsupported virtio-mmio read register offset 0x18"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("unsupported virtio-mmio read register offset 0x18".to_string())
        );

        let err = VirtioMmioRegisterHandler::new(7, 0x2a, &[])
            .expect_err("empty queue table should fail handler construction");
        assert_eq!(
            err.to_string(),
            "failed to initialize virtio-mmio queue registers: virtio-mmio queue table cannot be empty"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("virtio-mmio queue table cannot be empty".to_string())
        );
    }

    #[test]
    fn decodes_readable_generic_registers() {
        let cases = [
            (0x00, VirtioMmioRegister::MagicValue),
            (0x04, VirtioMmioRegister::Version),
            (0x08, VirtioMmioRegister::DeviceId),
            (0x0c, VirtioMmioRegister::VendorId),
            (0x10, VirtioMmioRegister::DeviceFeatures),
            (0x34, VirtioMmioRegister::QueueNumMax),
            (0x44, VirtioMmioRegister::QueueReady),
            (0x60, VirtioMmioRegister::InterruptStatus),
            (0x70, VirtioMmioRegister::Status),
            (0xfc, VirtioMmioRegister::ConfigGeneration),
        ];

        for (offset, expected) in cases {
            let access = decode(&read_operation(offset, 4));
            assert_eq!(access.kind(), MmioOperationKind::Read);
            assert_eq!(access.len(), 4);

            let VirtioMmioAccess::Register(register_access) = access else {
                panic!("expected register access");
            };
            assert_eq!(register_access.kind(), MmioOperationKind::Read);
            assert_eq!(register_access.register(), expected);
            assert_eq!(register_access.offset(), offset);
            assert!(register_access.register().is_readable());
        }
    }

    #[test]
    fn decodes_writable_generic_registers() {
        let cases = [
            (0x14, VirtioMmioRegister::DeviceFeaturesSel),
            (0x20, VirtioMmioRegister::DriverFeatures),
            (0x24, VirtioMmioRegister::DriverFeaturesSel),
            (0x30, VirtioMmioRegister::QueueSel),
            (0x38, VirtioMmioRegister::QueueNum),
            (0x44, VirtioMmioRegister::QueueReady),
            (0x50, VirtioMmioRegister::QueueNotify),
            (0x64, VirtioMmioRegister::InterruptAck),
            (0x70, VirtioMmioRegister::Status),
            (0x80, VirtioMmioRegister::QueueDescLow),
            (0x84, VirtioMmioRegister::QueueDescHigh),
            (0x90, VirtioMmioRegister::QueueDriverLow),
            (0x94, VirtioMmioRegister::QueueDriverHigh),
            (0xa0, VirtioMmioRegister::QueueDeviceLow),
            (0xa4, VirtioMmioRegister::QueueDeviceHigh),
        ];

        for (offset, expected) in cases {
            let access = decode(&write_operation(offset, &[1, 2, 3, 4]));
            assert_eq!(access.kind(), MmioOperationKind::Write);
            assert_eq!(access.len(), 4);

            let VirtioMmioAccess::Register(register_access) = access else {
                panic!("expected register access");
            };
            assert_eq!(register_access.kind(), MmioOperationKind::Write);
            assert_eq!(register_access.register(), expected);
            assert_eq!(register_access.offset(), offset);
            assert!(register_access.register().is_writable());
        }
    }

    #[test]
    fn classifies_device_config_reads_and_writes() {
        let read = decode(&read_operation(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, 8));
        let VirtioMmioAccess::DeviceConfig(read_config) = read else {
            panic!("expected device config read");
        };
        assert_eq!(read_config.kind(), MmioOperationKind::Read);
        assert_eq!(read_config.offset(), 0);
        assert_eq!(read_config.absolute_offset(), 0x100);
        assert_eq!(read_config.len(), 8);

        let write = decode(&write_operation(0x108, &[1, 2]));
        let VirtioMmioAccess::DeviceConfig(write_config) = write else {
            panic!("expected device config write");
        };
        assert_eq!(write_config.kind(), MmioOperationKind::Write);
        assert_eq!(write_config.offset(), 8);
        assert_eq!(write_config.absolute_offset(), 0x108);
        assert_eq!(write_config.len(), 2);
    }

    #[test]
    fn classifies_device_config_access_ending_at_window_boundary() {
        let access = decode(&read_operation(0xff8, 8));
        let VirtioMmioAccess::DeviceConfig(config_access) = access else {
            panic!("expected device config read");
        };

        assert_eq!(config_access.kind(), MmioOperationKind::Read);
        assert_eq!(config_access.offset(), 0xef8);
        assert_eq!(config_access.absolute_offset(), 0xff8);
        assert_eq!(config_access.len(), 8);
    }

    #[test]
    fn rejects_register_access_with_unsupported_size() {
        let err = decode_virtio_mmio_access(&read_operation(0x00, 2))
            .expect_err("two-byte generic register read should fail");

        assert_eq!(
            err,
            VirtioMmioAccessError::UnsupportedRegisterAccessSize {
                kind: MmioOperationKind::Read,
                offset: 0x00,
                len: 2,
                expected: 4,
            }
        );
    }

    #[test]
    fn rejects_reserved_generic_register_offsets() {
        let read_err = decode_virtio_mmio_access(&read_operation(0x18, 4))
            .expect_err("reserved generic register read should fail");
        assert_eq!(
            read_err,
            VirtioMmioAccessError::UnsupportedRegisterOffset {
                kind: MmioOperationKind::Read,
                offset: 0x18,
            }
        );

        let write_err = decode_virtio_mmio_access(&write_operation(0x18, &[1, 2, 3, 4]))
            .expect_err("reserved generic register write should fail");
        assert_eq!(
            write_err,
            VirtioMmioAccessError::UnsupportedRegisterOffset {
                kind: MmioOperationKind::Write,
                offset: 0x18,
            }
        );
    }

    #[test]
    fn rejects_unsupported_read_and_write_offsets() {
        let read_err = decode_virtio_mmio_access(&read_operation(0x14, 4))
            .expect_err("write-only register should not decode as readable");
        assert_eq!(
            read_err,
            VirtioMmioAccessError::UnsupportedRegisterOffset {
                kind: MmioOperationKind::Read,
                offset: 0x14,
            }
        );

        let write_err = decode_virtio_mmio_access(&write_operation(0x00, &[1, 2, 3, 4]))
            .expect_err("read-only register should not decode as writable");
        assert_eq!(
            write_err,
            VirtioMmioAccessError::UnsupportedRegisterOffset {
                kind: MmioOperationKind::Write,
                offset: 0x00,
            }
        );
    }

    #[test]
    fn rejects_register_access_crossing_boundary() {
        let err = decode_virtio_mmio_access(&read_operation(0x02, 4))
            .expect_err("cross-register read should fail");

        assert_eq!(
            err,
            VirtioMmioAccessError::RegisterAccessCrossesBoundary {
                kind: MmioOperationKind::Read,
                offset: 0x02,
                len: 4,
                register_offset: 0x00,
                register_size: 4,
            }
        );
    }

    #[test]
    fn rejects_first_offset_past_device_window() {
        let err = decode_virtio_mmio_access(&read_operation(VIRTIO_MMIO_DEVICE_WINDOW_SIZE, 1))
            .expect_err("access starting after device window should fail");

        assert_eq!(
            err,
            VirtioMmioAccessError::AccessOutsideDeviceWindow {
                kind: MmioOperationKind::Read,
                offset: VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
                len: 1,
                window_size: VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            }
        );
    }

    #[test]
    fn rejects_access_crossing_device_window_end() {
        let err = decode_virtio_mmio_access(&read_operation(0xffc, 8))
            .expect_err("access crossing device window should fail");

        assert_eq!(
            err,
            VirtioMmioAccessError::AccessOutsideDeviceWindow {
                kind: MmioOperationKind::Read,
                offset: 0xffc,
                len: 8,
                window_size: VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            }
        );
    }

    #[test]
    fn rejects_generic_access_crossing_into_device_config_space() {
        let err = decode_virtio_mmio_access(&read_operation(0xfe, 4))
            .expect_err("generic access crossing config space should fail");

        assert_eq!(
            err,
            VirtioMmioAccessError::RegisterAccessCrossesBoundary {
                kind: MmioOperationKind::Read,
                offset: 0xfe,
                len: 4,
                register_offset: 0xfc,
                register_size: 4,
            }
        );
    }

    #[test]
    fn displays_registers_and_errors() {
        assert_eq!(VirtioMmioRegister::QueueNotify.to_string(), "QueueNotify");

        let err = VirtioMmioAccessError::UnsupportedRegisterOffset {
            kind: MmioOperationKind::Write,
            offset: 0x0c,
        };
        assert_eq!(
            err.to_string(),
            "unsupported virtio-mmio write register offset 0xc"
        );
        assert!(err.source().is_none());
    }
}
