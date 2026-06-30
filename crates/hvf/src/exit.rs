//! Typed snapshots of Hypervisor.framework vCPU exits.

use std::fmt;

use bangbang_runtime::{
    memory::{GuestAddress, GuestMemoryError, GuestMemoryRange},
    mmio::{MmioAccess, MmioBus, MmioBusError, MmioRegionId},
};

const ESR_EC_SHIFT: u64 = 26;
const ESR_EC_MASK: u64 = 0x3f;
const ESR_EC_HVC: u8 = 0x16;
const ESR_EC_DATA_ABORT_LOWER_EL: u8 = 0x24;
const ESR_ISS_HVC_IMMEDIATE_MASK: u64 = 0xffff;
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
    pub fn decode_hvc(self) -> Result<HvfHvcExit, HvfHvcDecodeError> {
        let exception_class = exception_class(self.syndrome);
        if exception_class != ESR_EC_HVC {
            return Err(HvfHvcDecodeError::UnsupportedExceptionClass { exception_class });
        }

        Ok(HvfHvcExit {
            exit: self,
            immediate: (self.syndrome & ESR_ISS_HVC_IMMEDIATE_MASK) as u16,
        })
    }

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
pub struct HvfHvcExit {
    exit: HvfExceptionExit,
    immediate: u16,
}

impl HvfHvcExit {
    pub const fn exception_exit(self) -> HvfExceptionExit {
        self.exit
    }

    pub const fn immediate(self) -> u16 {
        self.immediate
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfHvcDecodeError {
    UnsupportedExceptionClass { exception_class: u8 },
}

impl fmt::Display for HvfHvcDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedExceptionClass { exception_class } => {
                write!(
                    f,
                    "unsupported HVF exception class 0x{exception_class:x} for HVC decode"
                )
            }
        }
    }
}

impl std::error::Error for HvfHvcDecodeError {}

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
    pub fn resolve(self, bus: &MmioBus) -> Result<HvfResolvedMmioAccess, HvfMmioResolveError> {
        let runtime_access = bus
            .lookup(self.address(), self.size().bytes())
            .map_err(|source| HvfMmioResolveError::BusLookup {
                access: self,
                source,
            })?;

        Ok(HvfResolvedMmioAccess {
            hvf_access: self,
            runtime_access,
        })
    }

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
pub struct HvfResolvedMmioAccess {
    hvf_access: HvfMmioAccess,
    runtime_access: MmioAccess,
}

impl HvfResolvedMmioAccess {
    pub const fn hvf_access(self) -> HvfMmioAccess {
        self.hvf_access
    }

    pub const fn runtime_access(self) -> MmioAccess {
        self.runtime_access
    }

    pub const fn region_id(self) -> MmioRegionId {
        self.runtime_access.region_id()
    }

    pub const fn offset(self) -> u64 {
        self.runtime_access.offset()
    }

    pub const fn range(self) -> GuestMemoryRange {
        self.runtime_access.range()
    }

    pub const fn direction(self) -> HvfMmioDirection {
        self.hvf_access.direction()
    }

    pub const fn size(self) -> HvfMmioAccessSize {
        self.hvf_access.size()
    }

    pub const fn register(self) -> HvfMmioRegister {
        self.hvf_access.register()
    }

    pub const fn sign_extend(self) -> bool {
        self.hvf_access.sign_extend()
    }

    pub const fn register_width(self) -> HvfMmioRegisterWidth {
        self.hvf_access.register_width()
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
pub enum HvfMmioResolveError {
    BusLookup {
        access: HvfMmioAccess,
        source: MmioBusError,
    },
}

impl fmt::Display for HvfMmioResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BusLookup { access, source } => {
                write!(
                    f,
                    "failed to resolve HVF MMIO access at {} with size {}: {source}",
                    access.address(),
                    access.size().bytes()
                )
            }
        }
    }
}

