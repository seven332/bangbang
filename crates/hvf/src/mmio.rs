//! HVF-specific MMIO operation construction and read completion.

use std::fmt;

use bangbang_runtime::{
    BackendError,
    mmio::{MmioAccessBytes, MmioAccessBytesError, MmioOperation, MmioOperationError},
};

use crate::exit::{
    HvfMmioAccessSize, HvfMmioDirection, HvfMmioRegister, HvfMmioRegisterWidth,
    HvfResolvedMmioAccess,
};
use crate::vcpu::HvfRegister;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfMmioCompletionError {
    UnsupportedRegister {
        register: HvfMmioRegister,
    },
    InvalidDirection {
        expected: HvfMmioDirection,
        actual: HvfMmioDirection,
    },
    ReadDataLengthMismatch {
        access: HvfResolvedMmioAccess,
        expected: usize,
        actual: usize,
    },
    RegisterReadFailed {
        register: HvfRegister,
        source: BackendError,
    },
    RegisterWriteFailed {
        register: HvfRegister,
        source: BackendError,
    },
    AccessBytes {
        source: MmioAccessBytesError,
    },
    Operation {
        source: MmioOperationError,
    },
}

impl fmt::Display for HvfMmioCompletionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedRegister { register } => {
                write!(
                    f,
                    "HVF MMIO access uses unsupported guest GPR {}",
                    register.raw_value()
                )
            }
            Self::InvalidDirection { expected, actual } => {
                write!(
                    f,
                    "HVF MMIO completion expected a {expected:?} access but got {actual:?}"
                )
            }
            Self::ReadDataLengthMismatch {
                access,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "HVF MMIO read completion for range {} returned {actual} bytes; expected {expected}",
                    access.range()
                )
            }
            Self::RegisterReadFailed { register, source } => {
                write!(
                    f,
                    "failed to read HVF register {} for MMIO completion: {source}",
                    register.raw()
                )
            }
            Self::RegisterWriteFailed { register, source } => {
                write!(
                    f,
                    "failed to write HVF register {} for MMIO completion: {source}",
                    register.raw()
                )
            }
            Self::AccessBytes { source } => {
                write!(f, "failed to build HVF MMIO access bytes: {source}")
            }
            Self::Operation { source } => {
                write!(f, "failed to build runtime MMIO operation: {source}")
            }
        }
    }
}

impl std::error::Error for HvfMmioCompletionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RegisterReadFailed { source, .. } | Self::RegisterWriteFailed { source, .. } => {
                Some(source)
            }
            Self::AccessBytes { source } => Some(source),
            Self::Operation { source } => Some(source),
            Self::UnsupportedRegister { .. }
            | Self::InvalidDirection { .. }
            | Self::ReadDataLengthMismatch { .. } => None,
        }
    }
}

pub(crate) fn build_mmio_operation(
    access: HvfResolvedMmioAccess,
    mut read_register: impl FnMut(HvfRegister) -> Result<u64, BackendError>,
) -> Result<MmioOperation, HvfMmioCompletionError> {
    match access.direction() {
        HvfMmioDirection::Read => {
            let _register = mmio_gpr(access.register())?;

            MmioOperation::read(access.runtime_access())
                .map_err(|source| HvfMmioCompletionError::Operation { source })
        }
        HvfMmioDirection::Write => {
            let register = mmio_gpr(access.register())?;
            let value = read_register(register).map_err(|source| {
                HvfMmioCompletionError::RegisterReadFailed { register, source }
            })?;
            let data = register_value_to_access_bytes(value, access.size())?;

            MmioOperation::write(access.runtime_access(), data)
                .map_err(|source| HvfMmioCompletionError::Operation { source })
        }
    }
}

