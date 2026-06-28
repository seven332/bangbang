//! Backend-neutral virtio-mmio register access decoding.

use std::fmt;

use crate::mmio::{MmioOperation, MmioOperationKind};

pub const VIRTIO_MMIO_DEVICE_WINDOW_SIZE: u64 = 0x1000;
pub const VIRTIO_MMIO_REGISTER_SPACE_SIZE: u64 = 0x100;
pub const VIRTIO_MMIO_DEVICE_CONFIG_OFFSET: u64 = 0x100;
pub const VIRTIO_MMIO_NOTIFY_OFFSET: u64 = 0x50;
pub const VIRTIO_MMIO_MAGIC_VALUE: u32 = 0x7472_6976;
pub const VIRTIO_MMIO_VERSION: u32 = 2;
pub const VIRTIO_MMIO_VENDOR_ID: u32 = 0;
pub const VIRTIO_MMIO_REGISTER_ACCESS_SIZE: usize = 4;

const VIRTIO_MMIO_REGISTER_ACCESS_SIZE_U64: u64 = 4;

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

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::{
        VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VIRTIO_MMIO_MAGIC_VALUE,
        VIRTIO_MMIO_NOTIFY_OFFSET, VIRTIO_MMIO_REGISTER_ACCESS_SIZE,
        VIRTIO_MMIO_REGISTER_SPACE_SIZE, VIRTIO_MMIO_VENDOR_ID, VIRTIO_MMIO_VERSION,
        VirtioMmioAccess, VirtioMmioAccessError, VirtioMmioRegister, decode_virtio_mmio_access,
    };
    use crate::memory::GuestAddress;
    use crate::mmio::{MmioAccessBytes, MmioBus, MmioOperation, MmioOperationKind, MmioRegionId};

    const BASE: u64 = 0x1000_0000;

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