impl std::error::Error for HvfMmioResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BusLookup { source, .. } => Some(source),
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
    pub fn resolve_with_mmio_bus(
        self,
        bus: &MmioBus,
    ) -> Result<HvfResolvedVcpuExit, HvfVcpuExitResolveError> {
        match self {
            Self::Canceled => Ok(HvfResolvedVcpuExit::Canceled),
            Self::Exception(exit) => {
                if let Ok(hvc) = exit.decode_hvc() {
                    return Ok(HvfResolvedVcpuExit::Hvc(hvc));
                }

                let access = exit
                    .decode_mmio_access()
                    .map_err(|source| HvfVcpuExitResolveError::MmioDecode { exit, source })?;
                let access = access
                    .resolve(bus)
                    .map_err(|source| HvfVcpuExitResolveError::MmioResolve { source })?;
                Ok(HvfResolvedVcpuExit::Mmio(access))
            }
            Self::VtimerActivated => Ok(HvfResolvedVcpuExit::VtimerActivated),
            Self::Unknown { reason } => Ok(HvfResolvedVcpuExit::Unknown { reason }),
        }
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfResolvedVcpuExit {
    Canceled,
    Hvc(HvfHvcExit),
    Mmio(HvfResolvedMmioAccess),
    VtimerActivated,
    Unknown { reason: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfVcpuExitResolveError {
    MmioDecode {
        exit: HvfExceptionExit,
        source: HvfMmioDecodeError,
    },
    MmioResolve {
        source: HvfMmioResolveError,
    },
}

impl fmt::Display for HvfVcpuExitResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MmioDecode { source, .. } => {
                write!(
                    f,
                    "failed to decode HVF vCPU exception exit as MMIO: {source}"
                )
            }
            Self::MmioResolve { source } => {
                write!(f, "failed to resolve HVF vCPU MMIO exit: {source}")
            }
        }
    }
}

impl std::error::Error for HvfVcpuExitResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MmioDecode { source, .. } => Some(source),
            Self::MmioResolve { source } => Some(source),
        }
    }
}