pub(crate) fn complete_mmio_read(
    access: HvfResolvedMmioAccess,
    data: MmioAccessBytes,
    mut set_register: impl FnMut(HvfRegister, u64) -> Result<(), BackendError>,
) -> Result<(), HvfMmioCompletionError> {
    if access.direction() != HvfMmioDirection::Read {
        return Err(HvfMmioCompletionError::InvalidDirection {
            expected: HvfMmioDirection::Read,
            actual: access.direction(),
        });
    }

    let expected = access_size_len(access.size());
    if data.len() != expected {
        return Err(HvfMmioCompletionError::ReadDataLengthMismatch {
            access,
            expected,
            actual: data.len(),
        });
    }

    let register = mmio_gpr(access.register())?;
    let value = read_data_register_value(data, access.sign_extend(), access.register_width());
    set_register(register, value)
        .map_err(|source| HvfMmioCompletionError::RegisterWriteFailed { register, source })
}

fn mmio_gpr(register: HvfMmioRegister) -> Result<HvfRegister, HvfMmioCompletionError> {
    HvfRegister::general_purpose(register.raw_value())
        .ok_or(HvfMmioCompletionError::UnsupportedRegister { register })
}

fn register_value_to_access_bytes(
    value: u64,
    size: HvfMmioAccessSize,
) -> Result<MmioAccessBytes, HvfMmioCompletionError> {
    let bytes = value.to_le_bytes();
    let (selected, _) = bytes.split_at(access_size_len(size));

    MmioAccessBytes::new(selected).map_err(|source| HvfMmioCompletionError::AccessBytes { source })
}

const fn access_size_len(size: HvfMmioAccessSize) -> usize {
    match size {
        HvfMmioAccessSize::Byte => 1,
        HvfMmioAccessSize::Halfword => 2,
        HvfMmioAccessSize::Word => 4,
        HvfMmioAccessSize::Doubleword => 8,
    }
}

fn read_data_register_value(
    data: MmioAccessBytes,
    sign_extend: bool,
    register_width: HvfMmioRegisterWidth,
) -> u64 {
    let value = if sign_extend {
        sign_extended_read_value(data)
    } else {
        zero_extended_read_value(data)
    };

    match register_width {
        HvfMmioRegisterWidth::Bits32 => value & u64::from(u32::MAX),
        HvfMmioRegisterWidth::Bits64 => value,
    }
}

fn zero_extended_read_value(data: MmioAccessBytes) -> u64 {
    let mut bytes = [0; 8];
    let (destination, _) = bytes.split_at_mut(data.len());
    destination.copy_from_slice(data.as_slice());
    u64::from_le_bytes(bytes)
}

