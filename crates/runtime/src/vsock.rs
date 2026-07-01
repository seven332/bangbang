//! Backend-neutral vsock configuration model.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::mmio::{MmioAccessBytes, MmioAccessBytesError, MmioHandlerError};
use crate::virtio_mmio::{
    VirtioMmioDeviceActivation, VirtioMmioDeviceActivationError, VirtioMmioDeviceActivationHandler,
    VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError, VirtioMmioDeviceConfigHandler,
    VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError,
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
    use crate::mmio::{MmioAccess, MmioAccessBytes, MmioBus, MmioRegionId};
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
        VIRTIO_DEVICE_STATUS_INIT, VIRTIO_MMIO_DEVICE_CONFIG_OFFSET,
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioDeviceActivation, VirtioMmioDeviceRegisters,
        VirtioMmioQueueRegisters, VirtioMmioRegister, VirtioMmioRegisterHandlerError,
    };

    use super::{
        MIN_GUEST_CID, VIRTIO_FEATURE_IN_ORDER, VIRTIO_FEATURE_VERSION_1,
        VIRTIO_RING_FEATURE_EVENT_IDX, VIRTIO_VSOCK_CONFIG_GUEST_CID_SIZE, VIRTIO_VSOCK_DEVICE_ID,
        VIRTIO_VSOCK_EVENT_QUEUE_INDEX, VIRTIO_VSOCK_QUEUE_COUNT, VIRTIO_VSOCK_QUEUE_SIZE,
        VIRTIO_VSOCK_QUEUE_SIZES, VIRTIO_VSOCK_RX_QUEUE_INDEX, VIRTIO_VSOCK_TX_QUEUE_INDEX,
        VirtioVsockConfigSpace, VirtioVsockDevice, VirtioVsockMmioHandler, VsockConfigError,
        VsockConfigInput, virtio_vsock_mmio_handler,
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
