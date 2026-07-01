//! Backend-neutral vsock configuration model.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::memory::{GuestAddress, GuestMemoryError};
use crate::mmio::{
    MmioAccessBytes, MmioAccessBytesError, MmioBusError, MmioDispatchError, MmioDispatcher,
    MmioHandlerError, MmioRegion, MmioRegionId,
};
use crate::virtio_mmio::{
    VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioDeviceActivation, VirtioMmioDeviceActivationError,
    VirtioMmioDeviceActivationHandler, VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError,
    VirtioMmioDeviceConfigHandler, VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError,
};

pub const MIN_GUEST_CID: u32 = 3;
pub const VIRTIO_VSOCK_DEVICE_ID: u32 = 19;
pub const VIRTIO_VSOCK_RX_QUEUE_INDEX: usize = 0;
pub const VIRTIO_VSOCK_TX_QUEUE_INDEX: usize = 1;
pub const VIRTIO_VSOCK_EVENT_QUEUE_INDEX: usize = 2;
pub const VIRTIO_VSOCK_QUEUE_COUNT: usize = 3;
pub const VIRTIO_VSOCK_QUEUE_SIZE: u16 = 256;
pub const VIRTIO_VSOCK_QUEUE_SIZES: [u16; VIRTIO_VSOCK_QUEUE_COUNT] =
    [VIRTIO_VSOCK_QUEUE_SIZE; VIRTIO_VSOCK_QUEUE_COUNT];
pub const VIRTIO_VSOCK_CONFIG_GUEST_CID_SIZE: usize = 8;
pub const VIRTIO_RING_FEATURE_EVENT_IDX: u32 = 29;
pub const VIRTIO_FEATURE_VERSION_1: u32 = 32;
pub const VIRTIO_FEATURE_IN_ORDER: u32 = 35;

const VIRTIO_VSOCK_QUEUE_INDEXES: [u32; VIRTIO_VSOCK_QUEUE_COUNT] = [0, 1, 2];

pub type VirtioVsockMmioHandler =
    VirtioMmioRegisterHandler<VirtioVsockConfigSpace, VirtioVsockDevice>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsockConfigInput {
    vsock_id: Option<String>,
    guest_cid: u32,
    uds_path: String,
}

impl VsockConfigInput {
    pub fn new(guest_cid: u32, uds_path: impl Into<String>) -> Self {
        Self {
            vsock_id: None,
            guest_cid,
            uds_path: uds_path.into(),
        }
    }

    pub fn with_vsock_id(mut self, vsock_id: impl Into<String>) -> Self {
        self.vsock_id = Some(vsock_id.into());
        self
    }

    pub fn vsock_id(&self) -> Option<&str> {
        self.vsock_id.as_deref()
    }

    pub const fn guest_cid(&self) -> u32 {
        self.guest_cid
    }

    pub fn uds_path(&self) -> &str {
        &self.uds_path
    }

