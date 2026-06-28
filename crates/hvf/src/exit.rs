//! Typed snapshots of Hypervisor.framework vCPU exits.

use std::fmt;

use bangbang_runtime::memory::{GuestAddress, GuestMemoryError, GuestMemoryRange};

const ESR_EC_SHIFT: u64 = 26;
const ESR_EC_MASK: u64 = 0x3f;
const ESR_EC_DATA_ABORT_LOWER_EL: u8 = 0x24;
const ESR_ISS_ISV: u64 = 1 << 24;
const ESR_ISS_SAS_SHIFT: u64 = 22;
const ESR_ISS_SAS_MASK: u64 = 0x3;
const ESR_ISS_SSE: u64 = 1 << 21;
const ESR_ISS_SRT_SHIFT: u64 = 16;
const ESR_ISS_SRT_MASK: u64 = 0x1f;
const ESR_ISS_SF: u64 = 1 << 15;
const ESR_ISS_CM: u64 = 1 << 8;
const ESR_ISS_S1PTW: u64 = 1 << 7;
const ESR_ISS_WNR: u64 = 1 << 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfExceptionExit {
    pub syndrome: u64,
    pub virtual_address: u64,
    pub physical_address: u64,
}

impl HvfExceptionExit {
    pub fn decode_mmio_access(self) -> Result<HvfMmioAccess, HvfMmioDecodeError> {
        let exception_class = exception_class(self.syndrome);
        if exception_class != ESR_EC_DATA_ABORT_LOWER_EL {
            return Err(HvfMmioDecodeError::UnsupportedExceptionClass { exception_class });
        }

        if self.syndrome & ESR_ISS_ISV == 0 {
            return Err(HvfMmioDecodeError::MissingInstructionSyndrome {
                syndrome: self.syndrome,
            });
        }

        if self.syndrome & (ESR_ISS_CM | ESR_ISS_S1PTW) != 0 {
            return Err(HvfMmioDecodeError::UnsupportedDataAbort {
                syndrome: self.syndrome,
            });
        }

        let size =
            HvfMmioAccessSize::from_sas((self.syndrome >> ESR_ISS_SAS_SHIFT) & ESR_ISS_SAS_MASK);
        let start = GuestAddress::new(self.physical_address);
        let range = GuestMemoryRange::new(start, size.bytes()).map_err(|source| {
            HvfMmioDecodeError::InvalidAccessRange {
                physical_address: start,
                size: size.bytes(),
                source,
            }
        })?;
        let raw_register = ((self.syndrome >> ESR_ISS_SRT_SHIFT) & ESR_ISS_SRT_MASK) as u8;

        Ok(HvfMmioAccess {
            range,
            size,
            direction: if self.syndrome & ESR_ISS_WNR == 0 {
                HvfMmioDirection::Read
            } else {
                HvfMmioDirection::Write
            },
            register: HvfMmioRegister(raw_register),
            sign_extend: self.syndrome & ESR_ISS_SSE != 0,
            register_width: if self.syndrome & ESR_ISS_SF == 0 {
                HvfMmioRegisterWidth::Bits32
            } else {
                HvfMmioRegisterWidth::Bits64
            },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfMmioAccess {
    range: GuestMemoryRange,
    size: HvfMmioAccessSize,
    direction: HvfMmioDirection,
    register: HvfMmioRegister,
    sign_extend: bool,
    register_width: HvfMmioRegisterWidth,
}

impl HvfMmioAccess {
    pub const fn address(self) -> GuestAddress {
        self.range.start()
    }

    pub const fn range(self) -> GuestMemoryRange {
        self.range
    }

    pub const fn size(self) -> HvfMmioAccessSize {
        self.size
    }

    pub const fn direction(self) -> HvfMmioDirection {
        self.direction
    }

    pub const fn register(self) -> HvfMmioRegister {
        self.register
    }

    pub const fn sign_extend(self) -> bool {
        self.sign_extend
    }

    pub const fn register_width(self) -> HvfMmioRegisterWidth {
        self.register_width
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfMmioDirection {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfMmioAccessSize {
    Byte,
    Halfword,
    Word,
    Doubleword,
}

impl HvfMmioAccessSize {
    const fn from_sas(value: u64) -> Self {
        match value {
            0 => Self::Byte,
            1 => Self::Halfword,
            2 => Self::Word,
            _ => Self::Doubleword,
        }
    }

    pub const fn bytes(self) -> u64 {
        match self {
            Self::Byte => 1,
            Self::Halfword => 2,
            Self::Word => 4,
            Self::Doubleword => 8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfMmioRegister(u8);

impl HvfMmioRegister {
    pub const fn new(value: u8) -> Option<Self> {
        if value <= ESR_ISS_SRT_MASK as u8 {
            Some(Self(value))
        } else {
            None
        }
    }

    pub const fn raw_value(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfMmioRegisterWidth {
    Bits32,
    Bits64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfMmioDecodeError {
    UnsupportedExceptionClass {
        exception_class: u8,
    },
    MissingInstructionSyndrome {
        syndrome: u64,
    },
    UnsupportedDataAbort {
        syndrome: u64,
    },
    InvalidAccessRange {
        physical_address: GuestAddress,
        size: u64,
        source: GuestMemoryError,
    },
}

impl fmt::Display for HvfMmioDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedExceptionClass { exception_class } => {
                write!(
                    f,
                    "unsupported HVF exception class 0x{exception_class:x} for MMIO decode"
                )
            }
            Self::MissingInstructionSyndrome { syndrome } => {
                write!(
                    f,
                    "HVF data abort syndrome 0x{syndrome:x} does not include instruction syndrome metadata"
                )
            }
            Self::UnsupportedDataAbort { syndrome } => {
                write!(
                    f,
                    "unsupported HVF data abort syndrome 0x{syndrome:x} for MMIO decode"
                )
            }
            Self::InvalidAccessRange {
                physical_address,
                size,
                source,
            } => {
                write!(
                    f,
                    "invalid HVF MMIO access range at {physical_address} with size {size}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for HvfMmioDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidAccessRange { source, .. } => Some(source),
            Self::UnsupportedExceptionClass { .. }
            | Self::MissingInstructionSyndrome { .. }
            | Self::UnsupportedDataAbort { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfVcpuExit {
    Canceled,
    Exception(HvfExceptionExit),
    VtimerActivated,
    Unknown { reason: u32 },
}

impl HvfVcpuExit {
    pub(crate) fn from_raw(exit: crate::ffi::HvVcpuExit) -> Self {
        match exit.reason {
            crate::ffi::HV_EXIT_REASON_CANCELED => Self::Canceled,
            crate::ffi::HV_EXIT_REASON_EXCEPTION => Self::Exception(HvfExceptionExit {
                syndrome: exit.exception.syndrome,
                virtual_address: exit.exception.virtual_address,
                physical_address: exit.exception.physical_address,
            }),
            crate::ffi::HV_EXIT_REASON_VTIMER_ACTIVATED => Self::VtimerActivated,
            crate::ffi::HV_EXIT_REASON_UNKNOWN => Self::Unknown {
                reason: crate::ffi::HV_EXIT_REASON_UNKNOWN,
            },
            reason => Self::Unknown { reason },
        }
    }
}

fn exception_class(syndrome: u64) -> u8 {
    ((syndrome >> ESR_EC_SHIFT) & ESR_EC_MASK) as u8
}

#[cfg(test)]
mod tests {
    use super::{
        ESR_EC_DATA_ABORT_LOWER_EL, ESR_EC_SHIFT, ESR_ISS_CM, ESR_ISS_ISV, ESR_ISS_S1PTW,
        ESR_ISS_SAS_SHIFT, ESR_ISS_SF, ESR_ISS_SRT_SHIFT, ESR_ISS_SSE, ESR_ISS_WNR,
        HvfExceptionExit, HvfMmioAccessSize, HvfMmioDecodeError, HvfMmioDirection, HvfMmioRegister,
        HvfMmioRegisterWidth, HvfVcpuExit,
    };
    use bangbang_runtime::memory::{GuestAddress, GuestMemoryError, GuestMemoryRange};

    fn raw_exit(reason: u32) -> crate::ffi::HvVcpuExit {
        crate::ffi::HvVcpuExit {
            reason,
            exception: crate::ffi::HvVcpuExitException {
                syndrome: 0x11,
                virtual_address: 0x22,
                physical_address: 0x33,
            },
        }
    }

    fn exception_exit(syndrome: u64, physical_address: u64) -> HvfExceptionExit {
        HvfExceptionExit {
            syndrome,
            virtual_address: 0x22,
            physical_address,
        }
    }

    fn data_abort_syndrome(
        size: HvfMmioAccessSize,
        direction: HvfMmioDirection,
        register: HvfMmioRegister,
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

        (u64::from(ESR_EC_DATA_ABORT_LOWER_EL) << ESR_EC_SHIFT)
            | ESR_ISS_ISV
            | (size_bits << ESR_ISS_SAS_SHIFT)
            | (u64::from(register.raw_value()) << ESR_ISS_SRT_SHIFT)
            | write_bit
    }

    fn range(start: u64, size: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(GuestAddress::new(start), size).expect("test range should be valid")
    }

    #[test]
    fn converts_canceled_exit() {
        assert_eq!(
            HvfVcpuExit::from_raw(raw_exit(crate::ffi::HV_EXIT_REASON_CANCELED)),
            HvfVcpuExit::Canceled
        );
    }

    #[test]
    fn converts_exception_exit() {
        assert_eq!(
            HvfVcpuExit::from_raw(raw_exit(crate::ffi::HV_EXIT_REASON_EXCEPTION)),
            HvfVcpuExit::Exception(HvfExceptionExit {
                syndrome: 0x11,
                virtual_address: 0x22,
                physical_address: 0x33,
            })
        );
    }

    #[test]
    fn converts_vtimer_activated_exit() {
        assert_eq!(
            HvfVcpuExit::from_raw(raw_exit(crate::ffi::HV_EXIT_REASON_VTIMER_ACTIVATED)),
            HvfVcpuExit::VtimerActivated
        );
    }

    #[test]
    fn preserves_sdk_unknown_exit_reason() {
        assert_eq!(
            HvfVcpuExit::from_raw(raw_exit(crate::ffi::HV_EXIT_REASON_UNKNOWN)),
            HvfVcpuExit::Unknown {
                reason: crate::ffi::HV_EXIT_REASON_UNKNOWN
            }
        );
    }

    #[test]
    fn preserves_future_unknown_exit_reason() {
        assert_eq!(
            HvfVcpuExit::from_raw(raw_exit(99)),
            HvfVcpuExit::Unknown { reason: 99 }
        );
    }

    #[test]
    fn decodes_mmio_write_access() {
        let register = HvfMmioRegister::new(3).expect("register should be valid");
        let access = exception_exit(
            data_abort_syndrome(HvfMmioAccessSize::Word, HvfMmioDirection::Write, register),
            0x1000,
        )
        .decode_mmio_access()
        .expect("write access should decode");

        assert_eq!(access.address(), GuestAddress::new(0x1000));
        assert_eq!(access.range(), range(0x1000, 4));
        assert_eq!(access.size(), HvfMmioAccessSize::Word);
        assert_eq!(access.direction(), HvfMmioDirection::Write);
        assert_eq!(access.register(), register);
        assert!(!access.sign_extend());
        assert_eq!(access.register_width(), HvfMmioRegisterWidth::Bits32);
    }

    #[test]
    fn decodes_mmio_read_access() {
        let register = HvfMmioRegister::new(4).expect("register should be valid");
        let access = exception_exit(
            data_abort_syndrome(
                HvfMmioAccessSize::Doubleword,
                HvfMmioDirection::Read,
                register,
            ) | ESR_ISS_SF,
            0x2000,
        )
        .decode_mmio_access()
        .expect("read access should decode");

        assert_eq!(access.range(), range(0x2000, 8));
        assert_eq!(access.size().bytes(), 8);
        assert_eq!(access.direction(), HvfMmioDirection::Read);
        assert_eq!(access.register(), register);
        assert_eq!(access.register_width(), HvfMmioRegisterWidth::Bits64);
    }

    #[test]
    fn decodes_all_supported_mmio_access_sizes() {
        for (size, bytes) in [
            (HvfMmioAccessSize::Byte, 1),
            (HvfMmioAccessSize::Halfword, 2),
            (HvfMmioAccessSize::Word, 4),
            (HvfMmioAccessSize::Doubleword, 8),
        ] {
            let access = exception_exit(
                data_abort_syndrome(
                    size,
                    HvfMmioDirection::Read,
                    HvfMmioRegister::new(0).expect("register should be valid"),
                ),
                0x3000,
            )
            .decode_mmio_access()
            .expect("access size should decode");

            assert_eq!(access.size(), size);
            assert_eq!(access.size().bytes(), bytes);
            assert_eq!(access.range(), range(0x3000, bytes));
        }
    }

    #[test]
    fn decodes_access_ending_at_max_exclusive_address() {
        let access = exception_exit(
            data_abort_syndrome(
                HvfMmioAccessSize::Byte,
                HvfMmioDirection::Read,
                HvfMmioRegister::new(0).expect("register should be valid"),
            ),
            u64::MAX - 1,
        )
        .decode_mmio_access()
        .expect("access ending at max exclusive address should decode");

        assert_eq!(access.range(), range(u64::MAX - 1, 1));
    }

    #[test]
    fn decodes_raw_register_boundaries() {
        let low = exception_exit(
            data_abort_syndrome(
                HvfMmioAccessSize::Byte,
                HvfMmioDirection::Read,
                HvfMmioRegister::new(0).expect("register should be valid"),
            ),
            0x4000,
        )
        .decode_mmio_access()
        .expect("register zero should decode");
        let high = exception_exit(
            data_abort_syndrome(
                HvfMmioAccessSize::Byte,
                HvfMmioDirection::Read,
                HvfMmioRegister::new(31).expect("register should be valid"),
            ),
            0x4000,
        )
        .decode_mmio_access()
        .expect("register thirty-one should decode");

        assert_eq!(low.register().raw_value(), 0);
        assert_eq!(high.register().raw_value(), 31);
        assert_eq!(HvfMmioRegister::new(32), None);
    }

    #[test]
    fn preserves_read_extension_metadata() {
        let access = exception_exit(
            data_abort_syndrome(
                HvfMmioAccessSize::Halfword,
                HvfMmioDirection::Read,
                HvfMmioRegister::new(5).expect("register should be valid"),
            ) | ESR_ISS_SSE
                | ESR_ISS_SF,
            0x5000,
        )
        .decode_mmio_access()
        .expect("sign-extending read should decode");

        assert!(access.sign_extend());
        assert_eq!(access.register_width(), HvfMmioRegisterWidth::Bits64);
    }

    #[test]
    fn rejects_non_data_abort_exception_class() {
        let err = exception_exit(ESR_ISS_ISV, 0x1000)
            .decode_mmio_access()
            .expect_err("non-data-abort exception should fail");

        assert_eq!(
            err,
            HvfMmioDecodeError::UnsupportedExceptionClass { exception_class: 0 }
        );
    }

    #[test]
    fn rejects_data_abort_from_same_exception_level() {
        let same_el_data_abort = (0x25 << ESR_EC_SHIFT) | ESR_ISS_ISV;
        let err = exception_exit(same_el_data_abort, 0x1000)
            .decode_mmio_access()
            .expect_err("same-EL data abort should fail");

        assert_eq!(
            err,
            HvfMmioDecodeError::UnsupportedExceptionClass {
                exception_class: 0x25
            }
        );
    }

    #[test]
    fn rejects_data_abort_without_instruction_syndrome() {
        let syndrome = u64::from(ESR_EC_DATA_ABORT_LOWER_EL) << ESR_EC_SHIFT;
        let err = exception_exit(syndrome, 0x1000)
            .decode_mmio_access()
            .expect_err("missing instruction syndrome should fail");

        assert_eq!(
            err,
            HvfMmioDecodeError::MissingInstructionSyndrome { syndrome }
        );
    }

    #[test]
    fn rejects_stage_one_translation_table_walk_abort() {
        let syndrome = data_abort_syndrome(
            HvfMmioAccessSize::Word,
            HvfMmioDirection::Read,
            HvfMmioRegister::new(1).expect("register should be valid"),
        ) | ESR_ISS_S1PTW;
        let err = exception_exit(syndrome, 0x1000)
            .decode_mmio_access()
            .expect_err("stage-one table-walk abort should fail");

        assert_eq!(err, HvfMmioDecodeError::UnsupportedDataAbort { syndrome });
    }

    #[test]
    fn rejects_cache_maintenance_abort() {
        let syndrome = data_abort_syndrome(
            HvfMmioAccessSize::Word,
            HvfMmioDirection::Read,
            HvfMmioRegister::new(1).expect("register should be valid"),
        ) | ESR_ISS_CM;
        let err = exception_exit(syndrome, 0x1000)
            .decode_mmio_access()
            .expect_err("cache-maintenance abort should fail");

        assert_eq!(err, HvfMmioDecodeError::UnsupportedDataAbort { syndrome });
    }

    #[test]
    fn rejects_access_range_overflow() {
        let syndrome = data_abort_syndrome(
            HvfMmioAccessSize::Halfword,
            HvfMmioDirection::Read,
            HvfMmioRegister::new(1).expect("register should be valid"),
        );
        let err = exception_exit(syndrome, u64::MAX)
            .decode_mmio_access()
            .expect_err("overflowing access range should fail");

        assert_eq!(
            err,
            HvfMmioDecodeError::InvalidAccessRange {
                physical_address: GuestAddress::new(u64::MAX),
                size: 2,
                source: GuestMemoryError::AddressOverflow {
                    start: GuestAddress::new(u64::MAX),
                    size: 2
                }
            }
        );
    }

    #[test]
    fn displays_mmio_decode_errors() {
        let err = HvfMmioDecodeError::UnsupportedExceptionClass {
            exception_class: 0x1,
        };

        assert_eq!(
            err.to_string(),
            "unsupported HVF exception class 0x1 for MMIO decode"
        );
    }
}