fn exception_class(syndrome: u64) -> u8 {
    ((syndrome >> ESR_EC_SHIFT) & ESR_EC_MASK) as u8
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::{
        ESR_EC_DATA_ABORT_LOWER_EL, ESR_EC_HVC, ESR_EC_SHIFT, ESR_ISS_CM, ESR_ISS_ISV,
        ESR_ISS_S1PTW, ESR_ISS_SAS_SHIFT, ESR_ISS_SF, ESR_ISS_SRT_SHIFT, ESR_ISS_SSE, ESR_ISS_WNR,
        HvfExceptionExit, HvfHvcDecodeError, HvfMmioAccessSize, HvfMmioDecodeError,
        HvfMmioDirection, HvfMmioRegister, HvfMmioRegisterWidth, HvfMmioResolveError,
        HvfResolvedVcpuExit, HvfVcpuExit, HvfVcpuExitResolveError,
    };
    use bangbang_runtime::{
        memory::{GuestAddress, GuestMemoryError, GuestMemoryRange},
        mmio::{MmioBus, MmioBusError, MmioRegion, MmioRegionId},
    };

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

    fn hvc_syndrome(immediate: u16) -> u64 {
        (u64::from(ESR_EC_HVC) << ESR_EC_SHIFT) | u64::from(immediate)
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

    fn region_id(value: u64) -> MmioRegionId {
        MmioRegionId::new(value)
    }

    fn insert_region(bus: &mut MmioBus, id: u64, start: u64, size: u64) -> MmioRegion {
        bus.insert(region_id(id), GuestAddress::new(start), size)
            .expect("test MMIO region should insert")
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
    fn decodes_hvc_exception_exit() {
        let exit = exception_exit(hvc_syndrome(0x1234), 0);
        let hvc = exit.decode_hvc().expect("HVC exit should decode");

        assert_eq!(hvc.exception_exit(), exit);
        assert_eq!(hvc.immediate(), 0x1234);
    }

    #[test]
    fn rejects_non_hvc_exception_for_hvc_decode() {
        let exit = exception_exit(ESR_ISS_ISV, 0x1000);
        let err = exit
            .decode_hvc()
            .expect_err("non-HVC exception should not decode as HVC");

        assert_eq!(
            err,
            HvfHvcDecodeError::UnsupportedExceptionClass { exception_class: 0 }
        );
        assert_eq!(
            err.to_string(),
            "unsupported HVF exception class 0x0 for HVC decode"
        );
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
    fn decodes_largest_access_ending_at_max_exclusive_address() {
        let access = exception_exit(
            data_abort_syndrome(
                HvfMmioAccessSize::Doubleword,
                HvfMmioDirection::Read,
                HvfMmioRegister::new(0).expect("register should be valid"),
            ),
            u64::MAX - 8,
        )
        .decode_mmio_access()
        .expect("access ending at max exclusive address should decode");

        assert_eq!(access.range(), range(u64::MAX - 8, 8));
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
    fn resolves_mmio_read_access_against_runtime_bus() {
        let mut bus = MmioBus::new();
        let region = insert_region(&mut bus, 7, 0x1000, 0x100);
        let register = HvfMmioRegister::new(5).expect("register should be valid");
        let access = exception_exit(
            data_abort_syndrome(
                HvfMmioAccessSize::Halfword,
                HvfMmioDirection::Read,
                register,
            ) | ESR_ISS_SSE
                | ESR_ISS_SF,
            0x1040,
        )
        .decode_mmio_access()
        .expect("read access should decode");

        let resolved = access.resolve(&bus).expect("access should resolve");

        assert_eq!(resolved.hvf_access(), access);
        assert_eq!(resolved.runtime_access().region(), region);
        assert_eq!(resolved.region_id(), region_id(7));
        assert_eq!(resolved.offset(), 0x40);
        assert_eq!(resolved.range(), range(0x1040, 2));
        assert_eq!(resolved.direction(), HvfMmioDirection::Read);
        assert_eq!(resolved.size(), HvfMmioAccessSize::Halfword);
        assert_eq!(resolved.register(), register);
        assert!(resolved.sign_extend());
        assert_eq!(resolved.register_width(), HvfMmioRegisterWidth::Bits64);
    }

    #[test]
    fn resolves_mmio_write_access_against_runtime_bus() {
        let mut bus = MmioBus::new();
        insert_region(&mut bus, 9, 0x2000, 0x100);
        let register = HvfMmioRegister::new(6).expect("register should be valid");
        let access = exception_exit(
            data_abort_syndrome(HvfMmioAccessSize::Word, HvfMmioDirection::Write, register),
            0x2080,
        )
        .decode_mmio_access()
        .expect("write access should decode");

        let resolved = access.resolve(&bus).expect("access should resolve");

        assert_eq!(resolved.region_id(), region_id(9));
        assert_eq!(resolved.offset(), 0x80);
        assert_eq!(resolved.range(), range(0x2080, 4));
        assert_eq!(resolved.direction(), HvfMmioDirection::Write);
        assert_eq!(resolved.size(), HvfMmioAccessSize::Word);
        assert_eq!(resolved.register(), register);
        assert!(!resolved.sign_extend());
        assert_eq!(resolved.register_width(), HvfMmioRegisterWidth::Bits32);
    }

    #[test]
    fn resolves_vcpu_mmio_read_exit_against_runtime_bus() {
        let mut bus = MmioBus::new();
        insert_region(&mut bus, 11, 0x4000, 0x100);
        let register = HvfMmioRegister::new(7).expect("register should be valid");
        let resolved = HvfVcpuExit::Exception(exception_exit(
            data_abort_syndrome(
                HvfMmioAccessSize::Doubleword,
                HvfMmioDirection::Read,
                register,
            ) | ESR_ISS_SF,
            0x4040,
        ))
        .resolve_with_mmio_bus(&bus)
        .expect("vCPU MMIO read exit should resolve");

        let HvfResolvedVcpuExit::Mmio(access) = resolved else {
            panic!("expected resolved MMIO exit, got {resolved:?}");
        };
        assert_eq!(access.region_id(), region_id(11));
        assert_eq!(access.offset(), 0x40);
        assert_eq!(access.range(), range(0x4040, 8));
        assert_eq!(access.direction(), HvfMmioDirection::Read);
        assert_eq!(access.size(), HvfMmioAccessSize::Doubleword);
        assert_eq!(access.register(), register);
        assert_eq!(access.register_width(), HvfMmioRegisterWidth::Bits64);
    }

    #[test]
    fn resolves_vcpu_mmio_write_exit_against_runtime_bus() {
        let mut bus = MmioBus::new();
        insert_region(&mut bus, 12, 0x5000, 0x100);
        let register = HvfMmioRegister::new(8).expect("register should be valid");
        let resolved = HvfVcpuExit::Exception(exception_exit(
            data_abort_syndrome(HvfMmioAccessSize::Word, HvfMmioDirection::Write, register),
            0x5080,
        ))
        .resolve_with_mmio_bus(&bus)
        .expect("vCPU MMIO write exit should resolve");

        let HvfResolvedVcpuExit::Mmio(access) = resolved else {
            panic!("expected resolved MMIO exit, got {resolved:?}");
        };
        assert_eq!(access.region_id(), region_id(12));
        assert_eq!(access.offset(), 0x80);
        assert_eq!(access.range(), range(0x5080, 4));
        assert_eq!(access.direction(), HvfMmioDirection::Write);
        assert_eq!(access.size(), HvfMmioAccessSize::Word);
        assert_eq!(access.register(), register);
    }

    #[test]
    fn resolves_hvc_vcpu_exit_without_bus_lookup() {
        let bus = MmioBus::new();
        let exit = exception_exit(hvc_syndrome(0), 0);
        let resolved = HvfVcpuExit::Exception(exit)
            .resolve_with_mmio_bus(&bus)
            .expect("HVC exit should resolve without MMIO bus ownership");

        let HvfResolvedVcpuExit::Hvc(hvc) = resolved else {
            panic!("expected resolved HVC exit, got {resolved:?}");
        };
        assert_eq!(hvc.exception_exit(), exit);
        assert_eq!(hvc.immediate(), 0);
    }

    #[test]
    fn resolves_non_mmio_vcpu_exits_without_bus_lookup() {
        let bus = MmioBus::new();

        assert_eq!(
            HvfVcpuExit::Canceled.resolve_with_mmio_bus(&bus),
            Ok(HvfResolvedVcpuExit::Canceled)
        );
        assert_eq!(
            HvfVcpuExit::VtimerActivated.resolve_with_mmio_bus(&bus),
            Ok(HvfResolvedVcpuExit::VtimerActivated)
        );
        assert_eq!(
            HvfVcpuExit::Unknown { reason: 99 }.resolve_with_mmio_bus(&bus),
            Ok(HvfResolvedVcpuExit::Unknown { reason: 99 })
        );
    }

    #[test]
    fn rejects_vcpu_exception_exit_when_mmio_decode_fails() {
        let bus = MmioBus::new();
        let exit = exception_exit(ESR_ISS_ISV, 0x1000);
        let err = HvfVcpuExit::Exception(exit)
            .resolve_with_mmio_bus(&bus)
            .expect_err("unsupported exception should not resolve");

        assert_eq!(
            err,
            HvfVcpuExitResolveError::MmioDecode {
                exit,
                source: HvfMmioDecodeError::UnsupportedExceptionClass { exception_class: 0 }
            }
        );
        assert_eq!(
            err.source().and_then(|source| source.downcast_ref()),
            Some(&HvfMmioDecodeError::UnsupportedExceptionClass { exception_class: 0 })
        );
    }

    #[test]
    fn rejects_vcpu_mmio_exit_when_runtime_bus_resolution_fails() {
        let bus = MmioBus::new();
        let register = HvfMmioRegister::new(1).expect("register should be valid");
        let syndrome =
            data_abort_syndrome(HvfMmioAccessSize::Word, HvfMmioDirection::Read, register);
        let exit = exception_exit(syndrome, 0x6000);
        let access = exit.decode_mmio_access().expect("access should decode");
        let err = HvfVcpuExit::Exception(exit)
            .resolve_with_mmio_bus(&bus)
            .expect_err("unowned MMIO exit should not resolve");
        let source = HvfMmioResolveError::BusLookup {
            access,
            source: MmioBusError::UnownedAccess {
                range: range(0x6000, 4),
            },
        };

        assert_eq!(err, HvfVcpuExitResolveError::MmioResolve { source });
        assert_eq!(
            err.source().and_then(|source| source.downcast_ref()),
            Some(&source)
        );
    }

    #[test]
    fn displays_vcpu_exit_resolution_errors() {
        let err = HvfVcpuExitResolveError::MmioDecode {
            exit: exception_exit(ESR_ISS_ISV, 0x1000),
            source: HvfMmioDecodeError::UnsupportedExceptionClass { exception_class: 0 },
        };

        assert_eq!(
            err.to_string(),
            "failed to decode HVF vCPU exception exit as MMIO: unsupported HVF exception class 0x0 for MMIO decode"
        );

        let access = exception_exit(
            data_abort_syndrome(
                HvfMmioAccessSize::Word,
                HvfMmioDirection::Read,
                HvfMmioRegister::new(1).expect("register should be valid"),
            ),
            0x7000,
        )
        .decode_mmio_access()
        .expect("access should decode");
        let err = HvfVcpuExitResolveError::MmioResolve {
            source: HvfMmioResolveError::BusLookup {
                access,
                source: MmioBusError::UnownedAccess {
                    range: range(0x7000, 4),
                },
            },
        };

        assert_eq!(
            err.to_string(),
            "failed to resolve HVF vCPU MMIO exit: failed to resolve HVF MMIO access at 0x7000 with size 4: MMIO access range [0x7000..0x7004) (4 bytes) is not owned by any region"
        );
    }

    #[test]
    fn rejects_unowned_mmio_access_resolution() {
        let bus = MmioBus::new();
        let access = exception_exit(
            data_abort_syndrome(
                HvfMmioAccessSize::Word,
                HvfMmioDirection::Read,
                HvfMmioRegister::new(1).expect("register should be valid"),
            ),
            0x3000,
        )
        .decode_mmio_access()
        .expect("access should decode");

        let err = access
            .resolve(&bus)
            .expect_err("unowned access should fail");

        assert_eq!(
            err,
            HvfMmioResolveError::BusLookup {
                access,
                source: MmioBusError::UnownedAccess {
                    range: range(0x3000, 4)
                }
            }
        );
    }

    #[test]
    fn rejects_mmio_access_crossing_adjacent_runtime_regions() {
        let mut bus = MmioBus::new();
        insert_region(&mut bus, 1, 0x1000, 0x100);
        insert_region(&mut bus, 2, 0x1100, 0x100);
        let access = exception_exit(
            data_abort_syndrome(
                HvfMmioAccessSize::Halfword,
                HvfMmioDirection::Read,
                HvfMmioRegister::new(1).expect("register should be valid"),
            ),
            0x10ff,
        )
        .decode_mmio_access()
        .expect("access should decode");

        let err = access
            .resolve(&bus)
            .expect_err("cross-region access should fail");

        assert_eq!(
            err,
            HvfMmioResolveError::BusLookup {
                access,
                source: MmioBusError::UnownedAccess {
                    range: range(0x10ff, 2)
                }
            }
        );
    }

    #[test]
    fn preserves_runtime_overflow_error_during_mmio_resolution() {
        let bus = MmioBus::new();
        let access = super::HvfMmioAccess {
            range: range(u64::MAX - 1, 1),
            size: HvfMmioAccessSize::Halfword,
            direction: HvfMmioDirection::Read,
            register: HvfMmioRegister::new(1).expect("register should be valid"),
            sign_extend: false,
            register_width: HvfMmioRegisterWidth::Bits32,
        };

        let err = access
            .resolve(&bus)
            .expect_err("overflowing runtime lookup should fail");

        assert_eq!(
            err,
            HvfMmioResolveError::BusLookup {
                access,
                source: MmioBusError::InvalidAccessRange {
                    start: GuestAddress::new(u64::MAX - 1),
                    size: 2,
                    source: GuestMemoryError::AddressOverflow {
                        start: GuestAddress::new(u64::MAX - 1),
                        size: 2
                    }
                }
            }
        );
    }

    #[test]
    fn displays_mmio_resolution_errors() {
        let access = exception_exit(
            data_abort_syndrome(
                HvfMmioAccessSize::Word,
                HvfMmioDirection::Read,
                HvfMmioRegister::new(1).expect("register should be valid"),
            ),
            0x3000,
        )
        .decode_mmio_access()
        .expect("access should decode");
        let source = MmioBusError::UnownedAccess {
            range: range(0x3000, 4),
        };
        let err = HvfMmioResolveError::BusLookup { access, source };

        assert_eq!(
            err.to_string(),
            "failed to resolve HVF MMIO access at 0x3000 with size 4: MMIO access range [0x3000..0x3004) (4 bytes) is not owned by any region"
        );
        assert_eq!(
            err.source().and_then(|source| source.downcast_ref()),
            Some(&source)
        );
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