    pub fn validate(self) -> Result<VsockConfig, VsockConfigError> {
        VsockConfig::try_from(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsockConfig {
    vsock_id: Option<String>,
    guest_cid: u32,
    uds_path: PathBuf,
}

impl VsockConfig {
    pub fn vsock_id(&self) -> Option<&str> {
        self.vsock_id.as_deref()
    }

    pub const fn guest_cid(&self) -> u32 {
        self.guest_cid
    }

    pub fn uds_path(&self) -> &Path {
        &self.uds_path
    }
}

impl TryFrom<VsockConfigInput> for VsockConfig {
    type Error = VsockConfigError;

    fn try_from(input: VsockConfigInput) -> Result<Self, Self::Error> {
        if input.guest_cid < MIN_GUEST_CID {
            return Err(VsockConfigError::GuestCidTooSmall {
                guest_cid: input.guest_cid,
                min: MIN_GUEST_CID,
            });
        }

        if let Some(vsock_id) = input.vsock_id.as_deref() {
            if vsock_id.is_empty() {
                return Err(VsockConfigError::EmptyVsockId);
            }
            if has_control_character(vsock_id) {
                return Err(VsockConfigError::InvalidVsockId {
                    vsock_id: vsock_id.to_string(),
                });
            }
        }

        if input.uds_path.is_empty() {
            return Err(VsockConfigError::EmptySocketPath);
        }
        if has_control_character(&input.uds_path) {
            return Err(VsockConfigError::InvalidSocketPath);
        }

        Ok(Self {
            vsock_id: input.vsock_id,
            guest_cid: input.guest_cid,
            uds_path: PathBuf::from(input.uds_path),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VsockConfigError {
    GuestCidTooSmall { guest_cid: u32, min: u32 },
    EmptyVsockId,
    InvalidVsockId { vsock_id: String },
    EmptySocketPath,
    InvalidSocketPath,
}

impl fmt::Display for VsockConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GuestCidTooSmall { guest_cid, min } => {
                write!(f, "vsock guest_cid {guest_cid} is below minimum {min}")
            }
            Self::EmptyVsockId => f.write_str("vsock_id must not be empty"),
            Self::InvalidVsockId { .. } => {
                f.write_str("vsock_id must not contain control characters")
            }
            Self::EmptySocketPath => f.write_str("vsock uds_path must not be empty"),
            Self::InvalidSocketPath => {
                f.write_str("vsock uds_path must not contain control characters")
            }
        }
    }
}

impl std::error::Error for VsockConfigError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioVsockConfigSpace {
    guest_cid: u64,
}

impl VirtioVsockConfigSpace {
    pub const fn new(guest_cid: u64) -> Self {
        Self { guest_cid }
    }

    pub const fn guest_cid(self) -> u64 {
        self.guest_cid
    }

    pub const fn available_features(self) -> u64 {
        virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
            | virtio_feature_bit(VIRTIO_FEATURE_IN_ORDER)
            | virtio_feature_bit(VIRTIO_RING_FEATURE_EVENT_IDX)
    }

    const fn guest_cid_bytes(self) -> [u8; VIRTIO_VSOCK_CONFIG_GUEST_CID_SIZE] {
        self.guest_cid.to_le_bytes()
    }
}

impl VirtioMmioDeviceConfigHandler for VirtioVsockConfigSpace {
    fn read_device_config(
        &self,
        access: VirtioMmioDeviceConfigAccess,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
        let bytes = self.guest_cid_bytes();
        let [b0, b1, b2, b3, b4, b5, b6, b7] = bytes;
        match (access.offset(), access.len()) {
            (0, 8) => MmioAccessBytes::new(&bytes),
            (0, 4) => MmioAccessBytes::new(&[b0, b1, b2, b3]),
            (4, 4) => MmioAccessBytes::new(&[b4, b5, b6, b7]),
            _ => {
                return Err(VirtioMmioDeviceConfigError::UnsupportedRead {
                    offset: access.offset(),
                    len: access.len(),
                });
            }
        }
        .map_err(config_bytes_error)
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VirtioVsockDevice {
    activated: bool,
}

impl VirtioVsockDevice {
    pub fn new() -> Self {
        Self::default()
    }

    pub const fn is_activated(&self) -> bool {
        self.activated
    }

    pub fn activate_vsock(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioMmioDeviceActivationError> {
        if activation.queue_count() != VIRTIO_VSOCK_QUEUE_COUNT {
            return Err(MmioHandlerError::new(format!(
                "virtio-vsock expected {VIRTIO_VSOCK_QUEUE_COUNT} queues, got {}",
                activation.queue_count()
            ))
            .into());
        }

        for queue_index in VIRTIO_VSOCK_QUEUE_INDEXES {
            let queue = activation.queue(queue_index).map_err(|source| {
                MmioHandlerError::new(format!(
                    "failed to read virtio-vsock queue {queue_index}: {source}"
                ))
            })?;
            if !queue.ready() || queue.size() == 0 {
                return Err(MmioHandlerError::new(format!(
                    "virtio-vsock queue {queue_index} must be configured and ready before activation"
                ))
                .into());
            }
        }

        self.activated = true;
        Ok(())
    }

    pub fn reset(&mut self) {
        self.activated = false;
    }
}

impl VirtioMmioDeviceActivationHandler for VirtioVsockDevice {
    fn activate(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioMmioDeviceActivationError> {
        self.activate_vsock(activation)
    }

    fn reset(&mut self) {
        VirtioVsockDevice::reset(self);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedVsockDevice {
    guest_cid: u32,
    uds_path: PathBuf,
    config_space: VirtioVsockConfigSpace,
    device: VirtioVsockDevice,
}

impl PreparedVsockDevice {
    pub fn from_config(config: &VsockConfig) -> Self {
        Self {
            guest_cid: config.guest_cid(),
            uds_path: config.uds_path().to_path_buf(),
            config_space: VirtioVsockConfigSpace::new(u64::from(config.guest_cid())),
            device: VirtioVsockDevice::new(),
        }
    }

    pub const fn guest_cid(&self) -> u32 {
        self.guest_cid
    }

    pub fn uds_path(&self) -> &Path {
        &self.uds_path
    }

    pub const fn config_space(&self) -> VirtioVsockConfigSpace {
        self.config_space
    }

    pub const fn device(&self) -> &VirtioVsockDevice {
        &self.device
    }

    pub fn into_parts(self) -> (u32, PathBuf, VirtioVsockConfigSpace, VirtioVsockDevice) {
        (
            self.guest_cid,
            self.uds_path,
            self.config_space,
            self.device,
        )
    }

    pub fn register_mmio(
        self,
        layout: VsockMmioLayout,
    ) -> Result<VsockMmioDevice, VsockMmioRegistrationError> {
        VsockMmioDevice::from_prepared(self, layout)
    }

    pub fn register_mmio_with_dispatcher(
        self,
        layout: VsockMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<VsockMmioDevice, VsockMmioRegistrationError> {
        VsockMmioDevice::from_prepared_with_dispatcher(self, layout, dispatcher)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VsockMmioLayout {
    address: GuestAddress,
    region_id: MmioRegionId,
}

impl VsockMmioLayout {
    pub const fn new(address: GuestAddress, region_id: MmioRegionId) -> Self {
        Self { address, region_id }
    }

    pub const fn address(self) -> GuestAddress {
        self.address
    }

    pub const fn region_id(self) -> MmioRegionId {
        self.region_id
    }

    fn region(self) -> Result<MmioRegion, VsockMmioRegistrationError> {
        MmioRegion::new(self.region_id, self.address, VIRTIO_MMIO_DEVICE_WINDOW_SIZE).map_err(
            |source| VsockMmioRegistrationError::InvalidRegion {
                region_id: self.region_id,
                address: self.address,
                source,
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsockMmioDeviceRegistration {
    guest_cid: u32,
    uds_path: PathBuf,
    region: MmioRegion,
}

impl VsockMmioDeviceRegistration {
    pub const fn guest_cid(&self) -> u32 {
        self.guest_cid
    }

    pub fn uds_path(&self) -> &Path {
        &self.uds_path
    }

    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    pub const fn region_id(&self) -> MmioRegionId {
        self.region.id()
    }

    pub const fn address(&self) -> GuestAddress {
        self.region.range().start()
    }
}

#[derive(Debug)]
pub struct VsockMmioDevice {
    dispatcher: MmioDispatcher,
    registration: VsockMmioDeviceRegistration,
}

impl VsockMmioDevice {
    pub fn from_prepared(
        prepared: PreparedVsockDevice,
        layout: VsockMmioLayout,
    ) -> Result<Self, VsockMmioRegistrationError> {
        Self::from_prepared_with_dispatcher(prepared, layout, MmioDispatcher::new())
    }

    pub fn from_prepared_with_dispatcher(
        prepared: PreparedVsockDevice,
        layout: VsockMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<Self, VsockMmioRegistrationError> {
        let region = layout.region()?;
        let (guest_cid, uds_path, config_space, device) = prepared.into_parts();
        let handler = VirtioMmioRegisterHandler::with_device_config_and_activation(
            VIRTIO_VSOCK_DEVICE_ID,
            config_space.available_features(),
            &VIRTIO_VSOCK_QUEUE_SIZES,
            config_space,
            device,
        )
        .map_err(|source| VsockMmioRegistrationError::BuildHandler {
            guest_cid,
            region_id: layout.region_id(),
            source,
        })?;
        let mut dispatcher = dispatcher;
        let inserted_region = dispatcher
            .insert_region(
                layout.region_id(),
                layout.address(),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .map_err(|source| VsockMmioRegistrationError::InsertRegion {
                guest_cid,
                region_id: layout.region_id(),
                address: layout.address(),
                source,
            })?;
        dispatcher
            .register_handler(layout.region_id(), handler)
            .map_err(|source| VsockMmioRegistrationError::RegisterHandler {
                guest_cid,
                region_id: layout.region_id(),
                source,
            })?;
        debug_assert_eq!(inserted_region, region);

        Ok(Self {
            dispatcher,
            registration: VsockMmioDeviceRegistration {
                guest_cid,
                uds_path,
                region,
            },
        })
    }

    pub fn dispatcher(&self) -> &MmioDispatcher {
        &self.dispatcher
    }

    pub fn dispatcher_mut(&mut self) -> &mut MmioDispatcher {
        &mut self.dispatcher
    }

    pub const fn registration(&self) -> &VsockMmioDeviceRegistration {
        &self.registration
    }

    pub fn into_parts(self) -> (MmioDispatcher, VsockMmioDeviceRegistration) {
        (self.dispatcher, self.registration)
    }
}

#[derive(Debug)]
pub enum VsockMmioRegistrationError {
    InvalidRegion {
        region_id: MmioRegionId,
        address: GuestAddress,
        source: GuestMemoryError,
    },
    BuildHandler {
        guest_cid: u32,
        region_id: MmioRegionId,
        source: VirtioMmioRegisterHandlerError,
    },
    InsertRegion {
        guest_cid: u32,
        region_id: MmioRegionId,
        address: GuestAddress,
        source: MmioBusError,
    },
    RegisterHandler {
        guest_cid: u32,
        region_id: MmioRegionId,
        source: MmioDispatchError,
    },
}

impl fmt::Display for VsockMmioRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRegion {
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "invalid vsock MMIO region id={region_id} at {address}: {source}"
                )
            }
            Self::BuildHandler {
                guest_cid,
                region_id,
                source,
            } => {
                write!(
                    f,
                    "failed to build vsock MMIO handler for guest CID {guest_cid} region id={region_id}: {source}"
                )
            }
            Self::InsertRegion {
                guest_cid,
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "failed to insert vsock MMIO region for guest CID {guest_cid} region id={region_id} at {address}: {source}"
                )
            }
            Self::RegisterHandler {
                guest_cid,
                region_id,
                source,
            } => {
                write!(
                    f,
                    "failed to register vsock MMIO handler for guest CID {guest_cid} region id={region_id}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VsockMmioRegistrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRegion { source, .. } => Some(source),
            Self::BuildHandler { source, .. } => Some(source),
            Self::InsertRegion { source, .. } => Some(source),
            Self::RegisterHandler { source, .. } => Some(source),
        }
    }
}

pub fn virtio_vsock_mmio_handler(
    guest_cid: u32,
) -> Result<VirtioVsockMmioHandler, VirtioMmioRegisterHandlerError> {
    let config = VirtioVsockConfigSpace::new(u64::from(guest_cid));
    VirtioMmioRegisterHandler::with_device_config_and_activation(
        VIRTIO_VSOCK_DEVICE_ID,
        config.available_features(),
        &VIRTIO_VSOCK_QUEUE_SIZES,
        config,
        VirtioVsockDevice::new(),
    )
}

const fn virtio_feature_bit(feature: u32) -> u64 {
    1_u64 << feature
}

fn config_bytes_error(source: MmioAccessBytesError) -> VirtioMmioDeviceConfigError {
    VirtioMmioDeviceConfigError::Handler {
        source: MmioHandlerError::new(format!("virtio-vsock config access bytes failed: {source}")),
    }
}

fn has_control_character(value: &str) -> bool {
    value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::path::Path;

    use crate::memory::GuestAddress;
    use crate::mmio::{
        MmioAccess, MmioAccessBytes, MmioBus, MmioDispatchOutcome, MmioDispatcher, MmioOperation,
        MmioRegionId,
    };
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
        VIRTIO_DEVICE_STATUS_INIT, VIRTIO_MMIO_DEVICE_CONFIG_OFFSET,
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioDeviceActivation, VirtioMmioDeviceRegisters,
        VirtioMmioQueueRegisters, VirtioMmioRegister, VirtioMmioRegisterHandlerError,
    };

    use super::{
        MIN_GUEST_CID, PreparedVsockDevice, VIRTIO_FEATURE_IN_ORDER, VIRTIO_FEATURE_VERSION_1,
        VIRTIO_RING_FEATURE_EVENT_IDX, VIRTIO_VSOCK_CONFIG_GUEST_CID_SIZE, VIRTIO_VSOCK_DEVICE_ID,
        VIRTIO_VSOCK_EVENT_QUEUE_INDEX, VIRTIO_VSOCK_QUEUE_COUNT, VIRTIO_VSOCK_QUEUE_SIZE,
        VIRTIO_VSOCK_QUEUE_SIZES, VIRTIO_VSOCK_RX_QUEUE_INDEX, VIRTIO_VSOCK_TX_QUEUE_INDEX,
        VirtioVsockConfigSpace, VirtioVsockDevice, VirtioVsockMmioHandler, VsockConfigError,
        VsockConfigInput, VsockMmioDevice, VsockMmioLayout, VsockMmioRegistrationError,
        virtio_vsock_mmio_handler,
    };

    const TEST_MMIO_BASE: u64 = 0x1000_0000;
    const QUEUE_CONFIG_STATUS: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    const DRIVER_OK_STATUS: u32 = QUEUE_CONFIG_STATUS | VIRTIO_DEVICE_STATUS_DRIVER_OK;
    const TEST_QUEUE_ADDRESS_BASE: u64 = 0x1000;
    const TEST_QUEUE_ADDRESS_STRIDE: u64 = 0x1000;

    fn validate(input: VsockConfigInput) -> Result<super::VsockConfig, VsockConfigError> {
        input.validate()
    }

    fn valid_vsock_config(guest_cid: u32, uds_path: impl Into<String>) -> super::VsockConfig {
        validate(VsockConfigInput::new(guest_cid, uds_path)).expect("valid config")
    }

    fn prepared_vsock_device(guest_cid: u32, uds_path: impl Into<String>) -> PreparedVsockDevice {
        PreparedVsockDevice::from_config(&valid_vsock_config(guest_cid, uds_path))
    }

    fn unique_missing_socket_path() -> String {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after Unix epoch")
            .as_nanos();
        format!(
            "bangbang-vsock-missing-parent-{}-{unique}/v.sock",
            std::process::id(),
        )
    }

    fn vsock_mmio_layout() -> VsockMmioLayout {
        VsockMmioLayout::new(GuestAddress::new(TEST_MMIO_BASE), MmioRegionId::new(2))
    }

    fn read_registered_config(device: &mut VsockMmioDevice, offset: u64, len: u64) -> Vec<u8> {
        let address = device
            .registration()
            .address()
            .checked_add(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + offset)
            .expect("test config address should not overflow");
        let access = device
            .dispatcher()
            .lookup(address, len)
            .expect("registered config access should resolve");
        let operation = MmioOperation::read(access).expect("registered config read should build");
        let outcome = device
            .dispatcher_mut()
            .dispatch(operation)
            .expect("registered config read should dispatch");
        let MmioDispatchOutcome::Read { data } = outcome else {
            panic!("read operation should return read outcome");
        };

        data.as_slice().to_vec()
    }

    fn virtio_mmio_access(offset: u64, len: u64) -> MmioAccess {
        let mut bus = MmioBus::new();
        bus.insert(
            MmioRegionId::new(1),
            GuestAddress::new(TEST_MMIO_BASE),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("virtio-mmio region should insert");
        bus.lookup(GuestAddress::new(TEST_MMIO_BASE + offset), len)
            .expect("virtio-mmio access should resolve")
    }

    fn device_config_access(offset: u64, len: u64) -> MmioAccess {
        virtio_mmio_access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + offset, len)
    }

    fn read_config(handler: &VirtioVsockMmioHandler, offset: u64, len: u64) -> Vec<u8> {
        handler
            .read_access(device_config_access(offset, len))
            .expect("vsock config read should succeed")
            .as_slice()
            .to_vec()
    }

    fn advance_handler_to_features_ok(handler: &mut VirtioVsockMmioHandler) {
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("status should accept ACKNOWLEDGE");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("status should accept DRIVER");
        handler
            .write_register(VirtioMmioRegister::Status, QUEUE_CONFIG_STATUS)
            .expect("status should accept FEATURES_OK");
    }

    fn guest_address_low(address: GuestAddress) -> u32 {
        u32::try_from(address.raw_value()).expect("test guest address should fit in low half")
    }

    fn configure_vsock_queues(handler: &mut VirtioVsockMmioHandler) {
        for queue_index in 0..VIRTIO_VSOCK_QUEUE_COUNT {
            let queue_index_u32 = u32::try_from(queue_index).expect("queue index should fit");
            let queue_base =
                TEST_QUEUE_ADDRESS_BASE + u64::from(queue_index_u32) * TEST_QUEUE_ADDRESS_STRIDE;
            handler
                .write_register(VirtioMmioRegister::QueueSel, queue_index_u32)
                .expect("queue select should write");
            handler
                .write_register(
                    VirtioMmioRegister::QueueNum,
                    u32::from(VIRTIO_VSOCK_QUEUE_SIZE),
                )
                .expect("queue size should write");
            handler
                .write_register(
                    VirtioMmioRegister::QueueDescLow,
                    guest_address_low(GuestAddress::new(queue_base)),
                )
                .expect("queue descriptor table should write");
            handler
                .write_register(
                    VirtioMmioRegister::QueueDriverLow,
                    guest_address_low(GuestAddress::new(queue_base + 0x200)),
                )
                .expect("queue driver ring should write");
            handler
                .write_register(
                    VirtioMmioRegister::QueueDeviceLow,
                    guest_address_low(GuestAddress::new(queue_base + 0x400)),
                )
                .expect("queue device ring should write");
            handler
                .write_register(VirtioMmioRegister::QueueReady, 1)
                .expect("queue ready should write");
        }
    }

    fn vsock_handler_for_config(config: VirtioVsockConfigSpace) -> VirtioVsockMmioHandler {
        VirtioVsockMmioHandler::with_device_config_and_activation(
            VIRTIO_VSOCK_DEVICE_ID,
            config.available_features(),
            &VIRTIO_VSOCK_QUEUE_SIZES,
            config,
            VirtioVsockDevice::new(),
        )
        .expect("vsock handler should build")
    }

    #[test]
    fn accepts_minimal_config() {
        let config =
            validate(VsockConfigInput::new(MIN_GUEST_CID, "./v.sock")).expect("valid config");

        assert_eq!(config.vsock_id(), None);
        assert_eq!(config.guest_cid(), MIN_GUEST_CID);
        assert_eq!(config.uds_path(), Path::new("./v.sock"));
    }

    #[test]
    fn accepts_optional_deprecated_vsock_id() {
        let config = validate(VsockConfigInput::new(42, "/tmp/v.sock").with_vsock_id("vsock_0"))
            .expect("valid config");

        assert_eq!(config.vsock_id(), Some("vsock_0"));
        assert_eq!(config.guest_cid(), 42);
        assert_eq!(config.uds_path(), Path::new("/tmp/v.sock"));
    }

    #[test]
    fn rejects_guest_cid_below_firecracker_minimum() {
        let err = validate(VsockConfigInput::new(2, "/tmp/v.sock"))
            .expect_err("small guest cid should fail");

        assert_eq!(
            err,
            VsockConfigError::GuestCidTooSmall {
                guest_cid: 2,
                min: MIN_GUEST_CID,
            }
        );
        assert_eq!(err.to_string(), "vsock guest_cid 2 is below minimum 3");
    }

    #[test]
    fn rejects_empty_vsock_id() {
        let err = validate(VsockConfigInput::new(3, "/tmp/v.sock").with_vsock_id(""))
            .expect_err("empty id should fail");

        assert_eq!(err, VsockConfigError::EmptyVsockId);
        assert_eq!(err.to_string(), "vsock_id must not be empty");
    }

    #[test]
    fn rejects_control_character_vsock_id_without_echoing_it() {
        let invalid = "id\nsecret";
        let err = validate(VsockConfigInput::new(3, "/tmp/v.sock").with_vsock_id(invalid))
            .expect_err("control character id should fail");

        assert_eq!(
            err,
            VsockConfigError::InvalidVsockId {
                vsock_id: invalid.to_string(),
            }
        );
        assert_eq!(
            err.to_string(),
            "vsock_id must not contain control characters"
        );
        assert!(!err.to_string().contains(invalid));
    }

    #[test]
    fn rejects_empty_socket_path() {
        let err =
            validate(VsockConfigInput::new(3, "")).expect_err("empty socket path should fail");

        assert_eq!(err, VsockConfigError::EmptySocketPath);
        assert_eq!(err.to_string(), "vsock uds_path must not be empty");
    }

    #[test]
    fn rejects_control_character_socket_path_without_echoing_it() {
        let invalid = "/tmp/v.sock\nsecret";
        let err = validate(VsockConfigInput::new(3, invalid))
            .expect_err("control character socket path should fail");

        assert_eq!(err, VsockConfigError::InvalidSocketPath);
        assert_eq!(
            err.to_string(),
            "vsock uds_path must not contain control characters"
        );
        assert!(!err.to_string().contains(invalid));
    }

    #[test]
    fn errors_have_no_sources() {
        assert!(VsockConfigError::EmptySocketPath.source().is_none());
    }

    #[test]
    fn prepared_vsock_device_preserves_config_and_inactive_device() {
        let config = valid_vsock_config(u32::MAX, "./relative-vsock.sock");
        let prepared = PreparedVsockDevice::from_config(&config);

        assert_eq!(prepared.guest_cid(), u32::MAX);
        assert_eq!(prepared.uds_path(), Path::new("./relative-vsock.sock"));
        assert_eq!(prepared.config_space().guest_cid(), u64::from(u32::MAX));
        assert!(!prepared.device().is_activated());
    }

    #[test]
    fn prepared_vsock_device_into_parts_consumes_owned_resource() {
        let config = valid_vsock_config(7, "relative-vsock.sock");
        let prepared = PreparedVsockDevice::from_config(&config);

        let (guest_cid, uds_path, config_space, device) = prepared.into_parts();

        assert_eq!(guest_cid, 7);
        assert_eq!(uds_path.as_path(), Path::new("relative-vsock.sock"));
        assert_eq!(config_space.guest_cid(), 7);
        assert!(!device.is_activated());
    }

    #[test]
    fn prepared_vsock_device_does_not_touch_missing_socket_path() {
        let socket_path = unique_missing_socket_path();
        let path = Path::new(&socket_path);
        let config = valid_vsock_config(8, socket_path.clone());

        assert!(!path.exists());

        let prepared = PreparedVsockDevice::from_config(&config);

        assert_eq!(prepared.uds_path(), path);
        assert!(!path.exists());
    }

    #[test]
    fn prepared_vsock_device_registers_mmio_in_fresh_dispatcher() {
        let mut device = prepared_vsock_device(42, "./v.sock")
            .register_mmio(vsock_mmio_layout())
            .expect("vsock device should register");
        let registration = device.registration();

        assert_eq!(registration.guest_cid(), 42);
        assert_eq!(registration.uds_path(), Path::new("./v.sock"));
        assert_eq!(registration.region_id(), MmioRegionId::new(2));
        assert_eq!(registration.address(), GuestAddress::new(TEST_MMIO_BASE));
        assert_eq!(
            registration.region().range().size(),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        );
        assert_eq!(device.dispatcher().regions(), &[registration.region()]);

        let region_id = registration.region_id();
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioVsockMmioHandler>(region_id)
            .expect("registered vsock handler should be present");
        assert!(!handler.is_device_activated());
        assert!(!handler.activation_handler().is_activated());
    }

    #[test]
    fn prepared_vsock_device_registers_mmio_in_existing_dispatcher() {
        let mut dispatcher = MmioDispatcher::new();
        let existing_region = dispatcher
            .insert_region(
                MmioRegionId::new(1),
                GuestAddress::new(TEST_MMIO_BASE - VIRTIO_MMIO_DEVICE_WINDOW_SIZE),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("existing region should insert");
        let device = prepared_vsock_device(9, "./v.sock")
            .register_mmio_with_dispatcher(vsock_mmio_layout(), dispatcher)
            .expect("vsock device should register with existing dispatcher");

        assert_eq!(device.dispatcher().regions().len(), 2);
        assert!(device.dispatcher().regions().contains(&existing_region));
        assert!(
            device
                .dispatcher()
                .regions()
                .contains(&device.registration().region())
        );
    }

    #[test]
    fn registered_vsock_mmio_dispatch_reads_guest_cid_config() {
        let mut device = prepared_vsock_device(0x1122_3344, "./v.sock")
            .register_mmio(vsock_mmio_layout())
            .expect("vsock device should register");

        assert_eq!(
            read_registered_config(&mut device, 0, 8),
            u64::from(0x1122_3344_u32).to_le_bytes().to_vec()
        );
        assert_eq!(
            read_registered_config(&mut device, 0, 4),
            0x1122_3344_u32.to_le_bytes().to_vec()
        );
        assert_eq!(
            read_registered_config(&mut device, 4, 4),
            0_u32.to_le_bytes().to_vec()
        );
    }

    #[test]
    fn registered_vsock_mmio_into_parts_consumes_owned_resource() {
        let device = prepared_vsock_device(11, "relative-vsock.sock")
            .register_mmio(vsock_mmio_layout())
            .expect("vsock device should register");

        let (dispatcher, registration) = device.into_parts();

        assert_eq!(dispatcher.regions(), &[registration.region()]);
        assert_eq!(registration.guest_cid(), 11);
        assert_eq!(registration.uds_path(), Path::new("relative-vsock.sock"));
    }

    #[test]
    fn prepared_vsock_device_rejects_overlapping_mmio_registration_without_path_leak() {
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(
                MmioRegionId::new(1),
                GuestAddress::new(TEST_MMIO_BASE),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("existing region should insert");

        let layout = VsockMmioLayout::new(
            GuestAddress::new(TEST_MMIO_BASE + 0x100),
            MmioRegionId::new(3),
        );
        let err = prepared_vsock_device(12, "secret-vsock-path.sock")
            .register_mmio_with_dispatcher(layout, dispatcher)
            .expect_err("overlapping region should fail");

        assert!(matches!(
            err,
            VsockMmioRegistrationError::InsertRegion {
                guest_cid: 12,
                region_id,
                ..
            } if region_id == MmioRegionId::new(3)
        ));
        assert!(err.source().is_some());
        assert!(!err.to_string().contains("secret-vsock-path"));
    }

    #[test]
    fn prepared_vsock_device_rejects_invalid_mmio_layout_without_path_leak() {
        let layout = VsockMmioLayout::new(GuestAddress::new(u64::MAX), MmioRegionId::new(4));
        let err = prepared_vsock_device(13, "secret-vsock-path.sock")
            .register_mmio(layout)
            .expect_err("overflowing region should fail");

        assert!(matches!(
            err,
            VsockMmioRegistrationError::InvalidRegion {
                region_id,
                address,
                ..
            } if region_id == MmioRegionId::new(4) && address == GuestAddress::new(u64::MAX)
        ));
        assert!(err.source().is_some());
        assert!(!err.to_string().contains("secret-vsock-path"));
    }

    #[test]
    fn prepared_vsock_device_rejects_duplicate_mmio_handler_without_path_leak() {
        let layout = vsock_mmio_layout();
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .register_handler(
                layout.region_id(),
                virtio_vsock_mmio_handler(3).expect("existing handler should build"),
            )
            .expect("existing handler should register");

        let err = prepared_vsock_device(14, "secret-vsock-path.sock")
            .register_mmio_with_dispatcher(layout, dispatcher)
            .expect_err("duplicate handler should fail");

        assert!(matches!(
            err,
            VsockMmioRegistrationError::RegisterHandler {
                guest_cid: 14,
                region_id,
                ..
            } if region_id == layout.region_id()
        ));
        assert!(err.source().is_some());
        assert!(!err.to_string().contains("secret-vsock-path"));
    }

    #[test]
    fn prepared_vsock_device_mmio_registration_does_not_touch_missing_socket_path() {
        let socket_path = unique_missing_socket_path();
        let path = Path::new(&socket_path);

        assert!(!path.exists());

        let device = prepared_vsock_device(13, socket_path.clone())
            .register_mmio(vsock_mmio_layout())
            .expect("vsock device should register");

        assert_eq!(device.registration().uds_path(), path);
        assert!(!path.exists());
    }

    #[test]
    fn virtio_vsock_constants_match_firecracker_shape() {
        assert_eq!(VIRTIO_VSOCK_DEVICE_ID, 19);
        assert_eq!(VIRTIO_VSOCK_RX_QUEUE_INDEX, 0);
        assert_eq!(VIRTIO_VSOCK_TX_QUEUE_INDEX, 1);
        assert_eq!(VIRTIO_VSOCK_EVENT_QUEUE_INDEX, 2);
        assert_eq!(VIRTIO_VSOCK_QUEUE_COUNT, 3);
        assert_eq!(VIRTIO_VSOCK_QUEUE_SIZE, 256);
        assert_eq!(VIRTIO_VSOCK_QUEUE_SIZES, [256, 256, 256]);
        assert_eq!(VIRTIO_VSOCK_CONFIG_GUEST_CID_SIZE, 8);
    }

    #[test]
    fn virtio_vsock_config_space_reports_firecracker_feature_bits() {
        let config = VirtioVsockConfigSpace::new(3);
        let features = config.available_features();
        let expected_features = (1_u64 << VIRTIO_FEATURE_VERSION_1)
            | (1_u64 << VIRTIO_FEATURE_IN_ORDER)
            | (1_u64 << VIRTIO_RING_FEATURE_EVENT_IDX);

        assert_eq!(config.guest_cid(), 3);
        assert_eq!(features, expected_features);
    }

    #[test]
    fn virtio_vsock_config_space_reads_guest_cid_as_u64() {
        let config = VirtioVsockConfigSpace::new(0x1122_3344_5566_7788);
        let handler = vsock_handler_for_config(config);

        assert_eq!(
            read_config(&handler, 0, 8),
            0x1122_3344_5566_7788_u64.to_le_bytes().to_vec()
        );
    }

    #[test]
    fn virtio_vsock_config_space_reads_guest_cid_halves() {
        let config = VirtioVsockConfigSpace::new(0x1122_3344_5566_7788);
        let handler = vsock_handler_for_config(config);

        assert_eq!(
            read_config(&handler, 0, 4),
            0x5566_7788_u32.to_le_bytes().to_vec()
        );
        assert_eq!(
            read_config(&handler, 4, 4),
            0x1122_3344_u32.to_le_bytes().to_vec()
        );
    }

    #[test]
    fn virtio_vsock_config_space_rejects_unsupported_reads() {
        let config = VirtioVsockConfigSpace::new(3);
        let handler = vsock_handler_for_config(config);

        let err = handler
            .read_access(device_config_access(2, 4))
            .expect_err("unsupported config read should fail");

        assert!(matches!(
            err,
            VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 2, len: 4 }
        ));

        let err = handler
            .read_access(device_config_access(8, 1))
            .expect_err("past-end config read should fail");

        assert!(matches!(
            err,
            VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 8, len: 1 }
        ));

        let err = handler
            .read_access(device_config_access(4, 8))
            .expect_err("straddling config read should fail");

        assert!(matches!(
            err,
            VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 4, len: 8 }
        ));
    }

    #[test]
    fn virtio_vsock_config_space_rejects_writes() {
        let mut handler = virtio_vsock_mmio_handler(3).expect("vsock handler should build");
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("status should accept ACKNOWLEDGE");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("status should accept DRIVER");

        let err = handler
            .write_access(
                device_config_access(0, 4),
                MmioAccessBytes::new(&0_u32.to_le_bytes()).expect("test bytes should fit"),
            )
            .expect_err("vsock config write should fail");

        assert!(matches!(
            err,
            VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigWrite { offset: 0, len: 4 }
        ));
    }

    #[test]
    fn virtio_vsock_mmio_handler_uses_device_id_and_queue_shape() {
        let mut handler = virtio_vsock_mmio_handler(42).expect("vsock handler should build");

        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::DeviceId)
                .expect("device id should read"),
            VIRTIO_VSOCK_DEVICE_ID
        );
        assert_eq!(
            handler.queue_registers().queue_count(),
            VIRTIO_VSOCK_QUEUE_COUNT
        );

        for queue_index in 0..VIRTIO_VSOCK_QUEUE_COUNT {
            handler
                .write_register(
                    VirtioMmioRegister::QueueSel,
                    u32::try_from(queue_index).expect("queue index should fit"),
                )
                .expect("queue select should write");
            assert_eq!(
                handler
                    .read_register(VirtioMmioRegister::QueueNumMax)
                    .expect("queue max should read"),
                u32::from(VIRTIO_VSOCK_QUEUE_SIZE)
            );
        }
    }

    #[test]
    fn virtio_vsock_device_activates_and_resets_through_handler() {
        let mut handler = virtio_vsock_mmio_handler(3).expect("vsock handler should build");

        assert!(!handler.is_device_activated());
        assert!(!handler.activation_handler().is_activated());

        advance_handler_to_features_ok(&mut handler);
        configure_vsock_queues(&mut handler);
        handler
            .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
            .expect("DRIVER_OK should activate vsock device");

        assert!(handler.is_device_activated());
        assert!(handler.activation_handler().is_activated());

        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_INIT)
            .expect("status reset should succeed");

        assert!(!handler.is_device_activated());
        assert!(!handler.activation_handler().is_activated());
    }

    #[test]
    fn virtio_vsock_device_rejects_driver_ok_before_queues_are_ready() {
        let mut handler = virtio_vsock_mmio_handler(3).expect("vsock handler should build");
        advance_handler_to_features_ok(&mut handler);

        let err = handler
            .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
            .expect_err("unready queues should reject DRIVER_OK");

        assert!(matches!(
            err,
            VirtioMmioRegisterHandlerError::DeviceActivation { .. }
        ));
        assert!(!handler.is_device_activated());
        assert!(!handler.activation_handler().is_activated());
    }

    #[test]
    fn virtio_vsock_device_rejects_unexpected_queue_count() {
        let registers = VirtioMmioDeviceRegisters::new(VIRTIO_VSOCK_DEVICE_ID, 0);
        let queues =
            VirtioMmioQueueRegisters::new(&[VIRTIO_VSOCK_QUEUE_SIZE, VIRTIO_VSOCK_QUEUE_SIZE])
                .expect("short queue table should build");
        let activation = VirtioMmioDeviceActivation::new(&registers, &queues);
        let mut device = VirtioVsockDevice::new();

        let err = device
            .activate_vsock(activation)
            .expect_err("unexpected queue count should fail activation");

        assert_eq!(
            err.to_string(),
            "virtio-mmio device activation handler failed: virtio-vsock expected 3 queues, got 2"
        );
        assert!(!device.is_activated());
    }
}
