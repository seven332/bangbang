//! HVF GIC v3 creation and metadata for later boot/FDT setup.

use std::fmt;

use bangbang_runtime::BackendError;

const GIC_REQUIRES_MACOS_15_MESSAGE: &str =
    "Hypervisor.framework GIC APIs require macOS 15.0 or newer";
const MMIO32_MEM_START: u64 = 1 << 30;
const DRAM_MEM_START: u64 = bangbang_runtime::memory::aarch64::DRAM_MEM_START;
const DYNAMIC_SYMBOL_SIZE_MISMATCH_MESSAGE: &str =
    "function pointer size does not match a dynamic symbol pointer";

const HV_GIC_INT_EL1_VIRTUAL_TIMER: u16 = 27;
const HV_GIC_INT_EL1_PHYSICAL_TIMER: u16 = 30;
const FIRST_SPI_INTID: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfGicRegion {
    pub base: u64,
    pub size: u64,
}

impl HvfGicRegion {
    pub const fn end_exclusive(self) -> u64 {
        self.base.saturating_add(self.size)
    }

    const fn overlaps(self, other: Self) -> bool {
        self.base < other.end_exclusive() && other.base < self.end_exclusive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfGicInterruptRange {
    pub base: u32,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfGicTimerInterrupts {
    pub el1_virtual_timer_intid: u32,
    pub el1_physical_timer_intid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfGicRedistributor {
    pub region: HvfGicRegion,
    pub single_redistributor_size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfGicMsiMetadata {
    pub region: HvfGicRegion,
    pub interrupt_range: HvfGicInterruptRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfGicMetadata {
    pub distributor: HvfGicRegion,
    pub redistributor: HvfGicRedistributor,
    pub spi_interrupt_range: HvfGicInterruptRange,
    pub timer_interrupts: HvfGicTimerInterrupts,
    pub msi: Option<HvfGicMsiMetadata>,
}

impl HvfGicMetadata {
    pub const FDT_COMPATIBILITY: &'static str = "arm,gic-v3";
    pub const FDT_INTERRUPT_CELLS: u32 = 3;
    pub const FDT_MAINTENANCE_IRQ: u32 = 9;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfGicError {
    Backend(BackendError),
    Unsupported(&'static str),
    InvalidState(&'static str),
    MissingSymbol(&'static str),
    ConfigCreateFailed,
    InvalidParameter {
        name: &'static str,
        value: u64,
    },
    AddressUnderflow {
        region: &'static str,
        limit: u64,
        size: u64,
    },
    UnalignedAddress {
        region: &'static str,
        address: u64,
        alignment: u64,
    },
    RegionOverlap {
        first: &'static str,
        second: &'static str,
    },
    RegionOverlapsDram {
        region: &'static str,
        end_exclusive: u64,
        dram_start: u64,
    },
}

impl fmt::Display for HvfGicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(source) => write!(f, "{source}"),
            Self::Unsupported(message) => write!(f, "unsupported GIC setup: {message}"),
            Self::InvalidState(message) => write!(f, "invalid GIC state: {message}"),
            Self::MissingSymbol(symbol) => write!(
                f,
                "Hypervisor.framework GIC symbol {symbol} is unavailable; macOS 15.0 or newer is required"
            ),
            Self::ConfigCreateFailed => {
                f.write_str("failed to create Hypervisor.framework GIC configuration")
            }
            Self::InvalidParameter { name, value } => {
                write!(
                    f,
                    "invalid Hypervisor.framework GIC parameter {name}={value}"
                )
            }
            Self::AddressUnderflow {
                region,
                limit,
                size,
            } => write!(
                f,
                "GIC {region} region of {size} bytes cannot fit below 0x{limit:x}"
            ),
            Self::UnalignedAddress {
                region,
                address,
                alignment,
            } => write!(
                f,
                "GIC {region} base 0x{address:x} is not aligned to {alignment} bytes"
            ),
            Self::RegionOverlap { first, second } => {
                write!(f, "GIC {first} region overlaps {second} region")
            }
            Self::RegionOverlapsDram {
                region,
                end_exclusive,
                dram_start,
            } => write!(
                f,
                "GIC {region} region ending at 0x{end_exclusive:x} overlaps DRAM starting at 0x{dram_start:x}"
            ),
        }
    }
}

impl std::error::Error for HvfGicError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(source) => Some(source),
            Self::Unsupported(_)
            | Self::InvalidState(_)
            | Self::MissingSymbol(_)
            | Self::ConfigCreateFailed
            | Self::InvalidParameter { .. }
            | Self::AddressUnderflow { .. }
            | Self::UnalignedAddress { .. }
            | Self::RegionOverlap { .. }
            | Self::RegionOverlapsDram { .. } => None,
        }
    }
}

impl From<BackendError> for HvfGicError {
    fn from(source: BackendError) -> Self {
        Self::Backend(source)
    }
}

pub(crate) trait HvfGicCreator: fmt::Debug + Send + Sync {
    fn create_gic(&self) -> Result<HvfGicMetadata, HvfGicError>;
}

#[derive(Debug, Default)]
pub(crate) struct RealHvfGicCreator;

#[derive(Debug, Clone, Copy)]
struct HvfGicParameters {
    distributor_size: u64,
    distributor_alignment: u64,
    redistributor_region_size: u64,
    redistributor_size: u64,
    redistributor_alignment: u64,
    spi_interrupt_range: HvfGicInterruptRange,
    timer_interrupts: HvfGicTimerInterrupts,
}

trait HvfGicApi {
    type Config;

    fn distributor_size(&self) -> Result<u64, HvfGicError>;
    fn distributor_alignment(&self) -> Result<u64, HvfGicError>;
    fn redistributor_region_size(&self) -> Result<u64, HvfGicError>;
    fn redistributor_size(&self) -> Result<u64, HvfGicError>;
    fn redistributor_alignment(&self) -> Result<u64, HvfGicError>;
    fn spi_interrupt_range(&self) -> Result<HvfGicInterruptRange, HvfGicError>;
    fn intid(&self, interrupt: u16) -> Result<u32, HvfGicError>;
    fn create_config(&self) -> Result<Self::Config, HvfGicError>;
    fn set_distributor_base(&self, config: &mut Self::Config, base: u64)
    -> Result<(), HvfGicError>;
    fn set_redistributor_base(
        &self,
        config: &mut Self::Config,
        base: u64,
    ) -> Result<(), HvfGicError>;
    fn create_gic(&self, config: &Self::Config) -> Result<(), HvfGicError>;
    fn release_config(&self, config: Self::Config);
}

impl HvfGicCreator for RealHvfGicCreator {
    fn create_gic(&self) -> Result<HvfGicMetadata, HvfGicError> {
        create_real_gic()
    }
}

fn create_gic_with_api(api: &impl HvfGicApi) -> Result<HvfGicMetadata, HvfGicError> {
    let parameters = query_parameters(api)?;
    let metadata = metadata_from_parameters(parameters)?;
    let mut config = GicConfigGuard::new(api)?;

    api.set_distributor_base(config.config_mut()?, metadata.distributor.base)?;
    api.set_redistributor_base(config.config_mut()?, metadata.redistributor.region.base)?;
    api.create_gic(config.config()?)?;

    Ok(metadata)
}

fn query_parameters(api: &impl HvfGicApi) -> Result<HvfGicParameters, HvfGicError> {
    Ok(HvfGicParameters {
        distributor_size: api.distributor_size()?,
        distributor_alignment: api.distributor_alignment()?,
        redistributor_region_size: api.redistributor_region_size()?,
        redistributor_size: api.redistributor_size()?,
        redistributor_alignment: api.redistributor_alignment()?,
        spi_interrupt_range: api.spi_interrupt_range()?,
        timer_interrupts: HvfGicTimerInterrupts {
            el1_virtual_timer_intid: api.intid(HV_GIC_INT_EL1_VIRTUAL_TIMER)?,
            el1_physical_timer_intid: api.intid(HV_GIC_INT_EL1_PHYSICAL_TIMER)?,
        },
    })
}

fn metadata_from_parameters(parameters: HvfGicParameters) -> Result<HvfGicMetadata, HvfGicError> {
    validate_parameter(
        "distributor_size",
        parameters.distributor_size,
        ParameterRule::NonZero,
    )?;
    validate_parameter(
        "distributor_base_alignment",
        parameters.distributor_alignment,
        ParameterRule::PowerOfTwo,
    )?;
    validate_parameter(
        "redistributor_region_size",
        parameters.redistributor_region_size,
        ParameterRule::NonZero,
    )?;
    validate_parameter(
        "redistributor_size",
        parameters.redistributor_size,
        ParameterRule::NonZero,
    )?;
    validate_parameter(
        "redistributor_base_alignment",
        parameters.redistributor_alignment,
        ParameterRule::PowerOfTwo,
    )?;
    if parameters.redistributor_size > parameters.redistributor_region_size {
        return Err(HvfGicError::InvalidParameter {
            name: "redistributor_size",
            value: parameters.redistributor_size,
        });
    }
    validate_spi_interrupt_range(parameters.spi_interrupt_range)?;

    let distributor = aligned_region_below(
        "distributor",
        MMIO32_MEM_START,
        parameters.distributor_size,
        parameters.distributor_alignment,
    )?;
    let redistributor = aligned_region_below(
        "redistributor",
        distributor.base,
        parameters.redistributor_region_size,
        parameters.redistributor_alignment,
    )?;

    validate_regions_do_not_overlap("distributor", distributor, "redistributor", redistributor)?;
    validate_region_below_dram("distributor", distributor)?;
    validate_region_below_dram("redistributor", redistributor)?;

    Ok(HvfGicMetadata {
        distributor,
        redistributor: HvfGicRedistributor {
            region: redistributor,
            single_redistributor_size: parameters.redistributor_size,
        },
        spi_interrupt_range: parameters.spi_interrupt_range,
        timer_interrupts: parameters.timer_interrupts,
        msi: None,
    })
}

#[derive(Debug, Clone, Copy)]
enum ParameterRule {
    NonZero,
    PowerOfTwo,
}

fn validate_parameter(
    name: &'static str,
    value: u64,
    rule: ParameterRule,
) -> Result<(), HvfGicError> {
    let valid = match rule {
        ParameterRule::NonZero => value != 0,
        ParameterRule::PowerOfTwo => value != 0 && value.is_power_of_two(),
    };

    if valid {
        Ok(())
    } else {
        Err(HvfGicError::InvalidParameter { name, value })
    }
}

fn validate_spi_interrupt_range(range: HvfGicInterruptRange) -> Result<(), HvfGicError> {
    if range.base < FIRST_SPI_INTID {
        return Err(HvfGicError::InvalidParameter {
            name: "spi_interrupt_range.base",
            value: u64::from(range.base),
        });
    }
    if range.count == 0 {
        return Err(HvfGicError::InvalidParameter {
            name: "spi_interrupt_range.count",
            value: 0,
        });
    }
    if range.base.checked_add(range.count).is_none() {
        return Err(HvfGicError::InvalidParameter {
            name: "spi_interrupt_range.end_exclusive",
            value: u64::from(range.base) + u64::from(range.count),
        });
    }

    Ok(())
}

fn aligned_region_below(
    region: &'static str,
    limit: u64,
    size: u64,
    alignment: u64,
) -> Result<HvfGicRegion, HvfGicError> {
    let Some(unadjusted_base) = limit.checked_sub(size) else {
        return Err(HvfGicError::AddressUnderflow {
            region,
            limit,
            size,
        });
    };

    let base = unadjusted_base & !(alignment - 1);
    let Some(end_exclusive) = base.checked_add(size) else {
        return Err(HvfGicError::AddressUnderflow {
            region,
            limit,
            size,
        });
    };
    if end_exclusive > limit {
        return Err(HvfGicError::AddressUnderflow {
            region,
            limit,
            size,
        });
    }
    if !base.is_multiple_of(alignment) {
        return Err(HvfGicError::UnalignedAddress {
            region,
            address: base,
            alignment,
        });
    }

    Ok(HvfGicRegion { base, size })
}

fn validate_regions_do_not_overlap(
    first_name: &'static str,
    first: HvfGicRegion,
    second_name: &'static str,
    second: HvfGicRegion,
) -> Result<(), HvfGicError> {
    if first.overlaps(second) {
        Err(HvfGicError::RegionOverlap {
            first: first_name,
            second: second_name,
        })
    } else {
        Ok(())
    }
}

fn validate_region_below_dram(
    region_name: &'static str,
    region: HvfGicRegion,
) -> Result<(), HvfGicError> {
    if region.end_exclusive() > DRAM_MEM_START {
        Err(HvfGicError::RegionOverlapsDram {
            region: region_name,
            end_exclusive: region.end_exclusive(),
            dram_start: DRAM_MEM_START,
        })
    } else {
        Ok(())
    }
}

struct GicConfigGuard<'api, Api: HvfGicApi + ?Sized> {
    api: &'api Api,
    config: Option<Api::Config>,
}

impl<'api, Api: HvfGicApi + ?Sized> GicConfigGuard<'api, Api> {
    fn new(api: &'api Api) -> Result<Self, HvfGicError> {
        Ok(Self {
            config: Some(api.create_config()?),
            api,
        })
    }

    fn config(&self) -> Result<&Api::Config, HvfGicError> {
        self.config.as_ref().ok_or(HvfGicError::InvalidState(
            "GIC config has already been released",
        ))
    }

    fn config_mut(&mut self) -> Result<&mut Api::Config, HvfGicError> {
        self.config.as_mut().ok_or(HvfGicError::InvalidState(
            "GIC config has already been released",
        ))
    }
}

impl<Api: HvfGicApi + ?Sized> Drop for GicConfigGuard<'_, Api> {
    fn drop(&mut self) {
        if let Some(config) = self.config.take() {
            self.api.release_config(config);
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn create_real_gic() -> Result<HvfGicMetadata, HvfGicError> {
    let api = LoadedHvfGicApi::load()?;
    create_gic_with_api(&api)
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn create_real_gic() -> Result<HvfGicMetadata, HvfGicError> {
    Err(HvfGicError::Unsupported(
        crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
    ))
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod dynamic {
    use std::ffi::{CStr, c_void};
    use std::fmt;
    use std::mem;
    use std::ptr::NonNull;

    use super::{DYNAMIC_SYMBOL_SIZE_MISMATCH_MESSAGE, HvfGicError, HvfGicInterruptRange};

    type HvReturn = i32;
    type HvGicConfig = NonNull<c_void>;
    type HvGicConfigCreate = unsafe extern "C" fn() -> *mut c_void;
    type HvGicSetBase = unsafe extern "C" fn(*mut c_void, u64) -> HvReturn;
    type HvGicCreate = unsafe extern "C" fn(*mut c_void) -> HvReturn;
    type HvGicGetSize = unsafe extern "C" fn(*mut usize) -> HvReturn;
    type HvGicGetSpiRange = unsafe extern "C" fn(*mut u32, *mut u32) -> HvReturn;
    type HvGicGetIntid = unsafe extern "C" fn(u16, *mut u32) -> HvReturn;
    type OsRelease = unsafe extern "C" fn(*mut c_void);

    const HYPERVISOR_FRAMEWORK_PATH: &CStr =
        c"/System/Library/Frameworks/Hypervisor.framework/Hypervisor";

    pub(super) struct LoadedHvfGicApi {
        _library: DynamicLibrary,
        symbols: HvfGicSymbols,
    }

    struct DynamicLibrary {
        handle: NonNull<c_void>,
    }

    #[derive(Clone, Copy)]
    struct HvfGicSymbols {
        config_create: HvGicConfigCreate,
        config_set_distributor_base: HvGicSetBase,
        config_set_redistributor_base: HvGicSetBase,
        create: HvGicCreate,
        get_distributor_size: HvGicGetSize,
        get_distributor_base_alignment: HvGicGetSize,
        get_redistributor_region_size: HvGicGetSize,
        get_redistributor_size: HvGicGetSize,
        get_redistributor_base_alignment: HvGicGetSize,
        get_spi_interrupt_range: HvGicGetSpiRange,
        get_intid: HvGicGetIntid,
        os_release: OsRelease,
    }

    impl LoadedHvfGicApi {
        pub(super) fn load() -> Result<Self, HvfGicError> {
            let library = DynamicLibrary::open(HYPERVISOR_FRAMEWORK_PATH)?;
            let symbols = HvfGicSymbols::load(library.handle())?;

            Ok(Self {
                _library: library,
                symbols,
            })
        }

        fn get_size(
            &self,
            function: HvGicGetSize,
            operation: &'static str,
        ) -> Result<u64, HvfGicError> {
            let mut value = 0usize;
            // SAFETY: `value` is a valid out-pointer for the duration of the call.
            unsafe { crate::ffi::check(function(&mut value), operation)? };

            u64::try_from(value).map_err(|_| HvfGicError::InvalidParameter {
                name: operation,
                value: u64::MAX,
            })
        }
    }

    impl fmt::Debug for LoadedHvfGicApi {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("LoadedHvfGicApi").finish_non_exhaustive()
        }
    }

    impl DynamicLibrary {
        fn open(path: &CStr) -> Result<Self, HvfGicError> {
            // SAFETY: `path` is a NUL-terminated static framework path.
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
            let handle = NonNull::new(handle).ok_or(HvfGicError::Unsupported(
                super::GIC_REQUIRES_MACOS_15_MESSAGE,
            ))?;

            Ok(Self { handle })
        }

        fn handle(&self) -> NonNull<c_void> {
            self.handle
        }
    }

    impl Drop for DynamicLibrary {
        fn drop(&mut self) {
            // SAFETY: `handle` was returned by `dlopen` and is closed exactly once here.
            unsafe {
                let _ = libc::dlclose(self.handle.as_ptr());
            }
        }
    }

    impl fmt::Debug for DynamicLibrary {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("DynamicLibrary")
                .field("handle", &self.handle)
                .finish()
        }
    }

    impl HvfGicSymbols {
        fn load(library: NonNull<c_void>) -> Result<Self, HvfGicError> {
            Ok(Self {
                config_create: load_symbol(
                    library,
                    c"hv_gic_config_create",
                    "hv_gic_config_create",
                )?,
                config_set_distributor_base: load_symbol(
                    library,
                    c"hv_gic_config_set_distributor_base",
                    "hv_gic_config_set_distributor_base",
                )?,
                config_set_redistributor_base: load_symbol(
                    library,
                    c"hv_gic_config_set_redistributor_base",
                    "hv_gic_config_set_redistributor_base",
                )?,
                create: load_symbol(library, c"hv_gic_create", "hv_gic_create")?,
                get_distributor_size: load_symbol(
                    library,
                    c"hv_gic_get_distributor_size",
                    "hv_gic_get_distributor_size",
                )?,
                get_distributor_base_alignment: load_symbol(
                    library,
                    c"hv_gic_get_distributor_base_alignment",
                    "hv_gic_get_distributor_base_alignment",
                )?,
                get_redistributor_region_size: load_symbol(
                    library,
                    c"hv_gic_get_redistributor_region_size",
                    "hv_gic_get_redistributor_region_size",
                )?,
                get_redistributor_size: load_symbol(
                    library,
                    c"hv_gic_get_redistributor_size",
                    "hv_gic_get_redistributor_size",
                )?,
                get_redistributor_base_alignment: load_symbol(
                    library,
                    c"hv_gic_get_redistributor_base_alignment",
                    "hv_gic_get_redistributor_base_alignment",
                )?,
                get_spi_interrupt_range: load_symbol(
                    library,
                    c"hv_gic_get_spi_interrupt_range",
                    "hv_gic_get_spi_interrupt_range",
                )?,
                get_intid: load_symbol(library, c"hv_gic_get_intid", "hv_gic_get_intid")?,
                os_release: load_symbol(
                    NonNull::new(libc::RTLD_DEFAULT)
                        .ok_or(HvfGicError::MissingSymbol("os_release"))?,
                    c"os_release",
                    "os_release",
                )?,
            })
        }
    }

    fn load_symbol<T: Copy>(
        handle: NonNull<c_void>,
        name: &CStr,
        symbol_name: &'static str,
    ) -> Result<T, HvfGicError> {
        if mem::size_of::<T>() != mem::size_of::<*mut c_void>() {
            return Err(HvfGicError::InvalidState(
                DYNAMIC_SYMBOL_SIZE_MISMATCH_MESSAGE,
            ));
        }

        // SAFETY: `handle` comes from `dlopen` or `RTLD_DEFAULT`, and `name`
        // is a NUL-terminated static symbol name.
        let symbol = unsafe { libc::dlsym(handle.as_ptr(), name.as_ptr()) };
        if symbol.is_null() {
            return Err(HvfGicError::MissingSymbol(symbol_name));
        }

        // SAFETY: The caller picks `T` to match the requested symbol's C
        // function type. Function pointers and dynamic symbol pointers have
        // the same representation on this target, checked above.
        Ok(unsafe { mem::transmute_copy::<*mut c_void, T>(&symbol) })
    }

    impl super::HvfGicApi for LoadedHvfGicApi {
        type Config = HvGicConfig;

        fn distributor_size(&self) -> Result<u64, HvfGicError> {
            self.get_size(
                self.symbols.get_distributor_size,
                "hv_gic_get_distributor_size",
            )
        }

        fn distributor_alignment(&self) -> Result<u64, HvfGicError> {
            self.get_size(
                self.symbols.get_distributor_base_alignment,
                "hv_gic_get_distributor_base_alignment",
            )
        }

        fn redistributor_region_size(&self) -> Result<u64, HvfGicError> {
            self.get_size(
                self.symbols.get_redistributor_region_size,
                "hv_gic_get_redistributor_region_size",
            )
        }

        fn redistributor_size(&self) -> Result<u64, HvfGicError> {
            self.get_size(
                self.symbols.get_redistributor_size,
                "hv_gic_get_redistributor_size",
            )
        }

        fn redistributor_alignment(&self) -> Result<u64, HvfGicError> {
            self.get_size(
                self.symbols.get_redistributor_base_alignment,
                "hv_gic_get_redistributor_base_alignment",
            )
        }

        fn spi_interrupt_range(&self) -> Result<HvfGicInterruptRange, HvfGicError> {
            let mut base = 0;
            let mut count = 0;
            // SAFETY: `base` and `count` are valid out-pointers for the duration of the call.
            unsafe {
                crate::ffi::check(
                    (self.symbols.get_spi_interrupt_range)(&mut base, &mut count),
                    "hv_gic_get_spi_interrupt_range",
                )?
            };

            Ok(HvfGicInterruptRange { base, count })
        }

        fn intid(&self, interrupt: u16) -> Result<u32, HvfGicError> {
            let mut intid = 0;
            // SAFETY: `intid` is a valid out-pointer for the duration of the call.
            unsafe {
                crate::ffi::check(
                    (self.symbols.get_intid)(interrupt, &mut intid),
                    "hv_gic_get_intid",
                )?
            };

            Ok(intid)
        }

        fn create_config(&self) -> Result<Self::Config, HvfGicError> {
            // SAFETY: Creates a new retained GIC config object per Hypervisor.framework.
            let config = unsafe { (self.symbols.config_create)() };
            NonNull::new(config).ok_or(HvfGicError::ConfigCreateFailed)
        }

        fn set_distributor_base(
            &self,
            config: &mut Self::Config,
            base: u64,
        ) -> Result<(), HvfGicError> {
            // SAFETY: `config` is a live GIC config object owned by the guard.
            unsafe {
                crate::ffi::check(
                    (self.symbols.config_set_distributor_base)(config.as_ptr(), base),
                    "hv_gic_config_set_distributor_base",
                )?
            };
            Ok(())
        }

        fn set_redistributor_base(
            &self,
            config: &mut Self::Config,
            base: u64,
        ) -> Result<(), HvfGicError> {
            // SAFETY: `config` is a live GIC config object owned by the guard.
            unsafe {
                crate::ffi::check(
                    (self.symbols.config_set_redistributor_base)(config.as_ptr(), base),
                    "hv_gic_config_set_redistributor_base",
                )?
            };
            Ok(())
        }

        fn create_gic(&self, config: &Self::Config) -> Result<(), HvfGicError> {
            // SAFETY: The VM is live, and `config` has valid distributor and
            // redistributor bases configured before this call.
            unsafe { crate::ffi::check((self.symbols.create)(config.as_ptr()), "hv_gic_create")? };
            Ok(())
        }

        fn release_config(&self, config: Self::Config) {
            // SAFETY: `config` is a retained OS object created by
            // `hv_gic_config_create` and is released exactly once by the guard.
            unsafe {
                (self.symbols.os_release)(config.as_ptr());
            }
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use dynamic::LoadedHvfGicApi;

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use bangbang_runtime::BackendError;

    use super::{
        GicConfigGuard, HV_GIC_INT_EL1_PHYSICAL_TIMER, HV_GIC_INT_EL1_VIRTUAL_TIMER, HvfGicApi,
        HvfGicError, HvfGicInterruptRange, HvfGicMetadata, HvfGicParameters, HvfGicRegion,
        HvfGicTimerInterrupts, create_gic_with_api, metadata_from_parameters,
    };

    const DIST_SIZE: u64 = 0x1_0000;
    const REDIST_REGION_SIZE: u64 = 0x2_0000;
    const REDIST_SIZE: u64 = 0x2_0000;
    const ALIGNMENT: u64 = 0x1_0000;

    #[test]
    fn metadata_places_gic_regions_below_mmio32_start() {
        let metadata = metadata_from_parameters(default_parameters())
            .expect("default GIC parameters should produce metadata");

        assert_eq!(
            metadata.distributor,
            HvfGicRegion {
                base: 0x3fff_0000,
                size: DIST_SIZE
            }
        );
        assert_eq!(
            metadata.redistributor.region,
            HvfGicRegion {
                base: 0x3ffd_0000,
                size: REDIST_REGION_SIZE
            }
        );
        assert_eq!(
            metadata.redistributor.single_redistributor_size,
            REDIST_SIZE
        );
        assert_eq!(
            metadata.spi_interrupt_range,
            HvfGicInterruptRange {
                base: 32,
                count: 96
            }
        );
        assert_eq!(
            metadata.timer_interrupts,
            HvfGicTimerInterrupts {
                el1_virtual_timer_intid: 27,
                el1_physical_timer_intid: 30,
            }
        );
        assert_eq!(metadata.msi, None);
        assert_eq!(HvfGicMetadata::FDT_COMPATIBILITY, "arm,gic-v3");
        assert_eq!(HvfGicMetadata::FDT_INTERRUPT_CELLS, 3);
        assert_eq!(HvfGicMetadata::FDT_MAINTENANCE_IRQ, 9);
    }

    #[test]
    fn metadata_aligns_regions_down_to_sdk_alignment() {
        let parameters = HvfGicParameters {
            distributor_size: 0x1_1000,
            redistributor_region_size: 0x2_1000,
            ..default_parameters()
        };

        let metadata =
            metadata_from_parameters(parameters).expect("unaligned sizes should align bases down");

        assert_eq!(metadata.distributor.base, 0x3ffe_0000);
        assert_eq!(metadata.distributor.end_exclusive(), 0x3fff_1000);
        assert_eq!(metadata.redistributor.region.base, 0x3ffb_0000);
        assert_eq!(metadata.redistributor.region.end_exclusive(), 0x3ffd_1000);
    }

    #[test]
    fn metadata_rejects_zero_sizes_before_config_creation() {
        let api = FakeGicApi::new(HvfGicParameters {
            distributor_size: 0,
            ..default_parameters()
        });

        assert_eq!(
            create_gic_with_api(&api),
            Err(HvfGicError::InvalidParameter {
                name: "distributor_size",
                value: 0,
            })
        );
        assert!(!api.created_config());
    }

    #[test]
    fn metadata_rejects_non_power_of_two_alignment() {
        let err = metadata_from_parameters(HvfGicParameters {
            redistributor_alignment: 3,
            ..default_parameters()
        })
        .expect_err("non-power-of-two alignment should fail");

        assert_eq!(
            err,
            HvfGicError::InvalidParameter {
                name: "redistributor_base_alignment",
                value: 3,
            }
        );
    }

    #[test]
    fn metadata_rejects_redistributor_size_larger_than_region() {
        let err = metadata_from_parameters(HvfGicParameters {
            redistributor_region_size: 0x1_0000,
            redistributor_size: 0x2_0000,
            ..default_parameters()
        })
        .expect_err("single redistributor larger than total region should fail");

        assert_eq!(
            err,
            HvfGicError::InvalidParameter {
                name: "redistributor_size",
                value: 0x2_0000,
            }
        );
    }

    #[test]
    fn metadata_rejects_non_spi_interrupt_range_base() {
        let err = metadata_from_parameters(HvfGicParameters {
            spi_interrupt_range: HvfGicInterruptRange { base: 31, count: 1 },
            ..default_parameters()
        })
        .expect_err("SPI range base below the first SPI INTID should fail");

        assert_eq!(
            err,
            HvfGicError::InvalidParameter {
                name: "spi_interrupt_range.base",
                value: 31,
            }
        );
    }

    #[test]
    fn metadata_rejects_zero_spi_interrupt_count_before_config_creation() {
        let api = FakeGicApi::new(HvfGicParameters {
            spi_interrupt_range: HvfGicInterruptRange { base: 32, count: 0 },
            ..default_parameters()
        });

        assert_eq!(
            create_gic_with_api(&api),
            Err(HvfGicError::InvalidParameter {
                name: "spi_interrupt_range.count",
                value: 0,
            })
        );
        assert!(!api.created_config());
    }

    #[test]
    fn metadata_rejects_spi_interrupt_range_overflow() {
        let err = metadata_from_parameters(HvfGicParameters {
            spi_interrupt_range: HvfGicInterruptRange {
                base: u32::MAX,
                count: 2,
            },
            ..default_parameters()
        })
        .expect_err("SPI range end should not overflow");

        assert_eq!(
            err,
            HvfGicError::InvalidParameter {
                name: "spi_interrupt_range.end_exclusive",
                value: u64::from(u32::MAX) + 2,
            }
        );
    }

    #[test]
    fn metadata_rejects_region_that_cannot_fit_below_mmio32() {
        let err = metadata_from_parameters(HvfGicParameters {
            redistributor_region_size: 0x4000_0000,
            ..default_parameters()
        })
        .expect_err("redistributor region should not fit below distributor");

        assert_eq!(
            err,
            HvfGicError::AddressUnderflow {
                region: "redistributor",
                limit: 0x3fff_0000,
                size: 0x4000_0000,
            }
        );
    }

    #[test]
    fn create_gic_configures_hvf_before_returning_metadata() {
        let api = FakeGicApi::default();

        let metadata = create_gic_with_api(&api).expect("GIC should be created");

        assert_eq!(metadata.distributor.base, 0x3fff_0000);
        assert_eq!(
            api.calls(),
            vec![
                "hv_gic_get_distributor_size",
                "hv_gic_get_distributor_base_alignment",
                "hv_gic_get_redistributor_region_size",
                "hv_gic_get_redistributor_size",
                "hv_gic_get_redistributor_base_alignment",
                "hv_gic_get_spi_interrupt_range",
                "hv_gic_get_intid",
                "hv_gic_get_intid",
                "hv_gic_config_create",
                "hv_gic_config_set_distributor_base",
                "hv_gic_config_set_redistributor_base",
                "hv_gic_create",
                "os_release",
            ]
        );
        assert_eq!(api.released_configs(), vec![1]);
    }

    #[test]
    fn create_gic_releases_config_after_set_failure() {
        let api = FakeGicApi::default().with_failure("hv_gic_config_set_redistributor_base");

        assert_eq!(
            create_gic_with_api(&api),
            Err(HvfGicError::Backend(BackendError::Hypervisor(
                "injected hv_gic_config_set_redistributor_base failure".to_string()
            )))
        );
        assert_eq!(
            api.calls(),
            vec![
                "hv_gic_get_distributor_size",
                "hv_gic_get_distributor_base_alignment",
                "hv_gic_get_redistributor_region_size",
                "hv_gic_get_redistributor_size",
                "hv_gic_get_redistributor_base_alignment",
                "hv_gic_get_spi_interrupt_range",
                "hv_gic_get_intid",
                "hv_gic_get_intid",
                "hv_gic_config_create",
                "hv_gic_config_set_distributor_base",
                "hv_gic_config_set_redistributor_base",
                "os_release",
            ]
        );
    }

    #[test]
    fn config_guard_releases_on_drop() {
        let api = FakeGicApi::default();

        {
            let _guard = GicConfigGuard::new(&api).expect("config should be created");
        }

        assert_eq!(api.calls(), vec!["hv_gic_config_create", "os_release"]);
    }

    fn default_parameters() -> HvfGicParameters {
        HvfGicParameters {
            distributor_size: DIST_SIZE,
            distributor_alignment: ALIGNMENT,
            redistributor_region_size: REDIST_REGION_SIZE,
            redistributor_size: REDIST_SIZE,
            redistributor_alignment: ALIGNMENT,
            spi_interrupt_range: HvfGicInterruptRange {
                base: 32,
                count: 96,
            },
            timer_interrupts: HvfGicTimerInterrupts {
                el1_virtual_timer_intid: 27,
                el1_physical_timer_intid: 30,
            },
        }
    }

    #[derive(Debug)]
    struct FakeGicApi {
        parameters: HvfGicParameters,
        state: Mutex<FakeGicApiState>,
    }

    impl Default for FakeGicApi {
        fn default() -> Self {
            Self::new(default_parameters())
        }
    }

    impl FakeGicApi {
        fn new(parameters: HvfGicParameters) -> Self {
            Self {
                parameters,
                state: Mutex::new(FakeGicApiState {
                    calls: Vec::new(),
                    next_config: 1,
                    released_configs: Vec::new(),
                    failure: None,
                    created_config: false,
                }),
            }
        }

        fn with_failure(self, failure: &'static str) -> Self {
            self.state
                .lock()
                .expect("fake GIC API state should be lockable")
                .failure = Some(failure);
            self
        }

        fn calls(&self) -> Vec<&'static str> {
            self.state
                .lock()
                .expect("fake GIC API state should be lockable")
                .calls
                .clone()
        }

        fn released_configs(&self) -> Vec<u64> {
            self.state
                .lock()
                .expect("fake GIC API state should be lockable")
                .released_configs
                .clone()
        }

        fn created_config(&self) -> bool {
            self.state
                .lock()
                .expect("fake GIC API state should be lockable")
                .created_config
        }

        fn record(&self, call: &'static str) -> Result<(), HvfGicError> {
            let mut state = self
                .state
                .lock()
                .expect("fake GIC API state should be lockable");
            state.calls.push(call);

            if state.failure == Some(call) {
                Err(HvfGicError::Backend(BackendError::Hypervisor(format!(
                    "injected {call} failure"
                ))))
            } else {
                Ok(())
            }
        }
    }

    impl HvfGicApi for FakeGicApi {
        type Config = u64;

        fn distributor_size(&self) -> Result<u64, HvfGicError> {
            self.record("hv_gic_get_distributor_size")?;
            Ok(self.parameters.distributor_size)
        }

        fn distributor_alignment(&self) -> Result<u64, HvfGicError> {
            self.record("hv_gic_get_distributor_base_alignment")?;
            Ok(self.parameters.distributor_alignment)
        }

        fn redistributor_region_size(&self) -> Result<u64, HvfGicError> {
            self.record("hv_gic_get_redistributor_region_size")?;
            Ok(self.parameters.redistributor_region_size)
        }

        fn redistributor_size(&self) -> Result<u64, HvfGicError> {
            self.record("hv_gic_get_redistributor_size")?;
            Ok(self.parameters.redistributor_size)
        }

        fn redistributor_alignment(&self) -> Result<u64, HvfGicError> {
            self.record("hv_gic_get_redistributor_base_alignment")?;
            Ok(self.parameters.redistributor_alignment)
        }

        fn spi_interrupt_range(&self) -> Result<HvfGicInterruptRange, HvfGicError> {
            self.record("hv_gic_get_spi_interrupt_range")?;
            Ok(self.parameters.spi_interrupt_range)
        }

        fn intid(&self, interrupt: u16) -> Result<u32, HvfGicError> {
            self.record("hv_gic_get_intid")?;
            match interrupt {
                HV_GIC_INT_EL1_VIRTUAL_TIMER => {
                    Ok(self.parameters.timer_interrupts.el1_virtual_timer_intid)
                }
                HV_GIC_INT_EL1_PHYSICAL_TIMER => {
                    Ok(self.parameters.timer_interrupts.el1_physical_timer_intid)
                }
                _ => Err(HvfGicError::InvalidParameter {
                    name: "interrupt",
                    value: u64::from(interrupt),
                }),
            }
        }

        fn create_config(&self) -> Result<Self::Config, HvfGicError> {
            self.record("hv_gic_config_create")?;
            let mut state = self
                .state
                .lock()
                .expect("fake GIC API state should be lockable");
            let config = state.next_config;
            state.next_config += 1;
            state.created_config = true;
            Ok(config)
        }

        fn set_distributor_base(&self, _: &mut Self::Config, _: u64) -> Result<(), HvfGicError> {
            self.record("hv_gic_config_set_distributor_base")
        }

        fn set_redistributor_base(&self, _: &mut Self::Config, _: u64) -> Result<(), HvfGicError> {
            self.record("hv_gic_config_set_redistributor_base")
        }

        fn create_gic(&self, _: &Self::Config) -> Result<(), HvfGicError> {
            self.record("hv_gic_create")
        }

        fn release_config(&self, config: Self::Config) {
            let mut state = self
                .state
                .lock()
                .expect("fake GIC API state should be lockable");
            state.calls.push("os_release");
            state.released_configs.push(config);
        }
    }

    #[derive(Debug)]
    struct FakeGicApiState {
        calls: Vec<&'static str>,
        next_config: u64,
        released_configs: Vec<u64>,
        failure: Option<&'static str>,
        created_config: bool,
    }
}