fn sign_extended_read_value(data: MmioAccessBytes) -> u64 {
    match data.as_slice() {
        [byte] => i64::from(i8::from_le_bytes([*byte])) as u64,
        [low, high] => i64::from(i16::from_le_bytes([*low, *high])) as u64,
        [b0, b1, b2, b3] => i64::from(i32::from_le_bytes([*b0, *b1, *b2, *b3])) as u64,
        [b0, b1, b2, b3, b4, b5, b6, b7] => {
            i64::from_le_bytes([*b0, *b1, *b2, *b3, *b4, *b5, *b6, *b7]) as u64
        }
        _ => zero_extended_read_value(data),
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use bangbang_runtime::{
        BackendError,
        memory::GuestAddress,
        mmio::{MmioBus, MmioOperation, MmioRegionId},
    };

    use super::{build_mmio_operation, complete_mmio_read};
    use crate::exit::{
        HvfExceptionExit, HvfMmioAccessSize, HvfMmioDirection, HvfMmioRegister,
        HvfMmioRegisterWidth, HvfResolvedVcpuExit, HvfVcpuExit,
    };
    use crate::mmio::HvfMmioCompletionError;
    use crate::vcpu::HvfRegister;

    const ESR_EC_DATA_ABORT_LOWER_EL: u64 = 0x24;
    const ESR_EC_SHIFT: u64 = 26;
    const ESR_ISS_ISV: u64 = 1 << 24;
    const ESR_ISS_SAS_SHIFT: u64 = 22;
    const ESR_ISS_SRT_SHIFT: u64 = 16;
    const ESR_ISS_SSE: u64 = 1 << 21;
    const ESR_ISS_SF: u64 = 1 << 15;
    const ESR_ISS_WNR: u64 = 1 << 6;

    fn resolved_access(
        size: HvfMmioAccessSize,
        direction: HvfMmioDirection,
        raw_register: u8,
        sign_extend: bool,
        register_width: HvfMmioRegisterWidth,
    ) -> crate::exit::HvfResolvedMmioAccess {
        let mut bus = MmioBus::new();
        bus.insert(MmioRegionId::new(7), GuestAddress::new(0x1000), 0x100)
            .expect("test region should insert");
        let register = HvfMmioRegister::new(raw_register).expect("test register should decode");
        let exit = HvfVcpuExit::Exception(HvfExceptionExit {
            syndrome: data_abort_syndrome(size, direction, register, sign_extend, register_width),
            virtual_address: 0x2000,
            physical_address: 0x1040,
        });

        let resolved = exit
            .resolve_with_mmio_bus(&bus)
            .expect("test access should resolve");
        let HvfResolvedVcpuExit::Mmio(access) = resolved else {
            panic!("expected MMIO exit");
        };
        access
    }

    fn data_abort_syndrome(
        size: HvfMmioAccessSize,
        direction: HvfMmioDirection,
        register: HvfMmioRegister,
        sign_extend: bool,
        register_width: HvfMmioRegisterWidth,
    ) -> u64 {
        let size_bits = match size {
            HvfMmioAccessSize::Byte => 0,
            HvfMmioAccessSize::Halfword => 1,
            HvfMmioAccessSize::Word => 2,
            HvfMmioAccessSize::Doubleword => 3,
        };
        let write_bit = match direction {
            HvfMmioDirection::Read => 0,
            HvfMmioDirection::Write => ESR_ISS_WNR,
        };
        let sign_extend_bit = if sign_extend { ESR_ISS_SSE } else { 0 };
        let register_width_bit = match register_width {
            HvfMmioRegisterWidth::Bits32 => 0,
            HvfMmioRegisterWidth::Bits64 => ESR_ISS_SF,
        };

        (ESR_EC_DATA_ABORT_LOWER_EL << ESR_EC_SHIFT)
            | ESR_ISS_ISV
            | (size_bits << ESR_ISS_SAS_SHIFT)
            | (u64::from(register.raw_value()) << ESR_ISS_SRT_SHIFT)
            | write_bit
            | sign_extend_bit
            | register_width_bit
    }

    fn bytes(value: &[u8]) -> bangbang_runtime::mmio::MmioAccessBytes {
        bangbang_runtime::mmio::MmioAccessBytes::new(value).expect("test bytes should be valid")
    }

    #[test]
    fn read_access_builds_runtime_read_operation_without_register_read() {
        let access = resolved_access(
            HvfMmioAccessSize::Word,
            HvfMmioDirection::Read,
            4,
            false,
            HvfMmioRegisterWidth::Bits32,
        );
        let operation = build_mmio_operation(access, |_| {
            Err(BackendError::InvalidState("read register should not run"))
        })
        .expect("read operation should build");

        assert_eq!(
            operation,
            MmioOperation::read(access.runtime_access()).expect("runtime read should build")
        );
    }

    #[test]
    fn write_access_uses_low_little_endian_register_bytes_for_each_size() {
        let register_value = 0x1122_3344_5566_7788;
        for (size, expected) in [
            (HvfMmioAccessSize::Byte, vec![0x88]),
            (HvfMmioAccessSize::Halfword, vec![0x88, 0x77]),
            (HvfMmioAccessSize::Word, vec![0x88, 0x77, 0x66, 0x55]),
            (
                HvfMmioAccessSize::Doubleword,
                vec![0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11],
            ),
        ] {
            let access = resolved_access(
                size,
                HvfMmioDirection::Write,
                5,
                false,
                HvfMmioRegisterWidth::Bits64,
            );
            let operation =
                build_mmio_operation(access, |_| Ok(register_value)).expect("write should build");

            let MmioOperation::Write { data, .. } = operation else {
                panic!("expected write operation");
            };
            assert_eq!(data.as_slice(), expected.as_slice());
        }
    }

    #[test]
    fn write_access_maps_boundary_guest_registers() {
        for (raw_register, expected_register) in
            [(0, crate::ffi::HV_REG_X0), (30, crate::ffi::HV_REG_X0 + 30)]
        {
            let access = resolved_access(
                HvfMmioAccessSize::Byte,
                HvfMmioDirection::Write,
                raw_register,
                false,
                HvfMmioRegisterWidth::Bits64,
            );
            let mut observed = None;

            build_mmio_operation(access, |register| {
                observed = Some(register);
                Ok(0)
            })
            .expect("boundary register should build");

            assert_eq!(observed.map(HvfRegister::raw), Some(expected_register));
        }
    }

    #[test]
    fn write_access_rejects_guest_register_thirty_one() {
        let access = resolved_access(
            HvfMmioAccessSize::Byte,
            HvfMmioDirection::Write,
            31,
            false,
            HvfMmioRegisterWidth::Bits64,
        );
        let mut register_read = false;
        let err = build_mmio_operation(access, |_| {
            register_read = true;
            Ok(0)
        })
        .expect_err("register thirty-one should be rejected");

        assert_eq!(
            err,
            HvfMmioCompletionError::UnsupportedRegister {
                register: HvfMmioRegister::new(31).expect("register should exist")
            }
        );
        assert!(!register_read);
    }

    #[test]
    fn read_access_rejects_guest_register_thirty_one_before_operation_build() {
        let access = resolved_access(
            HvfMmioAccessSize::Byte,
            HvfMmioDirection::Read,
            31,
            false,
            HvfMmioRegisterWidth::Bits64,
        );
        let err = build_mmio_operation(access, |_| {
            Err(BackendError::InvalidState("read register should not run"))
        })
        .expect_err("register thirty-one should be rejected");

        assert_eq!(
            err,
            HvfMmioCompletionError::UnsupportedRegister {
                register: HvfMmioRegister::new(31).expect("register should exist")
            }
        );
    }

    #[test]
    fn write_access_preserves_register_read_errors() {
        let access = resolved_access(
            HvfMmioAccessSize::Word,
            HvfMmioDirection::Write,
            2,
            false,
            HvfMmioRegisterWidth::Bits32,
        );
        let source = BackendError::InvalidState("fake register read failed");
        let err = build_mmio_operation(access, |_| Err(source.clone()))
            .expect_err("register read failure should be preserved");

        assert_eq!(
            err,
            HvfMmioCompletionError::RegisterReadFailed {
                register: HvfRegister::general_purpose(2).expect("register should map"),
                source: source.clone()
            }
        );
        assert_eq!(
            err.source().and_then(|source| source.downcast_ref()),
            Some(&source)
        );
    }

    #[test]
    fn read_completion_zero_extends_unsigned_values() {
        let access = resolved_access(
            HvfMmioAccessSize::Word,
            HvfMmioDirection::Read,
            3,
            false,
            HvfMmioRegisterWidth::Bits64,
        );
        let mut write = None;

        complete_mmio_read(access, bytes(&[0, 0, 0, 0x80]), |register, value| {
            write = Some((register, value));
            Ok(())
        })
        .expect("read completion should write register");

        assert_eq!(
            write,
            Some((
                HvfRegister::general_purpose(3).expect("register should map"),
                0x8000_0000
            ))
        );
    }

    #[test]
    fn read_completion_sign_extends_to_sixty_four_bits() {
        let access = resolved_access(
            HvfMmioAccessSize::Byte,
            HvfMmioDirection::Read,
            4,
            true,
            HvfMmioRegisterWidth::Bits64,
        );
        let mut value = None;

        complete_mmio_read(access, bytes(&[0x80]), |_, written| {
            value = Some(written);
            Ok(())
        })
        .expect("read completion should sign extend");

        assert_eq!(value, Some(0xffff_ffff_ffff_ff80));
    }

    #[test]
    fn read_completion_sign_extends_and_truncates_to_thirty_two_bits() {
        let access = resolved_access(
            HvfMmioAccessSize::Halfword,
            HvfMmioDirection::Read,
            4,
            true,
            HvfMmioRegisterWidth::Bits32,
        );
        let mut value = None;

        complete_mmio_read(access, bytes(&[0, 0x80]), |_, written| {
            value = Some(written);
            Ok(())
        })
        .expect("read completion should sign extend and truncate");

        assert_eq!(value, Some(0xffff_8000));
    }

    #[test]
    fn read_completion_preserves_doubleword_bits() {
        let access = resolved_access(
            HvfMmioAccessSize::Doubleword,
            HvfMmioDirection::Read,
            6,
            true,
            HvfMmioRegisterWidth::Bits64,
        );
        let mut value = None;

        complete_mmio_read(access, bytes(&[0, 1, 2, 3, 4, 5, 6, 0x80]), |_, written| {
            value = Some(written);
            Ok(())
        })
        .expect("read completion should preserve doubleword bits");

        assert_eq!(value, Some(0x8006_0504_0302_0100));
    }

    #[test]
    fn read_completion_rejects_write_access() {
        let access = resolved_access(
            HvfMmioAccessSize::Word,
            HvfMmioDirection::Write,
            4,
            false,
            HvfMmioRegisterWidth::Bits32,
        );
        let err = complete_mmio_read(access, bytes(&[1, 2, 3, 4]), |_, _| Ok(()))
            .expect_err("write access should be rejected");

        assert_eq!(
            err,
            HvfMmioCompletionError::InvalidDirection {
                expected: HvfMmioDirection::Read,
                actual: HvfMmioDirection::Write
            }
        );
    }

    #[test]
    fn read_completion_rejects_mismatched_data_length() {
        let access = resolved_access(
            HvfMmioAccessSize::Word,
            HvfMmioDirection::Read,
            4,
            false,
            HvfMmioRegisterWidth::Bits32,
        );
        let mut register_write = false;
        let err = complete_mmio_read(access, bytes(&[1]), |_, _| {
            register_write = true;
            Ok(())
        })
        .expect_err("short read data should be rejected");

        assert_eq!(
            err,
            HvfMmioCompletionError::ReadDataLengthMismatch {
                access,
                expected: 4,
                actual: 1
            }
        );
        assert!(!register_write);
    }

    #[test]
    fn read_completion_rejects_guest_register_thirty_one() {
        let access = resolved_access(
            HvfMmioAccessSize::Byte,
            HvfMmioDirection::Read,
            31,
            false,
            HvfMmioRegisterWidth::Bits64,
        );
        let mut register_write = false;
        let err = complete_mmio_read(access, bytes(&[0]), |_, _| {
            register_write = true;
            Ok(())
        })
        .expect_err("register thirty-one should be rejected");

        assert_eq!(
            err,
            HvfMmioCompletionError::UnsupportedRegister {
                register: HvfMmioRegister::new(31).expect("register should exist")
            }
        );
        assert!(!register_write);
    }

    #[test]
    fn read_completion_preserves_register_write_errors() {
        let access = resolved_access(
            HvfMmioAccessSize::Byte,
            HvfMmioDirection::Read,
            1,
            false,
            HvfMmioRegisterWidth::Bits64,
        );
        let source = BackendError::InvalidState("fake register write failed");
        let err = complete_mmio_read(access, bytes(&[0]), |_, _| Err(source.clone()))
            .expect_err("register write failure should be preserved");

        assert_eq!(
            err,
            HvfMmioCompletionError::RegisterWriteFailed {
                register: HvfRegister::general_purpose(1).expect("register should map"),
                source: source.clone()
            }
        );
        assert_eq!(
            err.source().and_then(|source| source.downcast_ref()),
            Some(&source)
        );
    }

    #[test]
    fn displays_completion_errors() {
        let err = HvfMmioCompletionError::UnsupportedRegister {
            register: HvfMmioRegister::new(31).expect("register should exist"),
        };
        assert_eq!(
            err.to_string(),
            "HVF MMIO access uses unsupported guest GPR 31"
        );

        let access = resolved_access(
            HvfMmioAccessSize::Word,
            HvfMmioDirection::Read,
            1,
            false,
            HvfMmioRegisterWidth::Bits64,
        );
        let err = HvfMmioCompletionError::ReadDataLengthMismatch {
            access,
            expected: 4,
            actual: 2,
        };
        assert_eq!(
            err.to_string(),
            "HVF MMIO read completion for range [0x1040..0x1044) (4 bytes) returned 2 bytes; expected 4"
        );
    }
}
