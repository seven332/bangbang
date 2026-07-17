//! Minimal arm64 Flattened Device Tree generation for guest boot metadata.

use std::fmt;

use vm_fdt::{Error as VmFdtError, FdtWriter};

use crate::boot::{BootCommandLineError, LoadedBootSource, LoadedInitrd};
use crate::interrupt::GuestInterruptLine;
use crate::memory::{
    GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryError, GuestMemoryLayout,
    GuestMemoryRange, aarch64,
};

const ROOT_COMPATIBILITY: &str = "linux,dummy-virt";
const GIC_PHANDLE: u32 = 1;
const CLOCK_PHANDLE: u32 = 2;
const MSI_PHANDLE: u32 = 3;
const ADDRESS_CELLS: u32 = 2;
const SIZE_CELLS: u32 = 2;
const CPU_ADDRESS_CELLS: u32 = 2;
const CPU_SIZE_CELLS: u32 = 0;
const CPU_REG_MASK: u64 = 0x7f_ffff;
const MAX_ARM64_FDT_CPUS: usize = 32;
const LAST_CACHE_PHANDLE: u32 = 4000;
const GIC_COMPATIBILITY: &str = "arm,gic-v3";
const RTC_NODE_PREFIX: &str = "rtc";
const RTC_COMPATIBILITY: &[u8] = b"arm,pl031\0arm,primecell\0";
const SERIAL_COMPATIBILITY: &str = "ns16550a";
const SERIAL_NODE_PREFIX: &str = "uart";
const VMGENID_NODE_NAME: &str = "vmgenid";
const VMGENID_COMPATIBILITY: &str = "microsoft,vmgenid";
const VMCLOCK_NODE_PREFIX: &str = "ptp";
const VMCLOCK_COMPATIBILITY: &str = "amazon,vmclock";
const APB_PCLK_NODE_NAME: &str = "apb-pclk";
const APB_PCLK_CLOCK_NAME: &str = "apb_pclk";
const APB_PCLK_CLOCK_OUTPUT_NAME: &str = "clk24mhz";
const APB_PCLK_CLOCK_FREQUENCY: u32 = 24_000_000;
const VIRTIO_MMIO_COMPATIBILITY: &str = "virtio,mmio";
const VIRTIO_MMIO_NODE_PREFIX: &str = "virtio_mmio";
const GIC_FDT_IRQ_TYPE_SPI: u32 = 0;
const GIC_FDT_IRQ_TYPE_PPI: u32 = 1;
const IRQ_TYPE_EDGE_RISING: u32 = 1;
const IRQ_TYPE_LEVEL_HIGH: u32 = 4;
const FIRST_PPI_INTID: u32 = 16;
const FIRST_SPI_INTID: u32 = 32;
const GICV2M_REGISTER_SIZE: u64 = 4;
const MEMORY_REG_CELLS_PER_RANGE: usize = 2;
const MEMORY_REG_CELL_SIZE: usize = 8;

pub const ARM64_FDT_SECURE_PHYSICAL_TIMER_PPI: u32 = 13;
pub const ARM64_FDT_NON_SECURE_PHYSICAL_TIMER_PPI: u32 = 14;
pub const ARM64_FDT_VIRTUAL_TIMER_PPI: u32 = 11;
pub const ARM64_FDT_HYPERVISOR_TIMER_PPI: u32 = 10;
pub const ARM64_FDT_VMGENID_SIZE: u64 = 16;
pub const ARM64_FDT_VMCLOCK_SIZE: u64 = 0x1000;
pub const ARM64_GICV2M_MSI_TYPER_OFFSET: u64 = 0x8;
pub const ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET: u64 = 0x40;
pub const ARM64_GICV2M_MSI_IIDR_OFFSET: u64 = 0xfcc;
pub const ARM64_GICV2M_SPI_END_EXCLUSIVE: u32 = 1019;
const LINUX_PCI_PROBE_ONLY: u32 = 1;
const ARM64_FDT_RNG_SEED_SIZE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtBootInfo<'a> {
    pub command_line: &'a str,
    pub initrd: Option<LoadedInitrd>,
}

impl<'a> From<&'a LoadedBootSource> for Arm64FdtBootInfo<'a> {
    fn from(source: &'a LoadedBootSource) -> Self {
        Self {
            command_line: source.command_line.as_str(),
            initrd: source.initrd,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtConfig<'a> {
    pub layout: &'a GuestMemoryLayout,
    pub boot: Arm64FdtBootInfo<'a>,
    pub vcpu_mpidrs: &'a [u64],
    pub cache_hierarchy: &'a Arm64FdtCacheHierarchy,
    pub gic: Arm64FdtGic,
    pub timer: Arm64FdtTimerInterrupts,
    pub rtc_device: Option<Arm64FdtRtcDevice>,
    pub serial_device: Option<Arm64FdtSerialDevice>,
    pub vmgenid_device: Option<Arm64FdtVmGenIdDevice>,
    pub vmclock_device: Option<Arm64FdtVmClockDevice>,
    pub virtio_mmio_devices: &'a [Arm64FdtVirtioMmioDevice],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Arm64FdtCacheType {
    Data,
    Instruction,
    Unified,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtCache {
    level: u8,
    cache_type: Arm64FdtCacheType,
    size: u32,
    line_size: u32,
    sets: u32,
    ways: u32,
    cpus_per_unit: u32,
}

impl Arm64FdtCache {
    pub fn new(
        level: u8,
        cache_type: Arm64FdtCacheType,
        size: u32,
        line_size: u32,
        sets: u32,
        ways: u32,
        cpus_per_unit: u32,
    ) -> Result<Self, Arm64FdtCacheError> {
        if !(1..=7).contains(&level) {
            return Err(Arm64FdtCacheError::InvalidLevel);
        }
        if size == 0 || line_size == 0 || sets == 0 || ways == 0 {
            return Err(Arm64FdtCacheError::ZeroGeometry);
        }
        if cpus_per_unit == 0 {
            return Err(Arm64FdtCacheError::ZeroSharing);
        }
        let calculated_size = line_size
            .checked_mul(sets)
            .and_then(|value| value.checked_mul(ways))
            .ok_or(Arm64FdtCacheError::GeometryOverflow)?;
        if calculated_size != size {
            return Err(Arm64FdtCacheError::GeometryMismatch);
        }

        Ok(Self {
            level,
            cache_type,
            size,
            line_size,
            sets,
            ways,
            cpus_per_unit,
        })
    }

    pub const fn level(self) -> u8 {
        self.level
    }

    pub const fn cache_type(self) -> Arm64FdtCacheType {
        self.cache_type
    }

    pub const fn size(self) -> u32 {
        self.size
    }

    pub const fn line_size(self) -> u32 {
        self.line_size
    }

    pub const fn sets(self) -> u32 {
        self.sets
    }

    pub const fn ways(self) -> u32 {
        self.ways
    }

    pub const fn cpus_per_unit(self) -> u32 {
        self.cpus_per_unit
    }
}

impl fmt::Debug for Arm64FdtCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Arm64FdtCache")
            .field("cache_geometry", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Arm64FdtCacheHierarchy {
    caches: Vec<Arm64FdtCache>,
}

impl Arm64FdtCacheHierarchy {
    pub fn new(mut caches: Vec<Arm64FdtCache>) -> Result<Self, Arm64FdtCacheHierarchyError> {
        caches.sort_by_key(|cache| (cache.level, cache.cache_type));
        validate_cache_hierarchy_shape(&caches)?;
        Ok(Self { caches })
    }

    pub fn caches(&self) -> &[Arm64FdtCache] {
        &self.caches
    }

    fn level_caches(&self, level: u8) -> &[Arm64FdtCache] {
        let start = self.caches.partition_point(|cache| cache.level < level);
        let end = self.caches.partition_point(|cache| cache.level <= level);
        self.caches.get(start..end).unwrap_or(&[])
    }

    fn validate_for_vcpu_count(
        &self,
        vcpu_count: usize,
    ) -> Result<(), Arm64FdtCacheHierarchyError> {
        if vcpu_count == 0 {
            return Err(Arm64FdtCacheHierarchyError::MissingCpu);
        }

        for cache in self.caches.iter().filter(|cache| cache.level > 1) {
            let share = usize::try_from(cache.cpus_per_unit)
                .map_err(|_| Arm64FdtCacheHierarchyError::PhandleOverflow)?;
            let unit_count = vcpu_count
                .checked_add(share - 1)
                .and_then(|value| value.checked_div(share))
                .ok_or(Arm64FdtCacheHierarchyError::PhandleOverflow)?;
            let last_unit = unit_count
                .checked_sub(1)
                .ok_or(Arm64FdtCacheHierarchyError::PhandleOverflow)?;
            cache_phandle(vcpu_count, cache.level, last_unit)?;
        }

        Ok(())
    }
}

impl fmt::Debug for Arm64FdtCacheHierarchy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Arm64FdtCacheHierarchy")
            .field("cache_topology", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Arm64FdtCacheError {
    InvalidLevel,
    ZeroGeometry,
    ZeroSharing,
    GeometryOverflow,
    GeometryMismatch,
}

impl fmt::Debug for Arm64FdtCacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Arm64FdtCacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLevel => f.write_str("cache level is outside the arm64 FDT range"),
            Self::ZeroGeometry => f.write_str("cache geometry contains a zero field"),
            Self::ZeroSharing => f.write_str("cache sharing count is zero"),
            Self::GeometryOverflow => f.write_str("cache geometry overflows the FDT width"),
            Self::GeometryMismatch => {
                f.write_str("cache size is inconsistent with line, set, and way geometry")
            }
        }
    }
}

impl std::error::Error for Arm64FdtCacheError {}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Arm64FdtCacheHierarchyError {
    Empty,
    MissingL1,
    MissingCpu,
    LevelGap,
    DuplicateType,
    MixedUnifiedAndSplit,
    NonUnifiedOuter,
    InconsistentSharing,
    InvalidL1Sharing,
    InvalidNestedSharing,
    PhandleOverflow,
    PhandleCollision,
}

impl fmt::Debug for Arm64FdtCacheHierarchyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Arm64FdtCacheHierarchyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("arm64 FDT cache hierarchy is empty"),
            Self::MissingL1 => f.write_str("arm64 FDT cache hierarchy has no level-one cache"),
            Self::MissingCpu => f.write_str("arm64 FDT cache hierarchy has no CPU"),
            Self::LevelGap => f.write_str("arm64 FDT cache hierarchy contains a level gap"),
            Self::DuplicateType => {
                f.write_str("arm64 FDT cache hierarchy repeats a cache type at one level")
            }
            Self::MixedUnifiedAndSplit => {
                f.write_str("arm64 FDT cache hierarchy mixes unified and split caches at one level")
            }
            Self::NonUnifiedOuter => f.write_str("arm64 FDT outer cache levels must be unified"),
            Self::InconsistentSharing => {
                f.write_str("arm64 FDT cache hierarchy uses inconsistent sharing within one level")
            }
            Self::InvalidL1Sharing => {
                f.write_str("arm64 FDT level-one caches must be private to one CPU")
            }
            Self::InvalidNestedSharing => {
                f.write_str("arm64 FDT cache sharing does not form nested CPU groups")
            }
            Self::PhandleOverflow => f.write_str("arm64 FDT cache phandle arithmetic overflowed"),
            Self::PhandleCollision => {
                f.write_str("arm64 FDT cache phandle collides with a reserved phandle")
            }
        }
    }
}

impl std::error::Error for Arm64FdtCacheHierarchyError {}

fn validate_cache_hierarchy_shape(
    caches: &[Arm64FdtCache],
) -> Result<(), Arm64FdtCacheHierarchyError> {
    if caches.is_empty() {
        return Err(Arm64FdtCacheHierarchyError::Empty);
    }
    if caches.first().is_none_or(|cache| cache.level != 1) {
        return Err(Arm64FdtCacheHierarchyError::MissingL1);
    }

    let mut start = 0;
    let mut expected_level = 1;
    let mut previous_share = 1;
    while let Some(first_cache) = caches.get(start) {
        let level = first_cache.level;
        if level != expected_level {
            return Err(Arm64FdtCacheHierarchyError::LevelGap);
        }
        let end = caches.partition_point(|cache| cache.level <= level);
        let level_caches = caches
            .get(start..end)
            .ok_or(Arm64FdtCacheHierarchyError::LevelGap)?;
        let share = level_caches
            .first()
            .ok_or(Arm64FdtCacheHierarchyError::LevelGap)?
            .cpus_per_unit;
        let has_unified = level_caches
            .iter()
            .any(|cache| cache.cache_type == Arm64FdtCacheType::Unified);

        for pair in level_caches.windows(2) {
            let [first, second] = pair else {
                continue;
            };
            if first.cache_type == second.cache_type {
                return Err(Arm64FdtCacheHierarchyError::DuplicateType);
            }
        }
        if has_unified && level_caches.len() != 1 {
            return Err(Arm64FdtCacheHierarchyError::MixedUnifiedAndSplit);
        }
        if level > 1 && !has_unified {
            return Err(Arm64FdtCacheHierarchyError::NonUnifiedOuter);
        }
        if level_caches
            .iter()
            .any(|cache| cache.cpus_per_unit != share)
        {
            return Err(Arm64FdtCacheHierarchyError::InconsistentSharing);
        }
        if level == 1 {
            if share != 1 {
                return Err(Arm64FdtCacheHierarchyError::InvalidL1Sharing);
            }
        } else if share < previous_share || !share.is_multiple_of(previous_share) {
            return Err(Arm64FdtCacheHierarchyError::InvalidNestedSharing);
        }

        previous_share = share;
        expected_level = expected_level
            .checked_add(1)
            .ok_or(Arm64FdtCacheHierarchyError::LevelGap)?;
        start = end;
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtRegion {
    pub base: u64,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtInterruptRange {
    pub base: u32,
    pub count: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtMsi {
    pub region: Arm64FdtRegion,
    pub interrupt_range: Arm64FdtInterruptRange,
}

impl fmt::Debug for Arm64FdtMsi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Arm64FdtMsi")
            .field("region", &"<redacted>")
            .field("interrupt_range", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtGic {
    pub distributor: Arm64FdtRegion,
    pub redistributor: Arm64FdtRegion,
    pub compatibility: &'static str,
    pub interrupt_cells: u32,
    pub maintenance_irq: u32,
    pub msi: Option<Arm64FdtMsi>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtTimerInterrupts {
    pub secure_physical: u32,
    pub non_secure_physical: u32,
    pub virtual_timer: u32,
    pub hypervisor: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtVirtioMmioDevice {
    pub region: Arm64FdtRegion,
    pub interrupt_line: GuestInterruptLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtSerialDevice {
    pub region: Arm64FdtRegion,
    pub interrupt_line: GuestInterruptLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtRtcDevice {
    pub region: Arm64FdtRegion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtVmGenIdDevice {
    pub region: Arm64FdtRegion,
    pub interrupt_line: GuestInterruptLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtVmClockDevice {
    pub region: Arm64FdtRegion,
    pub interrupt_line: GuestInterruptLine,
}

impl Arm64FdtTimerInterrupts {
    pub const fn firecracker_default() -> Self {
        Self {
            secure_physical: ARM64_FDT_SECURE_PHYSICAL_TIMER_PPI,
            non_secure_physical: ARM64_FDT_NON_SECURE_PHYSICAL_TIMER_PPI,
            virtual_timer: ARM64_FDT_VIRTUAL_TIMER_PPI,
            hypervisor: ARM64_FDT_HYPERVISOR_TIMER_PPI,
        }
    }

    pub fn from_el1_timer_intids(
        el1_virtual_timer_intid: u32,
        el1_physical_timer_intid: u32,
    ) -> Result<Self, Arm64FdtError> {
        let timer = Self {
            secure_physical: ARM64_FDT_SECURE_PHYSICAL_TIMER_PPI,
            non_secure_physical: ppi_from_intid(
                "el1_physical_timer_intid",
                el1_physical_timer_intid,
            )?,
            virtual_timer: ppi_from_intid("el1_virtual_timer_intid", el1_virtual_timer_intid)?,
            hypervisor: ARM64_FDT_HYPERVISOR_TIMER_PPI,
        };
        validate_timer(timer)?;
        Ok(timer)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtGuestMemoryWrite {
    pub address: GuestAddress,
    pub size: usize,
}

trait Arm64FdtRngSeedSource {
    fn fill_rng_seed(
        &mut self,
        destination: &mut [u8; ARM64_FDT_RNG_SEED_SIZE],
    ) -> Result<(), Arm64FdtRngSeedError>;
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Arm64FdtOsRngSeedSource;

impl Arm64FdtOsRngSeedSource {
    const fn new() -> Self {
        Self
    }
}

impl Arm64FdtRngSeedSource for Arm64FdtOsRngSeedSource {
    fn fill_rng_seed(
        &mut self,
        destination: &mut [u8; ARM64_FDT_RNG_SEED_SIZE],
    ) -> Result<(), Arm64FdtRngSeedError> {
        getrandom::fill(destination).map_err(|_| Arm64FdtRngSeedError::new())
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtRngSeedError;

impl Arm64FdtRngSeedError {
    pub const fn new() -> Self {
        Self
    }
}

impl fmt::Display for Arm64FdtRngSeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("failed to generate arm64 FDT rng-seed")
    }
}

impl std::error::Error for Arm64FdtRngSeedError {}

#[derive(Debug, PartialEq, Eq)]
pub enum Arm64FdtError {
    MissingCpu,
    TooManyCpus {
        count: usize,
        max: usize,
    },
    DuplicateCpuReg {
        first_index: usize,
        second_index: usize,
        reg: u64,
    },
    InvalidCacheHierarchy {
        source: Arm64FdtCacheHierarchyError,
    },
    InvalidLayout {
        source: GuestMemoryError,
    },
    NoGuestMemoryAfterSystemArea {
        first_range: GuestMemoryRange,
        system_size: u64,
    },
    InvalidDramStart {
        actual: GuestAddress,
        expected: GuestAddress,
    },
    GuestMemoryTooLarge {
        size: u64,
        max_size: u64,
    },
    UnexpectedDramLayout {
        range_index: usize,
        expected: Option<GuestMemoryRange>,
        actual: Option<GuestMemoryRange>,
    },
    GuestMemoryOverlapsMmio64 {
        range: GuestMemoryRange,
    },
    InvalidCommandLine {
        source: BootCommandLineError,
    },
    InvalidInitrdRange {
        source: GuestMemoryError,
    },
    InitrdNotInGuestMemory {
        range: GuestMemoryRange,
    },
    InitrdOverlapsFdt {
        end_exclusive: GuestAddress,
        fdt_address: GuestAddress,
    },
    InvalidGicRegion {
        name: &'static str,
        region: Arm64FdtRegion,
    },
    GicRegionsOverlap {
        first: &'static str,
        second: &'static str,
    },
    GicRegionOverlapsMemory {
        name: &'static str,
        region: Arm64FdtRegion,
        memory_range: GuestMemoryRange,
    },
    InvalidGicCompatibility {
        value: &'static str,
        expected: &'static str,
    },
    InvalidGicInterruptCells {
        value: u32,
    },
    InvalidPpi {
        name: &'static str,
        value: u32,
    },
    DuplicatePpi {
        first: &'static str,
        second: &'static str,
        value: u32,
    },
    InvalidPpiIntid {
        name: &'static str,
        intid: u32,
    },
    RngSeed {
        source: Arm64FdtRngSeedError,
    },
    InvalidGicMsiRegion,
    InvalidGicMsiInterruptRange,
    GicMsiRegionOverlaps {
        other: &'static str,
    },
    GicMsiRegionOverlapsMemory,
    InvalidVirtioMmioRegion {
        index: usize,
        region: Arm64FdtRegion,
        source: GuestMemoryError,
    },
    VirtioMmioRegionOverlapsMemory {
        index: usize,
        region: Arm64FdtRegion,
        memory_range: GuestMemoryRange,
    },
    VirtioMmioRegionOverlapsGic {
        index: usize,
        region: Arm64FdtRegion,
        gic: &'static str,
    },
    VirtioMmioRegionsOverlap {
        first_index: usize,
        second_index: usize,
        first_region: Arm64FdtRegion,
        second_region: Arm64FdtRegion,
    },
    InvalidVirtioMmioInterrupt {
        index: usize,
        line: GuestInterruptLine,
    },
    InvalidSerialRegion {
        region: Arm64FdtRegion,
        source: GuestMemoryError,
    },
    InvalidRtcRegion {
        region: Arm64FdtRegion,
        source: GuestMemoryError,
    },
    RtcRegionOverlapsMemory {
        region: Arm64FdtRegion,
        memory_range: GuestMemoryRange,
    },
    RtcRegionOverlapsGic {
        region: Arm64FdtRegion,
        gic: &'static str,
    },
    RtcRegionOverlapsVirtioMmio {
        region: Arm64FdtRegion,
        virtio_mmio_index: usize,
        virtio_mmio_region: Arm64FdtRegion,
    },
    SerialRegionOverlapsMemory {
        region: Arm64FdtRegion,
        memory_range: GuestMemoryRange,
    },
    SerialRegionOverlapsGic {
        region: Arm64FdtRegion,
        gic: &'static str,
    },
    SerialRegionOverlapsVirtioMmio {
        region: Arm64FdtRegion,
        virtio_mmio_index: usize,
        virtio_mmio_region: Arm64FdtRegion,
    },
    SerialRegionOverlapsRtc {
        region: Arm64FdtRegion,
        rtc_region: Arm64FdtRegion,
    },
    InvalidSerialInterrupt {
        line: GuestInterruptLine,
    },
    InvalidVmGenIdRegion {
        region: Arm64FdtRegion,
        source: GuestMemoryError,
    },
    InvalidVmGenIdSize {
        size: u64,
        expected: u64,
    },
    VmGenIdRegionOverlapsMemory {
        region: Arm64FdtRegion,
        memory_range: GuestMemoryRange,
    },
    VmGenIdRegionOverlapsGic {
        region: Arm64FdtRegion,
        gic: &'static str,
    },
    VmGenIdRegionOverlapsVirtioMmio {
        region: Arm64FdtRegion,
        virtio_mmio_index: usize,
        virtio_mmio_region: Arm64FdtRegion,
    },
    VmGenIdRegionOverlapsRtc {
        region: Arm64FdtRegion,
        rtc_region: Arm64FdtRegion,
    },
    VmGenIdRegionOverlapsSerial {
        region: Arm64FdtRegion,
        serial_region: Arm64FdtRegion,
    },
    InvalidVmGenIdInterrupt {
        line: GuestInterruptLine,
    },
    InvalidVmClockRegion {
        region: Arm64FdtRegion,
        source: GuestMemoryError,
    },
    InvalidVmClockSize {
        size: u64,
        expected: u64,
    },
    VmClockRegionOverlapsMemory {
        region: Arm64FdtRegion,
        memory_range: GuestMemoryRange,
    },
    VmClockRegionOverlapsGic {
        region: Arm64FdtRegion,
        gic: &'static str,
    },
    VmClockRegionOverlapsVirtioMmio {
        region: Arm64FdtRegion,
        virtio_mmio_index: usize,
        virtio_mmio_region: Arm64FdtRegion,
    },
    VmClockRegionOverlapsRtc {
        region: Arm64FdtRegion,
        rtc_region: Arm64FdtRegion,
    },
    VmClockRegionOverlapsSerial {
        region: Arm64FdtRegion,
        serial_region: Arm64FdtRegion,
    },
    VmClockRegionOverlapsVmGenId {
        region: Arm64FdtRegion,
        vmgenid_region: Arm64FdtRegion,
    },
    InvalidVmClockInterrupt {
        line: GuestInterruptLine,
    },
    CreateFdt {
        source: VmFdtError,
    },
    FdtTooLarge {
        size: usize,
        max_size: u64,
    },
    GuestMemoryLayoutMismatch {
        range_index: usize,
        expected: Option<GuestMemoryRange>,
        actual: Option<GuestMemoryRange>,
    },
    GuestMemoryWrite {
        source: GuestMemoryAccessError,
    },
}

impl fmt::Display for Arm64FdtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCpu => f.write_str("arm64 FDT requires at least one CPU"),
            Self::TooManyCpus { count, max } => {
                write!(f, "arm64 FDT supports at most {max} CPUs, got {count}")
            }
            Self::DuplicateCpuReg {
                first_index,
                second_index,
                reg,
            } => write!(
                f,
                "arm64 FDT CPU reg values must be distinct: CPU {first_index} and CPU {second_index} both use 0x{reg:x}"
            ),
            Self::InvalidCacheHierarchy { source } => {
                write!(f, "invalid arm64 FDT cache hierarchy: {source}")
            }
            Self::InvalidLayout { source } => {
                write!(f, "invalid arm64 FDT memory layout: {source}")
            }
            Self::NoGuestMemoryAfterSystemArea {
                first_range,
                system_size,
            } => write!(
                f,
                "guest memory first range {first_range} does not leave RAM after the reserved {system_size} byte system area"
            ),
            Self::InvalidDramStart { actual, expected } => write!(
                f,
                "arm64 FDT guest memory must start at {expected}, got {actual}"
            ),
            Self::GuestMemoryTooLarge { size, max_size } => write!(
                f,
                "arm64 FDT guest memory size {size} bytes exceeds {max_size} byte aarch64 maximum"
            ),
            Self::UnexpectedDramLayout { range_index, .. } => write!(
                f,
                "arm64 FDT guest memory layout does not match the aarch64 DRAM layout at range {range_index}"
            ),
            Self::GuestMemoryOverlapsMmio64 { range } => write!(
                f,
                "arm64 FDT guest memory range {range} overlaps the aarch64 MMIO64 gap"
            ),
            Self::InvalidCommandLine { source } => {
                write!(f, "invalid arm64 FDT command line: {source}")
            }
            Self::InvalidInitrdRange { source } => {
                write!(f, "invalid arm64 FDT initrd range: {source}")
            }
            Self::InitrdNotInGuestMemory { range } => write!(
                f,
                "arm64 FDT initrd range {range} is not fully inside guest memory advertised to the guest"
            ),
            Self::InitrdOverlapsFdt {
                end_exclusive,
                fdt_address,
            } => write!(
                f,
                "arm64 FDT initrd range ending at {end_exclusive} overlaps reserved FDT window starting at {fdt_address}"
            ),
            Self::InvalidGicRegion { name, region } => write!(
                f,
                "invalid arm64 FDT GIC {name} region: base=0x{:x}, size={}",
                region.base, region.size
            ),
            Self::GicRegionsOverlap { first, second } => {
                write!(f, "arm64 FDT GIC {first} region overlaps {second} region")
            }
            Self::GicRegionOverlapsMemory {
                name,
                region,
                memory_range,
            } => write!(
                f,
                "arm64 FDT GIC {name} region base=0x{:x}, size={} overlaps guest memory range {memory_range}",
                region.base, region.size
            ),
            Self::InvalidGicCompatibility { value, expected } => write!(
                f,
                "arm64 FDT GIC compatible must be {expected}, got {value}"
            ),
            Self::InvalidGicInterruptCells { value } => {
                write!(f, "arm64 FDT GIC #interrupt-cells must be 3, got {value}")
            }
            Self::InvalidPpi { name, value } => {
                write!(
                    f,
                    "arm64 FDT {name} PPI value must be below 16, got {value}"
                )
            }
            Self::DuplicatePpi {
                first,
                second,
                value,
            } => write!(
                f,
                "arm64 FDT PPIs must be distinct: {first} and {second} both use {value}"
            ),
            Self::InvalidPpiIntid { name, intid } => write!(
                f,
                "arm64 FDT {name} INTID must be in the PPI range [16, 32), got {intid}"
            ),
            Self::RngSeed { source } => {
                write!(f, "failed to create arm64 FDT rng-seed: {source}")
            }
            Self::InvalidGicMsiRegion => f.write_str("arm64 FDT GICv2m frame is invalid"),
            Self::InvalidGicMsiInterruptRange => {
                f.write_str("arm64 FDT GICv2m interrupt range is invalid")
            }
            Self::GicMsiRegionOverlaps { other } => {
                write!(f, "arm64 FDT GICv2m frame overlaps GIC {other} region")
            }
            Self::GicMsiRegionOverlapsMemory => {
                f.write_str("arm64 FDT GICv2m frame overlaps guest memory")
            }
            Self::InvalidVirtioMmioRegion {
                index,
                region,
                source,
            } => write!(
                f,
                "invalid arm64 FDT virtio-mmio device {index} region base=0x{:x}, size={}: {source}",
                region.base, region.size
            ),
            Self::VirtioMmioRegionOverlapsMemory {
                index,
                region,
                memory_range,
            } => write!(
                f,
                "arm64 FDT virtio-mmio device {index} region base=0x{:x}, size={} overlaps guest memory range {memory_range}",
                region.base, region.size
            ),
            Self::VirtioMmioRegionOverlapsGic { index, region, gic } => write!(
                f,
                "arm64 FDT virtio-mmio device {index} region base=0x{:x}, size={} overlaps GIC {gic} region",
                region.base, region.size
            ),
            Self::VirtioMmioRegionsOverlap {
                first_index,
                second_index,
                first_region,
                second_region,
            } => write!(
                f,
                "arm64 FDT virtio-mmio device {first_index} region base=0x{:x}, size={} overlaps device {second_index} region base=0x{:x}, size={}",
                first_region.base, first_region.size, second_region.base, second_region.size
            ),
            Self::InvalidVirtioMmioInterrupt { index, line } => write!(
                f,
                "arm64 FDT virtio-mmio device {index} interrupt line {line} must be an SPI INTID at least {FIRST_SPI_INTID}"
            ),
            Self::InvalidSerialRegion { region, source } => write!(
                f,
                "invalid arm64 FDT serial region base=0x{:x}, size={}: {source}",
                region.base, region.size
            ),
            Self::InvalidRtcRegion { region, source } => write!(
                f,
                "invalid arm64 FDT RTC region base=0x{:x}, size={}: {source}",
                region.base, region.size
            ),
            Self::RtcRegionOverlapsMemory {
                region,
                memory_range,
            } => write!(
                f,
                "arm64 FDT RTC region base=0x{:x}, size={} overlaps guest memory range {memory_range}",
                region.base, region.size
            ),
            Self::RtcRegionOverlapsGic { region, gic } => write!(
                f,
                "arm64 FDT RTC region base=0x{:x}, size={} overlaps GIC {gic} region",
                region.base, region.size
            ),
            Self::RtcRegionOverlapsVirtioMmio {
                region,
                virtio_mmio_index,
                virtio_mmio_region,
            } => write!(
                f,
                "arm64 FDT RTC region base=0x{:x}, size={} overlaps virtio-mmio device {virtio_mmio_index} region base=0x{:x}, size={}",
                region.base, region.size, virtio_mmio_region.base, virtio_mmio_region.size
            ),
            Self::SerialRegionOverlapsMemory {
                region,
                memory_range,
            } => write!(
                f,
                "arm64 FDT serial region base=0x{:x}, size={} overlaps guest memory range {memory_range}",
                region.base, region.size
            ),
            Self::SerialRegionOverlapsGic { region, gic } => write!(
                f,
                "arm64 FDT serial region base=0x{:x}, size={} overlaps GIC {gic} region",
                region.base, region.size
            ),
            Self::SerialRegionOverlapsVirtioMmio {
                region,
                virtio_mmio_index,
                virtio_mmio_region,
            } => write!(
                f,
                "arm64 FDT serial region base=0x{:x}, size={} overlaps virtio-mmio device {virtio_mmio_index} region base=0x{:x}, size={}",
                region.base, region.size, virtio_mmio_region.base, virtio_mmio_region.size
            ),
            Self::SerialRegionOverlapsRtc { region, rtc_region } => write!(
                f,
                "arm64 FDT serial region base=0x{:x}, size={} overlaps RTC region base=0x{:x}, size={}",
                region.base, region.size, rtc_region.base, rtc_region.size
            ),
            Self::InvalidSerialInterrupt { line } => write!(
                f,
                "arm64 FDT serial interrupt line {line} must be an SPI INTID at least {FIRST_SPI_INTID}"
            ),
            Self::InvalidVmGenIdRegion { region, source } => write!(
                f,
                "invalid arm64 FDT VMGenID region base=0x{:x}, size={}: {source}",
                region.base, region.size
            ),
            Self::InvalidVmGenIdSize { size, expected } => write!(
                f,
                "arm64 FDT VMGenID region size must be {expected} bytes, got {size}"
            ),
            Self::VmGenIdRegionOverlapsMemory {
                region,
                memory_range,
            } => write!(
                f,
                "arm64 FDT VMGenID region base=0x{:x}, size={} overlaps guest memory range {memory_range}",
                region.base, region.size
            ),
            Self::VmGenIdRegionOverlapsGic { region, gic } => write!(
                f,
                "arm64 FDT VMGenID region base=0x{:x}, size={} overlaps GIC {gic} region",
                region.base, region.size
            ),
            Self::VmGenIdRegionOverlapsVirtioMmio {
                region,
                virtio_mmio_index,
                virtio_mmio_region,
            } => write!(
                f,
                "arm64 FDT VMGenID region base=0x{:x}, size={} overlaps virtio-mmio device {virtio_mmio_index} region base=0x{:x}, size={}",
                region.base, region.size, virtio_mmio_region.base, virtio_mmio_region.size
            ),
            Self::VmGenIdRegionOverlapsRtc { region, rtc_region } => write!(
                f,
                "arm64 FDT VMGenID region base=0x{:x}, size={} overlaps RTC region base=0x{:x}, size={}",
                region.base, region.size, rtc_region.base, rtc_region.size
            ),
            Self::VmGenIdRegionOverlapsSerial {
                region,
                serial_region,
            } => write!(
                f,
                "arm64 FDT VMGenID region base=0x{:x}, size={} overlaps serial region base=0x{:x}, size={}",
                region.base, region.size, serial_region.base, serial_region.size
            ),
            Self::InvalidVmGenIdInterrupt { line } => write!(
                f,
                "arm64 FDT VMGenID interrupt line {line} must be an SPI INTID at least {FIRST_SPI_INTID}"
            ),
            Self::InvalidVmClockRegion { region, source } => write!(
                f,
                "invalid arm64 FDT VMClock region base=0x{:x}, size={}: {source}",
                region.base, region.size
            ),
            Self::InvalidVmClockSize { size, expected } => write!(
                f,
                "arm64 FDT VMClock region size must be {expected} bytes, got {size}"
            ),
            Self::VmClockRegionOverlapsMemory {
                region,
                memory_range,
            } => write!(
                f,
                "arm64 FDT VMClock region base=0x{:x}, size={} overlaps guest memory range {memory_range}",
                region.base, region.size
            ),
            Self::VmClockRegionOverlapsGic { region, gic } => write!(
                f,
                "arm64 FDT VMClock region base=0x{:x}, size={} overlaps GIC {gic} region",
                region.base, region.size
            ),
            Self::VmClockRegionOverlapsVirtioMmio {
                region,
                virtio_mmio_index,
                virtio_mmio_region,
            } => write!(
                f,
                "arm64 FDT VMClock region base=0x{:x}, size={} overlaps virtio-mmio device {virtio_mmio_index} region base=0x{:x}, size={}",
                region.base, region.size, virtio_mmio_region.base, virtio_mmio_region.size
            ),
            Self::VmClockRegionOverlapsRtc { region, rtc_region } => write!(
                f,
                "arm64 FDT VMClock region base=0x{:x}, size={} overlaps RTC region base=0x{:x}, size={}",
                region.base, region.size, rtc_region.base, rtc_region.size
            ),
            Self::VmClockRegionOverlapsSerial {
                region,
                serial_region,
            } => write!(
                f,
                "arm64 FDT VMClock region base=0x{:x}, size={} overlaps serial region base=0x{:x}, size={}",
                region.base, region.size, serial_region.base, serial_region.size
            ),
            Self::VmClockRegionOverlapsVmGenId {
                region,
                vmgenid_region,
            } => write!(
                f,
                "arm64 FDT VMClock region base=0x{:x}, size={} overlaps VMGenID region base=0x{:x}, size={}",
                region.base, region.size, vmgenid_region.base, vmgenid_region.size
            ),
            Self::InvalidVmClockInterrupt { line } => write!(
                f,
                "arm64 FDT VMClock interrupt line {line} must be an SPI INTID at least {FIRST_SPI_INTID}"
            ),
            Self::CreateFdt { source } => write!(f, "failed to create arm64 FDT: {source}"),
            Self::FdtTooLarge { size, max_size } => {
                write!(
                    f,
                    "arm64 FDT size {size} bytes exceeds reserved {max_size} byte window"
                )
            }
            Self::GuestMemoryLayoutMismatch { range_index, .. } => write!(
                f,
                "arm64 FDT guest memory layout does not match allocated memory at range {range_index}"
            ),
            Self::GuestMemoryWrite { source } => {
                write!(f, "failed to write arm64 FDT into guest memory: {source}")
            }
        }
    }
}

impl std::error::Error for Arm64FdtError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidLayout { source } => Some(source),
            Self::InvalidCacheHierarchy { source } => Some(source),
            Self::InvalidCommandLine { source } => Some(source),
            Self::InvalidInitrdRange { source } => Some(source),
            Self::InvalidVirtioMmioRegion { source, .. } => Some(source),
            Self::InvalidSerialRegion { source, .. } => Some(source),
            Self::InvalidRtcRegion { source, .. } => Some(source),
            Self::InvalidVmGenIdRegion { source, .. } => Some(source),
            Self::InvalidVmClockRegion { source, .. } => Some(source),
            Self::RngSeed { source } => Some(source),
            Self::CreateFdt { source } => Some(source),
            Self::GuestMemoryWrite { source } => Some(source),
            Self::MissingCpu
            | Self::TooManyCpus { .. }
            | Self::DuplicateCpuReg { .. }
            | Self::NoGuestMemoryAfterSystemArea { .. }
            | Self::InvalidDramStart { .. }
            | Self::GuestMemoryTooLarge { .. }
            | Self::UnexpectedDramLayout { .. }
            | Self::GuestMemoryOverlapsMmio64 { .. }
            | Self::InitrdNotInGuestMemory { .. }
            | Self::InitrdOverlapsFdt { .. }
            | Self::InvalidGicRegion { .. }
            | Self::GicRegionsOverlap { .. }
            | Self::GicRegionOverlapsMemory { .. }
            | Self::InvalidGicCompatibility { .. }
            | Self::InvalidGicInterruptCells { .. }
            | Self::InvalidPpi { .. }
            | Self::DuplicatePpi { .. }
            | Self::InvalidPpiIntid { .. }
            | Self::InvalidGicMsiRegion
            | Self::InvalidGicMsiInterruptRange
            | Self::GicMsiRegionOverlaps { .. }
            | Self::GicMsiRegionOverlapsMemory
            | Self::VirtioMmioRegionOverlapsMemory { .. }
            | Self::VirtioMmioRegionOverlapsGic { .. }
            | Self::VirtioMmioRegionsOverlap { .. }
            | Self::InvalidVirtioMmioInterrupt { .. }
            | Self::SerialRegionOverlapsMemory { .. }
            | Self::SerialRegionOverlapsGic { .. }
            | Self::SerialRegionOverlapsVirtioMmio { .. }
            | Self::InvalidSerialInterrupt { .. }
            | Self::InvalidVmGenIdSize { .. }
            | Self::VmGenIdRegionOverlapsMemory { .. }
            | Self::VmGenIdRegionOverlapsGic { .. }
            | Self::VmGenIdRegionOverlapsVirtioMmio { .. }
            | Self::VmGenIdRegionOverlapsRtc { .. }
            | Self::VmGenIdRegionOverlapsSerial { .. }
            | Self::InvalidVmGenIdInterrupt { .. }
            | Self::InvalidVmClockSize { .. }
            | Self::VmClockRegionOverlapsMemory { .. }
            | Self::VmClockRegionOverlapsGic { .. }
            | Self::VmClockRegionOverlapsVirtioMmio { .. }
            | Self::VmClockRegionOverlapsRtc { .. }
            | Self::VmClockRegionOverlapsSerial { .. }
            | Self::VmClockRegionOverlapsVmGenId { .. }
            | Self::InvalidVmClockInterrupt { .. }
            | Self::RtcRegionOverlapsMemory { .. }
            | Self::RtcRegionOverlapsGic { .. }
            | Self::RtcRegionOverlapsVirtioMmio { .. }
            | Self::FdtTooLarge { .. }
            | Self::GuestMemoryLayoutMismatch { .. }
            | Self::SerialRegionOverlapsRtc { .. } => None,
        }
    }
}

impl From<VmFdtError> for Arm64FdtError {
    fn from(source: VmFdtError) -> Self {
        Self::CreateFdt { source }
    }
}

impl From<Arm64FdtCacheHierarchyError> for Arm64FdtError {
    fn from(source: Arm64FdtCacheHierarchyError) -> Self {
        Self::InvalidCacheHierarchy { source }
    }
}

#[derive(Debug)]
struct ValidatedArm64FdtConfig {
    memory_reg_cells: Vec<u64>,
    rtc_device: Option<ValidatedArm64FdtRtcDevice>,
    serial_device: Option<ValidatedArm64FdtSerialDevice>,
    vmgenid_device: Option<ValidatedArm64FdtVmGenIdDevice>,
    vmclock_device: Option<ValidatedArm64FdtVmClockDevice>,
    virtio_mmio_devices: Vec<ValidatedArm64FdtVirtioMmioDevice>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ValidatedArm64FdtRtcDevice {
    region: Arm64FdtRegion,
    range: GuestMemoryRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ValidatedArm64FdtSerialDevice {
    region: Arm64FdtRegion,
    range: GuestMemoryRange,
    interrupt_cell: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ValidatedArm64FdtVmGenIdDevice {
    region: Arm64FdtRegion,
    range: GuestMemoryRange,
    interrupt_cell: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ValidatedArm64FdtVmClockDevice {
    region: Arm64FdtRegion,
    interrupt_cell: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ValidatedArm64FdtVirtioMmioDevice {
    index: usize,
    region: Arm64FdtRegion,
    range: GuestMemoryRange,
    interrupt_cell: u32,
}

pub fn build_arm64_fdt(config: &Arm64FdtConfig<'_>) -> Result<Vec<u8>, Arm64FdtError> {
    let mut rng_seed_source = Arm64FdtOsRngSeedSource::new();
    build_arm64_fdt_with_rng_seed_source(config, &mut rng_seed_source)
}

fn build_arm64_fdt_with_rng_seed_source<Source>(
    config: &Arm64FdtConfig<'_>,
    rng_seed_source: &mut Source,
) -> Result<Vec<u8>, Arm64FdtError>
where
    Source: Arm64FdtRngSeedSource + ?Sized,
{
    let validated = validate_config(config)?;
    let mut rng_seed = [0; ARM64_FDT_RNG_SEED_SIZE];
    rng_seed_source
        .fill_rng_seed(&mut rng_seed)
        .map_err(|source| Arm64FdtError::RngSeed { source })?;

    let mut fdt = FdtWriter::new()?;
    let root = fdt.begin_node("")?;
    fdt.property_string("compatible", ROOT_COMPATIBILITY)?;
    fdt.property_u32("#address-cells", ADDRESS_CELLS)?;
    fdt.property_u32("#size-cells", SIZE_CELLS)?;
    fdt.property_u32("interrupt-parent", GIC_PHANDLE)?;

    create_cpu_nodes(&mut fdt, config.vcpu_mpidrs, config.cache_hierarchy)?;
    create_memory_node(&mut fdt, &validated.memory_reg_cells)?;
    create_chosen_node(&mut fdt, config.boot, &rng_seed)?;
    create_gic_node(&mut fdt, config.gic)?;
    create_timer_node(&mut fdt, config.timer)?;
    if validated.rtc_device.is_some() || validated.serial_device.is_some() {
        create_clock_node(&mut fdt)?;
    }
    create_psci_node(&mut fdt)?;
    if let Some(rtc_device) = validated.rtc_device {
        create_rtc_node(&mut fdt, rtc_device)?;
    }
    if let Some(serial_device) = validated.serial_device {
        create_serial_node(&mut fdt, serial_device)?;
    }
    if let Some(vmgenid_device) = validated.vmgenid_device {
        create_vmgenid_node(&mut fdt, vmgenid_device)?;
    }
    if let Some(vmclock_device) = validated.vmclock_device {
        create_vmclock_node(&mut fdt, vmclock_device)?;
    }
    create_virtio_mmio_nodes(&mut fdt, &validated.virtio_mmio_devices)?;

    fdt.end_node(root)?;
    let bytes = fdt.finish()?;
    validate_fdt_size(bytes.len())?;
    Ok(bytes)
}

pub fn write_arm64_fdt(
    config: &Arm64FdtConfig<'_>,
    memory: &mut GuestMemory,
) -> Result<Arm64FdtGuestMemoryWrite, Arm64FdtError> {
    let mut rng_seed_source = Arm64FdtOsRngSeedSource::new();
    write_arm64_fdt_with_rng_seed_source(config, memory, &mut rng_seed_source)
}

fn write_arm64_fdt_with_rng_seed_source<Source>(
    config: &Arm64FdtConfig<'_>,
    memory: &mut GuestMemory,
    rng_seed_source: &mut Source,
) -> Result<Arm64FdtGuestMemoryWrite, Arm64FdtError>
where
    Source: Arm64FdtRngSeedSource + ?Sized,
{
    validate_guest_memory_matches_layout(config.layout, memory)?;
    let bytes = build_arm64_fdt_with_rng_seed_source(config, rng_seed_source)?;
    write_arm64_fdt_bytes(config.layout, memory, &bytes)
}

fn validate_config(config: &Arm64FdtConfig<'_>) -> Result<ValidatedArm64FdtConfig, Arm64FdtError> {
    if config.vcpu_mpidrs.is_empty() {
        return Err(Arm64FdtError::MissingCpu);
    }
    if config.vcpu_mpidrs.len() > MAX_ARM64_FDT_CPUS {
        return Err(Arm64FdtError::TooManyCpus {
            count: config.vcpu_mpidrs.len(),
            max: MAX_ARM64_FDT_CPUS,
        });
    }

    validate_cpu_regs(config.vcpu_mpidrs)?;
    config
        .cache_hierarchy
        .validate_for_vcpu_count(config.vcpu_mpidrs.len())
        .map_err(|source| Arm64FdtError::InvalidCacheHierarchy { source })?;
    validate_memory_layout(config.layout)?;
    let memory_reg_cells = memory_reg_cells(config.layout)?;
    validate_command_line(config.boot.command_line)?;
    validate_gic(config.layout, config.gic)?;
    validate_timer(config.timer)?;
    validate_gic_timer_ppis(config.gic, config.timer)?;
    let virtio_mmio_devices =
        validate_virtio_mmio_devices(config.layout, config.gic, config.virtio_mmio_devices)?;
    let rtc_device = config
        .rtc_device
        .map(|device| validate_rtc_device(config.layout, config.gic, device, &virtio_mmio_devices))
        .transpose()?;
    let serial_device = config
        .serial_device
        .map(|device| {
            validate_serial_device(
                config.layout,
                config.gic,
                device,
                &virtio_mmio_devices,
                rtc_device.as_ref(),
            )
        })
        .transpose()?;
    let vmgenid_device = config
        .vmgenid_device
        .map(|device| {
            validate_vmgenid_device(
                config.layout,
                config.gic,
                device,
                &virtio_mmio_devices,
                rtc_device.as_ref(),
                serial_device.as_ref(),
            )
        })
        .transpose()?;
    let vmclock_device = config
        .vmclock_device
        .map(|device| {
            validate_vmclock_device(
                config.layout,
                config.gic,
                device,
                &virtio_mmio_devices,
                rtc_device.as_ref(),
                serial_device.as_ref(),
                vmgenid_device.as_ref(),
            )
        })
        .transpose()?;
    if let Some(initrd) = config.boot.initrd {
        validate_initrd(config.layout, initrd)?;
    }

    Ok(ValidatedArm64FdtConfig {
        memory_reg_cells,
        rtc_device,
        serial_device,
        vmgenid_device,
        vmclock_device,
        virtio_mmio_devices,
    })
}

fn validate_cpu_regs(mpidrs: &[u64]) -> Result<(), Arm64FdtError> {
    for (first_index, first_reg) in mpidrs.iter().copied().map(cpu_reg).enumerate() {
        for (second_index, second_reg) in mpidrs
            .iter()
            .copied()
            .map(cpu_reg)
            .enumerate()
            .skip(first_index + 1)
        {
            if first_reg == second_reg {
                return Err(Arm64FdtError::DuplicateCpuReg {
                    first_index,
                    second_index,
                    reg: first_reg,
                });
            }
        }
    }

    Ok(())
}

fn validate_command_line(command_line: &str) -> Result<(), Arm64FdtError> {
    if command_line.as_bytes().contains(&0) {
        return Err(Arm64FdtError::InvalidCommandLine {
            source: BootCommandLineError::ContainsNul,
        });
    }

    let size_with_nul =
        command_line
            .len()
            .checked_add(1)
            .ok_or(Arm64FdtError::InvalidCommandLine {
                source: BootCommandLineError::TooLarge {
                    size_with_nul: usize::MAX,
                    max_size: aarch64::CMDLINE_MAX_SIZE,
                },
            })?;

    if size_with_nul > aarch64::CMDLINE_MAX_SIZE {
        return Err(Arm64FdtError::InvalidCommandLine {
            source: BootCommandLineError::TooLarge {
                size_with_nul,
                max_size: aarch64::CMDLINE_MAX_SIZE,
            },
        });
    }

    Ok(())
}

fn create_cpu_nodes(
    fdt: &mut FdtWriter,
    mpidrs: &[u64],
    cache_hierarchy: &Arm64FdtCacheHierarchy,
) -> Result<(), Arm64FdtError> {
    let cpus = fdt.begin_node("cpus")?;
    fdt.property_u32("#address-cells", CPU_ADDRESS_CELLS)?;
    fdt.property_u32("#size-cells", CPU_SIZE_CELLS)?;

    for (cpu_index, mpidr) in mpidrs.iter().copied().enumerate() {
        let reg = cpu_reg(mpidr);
        let cpu = fdt.begin_node(&format!("cpu@{reg:x}"))?;
        fdt.property_string("device_type", "cpu")?;
        fdt.property_string("compatible", "arm,arm-v8")?;
        fdt.property_string("enable-method", "psci")?;
        fdt.property_u64("reg", reg)?;
        create_l1_cache_properties(fdt, cache_hierarchy.level_caches(1))?;

        if let Some(first_outer) = cache_hierarchy.caches.iter().find(|cache| cache.level > 1) {
            let unit = cpu_index
                / usize::try_from(first_outer.cpus_per_unit)
                    .map_err(|_| Arm64FdtCacheHierarchyError::PhandleOverflow)?;
            fdt.property_u32(
                "next-level-cache",
                cache_phandle(mpidrs.len(), first_outer.level, unit)?,
            )?;
        }

        let last_level = cache_hierarchy
            .caches
            .last()
            .map(|cache| cache.level)
            .unwrap_or(1);
        for level in 2..=last_level {
            let level_caches = cache_hierarchy.level_caches(level);
            let level_cache = level_caches
                .first()
                .ok_or(Arm64FdtCacheHierarchyError::LevelGap)?;
            let share = usize::try_from(level_cache.cpus_per_unit)
                .map_err(|_| Arm64FdtCacheHierarchyError::PhandleOverflow)?;
            if cpu_index % share != 0 {
                continue;
            }

            let unit = cpu_index / share;
            let cache_node = fdt.begin_node(&format!("l{level}-{unit}-cache"))?;
            fdt.property_u32("phandle", cache_phandle(mpidrs.len(), level, unit)?)?;
            fdt.property_string("compatible", "cache")?;
            fdt.property_u32("cache-level", u32::from(level))?;
            create_cache_geometry_properties(fdt, level_caches)?;

            if level < last_level {
                let next_caches = cache_hierarchy.level_caches(level + 1);
                let next_cache = next_caches
                    .first()
                    .ok_or(Arm64FdtCacheHierarchyError::LevelGap)?;
                let next_share = usize::try_from(next_cache.cpus_per_unit)
                    .map_err(|_| Arm64FdtCacheHierarchyError::PhandleOverflow)?;
                fdt.property_u32(
                    "next-level-cache",
                    cache_phandle(mpidrs.len(), level + 1, cpu_index / next_share)?,
                )?;
            }
            fdt.end_node(cache_node)?;
        }
        fdt.end_node(cpu)?;
    }

    fdt.end_node(cpus)?;
    Ok(())
}

fn create_l1_cache_properties(
    fdt: &mut FdtWriter,
    caches: &[Arm64FdtCache],
) -> Result<(), Arm64FdtError> {
    create_cache_geometry_properties(fdt, caches)
}

fn create_cache_geometry_properties(
    fdt: &mut FdtWriter,
    caches: &[Arm64FdtCache],
) -> Result<(), Arm64FdtError> {
    for cache in caches {
        let (size_name, line_name, sets_name) = match cache.cache_type {
            Arm64FdtCacheType::Data => ("d-cache-size", "d-cache-line-size", "d-cache-sets"),
            Arm64FdtCacheType::Instruction => ("i-cache-size", "i-cache-line-size", "i-cache-sets"),
            Arm64FdtCacheType::Unified => {
                fdt.property_null("cache-unified")?;
                ("cache-size", "cache-line-size", "cache-sets")
            }
        };
        fdt.property_u32(size_name, cache.size)?;
        fdt.property_u32(line_name, cache.line_size)?;
        fdt.property_u32(sets_name, cache.sets)?;
    }
    Ok(())
}

fn cache_phandle(
    vcpu_count: usize,
    level: u8,
    unit: usize,
) -> Result<u32, Arm64FdtCacheHierarchyError> {
    let level_offset = usize::from(
        level
            .checked_sub(2)
            .ok_or(Arm64FdtCacheHierarchyError::PhandleOverflow)?,
    );
    let offset = vcpu_count
        .checked_mul(level_offset)
        .and_then(|value| value.checked_add(unit))
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(Arm64FdtCacheHierarchyError::PhandleOverflow)?;
    let phandle = LAST_CACHE_PHANDLE
        .checked_sub(offset)
        .ok_or(Arm64FdtCacheHierarchyError::PhandleOverflow)?;
    if phandle <= MSI_PHANDLE {
        return Err(Arm64FdtCacheHierarchyError::PhandleCollision);
    }
    Ok(phandle)
}

const fn cpu_reg(mpidr: u64) -> u64 {
    mpidr & CPU_REG_MASK
}

fn create_memory_node(fdt: &mut FdtWriter, reg_cells: &[u64]) -> Result<(), Arm64FdtError> {
    let memory = fdt.begin_node("memory@ram")?;
    fdt.property_string("device_type", "memory")?;
    fdt.property_array_u64("reg", reg_cells)?;
    fdt.end_node(memory)?;
    Ok(())
}

fn create_chosen_node(
    fdt: &mut FdtWriter,
    boot: Arm64FdtBootInfo<'_>,
    rng_seed: &[u8; ARM64_FDT_RNG_SEED_SIZE],
) -> Result<(), Arm64FdtError> {
    let chosen = fdt.begin_node("chosen")?;
    fdt.property_string("bootargs", boot.command_line)?;

    if let Some(initrd) = boot.initrd {
        let initrd_end = initrd_range(initrd)?.end_exclusive();
        fdt.property_u64("linux,initrd-start", initrd.address.raw_value())?;
        fdt.property_u64("linux,initrd-end", initrd_end.raw_value())?;
    }

    fdt.property_u32("linux,pci-probe-only", LINUX_PCI_PROBE_ONLY)?;
    fdt.property("rng-seed", rng_seed)?;
    fdt.end_node(chosen)?;
    Ok(())
}

fn create_gic_node(fdt: &mut FdtWriter, gic: Arm64FdtGic) -> Result<(), Arm64FdtError> {
    let interrupt = fdt.begin_node("intc")?;
    fdt.property_string("compatible", gic.compatibility)?;
    fdt.property_null("interrupt-controller")?;
    fdt.property_u32("#interrupt-cells", gic.interrupt_cells)?;
    fdt.property_array_u64(
        "reg",
        &[
            gic.distributor.base,
            gic.distributor.size,
            gic.redistributor.base,
            gic.redistributor.size,
        ],
    )?;
    fdt.property_phandle(GIC_PHANDLE)?;
    fdt.property_u32("#address-cells", ADDRESS_CELLS)?;
    fdt.property_u32("#size-cells", SIZE_CELLS)?;
    fdt.property_null("ranges")?;
    fdt.property_array_u32(
        "interrupts",
        &[
            GIC_FDT_IRQ_TYPE_PPI,
            gic.maintenance_irq,
            IRQ_TYPE_LEVEL_HIGH,
        ],
    )?;
    if let Some(msi) = gic.msi {
        let frame = fdt.begin_node(&format!("v2m@{:x}", msi.region.base))?;
        fdt.property_string("compatible", "arm,gic-v2m-frame")?;
        fdt.property_null("msi-controller")?;
        fdt.property_phandle(MSI_PHANDLE)?;
        fdt.property_array_u64("reg", &[msi.region.base, msi.region.size])?;
        fdt.end_node(frame)?;
    }
    fdt.end_node(interrupt)?;
    Ok(())
}

fn create_timer_node(
    fdt: &mut FdtWriter,
    timer: Arm64FdtTimerInterrupts,
) -> Result<(), Arm64FdtError> {
    let interrupts = [
        GIC_FDT_IRQ_TYPE_PPI,
        timer.secure_physical,
        IRQ_TYPE_LEVEL_HIGH,
        GIC_FDT_IRQ_TYPE_PPI,
        timer.non_secure_physical,
        IRQ_TYPE_LEVEL_HIGH,
        GIC_FDT_IRQ_TYPE_PPI,
        timer.virtual_timer,
        IRQ_TYPE_LEVEL_HIGH,
        GIC_FDT_IRQ_TYPE_PPI,
        timer.hypervisor,
        IRQ_TYPE_LEVEL_HIGH,
    ];

    let timer_node = fdt.begin_node("timer")?;
    fdt.property_string("compatible", "arm,armv8-timer")?;
    fdt.property_null("always-on")?;
    fdt.property_array_u32("interrupts", &interrupts)?;
    fdt.end_node(timer_node)?;
    Ok(())
}

fn create_clock_node(fdt: &mut FdtWriter) -> Result<(), Arm64FdtError> {
    let clock = fdt.begin_node(APB_PCLK_NODE_NAME)?;
    fdt.property_string("compatible", "fixed-clock")?;
    fdt.property_u32("#clock-cells", 0)?;
    fdt.property_u32("clock-frequency", APB_PCLK_CLOCK_FREQUENCY)?;
    fdt.property_string("clock-output-names", APB_PCLK_CLOCK_OUTPUT_NAME)?;
    fdt.property_phandle(CLOCK_PHANDLE)?;
    fdt.end_node(clock)?;
    Ok(())
}

fn create_psci_node(fdt: &mut FdtWriter) -> Result<(), Arm64FdtError> {
    let psci = fdt.begin_node("psci")?;
    fdt.property_string("compatible", "arm,psci-0.2")?;
    fdt.property_string("method", "hvc")?;
    fdt.end_node(psci)?;
    Ok(())
}

fn create_rtc_node(
    fdt: &mut FdtWriter,
    device: ValidatedArm64FdtRtcDevice,
) -> Result<(), Arm64FdtError> {
    let rtc = fdt.begin_node(&format!("{RTC_NODE_PREFIX}@{:x}", device.region.base))?;
    fdt.property("compatible", RTC_COMPATIBILITY)?;
    fdt.property_array_u64("reg", &[device.region.base, device.region.size])?;
    fdt.property_u32("clocks", CLOCK_PHANDLE)?;
    fdt.property_string("clock-names", APB_PCLK_CLOCK_NAME)?;
    fdt.end_node(rtc)?;
    Ok(())
}

fn create_serial_node(
    fdt: &mut FdtWriter,
    device: ValidatedArm64FdtSerialDevice,
) -> Result<(), Arm64FdtError> {
    let serial = fdt.begin_node(&format!("{SERIAL_NODE_PREFIX}@{:x}", device.region.base))?;
    fdt.property_string("compatible", SERIAL_COMPATIBILITY)?;
    fdt.property_array_u64("reg", &[device.region.base, device.region.size])?;
    fdt.property_u32("clocks", CLOCK_PHANDLE)?;
    fdt.property_string("clock-names", APB_PCLK_CLOCK_NAME)?;
    fdt.property_array_u32(
        "interrupts",
        &[
            GIC_FDT_IRQ_TYPE_SPI,
            device.interrupt_cell,
            IRQ_TYPE_EDGE_RISING,
        ],
    )?;
    fdt.end_node(serial)?;
    Ok(())
}

fn create_vmgenid_node(
    fdt: &mut FdtWriter,
    device: ValidatedArm64FdtVmGenIdDevice,
) -> Result<(), Arm64FdtError> {
    let vmgenid = fdt.begin_node(VMGENID_NODE_NAME)?;
    fdt.property_string("compatible", VMGENID_COMPATIBILITY)?;
    fdt.property_array_u64("reg", &[device.region.base, device.region.size])?;
    fdt.property_array_u32(
        "interrupts",
        &[
            GIC_FDT_IRQ_TYPE_SPI,
            device.interrupt_cell,
            IRQ_TYPE_EDGE_RISING,
        ],
    )?;
    fdt.end_node(vmgenid)?;
    Ok(())
}

fn create_vmclock_node(
    fdt: &mut FdtWriter,
    device: ValidatedArm64FdtVmClockDevice,
) -> Result<(), Arm64FdtError> {
    let vmclock = fdt.begin_node(&format!("{VMCLOCK_NODE_PREFIX}@{}", device.region.base))?;
    fdt.property_string("compatible", VMCLOCK_COMPATIBILITY)?;
    fdt.property_array_u64("reg", &[device.region.base, device.region.size])?;
    fdt.property_array_u32(
        "interrupts",
        &[
            GIC_FDT_IRQ_TYPE_SPI,
            device.interrupt_cell,
            IRQ_TYPE_EDGE_RISING,
        ],
    )?;
    fdt.end_node(vmclock)?;
    Ok(())
}

fn create_virtio_mmio_nodes(
    fdt: &mut FdtWriter,
    devices: &[ValidatedArm64FdtVirtioMmioDevice],
) -> Result<(), Arm64FdtError> {
    for device in devices {
        let node = fdt.begin_node(&format!(
            "{VIRTIO_MMIO_NODE_PREFIX}@{:x}",
            device.region.base
        ))?;
        fdt.property_null("dma-coherent")?;
        fdt.property_string("compatible", VIRTIO_MMIO_COMPATIBILITY)?;
        fdt.property_array_u64("reg", &[device.region.base, device.region.size])?;
        fdt.property_array_u32(
            "interrupts",
            &[
                GIC_FDT_IRQ_TYPE_SPI,
                device.interrupt_cell,
                IRQ_TYPE_EDGE_RISING,
            ],
        )?;
        fdt.property_u32("interrupt-parent", GIC_PHANDLE)?;
        fdt.end_node(node)?;
    }

    Ok(())
}

fn validate_memory_layout(layout: &GuestMemoryLayout) -> Result<(), Arm64FdtError> {
    validate_memory_reg_size(layout)?;
    validate_memory_ranges(layout)?;
    let guest_memory_size = validate_guest_memory_size(layout)?;
    validate_aarch64_dram_layout(layout, guest_memory_size)
}

fn validate_memory_reg_size(layout: &GuestMemoryLayout) -> Result<(), Arm64FdtError> {
    let cell_count = memory_reg_cell_count(layout)?;
    validate_fdt_size(memory_reg_size_lower_bound(cell_count)?)
}

fn validate_memory_ranges(layout: &GuestMemoryLayout) -> Result<(), Arm64FdtError> {
    let mmio64_gap = mmio64_gap_range()?;
    for (range_index, range) in layout.ranges().iter().copied().enumerate() {
        if range_index == 0 {
            if range.start().raw_value() != aarch64::SYSTEM_MEM_START {
                return Err(Arm64FdtError::InvalidDramStart {
                    actual: range.start(),
                    expected: GuestAddress::new(aarch64::SYSTEM_MEM_START),
                });
            }
            if range.size() <= aarch64::SYSTEM_MEM_SIZE {
                return Err(Arm64FdtError::NoGuestMemoryAfterSystemArea {
                    first_range: range,
                    system_size: aarch64::SYSTEM_MEM_SIZE,
                });
            }
        }

        validate_memory_range_excludes_mmio64_gap(range, mmio64_gap)?;
    }

    Ok(())
}

fn memory_reg_cells(layout: &GuestMemoryLayout) -> Result<Vec<u64>, Arm64FdtError> {
    let mut cells = Vec::with_capacity(memory_reg_cell_count(layout)?);
    for (range_index, range) in layout.ranges().iter().copied().enumerate() {
        let advertised_range = advertised_memory_range(range_index, range)?;
        cells.push(advertised_range.start().raw_value());
        cells.push(advertised_range.size());
    }

    Ok(cells)
}

fn validate_guest_memory_size(layout: &GuestMemoryLayout) -> Result<u64, Arm64FdtError> {
    let mut size = 0u64;
    for range in layout.ranges().iter().copied() {
        size = size
            .checked_add(range.size())
            .ok_or(Arm64FdtError::GuestMemoryTooLarge {
                size: u64::MAX,
                max_size: aarch64::DRAM_MEM_MAX_SIZE,
            })?;
    }

    if size > aarch64::DRAM_MEM_MAX_SIZE {
        Err(Arm64FdtError::GuestMemoryTooLarge {
            size,
            max_size: aarch64::DRAM_MEM_MAX_SIZE,
        })
    } else {
        Ok(size)
    }
}

fn validate_aarch64_dram_layout(
    layout: &GuestMemoryLayout,
    size: u64,
) -> Result<(), Arm64FdtError> {
    let expected_layout =
        aarch64::dram_layout(size).map_err(|source| Arm64FdtError::InvalidLayout { source })?;
    let actual_ranges = layout.ranges();
    let expected_ranges = expected_layout.ranges();
    let range_count = actual_ranges.len().max(expected_ranges.len());

    for range_index in 0..range_count {
        let expected = expected_ranges.get(range_index).copied();
        let actual = actual_ranges.get(range_index).copied();
        if expected != actual {
            return Err(Arm64FdtError::UnexpectedDramLayout {
                range_index,
                expected,
                actual,
            });
        }
    }

    Ok(())
}

fn memory_reg_cell_count(layout: &GuestMemoryLayout) -> Result<usize, Arm64FdtError> {
    layout
        .ranges()
        .len()
        .checked_mul(MEMORY_REG_CELLS_PER_RANGE)
        .ok_or(Arm64FdtError::FdtTooLarge {
            size: usize::MAX,
            max_size: aarch64::FDT_MAX_SIZE,
        })
}

fn memory_reg_size_lower_bound(cell_count: usize) -> Result<usize, Arm64FdtError> {
    cell_count
        .checked_mul(MEMORY_REG_CELL_SIZE)
        .ok_or(Arm64FdtError::FdtTooLarge {
            size: usize::MAX,
            max_size: aarch64::FDT_MAX_SIZE,
        })
}

fn mmio64_gap_range() -> Result<GuestMemoryRange, Arm64FdtError> {
    GuestMemoryRange::new(
        GuestAddress::new(aarch64::MMIO64_MEM_START),
        aarch64::MMIO64_MEM_SIZE,
    )
    .map_err(|source| Arm64FdtError::InvalidLayout { source })
}

fn validate_memory_range_excludes_mmio64_gap(
    range: GuestMemoryRange,
    mmio64_gap: GuestMemoryRange,
) -> Result<(), Arm64FdtError> {
    if range.overlaps(mmio64_gap) {
        Err(Arm64FdtError::GuestMemoryOverlapsMmio64 { range })
    } else {
        Ok(())
    }
}

fn validate_initrd(layout: &GuestMemoryLayout, initrd: LoadedInitrd) -> Result<(), Arm64FdtError> {
    let range = initrd_range(initrd)?;
    if !is_range_in_guest_memory_node(layout, range)? {
        return Err(Arm64FdtError::InitrdNotInGuestMemory { range });
    }

    let fdt_range = fdt_reserved_range(layout)?;
    if range.overlaps(fdt_range) {
        return Err(Arm64FdtError::InitrdOverlapsFdt {
            end_exclusive: range.end_exclusive(),
            fdt_address: fdt_range.start(),
        });
    }

    Ok(())
}

fn fdt_reserved_range(layout: &GuestMemoryLayout) -> Result<GuestMemoryRange, Arm64FdtError> {
    let address =
        aarch64::fdt_address(layout).map_err(|source| Arm64FdtError::InvalidLayout { source })?;
    GuestMemoryRange::new(address, aarch64::FDT_MAX_SIZE)
        .map_err(|source| Arm64FdtError::InvalidLayout { source })
}

fn initrd_range(initrd: LoadedInitrd) -> Result<GuestMemoryRange, Arm64FdtError> {
    GuestMemoryRange::new(initrd.address, initrd.size)
        .map_err(|source| Arm64FdtError::InvalidInitrdRange { source })
}

fn is_range_in_guest_memory_node(
    layout: &GuestMemoryLayout,
    range: GuestMemoryRange,
) -> Result<bool, Arm64FdtError> {
    for (range_index, memory_range) in layout.ranges().iter().copied().enumerate() {
        let start = if range_index == 0 {
            memory_range
                .start()
                .checked_add(aarch64::SYSTEM_MEM_SIZE)
                .ok_or(Arm64FdtError::InvalidLayout {
                    source: GuestMemoryError::AddressOverflow {
                        start: memory_range.start(),
                        size: aarch64::SYSTEM_MEM_SIZE,
                    },
                })?
        } else {
            memory_range.start()
        };

        if start <= range.start() && range.end_exclusive() <= memory_range.end_exclusive() {
            return Ok(true);
        }
    }

    Ok(false)
}

fn validate_gic(layout: &GuestMemoryLayout, gic: Arm64FdtGic) -> Result<(), Arm64FdtError> {
    let distributor = validate_gic_region("distributor", gic.distributor)?;
    let redistributor = validate_gic_region("redistributor", gic.redistributor)?;
    validate_gic_regions_do_not_overlap(
        "distributor",
        distributor,
        "redistributor",
        redistributor,
    )?;
    validate_gic_region_does_not_overlap_memory(
        layout,
        "distributor",
        gic.distributor,
        distributor,
    )?;
    validate_gic_region_does_not_overlap_memory(
        layout,
        "redistributor",
        gic.redistributor,
        redistributor,
    )?;

    if gic.compatibility != GIC_COMPATIBILITY {
        return Err(Arm64FdtError::InvalidGicCompatibility {
            value: gic.compatibility,
            expected: GIC_COMPATIBILITY,
        });
    }

    if gic.interrupt_cells != 3 {
        return Err(Arm64FdtError::InvalidGicInterruptCells {
            value: gic.interrupt_cells,
        });
    }

    validate_ppi("maintenance_irq", gic.maintenance_irq)?;

    if let Some(msi) = gic.msi {
        validate_gic_msi(layout, gic, msi)?;
    }

    Ok(())
}

fn validate_gic_msi(
    layout: &GuestMemoryLayout,
    gic: Arm64FdtGic,
    msi: Arm64FdtMsi,
) -> Result<(), Arm64FdtError> {
    let msi_range = GuestMemoryRange::new(GuestAddress::new(msi.region.base), msi.region.size)
        .map_err(|_| Arm64FdtError::InvalidGicMsiRegion)?;
    for register_offset in [
        ARM64_GICV2M_MSI_TYPER_OFFSET,
        ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET,
        ARM64_GICV2M_MSI_IIDR_OFFSET,
    ] {
        let required_frame_size = register_offset
            .checked_add(GICV2M_REGISTER_SIZE)
            .ok_or(Arm64FdtError::InvalidGicMsiRegion)?;
        if msi.region.size < required_frame_size {
            return Err(Arm64FdtError::InvalidGicMsiRegion);
        }
    }

    for (name, region) in [
        ("distributor", gic.distributor),
        ("redistributor", gic.redistributor),
    ] {
        let range = validate_gic_region(name, region)?;
        if msi_range.overlaps(range) {
            return Err(Arm64FdtError::GicMsiRegionOverlaps { other: name });
        }
    }
    if layout
        .ranges()
        .iter()
        .copied()
        .any(|memory_range| msi_range.overlaps(memory_range))
    {
        return Err(Arm64FdtError::GicMsiRegionOverlapsMemory);
    }

    let interrupt_end = msi
        .interrupt_range
        .base
        .checked_add(msi.interrupt_range.count)
        .ok_or(Arm64FdtError::InvalidGicMsiInterruptRange)?;
    if msi.interrupt_range.base < FIRST_SPI_INTID
        || msi.interrupt_range.count == 0
        || interrupt_end > ARM64_GICV2M_SPI_END_EXCLUSIVE
    {
        return Err(Arm64FdtError::InvalidGicMsiInterruptRange);
    }

    Ok(())
}

fn validate_gic_timer_ppis(
    gic: Arm64FdtGic,
    timer: Arm64FdtTimerInterrupts,
) -> Result<(), Arm64FdtError> {
    let timer_ppis = [
        ("secure_physical_timer", timer.secure_physical),
        ("non_secure_physical_timer", timer.non_secure_physical),
        ("virtual_timer", timer.virtual_timer),
        ("hypervisor_timer", timer.hypervisor),
    ];

    for (timer_name, timer_ppi) in timer_ppis {
        if gic.maintenance_irq == timer_ppi {
            return Err(Arm64FdtError::DuplicatePpi {
                first: "maintenance_irq",
                second: timer_name,
                value: timer_ppi,
            });
        }
    }

    Ok(())
}

fn validate_gic_region(
    name: &'static str,
    region: Arm64FdtRegion,
) -> Result<GuestMemoryRange, Arm64FdtError> {
    GuestMemoryRange::new(GuestAddress::new(region.base), region.size)
        .map_err(|_| Arm64FdtError::InvalidGicRegion { name, region })
}

fn validated_gic_regions(
    gic: Arm64FdtGic,
) -> Result<[Option<(&'static str, GuestMemoryRange)>; 3], Arm64FdtError> {
    let msi = gic
        .msi
        .map(|msi| {
            GuestMemoryRange::new(GuestAddress::new(msi.region.base), msi.region.size)
                .map(|range| ("msi", range))
                .map_err(|_| Arm64FdtError::InvalidGicMsiRegion)
        })
        .transpose()?;

    Ok([
        Some((
            "distributor",
            validate_gic_region("distributor", gic.distributor)?,
        )),
        Some((
            "redistributor",
            validate_gic_region("redistributor", gic.redistributor)?,
        )),
        msi,
    ])
}

fn validate_gic_regions_do_not_overlap(
    first_name: &'static str,
    first: GuestMemoryRange,
    second_name: &'static str,
    second: GuestMemoryRange,
) -> Result<(), Arm64FdtError> {
    if first.overlaps(second) {
        Err(Arm64FdtError::GicRegionsOverlap {
            first: first_name,
            second: second_name,
        })
    } else {
        Ok(())
    }
}

fn validate_gic_region_does_not_overlap_memory(
    layout: &GuestMemoryLayout,
    name: &'static str,
    region: Arm64FdtRegion,
    gic_range: GuestMemoryRange,
) -> Result<(), Arm64FdtError> {
    for memory_range in layout.ranges().iter().copied() {
        if gic_range.overlaps(memory_range) {
            return Err(Arm64FdtError::GicRegionOverlapsMemory {
                name,
                region,
                memory_range,
            });
        }
    }

    Ok(())
}

fn validate_serial_device(
    layout: &GuestMemoryLayout,
    gic: Arm64FdtGic,
    device: Arm64FdtSerialDevice,
    virtio_mmio_devices: &[ValidatedArm64FdtVirtioMmioDevice],
    rtc_device: Option<&ValidatedArm64FdtRtcDevice>,
) -> Result<ValidatedArm64FdtSerialDevice, Arm64FdtError> {
    let range = validate_serial_region(device.region)?;
    validate_serial_region_does_not_overlap_memory(layout, device.region, range)?;
    validate_serial_region_does_not_overlap_gic(device.region, range, gic)?;
    validate_serial_region_does_not_overlap_virtio_mmio(device.region, range, virtio_mmio_devices)?;
    validate_serial_region_does_not_overlap_rtc(device.region, range, rtc_device)?;

    Ok(ValidatedArm64FdtSerialDevice {
        region: device.region,
        range,
        interrupt_cell: serial_interrupt_cell(device.interrupt_line)?,
    })
}

fn validate_rtc_device(
    layout: &GuestMemoryLayout,
    gic: Arm64FdtGic,
    device: Arm64FdtRtcDevice,
    virtio_mmio_devices: &[ValidatedArm64FdtVirtioMmioDevice],
) -> Result<ValidatedArm64FdtRtcDevice, Arm64FdtError> {
    let range = validate_rtc_region(device.region)?;
    validate_rtc_region_does_not_overlap_memory(layout, device.region, range)?;
    validate_rtc_region_does_not_overlap_gic(device.region, range, gic)?;
    validate_rtc_region_does_not_overlap_virtio_mmio(device.region, range, virtio_mmio_devices)?;

    Ok(ValidatedArm64FdtRtcDevice {
        region: device.region,
        range,
    })
}

fn validate_rtc_region(region: Arm64FdtRegion) -> Result<GuestMemoryRange, Arm64FdtError> {
    GuestMemoryRange::new(GuestAddress::new(region.base), region.size)
        .map_err(|source| Arm64FdtError::InvalidRtcRegion { region, source })
}

fn validate_rtc_region_does_not_overlap_memory(
    layout: &GuestMemoryLayout,
    region: Arm64FdtRegion,
    rtc_range: GuestMemoryRange,
) -> Result<(), Arm64FdtError> {
    for memory_range in layout.ranges().iter().copied() {
        if rtc_range.overlaps(memory_range) {
            return Err(Arm64FdtError::RtcRegionOverlapsMemory {
                region,
                memory_range,
            });
        }
    }

    Ok(())
}

fn validate_rtc_region_does_not_overlap_gic(
    region: Arm64FdtRegion,
    rtc_range: GuestMemoryRange,
    gic: Arm64FdtGic,
) -> Result<(), Arm64FdtError> {
    for (gic_name, gic_range) in validated_gic_regions(gic)?.into_iter().flatten() {
        if rtc_range.overlaps(gic_range) {
            return Err(Arm64FdtError::RtcRegionOverlapsGic {
                region,
                gic: gic_name,
            });
        }
    }

    Ok(())
}

fn validate_rtc_region_does_not_overlap_virtio_mmio(
    region: Arm64FdtRegion,
    rtc_range: GuestMemoryRange,
    virtio_mmio_devices: &[ValidatedArm64FdtVirtioMmioDevice],
) -> Result<(), Arm64FdtError> {
    for device in virtio_mmio_devices {
        if rtc_range.overlaps(device.range) {
            return Err(Arm64FdtError::RtcRegionOverlapsVirtioMmio {
                region,
                virtio_mmio_index: device.index,
                virtio_mmio_region: device.region,
            });
        }
    }

    Ok(())
}

fn validate_serial_region(region: Arm64FdtRegion) -> Result<GuestMemoryRange, Arm64FdtError> {
    GuestMemoryRange::new(GuestAddress::new(region.base), region.size)
        .map_err(|source| Arm64FdtError::InvalidSerialRegion { region, source })
}

fn validate_serial_region_does_not_overlap_memory(
    layout: &GuestMemoryLayout,
    region: Arm64FdtRegion,
    serial_range: GuestMemoryRange,
) -> Result<(), Arm64FdtError> {
    for memory_range in layout.ranges().iter().copied() {
        if serial_range.overlaps(memory_range) {
            return Err(Arm64FdtError::SerialRegionOverlapsMemory {
                region,
                memory_range,
            });
        }
    }

    Ok(())
}

fn validate_serial_region_does_not_overlap_gic(
    region: Arm64FdtRegion,
    serial_range: GuestMemoryRange,
    gic: Arm64FdtGic,
) -> Result<(), Arm64FdtError> {
    for (gic_name, gic_range) in validated_gic_regions(gic)?.into_iter().flatten() {
        if serial_range.overlaps(gic_range) {
            return Err(Arm64FdtError::SerialRegionOverlapsGic {
                region,
                gic: gic_name,
            });
        }
    }

    Ok(())
}

fn validate_serial_region_does_not_overlap_virtio_mmio(
    region: Arm64FdtRegion,
    serial_range: GuestMemoryRange,
    virtio_mmio_devices: &[ValidatedArm64FdtVirtioMmioDevice],
) -> Result<(), Arm64FdtError> {
    for device in virtio_mmio_devices {
        if serial_range.overlaps(device.range) {
            return Err(Arm64FdtError::SerialRegionOverlapsVirtioMmio {
                region,
                virtio_mmio_index: device.index,
                virtio_mmio_region: device.region,
            });
        }
    }

    Ok(())
}

fn validate_serial_region_does_not_overlap_rtc(
    region: Arm64FdtRegion,
    serial_range: GuestMemoryRange,
    rtc_device: Option<&ValidatedArm64FdtRtcDevice>,
) -> Result<(), Arm64FdtError> {
    if let Some(rtc) = rtc_device
        && serial_range.overlaps(rtc.range)
    {
        return Err(Arm64FdtError::SerialRegionOverlapsRtc {
            region,
            rtc_region: rtc.region,
        });
    }

    Ok(())
}

fn serial_interrupt_cell(line: GuestInterruptLine) -> Result<u32, Arm64FdtError> {
    line.raw_value()
        .checked_sub(FIRST_SPI_INTID)
        .ok_or(Arm64FdtError::InvalidSerialInterrupt { line })
}

fn validate_vmgenid_device(
    layout: &GuestMemoryLayout,
    gic: Arm64FdtGic,
    device: Arm64FdtVmGenIdDevice,
    virtio_mmio_devices: &[ValidatedArm64FdtVirtioMmioDevice],
    rtc_device: Option<&ValidatedArm64FdtRtcDevice>,
    serial_device: Option<&ValidatedArm64FdtSerialDevice>,
) -> Result<ValidatedArm64FdtVmGenIdDevice, Arm64FdtError> {
    let range = validate_vmgenid_region(device.region)?;
    validate_vmgenid_region_does_not_overlap_memory(layout, device.region, range)?;
    validate_vmgenid_region_does_not_overlap_gic(device.region, range, gic)?;
    validate_vmgenid_region_does_not_overlap_virtio_mmio(
        device.region,
        range,
        virtio_mmio_devices,
    )?;
    validate_vmgenid_region_does_not_overlap_rtc(device.region, range, rtc_device)?;
    validate_vmgenid_region_does_not_overlap_serial(device.region, range, serial_device)?;

    Ok(ValidatedArm64FdtVmGenIdDevice {
        region: device.region,
        range,
        interrupt_cell: vmgenid_interrupt_cell(device.interrupt_line)?,
    })
}

fn validate_vmgenid_region(region: Arm64FdtRegion) -> Result<GuestMemoryRange, Arm64FdtError> {
    let range = GuestMemoryRange::new(GuestAddress::new(region.base), region.size)
        .map_err(|source| Arm64FdtError::InvalidVmGenIdRegion { region, source })?;
    if region.size != ARM64_FDT_VMGENID_SIZE {
        return Err(Arm64FdtError::InvalidVmGenIdSize {
            size: region.size,
            expected: ARM64_FDT_VMGENID_SIZE,
        });
    }

    Ok(range)
}

fn validate_vmgenid_region_does_not_overlap_memory(
    layout: &GuestMemoryLayout,
    region: Arm64FdtRegion,
    vmgenid_range: GuestMemoryRange,
) -> Result<(), Arm64FdtError> {
    for (range_index, memory_range) in layout.ranges().iter().copied().enumerate() {
        let advertised_range = advertised_memory_range(range_index, memory_range)?;
        if vmgenid_range.overlaps(advertised_range) {
            return Err(Arm64FdtError::VmGenIdRegionOverlapsMemory {
                region,
                memory_range: advertised_range,
            });
        }
    }

    Ok(())
}

fn advertised_memory_range(
    range_index: usize,
    memory_range: GuestMemoryRange,
) -> Result<GuestMemoryRange, Arm64FdtError> {
    if range_index != 0 {
        return Ok(memory_range);
    }

    let start = memory_range
        .start()
        .checked_add(aarch64::SYSTEM_MEM_SIZE)
        .ok_or(Arm64FdtError::InvalidLayout {
            source: GuestMemoryError::AddressOverflow {
                start: memory_range.start(),
                size: aarch64::SYSTEM_MEM_SIZE,
            },
        })?;
    let size = memory_range
        .size()
        .checked_sub(aarch64::SYSTEM_MEM_SIZE)
        .ok_or(Arm64FdtError::NoGuestMemoryAfterSystemArea {
            first_range: memory_range,
            system_size: aarch64::SYSTEM_MEM_SIZE,
        })?;

    GuestMemoryRange::new(start, size).map_err(|source| Arm64FdtError::InvalidLayout { source })
}

fn validate_vmgenid_region_does_not_overlap_gic(
    region: Arm64FdtRegion,
    vmgenid_range: GuestMemoryRange,
    gic: Arm64FdtGic,
) -> Result<(), Arm64FdtError> {
    for (gic_name, gic_range) in validated_gic_regions(gic)?.into_iter().flatten() {
        if vmgenid_range.overlaps(gic_range) {
            return Err(Arm64FdtError::VmGenIdRegionOverlapsGic {
                region,
                gic: gic_name,
            });
        }
    }

    Ok(())
}

fn validate_vmgenid_region_does_not_overlap_virtio_mmio(
    region: Arm64FdtRegion,
    vmgenid_range: GuestMemoryRange,
    virtio_mmio_devices: &[ValidatedArm64FdtVirtioMmioDevice],
) -> Result<(), Arm64FdtError> {
    for device in virtio_mmio_devices {
        if vmgenid_range.overlaps(device.range) {
            return Err(Arm64FdtError::VmGenIdRegionOverlapsVirtioMmio {
                region,
                virtio_mmio_index: device.index,
                virtio_mmio_region: device.region,
            });
        }
    }

    Ok(())
}

fn validate_vmgenid_region_does_not_overlap_rtc(
    region: Arm64FdtRegion,
    vmgenid_range: GuestMemoryRange,
    rtc_device: Option<&ValidatedArm64FdtRtcDevice>,
) -> Result<(), Arm64FdtError> {
    if let Some(rtc) = rtc_device
        && vmgenid_range.overlaps(rtc.range)
    {
        return Err(Arm64FdtError::VmGenIdRegionOverlapsRtc {
            region,
            rtc_region: rtc.region,
        });
    }

    Ok(())
}

fn validate_vmgenid_region_does_not_overlap_serial(
    region: Arm64FdtRegion,
    vmgenid_range: GuestMemoryRange,
    serial_device: Option<&ValidatedArm64FdtSerialDevice>,
) -> Result<(), Arm64FdtError> {
    if let Some(serial) = serial_device
        && vmgenid_range.overlaps(serial.range)
    {
        return Err(Arm64FdtError::VmGenIdRegionOverlapsSerial {
            region,
            serial_region: serial.region,
        });
    }

    Ok(())
}

fn vmgenid_interrupt_cell(line: GuestInterruptLine) -> Result<u32, Arm64FdtError> {
    line.raw_value()
        .checked_sub(FIRST_SPI_INTID)
        .ok_or(Arm64FdtError::InvalidVmGenIdInterrupt { line })
}

fn validate_vmclock_device(
    layout: &GuestMemoryLayout,
    gic: Arm64FdtGic,
    device: Arm64FdtVmClockDevice,
    virtio_mmio_devices: &[ValidatedArm64FdtVirtioMmioDevice],
    rtc_device: Option<&ValidatedArm64FdtRtcDevice>,
    serial_device: Option<&ValidatedArm64FdtSerialDevice>,
    vmgenid_device: Option<&ValidatedArm64FdtVmGenIdDevice>,
) -> Result<ValidatedArm64FdtVmClockDevice, Arm64FdtError> {
    let range = validate_vmclock_region(device.region)?;
    validate_vmclock_region_does_not_overlap_memory(layout, device.region, range)?;
    validate_vmclock_region_does_not_overlap_gic(device.region, range, gic)?;
    validate_vmclock_region_does_not_overlap_virtio_mmio(
        device.region,
        range,
        virtio_mmio_devices,
    )?;
    validate_vmclock_region_does_not_overlap_rtc(device.region, range, rtc_device)?;
    validate_vmclock_region_does_not_overlap_serial(device.region, range, serial_device)?;
    validate_vmclock_region_does_not_overlap_vmgenid(device.region, range, vmgenid_device)?;

    Ok(ValidatedArm64FdtVmClockDevice {
        region: device.region,
        interrupt_cell: vmclock_interrupt_cell(device.interrupt_line)?,
    })
}

fn validate_vmclock_region(region: Arm64FdtRegion) -> Result<GuestMemoryRange, Arm64FdtError> {
    let range = GuestMemoryRange::new(GuestAddress::new(region.base), region.size)
        .map_err(|source| Arm64FdtError::InvalidVmClockRegion { region, source })?;
    if region.size != ARM64_FDT_VMCLOCK_SIZE {
        return Err(Arm64FdtError::InvalidVmClockSize {
            size: region.size,
            expected: ARM64_FDT_VMCLOCK_SIZE,
        });
    }

    Ok(range)
}

fn validate_vmclock_region_does_not_overlap_memory(
    layout: &GuestMemoryLayout,
    region: Arm64FdtRegion,
    vmclock_range: GuestMemoryRange,
) -> Result<(), Arm64FdtError> {
    for (range_index, memory_range) in layout.ranges().iter().copied().enumerate() {
        let advertised_range = advertised_memory_range(range_index, memory_range)?;
        if vmclock_range.overlaps(advertised_range) {
            return Err(Arm64FdtError::VmClockRegionOverlapsMemory {
                region,
                memory_range: advertised_range,
            });
        }
    }

    Ok(())
}

fn validate_vmclock_region_does_not_overlap_gic(
    region: Arm64FdtRegion,
    vmclock_range: GuestMemoryRange,
    gic: Arm64FdtGic,
) -> Result<(), Arm64FdtError> {
    for (gic_name, gic_range) in validated_gic_regions(gic)?.into_iter().flatten() {
        if vmclock_range.overlaps(gic_range) {
            return Err(Arm64FdtError::VmClockRegionOverlapsGic {
                region,
                gic: gic_name,
            });
        }
    }

    Ok(())
}

fn validate_vmclock_region_does_not_overlap_virtio_mmio(
    region: Arm64FdtRegion,
    vmclock_range: GuestMemoryRange,
    virtio_mmio_devices: &[ValidatedArm64FdtVirtioMmioDevice],
) -> Result<(), Arm64FdtError> {
    for device in virtio_mmio_devices {
        if vmclock_range.overlaps(device.range) {
            return Err(Arm64FdtError::VmClockRegionOverlapsVirtioMmio {
                region,
                virtio_mmio_index: device.index,
                virtio_mmio_region: device.region,
            });
        }
    }

    Ok(())
}

fn validate_vmclock_region_does_not_overlap_rtc(
    region: Arm64FdtRegion,
    vmclock_range: GuestMemoryRange,
    rtc_device: Option<&ValidatedArm64FdtRtcDevice>,
) -> Result<(), Arm64FdtError> {
    if let Some(rtc) = rtc_device
        && vmclock_range.overlaps(rtc.range)
    {
        return Err(Arm64FdtError::VmClockRegionOverlapsRtc {
            region,
            rtc_region: rtc.region,
        });
    }

    Ok(())
}

fn validate_vmclock_region_does_not_overlap_serial(
    region: Arm64FdtRegion,
    vmclock_range: GuestMemoryRange,
    serial_device: Option<&ValidatedArm64FdtSerialDevice>,
) -> Result<(), Arm64FdtError> {
    if let Some(serial) = serial_device
        && vmclock_range.overlaps(serial.range)
    {
        return Err(Arm64FdtError::VmClockRegionOverlapsSerial {
            region,
            serial_region: serial.region,
        });
    }

    Ok(())
}

fn validate_vmclock_region_does_not_overlap_vmgenid(
    region: Arm64FdtRegion,
    vmclock_range: GuestMemoryRange,
    vmgenid_device: Option<&ValidatedArm64FdtVmGenIdDevice>,
) -> Result<(), Arm64FdtError> {
    if let Some(vmgenid) = vmgenid_device
        && vmclock_range.overlaps(vmgenid.range)
    {
        return Err(Arm64FdtError::VmClockRegionOverlapsVmGenId {
            region,
            vmgenid_region: vmgenid.region,
        });
    }

    Ok(())
}

fn vmclock_interrupt_cell(line: GuestInterruptLine) -> Result<u32, Arm64FdtError> {
    line.raw_value()
        .checked_sub(FIRST_SPI_INTID)
        .ok_or(Arm64FdtError::InvalidVmClockInterrupt { line })
}

fn validate_virtio_mmio_devices(
    layout: &GuestMemoryLayout,
    gic: Arm64FdtGic,
    devices: &[Arm64FdtVirtioMmioDevice],
) -> Result<Vec<ValidatedArm64FdtVirtioMmioDevice>, Arm64FdtError> {
    let mut validated = Vec::with_capacity(devices.len());
    for (index, device) in devices.iter().copied().enumerate() {
        let range = validate_virtio_mmio_region(index, device.region)?;
        validate_virtio_mmio_region_does_not_overlap_memory(layout, index, device.region, range)?;
        validate_virtio_mmio_region_does_not_overlap_gic(index, device.region, range, gic)?;
        validate_virtio_mmio_region_does_not_overlap_devices(
            index,
            device.region,
            range,
            &validated,
        )?;
        validated.push(ValidatedArm64FdtVirtioMmioDevice {
            index,
            region: device.region,
            range,
            interrupt_cell: virtio_mmio_interrupt_cell(index, device.interrupt_line)?,
        });
    }

    validated.sort_by_key(|device| device.region.base);
    Ok(validated)
}

fn validate_virtio_mmio_region(
    index: usize,
    region: Arm64FdtRegion,
) -> Result<GuestMemoryRange, Arm64FdtError> {
    GuestMemoryRange::new(GuestAddress::new(region.base), region.size).map_err(|source| {
        Arm64FdtError::InvalidVirtioMmioRegion {
            index,
            region,
            source,
        }
    })
}

fn validate_virtio_mmio_region_does_not_overlap_memory(
    layout: &GuestMemoryLayout,
    index: usize,
    region: Arm64FdtRegion,
    device_range: GuestMemoryRange,
) -> Result<(), Arm64FdtError> {
    for memory_range in layout.ranges().iter().copied() {
        if device_range.overlaps(memory_range) {
            return Err(Arm64FdtError::VirtioMmioRegionOverlapsMemory {
                index,
                region,
                memory_range,
            });
        }
    }

    Ok(())
}

fn validate_virtio_mmio_region_does_not_overlap_gic(
    index: usize,
    region: Arm64FdtRegion,
    device_range: GuestMemoryRange,
    gic: Arm64FdtGic,
) -> Result<(), Arm64FdtError> {
    for (gic_name, gic_range) in validated_gic_regions(gic)?.into_iter().flatten() {
        if device_range.overlaps(gic_range) {
            return Err(Arm64FdtError::VirtioMmioRegionOverlapsGic {
                index,
                region,
                gic: gic_name,
            });
        }
    }

    Ok(())
}

fn validate_virtio_mmio_region_does_not_overlap_devices(
    index: usize,
    region: Arm64FdtRegion,
    device_range: GuestMemoryRange,
    previous_devices: &[ValidatedArm64FdtVirtioMmioDevice],
) -> Result<(), Arm64FdtError> {
    for previous_device in previous_devices {
        if device_range.overlaps(previous_device.range) {
            return Err(Arm64FdtError::VirtioMmioRegionsOverlap {
                first_index: previous_device.index,
                second_index: index,
                first_region: previous_device.region,
                second_region: region,
            });
        }
    }

    Ok(())
}

fn virtio_mmio_interrupt_cell(
    index: usize,
    line: GuestInterruptLine,
) -> Result<u32, Arm64FdtError> {
    line.raw_value()
        .checked_sub(FIRST_SPI_INTID)
        .ok_or(Arm64FdtError::InvalidVirtioMmioInterrupt { index, line })
}

fn validate_timer(timer: Arm64FdtTimerInterrupts) -> Result<(), Arm64FdtError> {
    validate_ppi("secure_physical_timer", timer.secure_physical)?;
    validate_ppi("non_secure_physical_timer", timer.non_secure_physical)?;
    validate_ppi("virtual_timer", timer.virtual_timer)?;
    validate_ppi("hypervisor_timer", timer.hypervisor)?;
    validate_distinct_timer_ppis(timer)?;
    Ok(())
}

fn validate_ppi(name: &'static str, value: u32) -> Result<(), Arm64FdtError> {
    if value < FIRST_PPI_INTID {
        Ok(())
    } else {
        Err(Arm64FdtError::InvalidPpi { name, value })
    }
}

fn validate_distinct_timer_ppis(timer: Arm64FdtTimerInterrupts) -> Result<(), Arm64FdtError> {
    let values = [
        ("secure_physical_timer", timer.secure_physical),
        ("non_secure_physical_timer", timer.non_secure_physical),
        ("virtual_timer", timer.virtual_timer),
        ("hypervisor_timer", timer.hypervisor),
    ];

    for (left_index, (left_name, left_value)) in values.iter().copied().enumerate() {
        for (right_name, right_value) in values.iter().copied().skip(left_index + 1) {
            if left_value == right_value {
                return Err(Arm64FdtError::DuplicatePpi {
                    first: left_name,
                    second: right_name,
                    value: left_value,
                });
            }
        }
    }

    Ok(())
}

fn ppi_from_intid(name: &'static str, intid: u32) -> Result<u32, Arm64FdtError> {
    if (FIRST_PPI_INTID..FIRST_SPI_INTID).contains(&intid) {
        Ok(intid - FIRST_PPI_INTID)
    } else {
        Err(Arm64FdtError::InvalidPpiIntid { name, intid })
    }
}

fn validate_fdt_size(size: usize) -> Result<(), Arm64FdtError> {
    if size_as_u64(size) > aarch64::FDT_MAX_SIZE {
        Err(Arm64FdtError::FdtTooLarge {
            size,
            max_size: aarch64::FDT_MAX_SIZE,
        })
    } else {
        Ok(())
    }
}

fn write_arm64_fdt_bytes(
    layout: &GuestMemoryLayout,
    memory: &mut GuestMemory,
    bytes: &[u8],
) -> Result<Arm64FdtGuestMemoryWrite, Arm64FdtError> {
    validate_fdt_size(bytes.len())?;
    let address =
        aarch64::fdt_address(layout).map_err(|source| Arm64FdtError::InvalidLayout { source })?;
    memory
        .write_slice(bytes, address)
        .map_err(|source| Arm64FdtError::GuestMemoryWrite { source })?;

    Ok(Arm64FdtGuestMemoryWrite {
        address,
        size: bytes.len(),
    })
}

fn validate_guest_memory_matches_layout(
    layout: &GuestMemoryLayout,
    memory: &GuestMemory,
) -> Result<(), Arm64FdtError> {
    let layout_ranges = layout.ranges();
    let memory_regions = memory.regions();
    let range_count = layout_ranges.len().max(memory_regions.len());

    for range_index in 0..range_count {
        let expected = layout_ranges.get(range_index).copied();
        let actual = memory_regions.get(range_index).map(|region| region.range());
        if expected != actual {
            return Err(Arm64FdtError::GuestMemoryLayoutMismatch {
                range_index,
                expected,
                actual,
            });
        }
    }

    Ok(())
}

fn size_as_u64(size: usize) -> u64 {
    u64::try_from(size).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use device_tree::{DeviceTree, Node};

    use super::*;

    const TEST_MEMORY_SIZE: u64 = aarch64::SYSTEM_MEM_SIZE + aarch64::FDT_MAX_SIZE + 0x40_0000;
    const TEST_INITRD_ADDRESS: GuestAddress =
        GuestAddress::new(aarch64::DRAM_MEM_START + 0x30_0000);
    const TEST_INITRD_SIZE: u64 = 0x1000;
    const TEST_VCPU_MPIDRS: &[u64] = &[0];
    const TEST_RNG_SEED: [u8; ARM64_FDT_RNG_SEED_SIZE] = [0xa5; ARM64_FDT_RNG_SEED_SIZE];
    static TEST_CACHE_HIERARCHY: LazyLock<Arm64FdtCacheHierarchy> = LazyLock::new(|| {
        Arm64FdtCacheHierarchy::new(vec![
            Arm64FdtCache::new(1, Arm64FdtCacheType::Unified, 32_768, 64, 64, 8, 1)
                .expect("test L1 cache should be valid"),
        ])
        .expect("test cache hierarchy should be valid")
    });

    fn test_cache(
        level: u8,
        cache_type: Arm64FdtCacheType,
        size: u32,
        line_size: u32,
        sets: u32,
        ways: u32,
        cpus_per_unit: u32,
    ) -> Arm64FdtCache {
        Arm64FdtCache::new(
            level,
            cache_type,
            size,
            line_size,
            sets,
            ways,
            cpus_per_unit,
        )
        .expect("test cache should be valid")
    }

    fn split_three_level_cache_hierarchy(
        l2_cpus_per_unit: u32,
        l3_cpus_per_unit: u32,
    ) -> Arm64FdtCacheHierarchy {
        Arm64FdtCacheHierarchy::new(vec![
            test_cache(1, Arm64FdtCacheType::Instruction, 131_072, 64, 512, 4, 1),
            test_cache(1, Arm64FdtCacheType::Data, 65_536, 64, 128, 8, 1),
            test_cache(
                2,
                Arm64FdtCacheType::Unified,
                4_194_304,
                128,
                2048,
                16,
                l2_cpus_per_unit,
            ),
            test_cache(
                3,
                Arm64FdtCacheType::Unified,
                8_388_608,
                128,
                4096,
                16,
                l3_cpus_per_unit,
            ),
        ])
        .expect("test cache hierarchy should be valid")
    }

    #[test]
    fn cache_geometry_validation_is_checked_and_redacted() {
        assert_eq!(
            Arm64FdtCache::new(0, Arm64FdtCacheType::Data, 1, 1, 1, 1, 1),
            Err(Arm64FdtCacheError::InvalidLevel)
        );
        assert_eq!(
            Arm64FdtCache::new(8, Arm64FdtCacheType::Data, 1, 1, 1, 1, 1),
            Err(Arm64FdtCacheError::InvalidLevel)
        );
        for geometry in [(0, 1, 1, 1), (1, 0, 1, 1), (1, 1, 0, 1), (1, 1, 1, 0)] {
            let (size, line_size, sets, ways) = geometry;
            assert_eq!(
                Arm64FdtCache::new(1, Arm64FdtCacheType::Data, size, line_size, sets, ways, 1,),
                Err(Arm64FdtCacheError::ZeroGeometry)
            );
        }
        assert_eq!(
            Arm64FdtCache::new(1, Arm64FdtCacheType::Data, 1, 1, 1, 1, 0),
            Err(Arm64FdtCacheError::ZeroSharing)
        );
        assert_eq!(
            Arm64FdtCache::new(1, Arm64FdtCacheType::Data, u32::MAX, u32::MAX, 2, 2, 1,),
            Err(Arm64FdtCacheError::GeometryOverflow)
        );
        assert_eq!(
            Arm64FdtCache::new(1, Arm64FdtCacheType::Data, 2, 1, 1, 1, 1),
            Err(Arm64FdtCacheError::GeometryMismatch)
        );

        let cache = test_cache(1, Arm64FdtCacheType::Data, 65_536, 64, 128, 8, 1);
        assert_eq!(
            format!("{cache:?}"),
            "Arm64FdtCache { cache_geometry: \"<redacted>\" }"
        );
    }

    #[test]
    fn cache_hierarchy_rejects_every_invalid_graph_shape() {
        assert_eq!(
            Arm64FdtCacheHierarchy::new(vec![]),
            Err(Arm64FdtCacheHierarchyError::Empty)
        );

        let l1d = test_cache(1, Arm64FdtCacheType::Data, 65_536, 64, 128, 8, 1);
        let l1i = test_cache(1, Arm64FdtCacheType::Instruction, 131_072, 64, 512, 4, 1);
        let l1u = test_cache(1, Arm64FdtCacheType::Unified, 32_768, 64, 64, 8, 1);
        let l2 = test_cache(2, Arm64FdtCacheType::Unified, 4_194_304, 128, 2048, 16, 4);
        let l3 = test_cache(3, Arm64FdtCacheType::Unified, 8_388_608, 128, 4096, 16, 8);

        assert_eq!(
            Arm64FdtCacheHierarchy::new(vec![l2]),
            Err(Arm64FdtCacheHierarchyError::MissingL1)
        );
        assert_eq!(
            Arm64FdtCacheHierarchy::new(vec![l1d, l3]),
            Err(Arm64FdtCacheHierarchyError::LevelGap)
        );
        assert_eq!(
            Arm64FdtCacheHierarchy::new(vec![l1d, l1d]),
            Err(Arm64FdtCacheHierarchyError::DuplicateType)
        );
        assert_eq!(
            Arm64FdtCacheHierarchy::new(vec![l1d, l1u]),
            Err(Arm64FdtCacheHierarchyError::MixedUnifiedAndSplit)
        );
        assert_eq!(
            Arm64FdtCacheHierarchy::new(vec![
                l1d,
                test_cache(2, Arm64FdtCacheType::Data, 65_536, 64, 128, 8, 4),
                test_cache(2, Arm64FdtCacheType::Instruction, 131_072, 64, 512, 4, 4,),
            ]),
            Err(Arm64FdtCacheHierarchyError::NonUnifiedOuter)
        );
        assert_eq!(
            Arm64FdtCacheHierarchy::new(vec![
                l1d,
                test_cache(1, Arm64FdtCacheType::Instruction, 131_072, 64, 512, 4, 2,),
            ]),
            Err(Arm64FdtCacheHierarchyError::InconsistentSharing)
        );
        assert_eq!(
            Arm64FdtCacheHierarchy::new(vec![test_cache(
                1,
                Arm64FdtCacheType::Unified,
                32_768,
                64,
                64,
                8,
                2,
            )]),
            Err(Arm64FdtCacheHierarchyError::InvalidL1Sharing)
        );
        assert_eq!(
            Arm64FdtCacheHierarchy::new(vec![
                l1i,
                l1d,
                l2,
                test_cache(3, Arm64FdtCacheType::Unified, 8_388_608, 128, 4096, 16, 6,),
            ]),
            Err(Arm64FdtCacheHierarchyError::InvalidNestedSharing)
        );

        let hierarchy = Arm64FdtCacheHierarchy::new(vec![l1u]).expect("L1 should be valid");
        assert_eq!(
            hierarchy.validate_for_vcpu_count(0),
            Err(Arm64FdtCacheHierarchyError::MissingCpu)
        );
        assert_eq!(
            cache_phandle(usize::MAX, 7, usize::MAX),
            Err(Arm64FdtCacheHierarchyError::PhandleOverflow)
        );
        assert_eq!(
            cache_phandle(800, 7, 0),
            Err(Arm64FdtCacheHierarchyError::PhandleCollision)
        );
        assert_eq!(
            cache_phandle(
                1,
                2,
                usize::try_from(LAST_CACHE_PHANDLE - MSI_PHANDLE)
                    .expect("reserved phandle offset should fit usize"),
            ),
            Err(Arm64FdtCacheHierarchyError::PhandleCollision)
        );
        assert_eq!(
            cache_phandle(
                1,
                2,
                usize::try_from(LAST_CACHE_PHANDLE - MSI_PHANDLE - 1)
                    .expect("first unreserved phandle offset should fit usize"),
            ),
            Ok(MSI_PHANDLE + 1)
        );
        assert_eq!(
            format!("{:?}", split_three_level_cache_hierarchy(4, 8)),
            "Arm64FdtCacheHierarchy { cache_topology: \"<redacted>\" }"
        );
    }

    #[test]
    fn one_cpu_fdt_contains_exact_split_l1_and_linked_outer_caches() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let hierarchy = split_three_level_cache_hierarchy(4, 8);
        let mut config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );
        config.cache_hierarchy = &hierarchy;

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let cpu = required_node(&tree, "/cpus/cpu@0");
        assert_eq!(cpu.prop_u32("d-cache-size").unwrap(), 65_536);
        assert_eq!(cpu.prop_u32("d-cache-line-size").unwrap(), 64);
        assert_eq!(cpu.prop_u32("d-cache-sets").unwrap(), 128);
        assert_eq!(cpu.prop_u32("i-cache-size").unwrap(), 131_072);
        assert_eq!(cpu.prop_u32("i-cache-line-size").unwrap(), 64);
        assert_eq!(cpu.prop_u32("i-cache-sets").unwrap(), 512);
        assert!(!cpu.has_prop("cache-unified"));
        assert_eq!(cpu.prop_u32("next-level-cache").unwrap(), 4000);

        let l2 = required_node(&tree, "/cpus/cpu@0/l2-0-cache");
        assert_eq!(l2.prop_u32("phandle").unwrap(), 4000);
        assert_eq!(l2.prop_str("compatible").unwrap(), "cache");
        assert_eq!(l2.prop_u32("cache-level").unwrap(), 2);
        assert!(l2.has_prop("cache-unified"));
        assert_eq!(l2.prop_u32("cache-size").unwrap(), 4_194_304);
        assert_eq!(l2.prop_u32("cache-line-size").unwrap(), 128);
        assert_eq!(l2.prop_u32("cache-sets").unwrap(), 2048);
        assert_eq!(l2.prop_u32("next-level-cache").unwrap(), 3999);

        let l3 = required_node(&tree, "/cpus/cpu@0/l3-0-cache");
        assert_eq!(l3.prop_u32("phandle").unwrap(), 3999);
        assert_eq!(l3.prop_u32("cache-level").unwrap(), 3);
        assert!(l3.has_prop("cache-unified"));
        assert_eq!(l3.prop_u32("cache-size").unwrap(), 8_388_608);
        assert_eq!(l3.prop_u32("cache-line-size").unwrap(), 128);
        assert_eq!(l3.prop_u32("cache-sets").unwrap(), 4096);
        assert!(!l3.has_prop("next-level-cache"));
    }

    #[test]
    fn partial_last_unit_creates_each_l2_node_once() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let hierarchy = Arm64FdtCacheHierarchy::new(vec![
            test_cache(1, Arm64FdtCacheType::Unified, 32_768, 64, 64, 8, 1),
            test_cache(2, Arm64FdtCacheType::Unified, 4_194_304, 128, 2048, 16, 4),
        ])
        .expect("test cache hierarchy should be valid");
        let mpidrs = [0, 1, 2, 3, 4, 5];
        let mut config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );
        config.vcpu_mpidrs = &mpidrs;
        config.cache_hierarchy = &hierarchy;

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        for cpu_index in 0..6 {
            let cpu = required_node(&tree, &format!("/cpus/cpu@{cpu_index:x}"));
            assert!(cpu.has_prop("cache-unified"));
            assert_eq!(
                cpu.prop_u32("next-level-cache").unwrap(),
                if cpu_index < 4 { 4000 } else { 3999 }
            );
        }
        let l2_0 = required_node(&tree, "/cpus/cpu@0/l2-0-cache");
        let l2_1 = required_node(&tree, "/cpus/cpu@4/l2-1-cache");
        assert_eq!(l2_0.prop_u32("phandle").unwrap(), 4000);
        assert_eq!(l2_1.prop_u32("phandle").unwrap(), 3999);
        for cpu_index in [1, 2, 3, 5] {
            assert!(
                tree.find(&format!("/cpus/cpu@{cpu_index:x}/l2-0-cache"))
                    .is_none()
            );
            assert!(
                tree.find(&format!("/cpus/cpu@{cpu_index:x}/l2-1-cache"))
                    .is_none()
            );
        }
    }

    #[test]
    fn nested_l3_groups_have_exact_stable_phandles_and_links() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let hierarchy = split_three_level_cache_hierarchy(2, 4);
        let mpidrs = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let mut config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );
        config.vcpu_mpidrs = &mpidrs;
        config.cache_hierarchy = &hierarchy;

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        for cpu_index in 0..10 {
            let cpu = required_node(&tree, &format!("/cpus/cpu@{cpu_index:x}"));
            assert_eq!(
                cpu.prop_u32("next-level-cache").unwrap(),
                4000 - cpu_index / 2
            );
        }

        for l2_unit in 0..5 {
            let owner = l2_unit * 2;
            let l2 = required_node(&tree, &format!("/cpus/cpu@{owner:x}/l2-{l2_unit}-cache"));
            assert_eq!(l2.prop_u32("phandle").unwrap(), 4000 - l2_unit);
            assert_eq!(l2.prop_u32("next-level-cache").unwrap(), 3990 - l2_unit / 2);
        }

        for l3_unit in 0..3 {
            let owner = l3_unit * 4;
            let l3 = required_node(&tree, &format!("/cpus/cpu@{owner:x}/l3-{l3_unit}-cache"));
            assert_eq!(l3.prop_u32("phandle").unwrap(), 3990 - l3_unit);
            assert!(!l3.has_prop("next-level-cache"));
        }
    }

    #[test]
    fn builds_minimal_firecracker_shaped_fdt_with_initrd() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "console=ttyAMA0 reboot=k",
                initrd: Some(test_initrd()),
            },
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");

        assert_eq!(
            tree.root.prop_str("compatible").unwrap(),
            ROOT_COMPATIBILITY
        );
        assert_eq!(tree.root.prop_u32("#address-cells").unwrap(), 2);
        assert_eq!(tree.root.prop_u32("#size-cells").unwrap(), 2);
        assert_eq!(tree.root.prop_u32("interrupt-parent").unwrap(), GIC_PHANDLE);

        let root_children: Vec<&str> = tree
            .root
            .children
            .iter()
            .map(|child| child.name.as_str())
            .collect();
        assert_eq!(
            root_children,
            ["cpus", "memory@ram", "chosen", "intc", "timer", "psci"]
        );
        assert!(tree.find("/apb-pclk").is_none());
        assert!(tree.find("/uart@40002000").is_none());

        let cpu = required_node(&tree, "/cpus/cpu@0");
        assert_eq!(cpu.prop_str("device_type").unwrap(), "cpu");
        assert_eq!(cpu.prop_str("compatible").unwrap(), "arm,arm-v8");
        assert_eq!(cpu.prop_str("enable-method").unwrap(), "psci");
        assert_eq!(cpu.prop_u64("reg").unwrap(), 0);

        let chosen = required_node(&tree, "/chosen");
        assert_eq!(
            chosen.prop_str("bootargs").unwrap(),
            "console=ttyAMA0 reboot=k"
        );
        assert_eq!(
            chosen.prop_u64("linux,initrd-start").unwrap(),
            TEST_INITRD_ADDRESS.raw_value()
        );
        assert_eq!(
            chosen.prop_u64("linux,initrd-end").unwrap(),
            TEST_INITRD_ADDRESS.raw_value() + TEST_INITRD_SIZE
        );
        assert_eq!(
            chosen.prop_raw("rng-seed").unwrap(),
            TEST_RNG_SEED.as_slice()
        );
        assert_eq!(
            chosen.prop_u32("linux,pci-probe-only").unwrap(),
            LINUX_PCI_PROBE_ONLY
        );

        let psci = required_node(&tree, "/psci");
        assert_eq!(psci.prop_str("compatible").unwrap(), "arm,psci-0.2");
        assert_eq!(psci.prop_str("method").unwrap(), "hvc");
    }

    #[test]
    fn omits_initrd_properties_when_initrd_is_not_loaded() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let chosen = required_node(&tree, "/chosen");

        assert_eq!(chosen.prop_str("bootargs").unwrap(), "panic=1");
        assert!(!chosen.has_prop("linux,initrd-start"));
        assert!(!chosen.has_prop("linux,initrd-end"));
    }

    #[test]
    fn chosen_node_contains_injected_rng_seed() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );
        let mut seed = [0; ARM64_FDT_RNG_SEED_SIZE];
        for (index, byte) in seed.iter_mut().enumerate() {
            *byte = u8::try_from(index).expect("test seed index should fit in u8");
        }
        let mut source = FixedRngSeedSource::new(seed);

        let bytes = build_arm64_fdt_with_rng_seed_source(&config, &mut source)
            .expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let chosen = required_node(&tree, "/chosen");
        let rng_seed = chosen.prop_raw("rng-seed").expect("rng-seed should exist");

        assert_eq!(rng_seed.len(), ARM64_FDT_RNG_SEED_SIZE);
        assert_eq!(rng_seed, seed.as_slice());
    }

    #[test]
    fn default_build_emits_os_rng_seed() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );

        let bytes = build_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let chosen = required_node(&tree, "/chosen");

        assert_eq!(
            chosen
                .prop_raw("rng-seed")
                .expect("rng-seed should exist")
                .len(),
            ARM64_FDT_RNG_SEED_SIZE
        );
    }

    #[test]
    fn reports_rng_seed_source_errors() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );
        let mut source = FailingRngSeedSource;

        let err = build_arm64_fdt_with_rng_seed_source(&config, &mut source)
            .expect_err("rng-seed failure should fail FDT generation");

        assert_eq!(
            err,
            Arm64FdtError::RngSeed {
                source: Arm64FdtRngSeedError::new(),
            }
        );
        assert_eq!(
            err.to_string(),
            "failed to create arm64 FDT rng-seed: failed to generate arm64 FDT rng-seed"
        );
        assert_eq!(
            std::error::Error::source(&err)
                .expect("rng-seed error should expose its source")
                .to_string(),
            "failed to generate arm64 FDT rng-seed"
        );
    }

    #[test]
    fn accepts_command_line_at_fdt_limit() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let command_line = "x".repeat(aarch64::CMDLINE_MAX_SIZE - 1);
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: &command_line,
                initrd: None,
            },
        );

        let bytes = build_test_arm64_fdt(&config).expect("max-sized command line should fit");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let chosen = required_node(&tree, "/chosen");

        assert_eq!(chosen.prop_str("bootargs").unwrap(), command_line);
    }

    #[test]
    fn rejects_oversized_command_line() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let command_line = "x".repeat(aarch64::CMDLINE_MAX_SIZE);
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: &command_line,
                initrd: None,
            },
        );

        let err = build_test_arm64_fdt(&config).expect_err("oversized command line should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidCommandLine {
                source: BootCommandLineError::TooLarge {
                    size_with_nul: aarch64::CMDLINE_MAX_SIZE + 1,
                    max_size: aarch64::CMDLINE_MAX_SIZE,
                },
            }
        );
    }

    #[test]
    fn rejects_command_line_with_nul() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1\0debug",
                initrd: None,
            },
        );

        let err = build_test_arm64_fdt(&config).expect_err("NUL command line should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidCommandLine {
                source: BootCommandLineError::ContainsNul,
            }
        );
    }

    #[test]
    fn rejects_empty_initrd_range() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let address = GuestAddress::new(aarch64::DRAM_MEM_START + aarch64::SYSTEM_MEM_SIZE);
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: Some(LoadedInitrd { address, size: 0 }),
            },
        );

        let err = build_test_arm64_fdt(&config).expect_err("empty initrd range should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidInitrdRange {
                source: GuestMemoryError::EmptyRange { start: address },
            }
        );
    }

    #[test]
    fn rejects_initrd_outside_guest_memory_node() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let range = GuestMemoryRange::new(GuestAddress::new(aarch64::DRAM_MEM_START), 0x1000)
            .expect("test initrd range should be valid");
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: Some(LoadedInitrd {
                    address: range.start(),
                    size: range.size(),
                }),
            },
        );

        let err = build_test_arm64_fdt(&config)
            .expect_err("initrd outside advertised memory should fail");

        assert_eq!(err, Arm64FdtError::InitrdNotInGuestMemory { range });
    }

    #[test]
    fn rejects_initrd_overlapping_fdt() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let fdt_address = aarch64::fdt_address(&layout).expect("FDT address should resolve");
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: Some(LoadedInitrd {
                    address: fdt_address,
                    size: 1,
                }),
            },
        );

        let err = build_test_arm64_fdt(&config).expect_err("initrd overlapping FDT should fail");

        assert_eq!(
            err,
            Arm64FdtError::InitrdOverlapsFdt {
                end_exclusive: fdt_address
                    .checked_add(1)
                    .expect("test initrd end should not overflow"),
                fdt_address,
            }
        );
        assert_eq!(
            err.to_string(),
            format!(
                "arm64 FDT initrd range ending at {} overlaps reserved FDT window starting at {}",
                fdt_address
                    .checked_add(1)
                    .expect("test initrd end should not overflow"),
                fdt_address
            )
        );
    }

    #[test]
    fn accepts_initrd_in_later_dram_range_after_fdt_window() {
        let layout = aarch64::dram_layout(aarch64::MMIO64_MEM_START - aarch64::DRAM_MEM_START + 1)
            .expect("split layout should be valid");
        let second_range = layout.ranges()[1];
        let initrd = LoadedInitrd {
            address: second_range.start(),
            size: second_range.size(),
        };
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: Some(initrd),
            },
        );

        let bytes =
            build_test_arm64_fdt(&config).expect("later-range initrd should not overlap FDT");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let chosen = required_node(&tree, "/chosen");

        assert_eq!(
            chosen.prop_u64("linux,initrd-start").unwrap(),
            second_range.start().raw_value()
        );
        assert_eq!(
            chosen.prop_u64("linux,initrd-end").unwrap(),
            second_range.end_exclusive().raw_value()
        );
    }

    #[test]
    fn memory_node_excludes_reserved_system_area() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let memory = required_node(&tree, "/memory@ram");
        let reg = prop_u64_cells(memory, "reg");

        assert_eq!(
            reg,
            vec![
                aarch64::DRAM_MEM_START + aarch64::SYSTEM_MEM_SIZE,
                TEST_MEMORY_SIZE - aarch64::SYSTEM_MEM_SIZE,
            ]
        );
    }

    #[test]
    fn rejects_memory_range_overlapping_mmio64_gap() {
        let first_range = GuestMemoryRange::new(
            GuestAddress::new(aarch64::DRAM_MEM_START),
            aarch64::SYSTEM_MEM_SIZE + aarch64::GUEST_PAGE_SIZE,
        )
        .expect("first memory range should be valid");
        let mmio64_range = GuestMemoryRange::new(
            GuestAddress::new(aarch64::MMIO64_MEM_START),
            aarch64::GUEST_PAGE_SIZE,
        )
        .expect("MMIO64-overlapping test range should be valid");
        let layout = GuestMemoryLayout::new(vec![first_range, mmio64_range])
            .expect("test layout should be valid");
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );

        let err = build_test_arm64_fdt(&config).expect_err("MMIO64 gap RAM should fail");

        assert_eq!(
            err,
            Arm64FdtError::GuestMemoryOverlapsMmio64 {
                range: mmio64_range,
            }
        );
    }

    #[test]
    fn rejects_memory_larger_than_arm64_dram_max() {
        let first_size = aarch64::MMIO64_MEM_START - aarch64::DRAM_MEM_START;
        let second_size = aarch64::DRAM_MEM_MAX_SIZE - first_size + 1;
        let first_range =
            GuestMemoryRange::new(GuestAddress::new(aarch64::DRAM_MEM_START), first_size)
                .expect("first max-size test range should be valid");
        let second_range = GuestMemoryRange::new(
            GuestAddress::new(aarch64::FIRST_ADDR_PAST_64BITS_MMIO),
            second_size,
        )
        .expect("second max-size test range should be valid");
        let layout = GuestMemoryLayout::new(vec![first_range, second_range])
            .expect("oversized aarch64 layout should be structurally valid");
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );

        let err = build_test_arm64_fdt(&config).expect_err("oversized guest memory should fail");

        assert_eq!(
            err,
            Arm64FdtError::GuestMemoryTooLarge {
                size: aarch64::DRAM_MEM_MAX_SIZE + 1,
                max_size: aarch64::DRAM_MEM_MAX_SIZE,
            }
        );
    }

    #[test]
    fn rejects_sparse_memory_layout() {
        let first_range = GuestMemoryRange::new(
            GuestAddress::new(aarch64::DRAM_MEM_START),
            aarch64::SYSTEM_MEM_SIZE + aarch64::GUEST_PAGE_SIZE,
        )
        .expect("first sparse test range should be valid");
        let second_range = GuestMemoryRange::new(
            GuestAddress::new(
                aarch64::DRAM_MEM_START + aarch64::SYSTEM_MEM_SIZE + (2 * aarch64::GUEST_PAGE_SIZE),
            ),
            aarch64::GUEST_PAGE_SIZE,
        )
        .expect("second sparse test range should be valid");
        let layout = GuestMemoryLayout::new(vec![first_range, second_range])
            .expect("sparse layout should be structurally valid");
        let expected_layout = aarch64::dram_layout(layout.total_size())
            .expect("expected dense aarch64 layout should be valid");
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );

        let err = build_test_arm64_fdt(&config).expect_err("sparse memory layout should fail");

        assert_eq!(
            err,
            Arm64FdtError::UnexpectedDramLayout {
                range_index: 0,
                expected: Some(expected_layout.ranges()[0]),
                actual: Some(first_range),
            }
        );
    }

    #[test]
    fn memory_node_keeps_later_dram_ranges() {
        let layout = aarch64::dram_layout(aarch64::MMIO64_MEM_START - aarch64::DRAM_MEM_START + 1)
            .expect("split layout should be valid");
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let memory = required_node(&tree, "/memory@ram");

        assert_eq!(
            prop_u64_cells(memory, "reg"),
            vec![
                aarch64::DRAM_MEM_START + aarch64::SYSTEM_MEM_SIZE,
                aarch64::MMIO64_MEM_START - aarch64::DRAM_MEM_START - aarch64::SYSTEM_MEM_SIZE,
                aarch64::FIRST_ADDR_PAST_64BITS_MMIO,
                1,
            ]
        );
    }

    #[test]
    fn gic_node_uses_metadata_without_msi_child() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let intc = required_node(&tree, "/intc");

        assert_eq!(intc.prop_str("compatible").unwrap(), "arm,gic-v3");
        assert!(intc.has_prop("interrupt-controller"));
        assert_eq!(intc.prop_u32("#interrupt-cells").unwrap(), 3);
        assert_eq!(intc.prop_u32("phandle").unwrap(), GIC_PHANDLE);
        assert!(intc.has_prop("ranges"));
        assert_eq!(
            prop_u64_cells(intc, "reg"),
            vec![0x3fff_0000, 0x1_0000, 0x3ffd_0000, 0x2_0000]
        );
        assert_eq!(prop_u32_cells(intc, "interrupts"), vec![1, 9, 4]);
        assert!(!intc.has_prop("msi-controller"));
        assert!(!intc.has_prop("mbi-ranges"));
        assert!(!intc.has_prop("mbi-alias"));
        assert!(!intc.has_prop("#msi-cells"));
        assert!(intc.children.is_empty());
    }

    #[test]
    fn rejects_overlapping_gic_regions() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = Arm64FdtConfig {
            gic: Arm64FdtGic {
                redistributor: Arm64FdtRegion {
                    base: 0x3ffe_0000,
                    size: 0x2_0000,
                },
                ..test_gic()
            },
            ..test_config(
                &layout,
                Arm64FdtBootInfo {
                    command_line: "panic=1",
                    initrd: None,
                },
            )
        };

        let err = build_test_arm64_fdt(&config).expect_err("overlapping GIC regions should fail");

        assert_eq!(
            err,
            Arm64FdtError::GicRegionsOverlap {
                first: "distributor",
                second: "redistributor",
            }
        );
    }

    #[test]
    fn rejects_gic_region_overlapping_guest_memory() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let region = Arm64FdtRegion {
            base: aarch64::DRAM_MEM_START,
            size: 0x1000,
        };
        let config = Arm64FdtConfig {
            gic: Arm64FdtGic {
                distributor: region,
                ..test_gic()
            },
            ..test_config(
                &layout,
                Arm64FdtBootInfo {
                    command_line: "panic=1",
                    initrd: None,
                },
            )
        };

        let err =
            build_test_arm64_fdt(&config).expect_err("GIC region overlapping memory should fail");

        assert_eq!(
            err,
            Arm64FdtError::GicRegionOverlapsMemory {
                name: "distributor",
                region,
                memory_range: layout.ranges()[0],
            }
        );
    }

    #[test]
    fn rejects_unexpected_gic_compatibility() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = Arm64FdtConfig {
            gic: Arm64FdtGic {
                compatibility: "arm,gic-v2",
                ..test_gic()
            },
            ..test_config(
                &layout,
                Arm64FdtBootInfo {
                    command_line: "panic=1",
                    initrd: None,
                },
            )
        };

        let err = build_test_arm64_fdt(&config).expect_err("unexpected GIC compatible should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidGicCompatibility {
                value: "arm,gic-v2",
                expected: GIC_COMPATIBILITY,
            }
        );
    }

    #[test]
    fn rejects_gic_maintenance_irq_reusing_timer_ppi() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = Arm64FdtConfig {
            gic: Arm64FdtGic {
                maintenance_irq: ARM64_FDT_SECURE_PHYSICAL_TIMER_PPI,
                ..test_gic()
            },
            ..test_config(
                &layout,
                Arm64FdtBootInfo {
                    command_line: "panic=1",
                    initrd: None,
                },
            )
        };

        let err = build_test_arm64_fdt(&config).expect_err("reused maintenance PPI should fail");

        assert_eq!(
            err,
            Arm64FdtError::DuplicatePpi {
                first: "maintenance_irq",
                second: "secure_physical_timer",
                value: ARM64_FDT_SECURE_PHYSICAL_TIMER_PPI,
            }
        );
    }

    #[test]
    fn timer_node_uses_firecracker_ppi_cells() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let timer = required_node(&tree, "/timer");

        assert_eq!(timer.prop_str("compatible").unwrap(), "arm,armv8-timer");
        assert!(timer.has_prop("always-on"));
        assert_eq!(
            prop_u32_cells(timer, "interrupts"),
            vec![1, 13, 4, 1, 14, 4, 1, 11, 4, 1, 10, 4]
        );
    }

    #[test]
    fn serial_node_uses_firecracker_shape() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let serial = serial_device(0x4000_2000, 0x1000, 32);
        let config = test_config_with_serial_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            serial,
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let clock = required_node(&tree, "/apb-pclk");
        let serial_node = required_node(&tree, "/uart@40002000");

        assert_eq!(clock.prop_str("compatible").unwrap(), "fixed-clock");
        assert_eq!(clock.prop_u32("#clock-cells").unwrap(), 0);
        assert_eq!(
            clock.prop_u32("clock-frequency").unwrap(),
            APB_PCLK_CLOCK_FREQUENCY
        );
        assert_eq!(
            clock.prop_str("clock-output-names").unwrap(),
            APB_PCLK_CLOCK_OUTPUT_NAME
        );
        assert_eq!(clock.prop_u32("phandle").unwrap(), CLOCK_PHANDLE);

        assert_eq!(serial_node.prop_str("compatible").unwrap(), "ns16550a");
        assert_eq!(
            prop_u64_cells(serial_node, "reg"),
            vec![0x4000_2000, 0x1000]
        );
        assert_eq!(serial_node.prop_u32("clocks").unwrap(), CLOCK_PHANDLE);
        assert_eq!(
            serial_node.prop_str("clock-names").unwrap(),
            APB_PCLK_CLOCK_NAME
        );
        assert_eq!(prop_u32_cells(serial_node, "interrupts"), vec![0, 0, 1]);
        assert!(!serial_node.has_prop("interrupt-parent"));
        assert_eq!(tree.root.prop_u32("interrupt-parent").unwrap(), GIC_PHANDLE);
    }

    #[test]
    fn serial_node_is_ordered_before_sorted_virtio_mmio_nodes() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let serial = serial_device(0x4000_2000, 0x1000, 33);
        let devices = [
            virtio_mmio_device(0x4000_5000, 0x1000, 35),
            virtio_mmio_device(0x4000_3000, 0x1000, 34),
        ];
        let config = test_config_with_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(serial),
            &devices,
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let root_children: Vec<&str> = tree
            .root
            .children
            .iter()
            .map(|child| child.name.as_str())
            .collect();

        assert_eq!(
            root_children,
            [
                "cpus",
                "memory@ram",
                "chosen",
                "intc",
                "timer",
                "apb-pclk",
                "psci",
                "uart@40002000",
                "virtio_mmio@40003000",
                "virtio_mmio@40005000",
            ]
        );
        let serial_node = required_node(&tree, "/uart@40002000");
        let first_virtio = required_node(&tree, "/virtio_mmio@40003000");
        let second_virtio = required_node(&tree, "/virtio_mmio@40005000");
        assert_eq!(prop_u32_cells(serial_node, "interrupts"), vec![0, 1, 1]);
        assert_eq!(prop_u32_cells(first_virtio, "interrupts"), vec![0, 2, 1]);
        assert_eq!(prop_u32_cells(second_virtio, "interrupts"), vec![0, 3, 1]);
    }

    #[test]
    fn vmgenid_node_uses_firecracker_shape() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmgenid = vmgenid_device(0x4000_3000, ARM64_FDT_VMGENID_SIZE, 32);
        let config = test_config_with_vmgenid_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            vmgenid,
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let vmgenid_node = required_node(&tree, "/vmgenid");

        assert!(tree.find("/apb-pclk").is_none());
        assert_eq!(
            vmgenid_node.prop_str("compatible").unwrap(),
            VMGENID_COMPATIBILITY
        );
        assert_eq!(
            prop_u64_cells(vmgenid_node, "reg"),
            vec![0x4000_3000, ARM64_FDT_VMGENID_SIZE]
        );
        assert_eq!(prop_u32_cells(vmgenid_node, "interrupts"), vec![0, 0, 1]);
        assert!(!vmgenid_node.has_prop("interrupt-parent"));
        assert_eq!(tree.root.prop_u32("interrupt-parent").unwrap(), GIC_PHANDLE);
    }

    #[test]
    fn vmgenid_node_is_ordered_before_sorted_virtio_mmio_nodes() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let rtc = rtc_device(0x4000_1000, 0x1000);
        let serial = serial_device(0x4000_2000, 0x1000, 33);
        let vmgenid = vmgenid_device(0x4000_3000, ARM64_FDT_VMGENID_SIZE, 34);
        let devices = [
            virtio_mmio_device(0x4000_5000, 0x1000, 35),
            virtio_mmio_device(0x4000_4000, 0x1000, 36),
        ];
        let config = test_config_with_vmgenid_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmgenid),
            Some(rtc),
            Some(serial),
            &devices,
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let root_children: Vec<&str> = tree
            .root
            .children
            .iter()
            .map(|child| child.name.as_str())
            .collect();

        assert_eq!(
            root_children,
            [
                "cpus",
                "memory@ram",
                "chosen",
                "intc",
                "timer",
                "apb-pclk",
                "psci",
                "rtc@40001000",
                "uart@40002000",
                "vmgenid",
                "virtio_mmio@40004000",
                "virtio_mmio@40005000",
            ]
        );
        let vmgenid_node = required_node(&tree, "/vmgenid");
        let first_virtio = required_node(&tree, "/virtio_mmio@40004000");
        let second_virtio = required_node(&tree, "/virtio_mmio@40005000");
        assert_eq!(prop_u32_cells(vmgenid_node, "interrupts"), vec![0, 2, 1]);
        assert_eq!(prop_u32_cells(first_virtio, "interrupts"), vec![0, 4, 1]);
        assert_eq!(prop_u32_cells(second_virtio, "interrupts"), vec![0, 3, 1]);
    }

    #[test]
    fn rejects_invalid_vmgenid_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmgenid = vmgenid_device(0x4000_3000, 0, 32);
        let config = test_config_with_vmgenid_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            vmgenid,
        );

        let err = build_test_arm64_fdt(&config).expect_err("empty VMGenID region should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidVmGenIdRegion {
                region: vmgenid.region,
                source: GuestMemoryError::EmptyRange {
                    start: GuestAddress::new(0x4000_3000),
                },
            }
        );
    }

    #[test]
    fn rejects_vmgenid_region_with_non_firecracker_size() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmgenid = vmgenid_device(0x4000_3000, ARM64_FDT_VMGENID_SIZE + 1, 32);
        let config = test_config_with_vmgenid_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            vmgenid,
        );

        let err = build_test_arm64_fdt(&config).expect_err("oversized VMGenID region should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidVmGenIdSize {
                size: ARM64_FDT_VMGENID_SIZE + 1,
                expected: ARM64_FDT_VMGENID_SIZE,
            }
        );
    }

    #[test]
    fn rejects_overflowing_vmgenid_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmgenid = vmgenid_device(u64::MAX, 1, 32);
        let config = test_config_with_vmgenid_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            vmgenid,
        );

        let err =
            build_test_arm64_fdt(&config).expect_err("overflowing VMGenID region should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidVmGenIdRegion {
                region: vmgenid.region,
                source: GuestMemoryError::AddressOverflow {
                    start: GuestAddress::new(u64::MAX),
                    size: 1,
                },
            }
        );
    }

    #[test]
    fn rejects_vmgenid_region_overlapping_guest_memory() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmgenid = vmgenid_device(
            aarch64::DRAM_MEM_START + aarch64::SYSTEM_MEM_SIZE,
            ARM64_FDT_VMGENID_SIZE,
            32,
        );
        let config = test_config_with_vmgenid_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            vmgenid,
        );

        let err = build_test_arm64_fdt(&config).expect_err("RAM-overlapping VMGenID should fail");

        assert_eq!(
            err,
            Arm64FdtError::VmGenIdRegionOverlapsMemory {
                region: vmgenid.region,
                memory_range: GuestMemoryRange::new(
                    GuestAddress::new(aarch64::DRAM_MEM_START + aarch64::SYSTEM_MEM_SIZE),
                    TEST_MEMORY_SIZE - aarch64::SYSTEM_MEM_SIZE,
                )
                .expect("advertised memory range should be valid"),
            }
        );
    }

    #[test]
    fn accepts_vmgenid_region_inside_reserved_system_memory() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmgenid = vmgenid_device(
            aarch64::DRAM_MEM_START + aarch64::SYSTEM_MEM_SIZE - ARM64_FDT_VMGENID_SIZE,
            ARM64_FDT_VMGENID_SIZE,
            32,
        );
        let config = test_config_with_vmgenid_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            vmgenid,
        );

        let bytes =
            build_test_arm64_fdt(&config).expect("system-memory VMGenID should be accepted");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let vmgenid_node = required_node(&tree, "/vmgenid");

        assert_eq!(
            prop_u64_cells(vmgenid_node, "reg"),
            vec![vmgenid.region.base, ARM64_FDT_VMGENID_SIZE]
        );
    }

    #[test]
    fn rejects_vmgenid_region_overlapping_gic() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmgenid = vmgenid_device(0x3fff_0000, ARM64_FDT_VMGENID_SIZE, 32);
        let config = test_config_with_vmgenid_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            vmgenid,
        );

        let err = build_test_arm64_fdt(&config).expect_err("GIC-overlapping VMGenID should fail");

        assert_eq!(
            err,
            Arm64FdtError::VmGenIdRegionOverlapsGic {
                region: vmgenid.region,
                gic: "distributor",
            }
        );
    }

    #[test]
    fn rejects_vmgenid_region_overlapping_virtio_mmio_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmgenid = vmgenid_device(0x4000_2000, ARM64_FDT_VMGENID_SIZE, 32);
        let devices = [virtio_mmio_device(0x4000_1000, 0x2000, 33)];
        let config = test_config_with_vmgenid_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmgenid),
            None,
            None,
            &devices,
        );

        let err =
            build_test_arm64_fdt(&config).expect_err("virtio-overlapping VMGenID should fail");

        assert_eq!(
            err,
            Arm64FdtError::VmGenIdRegionOverlapsVirtioMmio {
                region: vmgenid.region,
                virtio_mmio_index: 0,
                virtio_mmio_region: devices[0].region,
            }
        );
    }

    #[test]
    fn rejects_vmgenid_region_overlapping_rtc_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let rtc = rtc_device(0x4000_1000, 0x2000);
        let vmgenid = vmgenid_device(0x4000_2000, ARM64_FDT_VMGENID_SIZE, 32);
        let config = test_config_with_vmgenid_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmgenid),
            Some(rtc),
            None,
            &[],
        );

        let err = build_test_arm64_fdt(&config).expect_err("RTC-overlapping VMGenID should fail");

        assert_eq!(
            err,
            Arm64FdtError::VmGenIdRegionOverlapsRtc {
                region: vmgenid.region,
                rtc_region: rtc.region,
            }
        );
    }

    #[test]
    fn rejects_vmgenid_region_overlapping_serial_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let serial = serial_device(0x4000_1000, 0x2000, 33);
        let vmgenid = vmgenid_device(0x4000_2000, ARM64_FDT_VMGENID_SIZE, 32);
        let config = test_config_with_vmgenid_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmgenid),
            None,
            Some(serial),
            &[],
        );

        let err =
            build_test_arm64_fdt(&config).expect_err("serial-overlapping VMGenID should fail");

        assert_eq!(
            err,
            Arm64FdtError::VmGenIdRegionOverlapsSerial {
                region: vmgenid.region,
                serial_region: serial.region,
            }
        );
    }

    #[test]
    fn accepts_vmgenid_region_adjacent_to_guest_memory() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let memory_adjacent_base = aarch64::DRAM_MEM_START - ARM64_FDT_VMGENID_SIZE;
        let vmgenid = vmgenid_device(memory_adjacent_base, ARM64_FDT_VMGENID_SIZE, 32);
        let config = test_config_with_vmgenid_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            vmgenid,
        );

        let bytes =
            build_test_arm64_fdt(&config).expect("memory-adjacent VMGenID should be accepted");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let vmgenid_node = required_node(&tree, "/vmgenid");

        assert_eq!(
            prop_u64_cells(vmgenid_node, "reg"),
            vec![memory_adjacent_base, ARM64_FDT_VMGENID_SIZE]
        );
    }

    #[test]
    fn accepts_vmgenid_region_adjacent_to_gic_and_devices() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let rtc = rtc_device(0x4000_0010, 0x1000);
        let serial = serial_device(0x4000_1010, 0x1000, 33);
        let vmgenid = vmgenid_device(0x4000_0000, ARM64_FDT_VMGENID_SIZE, 32);
        let devices = [virtio_mmio_device(0x4000_2010, 0x1000, 34)];
        let config = test_config_with_vmgenid_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmgenid),
            Some(rtc),
            Some(serial),
            &devices,
        );

        let bytes =
            build_test_arm64_fdt(&config).expect("adjacent VMGenID region should be accepted");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let vmgenid_node = required_node(&tree, "/vmgenid");
        let rtc_node = required_node(&tree, "/rtc@40000010");
        let serial_node = required_node(&tree, "/uart@40001010");
        let virtio_node = required_node(&tree, "/virtio_mmio@40002010");

        assert_eq!(
            prop_u64_cells(vmgenid_node, "reg"),
            vec![0x4000_0000, ARM64_FDT_VMGENID_SIZE]
        );
        assert_eq!(prop_u64_cells(rtc_node, "reg"), vec![0x4000_0010, 0x1000]);
        assert_eq!(
            prop_u64_cells(serial_node, "reg"),
            vec![0x4000_1010, 0x1000]
        );
        assert_eq!(
            prop_u64_cells(virtio_node, "reg"),
            vec![0x4000_2010, 0x1000]
        );
    }

    #[test]
    fn rejects_non_spi_vmgenid_interrupt_line() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmgenid = vmgenid_device(0x4000_3000, ARM64_FDT_VMGENID_SIZE, 31);
        let config = test_config_with_vmgenid_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            vmgenid,
        );

        let err = build_test_arm64_fdt(&config).expect_err("non-SPI VMGenID interrupt should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidVmGenIdInterrupt {
                line: vmgenid.interrupt_line,
            }
        );
    }

    #[test]
    fn vmclock_node_uses_firecracker_shape() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmclock = vmclock_device(0x4000_3000, ARM64_FDT_VMCLOCK_SIZE, 32);
        let config = test_config_with_vmclock_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmclock),
            None,
            None,
            None,
            &[],
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let vmclock_path = format!("/ptp@{}", vmclock.region.base);
        let vmclock_node = required_node(&tree, &vmclock_path);

        assert!(tree.find("/apb-pclk").is_none());
        assert_eq!(
            vmclock_node.prop_str("compatible").unwrap(),
            VMCLOCK_COMPATIBILITY
        );
        assert_eq!(
            prop_u64_cells(vmclock_node, "reg"),
            vec![0x4000_3000, ARM64_FDT_VMCLOCK_SIZE]
        );
        assert_eq!(prop_u32_cells(vmclock_node, "interrupts"), vec![0, 0, 1]);
        assert!(!vmclock_node.has_prop("interrupt-parent"));
        assert_eq!(tree.root.prop_u32("interrupt-parent").unwrap(), GIC_PHANDLE);
    }

    #[test]
    fn vmclock_node_is_ordered_after_vmgenid_before_sorted_virtio_mmio_nodes() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let rtc = rtc_device(0x4000_1000, 0x1000);
        let serial = serial_device(0x4000_2000, 0x1000, 33);
        let vmgenid = vmgenid_device(0x4000_3000, ARM64_FDT_VMGENID_SIZE, 34);
        let vmclock = vmclock_device(0x4000_4000, ARM64_FDT_VMCLOCK_SIZE, 35);
        let devices = [
            virtio_mmio_device(0x4000_6000, 0x1000, 36),
            virtio_mmio_device(0x4000_5000, 0x1000, 37),
        ];
        let config = test_config_with_vmclock_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmclock),
            Some(vmgenid),
            Some(rtc),
            Some(serial),
            &devices,
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let root_children: Vec<&str> = tree
            .root
            .children
            .iter()
            .map(|child| child.name.as_str())
            .collect();

        assert_eq!(
            root_children,
            [
                "cpus",
                "memory@ram",
                "chosen",
                "intc",
                "timer",
                "apb-pclk",
                "psci",
                "rtc@40001000",
                "uart@40002000",
                "vmgenid",
                "ptp@1073758208",
                "virtio_mmio@40005000",
                "virtio_mmio@40006000",
            ]
        );
        let vmclock_node = required_node(&tree, "/ptp@1073758208");
        let first_virtio = required_node(&tree, "/virtio_mmio@40005000");
        let second_virtio = required_node(&tree, "/virtio_mmio@40006000");
        assert_eq!(prop_u32_cells(vmclock_node, "interrupts"), vec![0, 3, 1]);
        assert_eq!(prop_u32_cells(first_virtio, "interrupts"), vec![0, 5, 1]);
        assert_eq!(prop_u32_cells(second_virtio, "interrupts"), vec![0, 4, 1]);
    }

    #[test]
    fn rejects_invalid_vmclock_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmclock = vmclock_device(0x4000_3000, 0, 32);
        let config = test_config_with_vmclock_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmclock),
            None,
            None,
            None,
            &[],
        );

        let err = build_test_arm64_fdt(&config).expect_err("empty VMClock region should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidVmClockRegion {
                region: vmclock.region,
                source: GuestMemoryError::EmptyRange {
                    start: GuestAddress::new(0x4000_3000),
                },
            }
        );
    }

    #[test]
    fn rejects_vmclock_region_with_non_firecracker_size() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmclock = vmclock_device(0x4000_3000, ARM64_FDT_VMCLOCK_SIZE + 1, 32);
        let config = test_config_with_vmclock_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmclock),
            None,
            None,
            None,
            &[],
        );

        let err = build_test_arm64_fdt(&config).expect_err("oversized VMClock region should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidVmClockSize {
                size: ARM64_FDT_VMCLOCK_SIZE + 1,
                expected: ARM64_FDT_VMCLOCK_SIZE,
            }
        );
    }

    #[test]
    fn rejects_overflowing_vmclock_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmclock = vmclock_device(u64::MAX, 1, 32);
        let config = test_config_with_vmclock_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmclock),
            None,
            None,
            None,
            &[],
        );

        let err =
            build_test_arm64_fdt(&config).expect_err("overflowing VMClock region should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidVmClockRegion {
                region: vmclock.region,
                source: GuestMemoryError::AddressOverflow {
                    start: GuestAddress::new(u64::MAX),
                    size: 1,
                },
            }
        );
    }

    #[test]
    fn rejects_vmclock_region_overlapping_guest_memory() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmclock = vmclock_device(
            aarch64::DRAM_MEM_START + aarch64::SYSTEM_MEM_SIZE,
            ARM64_FDT_VMCLOCK_SIZE,
            32,
        );
        let config = test_config_with_vmclock_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmclock),
            None,
            None,
            None,
            &[],
        );

        let err = build_test_arm64_fdt(&config).expect_err("RAM-overlapping VMClock should fail");

        assert_eq!(
            err,
            Arm64FdtError::VmClockRegionOverlapsMemory {
                region: vmclock.region,
                memory_range: GuestMemoryRange::new(
                    GuestAddress::new(aarch64::DRAM_MEM_START + aarch64::SYSTEM_MEM_SIZE),
                    TEST_MEMORY_SIZE - aarch64::SYSTEM_MEM_SIZE,
                )
                .expect("advertised memory range should be valid"),
            }
        );
    }

    #[test]
    fn accepts_vmclock_region_inside_reserved_system_memory() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmclock = vmclock_device(
            aarch64::DRAM_MEM_START + aarch64::SYSTEM_MEM_SIZE - ARM64_FDT_VMCLOCK_SIZE,
            ARM64_FDT_VMCLOCK_SIZE,
            32,
        );
        let config = test_config_with_vmclock_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmclock),
            None,
            None,
            None,
            &[],
        );

        let bytes =
            build_test_arm64_fdt(&config).expect("system-memory VMClock should be accepted");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let vmclock_path = format!("/ptp@{}", vmclock.region.base);
        let vmclock_node = required_node(&tree, &vmclock_path);

        assert_eq!(
            prop_u64_cells(vmclock_node, "reg"),
            vec![vmclock.region.base, ARM64_FDT_VMCLOCK_SIZE]
        );
    }

    #[test]
    fn rejects_vmclock_region_overlapping_gic() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmclock = vmclock_device(0x3fff_0000, ARM64_FDT_VMCLOCK_SIZE, 32);
        let config = test_config_with_vmclock_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmclock),
            None,
            None,
            None,
            &[],
        );

        let err = build_test_arm64_fdt(&config).expect_err("GIC-overlapping VMClock should fail");

        assert_eq!(
            err,
            Arm64FdtError::VmClockRegionOverlapsGic {
                region: vmclock.region,
                gic: "distributor",
            }
        );
    }

    #[test]
    fn rejects_vmclock_region_overlapping_virtio_mmio_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmclock = vmclock_device(0x4000_2000, ARM64_FDT_VMCLOCK_SIZE, 32);
        let devices = [virtio_mmio_device(0x4000_1000, 0x2000, 33)];
        let config = test_config_with_vmclock_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmclock),
            None,
            None,
            None,
            &devices,
        );

        let err =
            build_test_arm64_fdt(&config).expect_err("virtio-overlapping VMClock should fail");

        assert_eq!(
            err,
            Arm64FdtError::VmClockRegionOverlapsVirtioMmio {
                region: vmclock.region,
                virtio_mmio_index: 0,
                virtio_mmio_region: devices[0].region,
            }
        );
    }

    #[test]
    fn rejects_vmclock_region_overlapping_rtc_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let rtc = rtc_device(0x4000_1000, 0x2000);
        let vmclock = vmclock_device(0x4000_2000, ARM64_FDT_VMCLOCK_SIZE, 32);
        let config = test_config_with_vmclock_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmclock),
            None,
            Some(rtc),
            None,
            &[],
        );

        let err = build_test_arm64_fdt(&config).expect_err("RTC-overlapping VMClock should fail");

        assert_eq!(
            err,
            Arm64FdtError::VmClockRegionOverlapsRtc {
                region: vmclock.region,
                rtc_region: rtc.region,
            }
        );
    }

    #[test]
    fn rejects_vmclock_region_overlapping_serial_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let serial = serial_device(0x4000_1000, 0x2000, 33);
        let vmclock = vmclock_device(0x4000_2000, ARM64_FDT_VMCLOCK_SIZE, 32);
        let config = test_config_with_vmclock_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmclock),
            None,
            None,
            Some(serial),
            &[],
        );

        let err =
            build_test_arm64_fdt(&config).expect_err("serial-overlapping VMClock should fail");

        assert_eq!(
            err,
            Arm64FdtError::VmClockRegionOverlapsSerial {
                region: vmclock.region,
                serial_region: serial.region,
            }
        );
    }

    #[test]
    fn rejects_vmclock_region_overlapping_vmgenid_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmgenid = vmgenid_device(0x4000_0ff0, ARM64_FDT_VMGENID_SIZE, 32);
        let vmclock = vmclock_device(0x4000_0000, ARM64_FDT_VMCLOCK_SIZE, 33);
        let config = test_config_with_vmclock_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmclock),
            Some(vmgenid),
            None,
            None,
            &[],
        );

        let err =
            build_test_arm64_fdt(&config).expect_err("VMGenID-overlapping VMClock should fail");

        assert_eq!(
            err,
            Arm64FdtError::VmClockRegionOverlapsVmGenId {
                region: vmclock.region,
                vmgenid_region: vmgenid.region,
            }
        );
    }

    #[test]
    fn accepts_vmclock_region_adjacent_to_vmgenid_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmgenid = vmgenid_device(0x4000_0000, ARM64_FDT_VMGENID_SIZE, 32);
        let vmclock = vmclock_device(
            0x4000_0000 + ARM64_FDT_VMGENID_SIZE,
            ARM64_FDT_VMCLOCK_SIZE,
            33,
        );
        let config = test_config_with_vmclock_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmclock),
            Some(vmgenid),
            None,
            None,
            &[],
        );

        let bytes = build_test_arm64_fdt(&config).expect("adjacent regions should be accepted");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let vmclock_path = format!("/ptp@{}", vmclock.region.base);
        let vmclock_node = required_node(&tree, &vmclock_path);

        assert_eq!(
            prop_u64_cells(vmclock_node, "reg"),
            vec![vmclock.region.base, ARM64_FDT_VMCLOCK_SIZE]
        );
    }

    #[test]
    fn rejects_non_spi_vmclock_interrupt_line() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let vmclock = vmclock_device(0x4000_3000, ARM64_FDT_VMCLOCK_SIZE, 31);
        let config = test_config_with_vmclock_and_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(vmclock),
            None,
            None,
            None,
            &[],
        );

        let err = build_test_arm64_fdt(&config).expect_err("non-SPI VMClock interrupt should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidVmClockInterrupt {
                line: vmclock.interrupt_line,
            }
        );
    }

    #[test]
    fn virtio_mmio_node_uses_firecracker_shape() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let devices = [virtio_mmio_device(0x4000_1000, 0x1000, 32)];
        let config = test_config_with_virtio_mmio_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            &devices,
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let virtio = required_node(&tree, "/virtio_mmio@40001000");

        assert!(virtio.has_prop("dma-coherent"));
        assert_eq!(virtio.prop_str("compatible").unwrap(), "virtio,mmio");
        assert_eq!(prop_u64_cells(virtio, "reg"), vec![0x4000_1000, 0x1000]);
        assert_eq!(prop_u32_cells(virtio, "interrupts"), vec![0, 0, 1]);
        assert_eq!(virtio.prop_u32("interrupt-parent").unwrap(), GIC_PHANDLE);
    }

    #[test]
    fn virtio_mmio_nodes_are_sorted_by_address() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let devices = [
            virtio_mmio_device(0x4000_3000, 0x1000, 34),
            virtio_mmio_device(0x4000_1000, 0x1000, 32),
        ];
        let config = test_config_with_virtio_mmio_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            &devices,
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let root_children: Vec<&str> = tree
            .root
            .children
            .iter()
            .map(|child| child.name.as_str())
            .collect();

        assert_eq!(
            root_children,
            [
                "cpus",
                "memory@ram",
                "chosen",
                "intc",
                "timer",
                "psci",
                "virtio_mmio@40001000",
                "virtio_mmio@40003000",
            ]
        );
        let first = required_node(&tree, "/virtio_mmio@40001000");
        let second = required_node(&tree, "/virtio_mmio@40003000");
        assert_eq!(prop_u32_cells(first, "interrupts"), vec![0, 0, 1]);
        assert_eq!(prop_u32_cells(second, "interrupts"), vec![0, 2, 1]);
    }

    #[test]
    fn rejects_invalid_virtio_mmio_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let devices = [virtio_mmio_device(0x4000_1000, 0, 32)];
        let config = test_config_with_virtio_mmio_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            &devices,
        );

        let err = build_test_arm64_fdt(&config).expect_err("empty device region should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidVirtioMmioRegion {
                index: 0,
                region: devices[0].region,
                source: GuestMemoryError::EmptyRange {
                    start: GuestAddress::new(0x4000_1000),
                },
            }
        );
    }

    #[test]
    fn rejects_overflowing_virtio_mmio_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let devices = [virtio_mmio_device(u64::MAX, 1, 32)];
        let config = test_config_with_virtio_mmio_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            &devices,
        );

        let err = build_test_arm64_fdt(&config).expect_err("overflowing device region should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidVirtioMmioRegion {
                index: 0,
                region: devices[0].region,
                source: GuestMemoryError::AddressOverflow {
                    start: GuestAddress::new(u64::MAX),
                    size: 1,
                },
            }
        );
    }

    #[test]
    fn rejects_virtio_mmio_region_overlapping_guest_memory() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let devices = [virtio_mmio_device(aarch64::DRAM_MEM_START, 0x1000, 32)];
        let config = test_config_with_virtio_mmio_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            &devices,
        );

        let err = build_test_arm64_fdt(&config).expect_err("RAM-overlapping device should fail");

        assert_eq!(
            err,
            Arm64FdtError::VirtioMmioRegionOverlapsMemory {
                index: 0,
                region: devices[0].region,
                memory_range: layout.ranges()[0],
            }
        );
    }

    #[test]
    fn rejects_virtio_mmio_region_overlapping_gic() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let devices = [virtio_mmio_device(0x3fff_0000, 0x1000, 32)];
        let config = test_config_with_virtio_mmio_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            &devices,
        );

        let err = build_test_arm64_fdt(&config).expect_err("GIC-overlapping device should fail");

        assert_eq!(
            err,
            Arm64FdtError::VirtioMmioRegionOverlapsGic {
                index: 0,
                region: devices[0].region,
                gic: "distributor",
            }
        );
    }

    #[test]
    fn rejects_overlapping_virtio_mmio_regions() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let devices = [
            virtio_mmio_device(0x4000_0000, 0x2000, 32),
            virtio_mmio_device(0x4000_1000, 0x1000, 33),
        ];
        let config = test_config_with_virtio_mmio_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            &devices,
        );

        let err = build_test_arm64_fdt(&config).expect_err("overlapping devices should fail");

        assert_eq!(
            err,
            Arm64FdtError::VirtioMmioRegionsOverlap {
                first_index: 0,
                second_index: 1,
                first_region: devices[0].region,
                second_region: devices[1].region,
            }
        );
    }

    #[test]
    fn accepts_adjacent_virtio_mmio_regions() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let memory_adjacent_base = aarch64::DRAM_MEM_START - 0x1000;
        let devices = [
            virtio_mmio_device(0x4000_0000, 0x1000, 32),
            virtio_mmio_device(0x4000_1000, 0x1000, 33),
            virtio_mmio_device(memory_adjacent_base, 0x1000, 34),
        ];
        let config = test_config_with_virtio_mmio_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            &devices,
        );

        let bytes =
            build_test_arm64_fdt(&config).expect("adjacent device regions should be accepted");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let gic_adjacent = required_node(&tree, "/virtio_mmio@40000000");
        let device_adjacent = required_node(&tree, "/virtio_mmio@40001000");
        let memory_adjacent =
            required_node(&tree, &format!("/virtio_mmio@{memory_adjacent_base:x}"));

        assert_eq!(
            prop_u64_cells(gic_adjacent, "reg"),
            vec![0x4000_0000, 0x1000]
        );
        assert_eq!(
            prop_u64_cells(device_adjacent, "reg"),
            vec![0x4000_1000, 0x1000]
        );
        assert_eq!(
            prop_u64_cells(memory_adjacent, "reg"),
            vec![memory_adjacent_base, 0x1000]
        );
    }

    #[test]
    fn rejects_duplicate_virtio_mmio_regions() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let devices = [
            virtio_mmio_device(0x4000_0000, 0x1000, 32),
            virtio_mmio_device(0x4000_0000, 0x1000, 33),
        ];
        let config = test_config_with_virtio_mmio_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            &devices,
        );

        let err = build_test_arm64_fdt(&config).expect_err("duplicate devices should fail");

        assert_eq!(
            err,
            Arm64FdtError::VirtioMmioRegionsOverlap {
                first_index: 0,
                second_index: 1,
                first_region: devices[0].region,
                second_region: devices[1].region,
            }
        );
    }

    #[test]
    fn rejects_non_spi_virtio_mmio_interrupt_line() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let devices = [virtio_mmio_device(0x4000_1000, 0x1000, 31)];
        let config = test_config_with_virtio_mmio_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            &devices,
        );

        let err = build_test_arm64_fdt(&config).expect_err("non-SPI interrupt should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidVirtioMmioInterrupt {
                index: 0,
                line: devices[0].interrupt_line,
            }
        );
    }

    #[test]
    fn rejects_invalid_serial_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let serial = serial_device(0x4000_2000, 0, 32);
        let config = test_config_with_serial_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            serial,
        );

        let err = build_test_arm64_fdt(&config).expect_err("empty serial region should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidSerialRegion {
                region: serial.region,
                source: GuestMemoryError::EmptyRange {
                    start: GuestAddress::new(0x4000_2000),
                },
            }
        );
    }

    #[test]
    fn rejects_overflowing_serial_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let serial = serial_device(u64::MAX, 1, 32);
        let config = test_config_with_serial_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            serial,
        );

        let err = build_test_arm64_fdt(&config).expect_err("overflowing serial region should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidSerialRegion {
                region: serial.region,
                source: GuestMemoryError::AddressOverflow {
                    start: GuestAddress::new(u64::MAX),
                    size: 1,
                },
            }
        );
    }

    #[test]
    fn rejects_serial_region_overlapping_guest_memory() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let serial = serial_device(aarch64::DRAM_MEM_START, 0x1000, 32);
        let config = test_config_with_serial_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            serial,
        );

        let err = build_test_arm64_fdt(&config).expect_err("RAM-overlapping serial should fail");

        assert_eq!(
            err,
            Arm64FdtError::SerialRegionOverlapsMemory {
                region: serial.region,
                memory_range: layout.ranges()[0],
            }
        );
    }

    #[test]
    fn rejects_serial_region_overlapping_gic() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let serial = serial_device(0x3fff_0000, 0x1000, 32);
        let config = test_config_with_serial_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            serial,
        );

        let err = build_test_arm64_fdt(&config).expect_err("GIC-overlapping serial should fail");

        assert_eq!(
            err,
            Arm64FdtError::SerialRegionOverlapsGic {
                region: serial.region,
                gic: "distributor",
            }
        );
    }

    #[test]
    fn rejects_serial_region_overlapping_virtio_mmio_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let serial = serial_device(0x4000_2000, 0x1000, 32);
        let devices = [virtio_mmio_device(0x4000_1000, 0x2000, 33)];
        let config = test_config_with_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(serial),
            &devices,
        );

        let err = build_test_arm64_fdt(&config).expect_err("virtio-overlapping serial should fail");

        assert_eq!(
            err,
            Arm64FdtError::SerialRegionOverlapsVirtioMmio {
                region: serial.region,
                virtio_mmio_index: 0,
                virtio_mmio_region: devices[0].region,
            }
        );
    }

    #[test]
    fn accepts_serial_region_adjacent_to_gic_and_virtio_mmio_regions() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let serial = serial_device(0x4000_0000, 0x1000, 32);
        let devices = [virtio_mmio_device(0x4000_1000, 0x1000, 33)];
        let config = test_config_with_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(serial),
            &devices,
        );

        let bytes =
            build_test_arm64_fdt(&config).expect("adjacent serial region should be accepted");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let serial_node = required_node(&tree, "/uart@40000000");
        let virtio_node = required_node(&tree, "/virtio_mmio@40001000");

        assert_eq!(
            prop_u64_cells(serial_node, "reg"),
            vec![0x4000_0000, 0x1000]
        );
        assert_eq!(
            prop_u64_cells(virtio_node, "reg"),
            vec![0x4000_1000, 0x1000]
        );
    }

    #[test]
    fn accepts_serial_region_adjacent_to_guest_memory() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let memory_adjacent_base = aarch64::DRAM_MEM_START - 0x1000;
        let serial = serial_device(memory_adjacent_base, 0x1000, 32);
        let config = test_config_with_serial_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            serial,
        );

        let bytes =
            build_test_arm64_fdt(&config).expect("memory-adjacent serial should be accepted");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let serial_node = required_node(&tree, &format!("/uart@{memory_adjacent_base:x}"));

        assert_eq!(
            prop_u64_cells(serial_node, "reg"),
            vec![memory_adjacent_base, 0x1000]
        );
    }

    #[test]
    fn rejects_invalid_rtc_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let rtc = rtc_device(0x4000_1000, 0);
        let config = test_config_with_rtc_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            rtc,
        );

        let err = build_test_arm64_fdt(&config).expect_err("empty RTC region should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidRtcRegion {
                region: rtc.region,
                source: GuestMemoryError::EmptyRange {
                    start: GuestAddress::new(0x4000_1000),
                },
            }
        );
    }

    #[test]
    fn rejects_overflowing_rtc_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let rtc = rtc_device(u64::MAX, 1);
        let config = test_config_with_rtc_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            rtc,
        );

        let err = build_test_arm64_fdt(&config).expect_err("overflowing RTC region should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidRtcRegion {
                region: rtc.region,
                source: GuestMemoryError::AddressOverflow {
                    start: GuestAddress::new(u64::MAX),
                    size: 1,
                },
            }
        );
    }

    #[test]
    fn rejects_rtc_region_overlapping_guest_memory() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let rtc = rtc_device(aarch64::DRAM_MEM_START, 0x1000);
        let config = test_config_with_rtc_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            rtc,
        );

        let err = build_test_arm64_fdt(&config).expect_err("RAM-overlapping RTC should fail");

        assert_eq!(
            err,
            Arm64FdtError::RtcRegionOverlapsMemory {
                region: rtc.region,
                memory_range: layout.ranges()[0],
            }
        );
    }

    #[test]
    fn rejects_rtc_region_overlapping_gic() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let rtc = rtc_device(0x3fff_0000, 0x1000);
        let config = test_config_with_rtc_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            rtc,
        );

        let err = build_test_arm64_fdt(&config).expect_err("GIC-overlapping RTC should fail");

        assert_eq!(
            err,
            Arm64FdtError::RtcRegionOverlapsGic {
                region: rtc.region,
                gic: "distributor",
            }
        );
    }

    #[test]
    fn rejects_rtc_region_overlapping_virtio_mmio_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let rtc = rtc_device(0x4000_2000, 0x1000);
        let devices = [virtio_mmio_device(0x4000_1000, 0x2000, 33)];
        let config = test_config_with_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(rtc),
            None,
            &devices,
        );

        let err = build_test_arm64_fdt(&config).expect_err("virtio-overlapping RTC should fail");

        assert_eq!(
            err,
            Arm64FdtError::RtcRegionOverlapsVirtioMmio {
                region: rtc.region,
                virtio_mmio_index: 0,
                virtio_mmio_region: devices[0].region,
            }
        );
    }

    #[test]
    fn rejects_serial_region_overlapping_rtc_region() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let rtc = rtc_device(0x4000_1000, 0x2000);
        let serial = serial_device(0x4000_2000, 0x1000, 32);
        let config = test_config_with_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(rtc),
            Some(serial),
            &[],
        );

        let err = build_test_arm64_fdt(&config).expect_err("RTC-overlapping serial should fail");

        assert_eq!(
            err,
            Arm64FdtError::SerialRegionOverlapsRtc {
                region: serial.region,
                rtc_region: rtc.region,
            }
        );
    }

    #[test]
    fn accepts_rtc_region_adjacent_to_serial_and_virtio_mmio_regions() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let rtc = rtc_device(0x4000_1000, 0x1000);
        let serial = serial_device(0x4000_2000, 0x1000, 32);
        let devices = [virtio_mmio_device(0x4000_3000, 0x1000, 33)];
        let config = test_config_with_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(rtc),
            Some(serial),
            &devices,
        );

        let bytes = build_test_arm64_fdt(&config).expect("adjacent RTC region should be accepted");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let rtc_node = required_node(&tree, "/rtc@40001000");
        let serial_node = required_node(&tree, "/uart@40002000");
        let virtio_node = required_node(&tree, "/virtio_mmio@40003000");

        assert_eq!(prop_u64_cells(rtc_node, "reg"), vec![0x4000_1000, 0x1000]);
        assert_eq!(
            prop_u64_cells(serial_node, "reg"),
            vec![0x4000_2000, 0x1000]
        );
        assert_eq!(
            prop_u64_cells(virtio_node, "reg"),
            vec![0x4000_3000, 0x1000]
        );
    }

    #[test]
    fn rejects_non_spi_serial_interrupt_line() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let serial = serial_device(0x4000_2000, 0x1000, 31);
        let config = test_config_with_serial_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            serial,
        );

        let err = build_test_arm64_fdt(&config).expect_err("non-SPI serial interrupt should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidSerialInterrupt {
                line: serial.interrupt_line,
            }
        );
    }

    #[test]
    fn writes_fdt_to_reserved_guest_memory_address() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let mut memory = GuestMemory::allocate(&layout).expect("guest memory should allocate");
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );

        let write = write_test_arm64_fdt(&config, &mut memory).expect("FDT should write");

        assert_eq!(
            write.address,
            aarch64::fdt_address(&layout).expect("FDT address should resolve")
        );
        assert!(size_as_u64(write.size) <= aarch64::FDT_MAX_SIZE);

        let mut read_back = vec![0; write.size];
        memory
            .read_slice(&mut read_back, write.address)
            .expect("written FDT should be readable");
        DeviceTree::load(&read_back).expect("written FDT should parse");
    }

    #[test]
    fn rejects_oversized_fdt_before_guest_memory_write() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let mut memory = GuestMemory::allocate(&layout).expect("guest memory should allocate");
        let address = aarch64::fdt_address(&layout).expect("FDT address should resolve");
        let mut before = vec![0; 16];
        memory
            .read_slice(&mut before, address)
            .expect("initial FDT bytes should read");
        let oversized = vec![0; aarch64::FDT_MAX_SIZE as usize + 1];

        let err = write_arm64_fdt_bytes(&layout, &mut memory, &oversized)
            .expect_err("oversized FDT should fail");

        assert_eq!(
            err,
            Arm64FdtError::FdtTooLarge {
                size: oversized.len(),
                max_size: aarch64::FDT_MAX_SIZE,
            }
        );
        let mut after = vec![0; before.len()];
        memory
            .read_slice(&mut after, address)
            .expect("FDT bytes should remain readable");
        assert_eq!(after, before);
    }

    #[test]
    fn rejects_rng_seed_failure_before_guest_memory_write() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let mut memory = GuestMemory::allocate(&layout).expect("guest memory should allocate");
        let address = aarch64::fdt_address(&layout).expect("FDT address should resolve");
        let before = [0x5a; 16];
        memory
            .write_slice(&before, address)
            .expect("initial sentinel should write");
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );
        let mut source = FailingRngSeedSource;

        let err = write_arm64_fdt_with_rng_seed_source(&config, &mut memory, &mut source)
            .expect_err("rng-seed failure should fail before writing FDT bytes");

        assert_eq!(
            err,
            Arm64FdtError::RngSeed {
                source: Arm64FdtRngSeedError::new(),
            }
        );
        let mut after = [0; 16];
        memory
            .read_slice(&mut after, address)
            .expect("FDT bytes should remain readable");
        assert_eq!(after, before);
    }

    #[test]
    fn rejects_oversized_generated_fdt() {
        let layout = oversized_fdt_layout();
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );

        let err = build_test_arm64_fdt(&config).expect_err("oversized generated FDT should fail");

        assert_eq!(
            err,
            Arm64FdtError::FdtTooLarge {
                size: layout.ranges().len() * MEMORY_REG_CELLS_PER_RANGE * MEMORY_REG_CELL_SIZE,
                max_size: aarch64::FDT_MAX_SIZE,
            }
        );
    }

    #[test]
    fn gic_node_publishes_hardware_described_gicv2m_child() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = Arm64FdtConfig {
            gic: Arm64FdtGic {
                msi: Some(test_msi()),
                ..test_gic()
            },
            ..test_config(
                &layout,
                Arm64FdtBootInfo {
                    command_line: "panic=1",
                    initrd: None,
                },
            )
        };

        let bytes = build_test_arm64_fdt(&config).expect("valid GICv2m metadata should build");
        let tree = DeviceTree::load(&bytes).expect("GICv2m FDT should parse");
        let intc = required_node(&tree, "/intc");
        let frame = required_node(&tree, "/intc/v2m@3ffc0000");

        assert!(!intc.has_prop("msi-controller"));
        assert!(!intc.has_prop("mbi-ranges"));
        assert!(!intc.has_prop("mbi-alias"));
        assert!(!intc.has_prop("#msi-cells"));
        assert_eq!(intc.children.len(), 1);
        assert_eq!(frame.prop_str("compatible").unwrap(), "arm,gic-v2m-frame");
        assert!(
            frame
                .prop_raw("msi-controller")
                .expect("MSI controller property should exist")
                .is_empty()
        );
        assert_eq!(frame.prop_u32("phandle").unwrap(), MSI_PHANDLE);
        assert_eq!(prop_u64_cells(frame, "reg"), vec![0x3ffc_0000, 0x1_0000]);
        assert!(!frame.has_prop("arm,msi-base-spi"));
        assert!(!frame.has_prop("arm,msi-num-spis"));
        assert!(!frame.has_prop("#msi-cells"));
        assert!(frame.children.is_empty());
        assert!(tree.find("/intc/msic").is_none());
        assert_eq!(
            format!("{:?}", test_msi()),
            "Arm64FdtMsi { region: \"<redacted>\", interrupt_range: \"<redacted>\" }"
        );
    }

    #[test]
    fn rejects_invalid_gicv2m_frame_and_interrupt_range_without_raw_values() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let invalid_frames = [
            Arm64FdtRegion {
                base: 0x3ffc_0000,
                size: 0,
            },
            Arm64FdtRegion {
                base: 0x3ffc_0000,
                size: ARM64_GICV2M_MSI_IIDR_OFFSET + GICV2M_REGISTER_SIZE - 1,
            },
            Arm64FdtRegion {
                base: u64::MAX,
                size: 0x1_0000,
            },
        ];
        for region in invalid_frames {
            let config = Arm64FdtConfig {
                gic: Arm64FdtGic {
                    msi: Some(Arm64FdtMsi {
                        region,
                        ..test_msi()
                    }),
                    ..test_gic()
                },
                ..test_config(
                    &layout,
                    Arm64FdtBootInfo {
                        command_line: "panic=1",
                        initrd: None,
                    },
                )
            };

            let err = build_test_arm64_fdt(&config).expect_err("invalid GICv2m frame should fail");
            assert_eq!(err, Arm64FdtError::InvalidGicMsiRegion);
            assert_eq!(err.to_string(), "arm64 FDT GICv2m frame is invalid");
        }

        let invalid_ranges = [
            Arm64FdtInterruptRange { base: 31, count: 1 },
            Arm64FdtInterruptRange {
                base: FIRST_SPI_INTID,
                count: 0,
            },
            Arm64FdtInterruptRange {
                base: u32::MAX,
                count: 2,
            },
            Arm64FdtInterruptRange {
                base: ARM64_GICV2M_SPI_END_EXCLUSIVE - 1,
                count: 2,
            },
        ];
        for interrupt_range in invalid_ranges {
            let config = Arm64FdtConfig {
                gic: Arm64FdtGic {
                    msi: Some(Arm64FdtMsi {
                        interrupt_range,
                        ..test_msi()
                    }),
                    ..test_gic()
                },
                ..test_config(
                    &layout,
                    Arm64FdtBootInfo {
                        command_line: "panic=1",
                        initrd: None,
                    },
                )
            };

            let err = build_test_arm64_fdt(&config)
                .expect_err("invalid GICv2m interrupt range should fail");
            assert_eq!(err, Arm64FdtError::InvalidGicMsiInterruptRange);
            assert_eq!(
                err.to_string(),
                "arm64 FDT GICv2m interrupt range is invalid"
            );
        }
    }

    #[test]
    fn rejects_gicv2m_frame_overlapping_gic_memory_or_device_regions() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        for (region, expected) in [
            (
                test_gic().distributor,
                Arm64FdtError::GicMsiRegionOverlaps {
                    other: "distributor",
                },
            ),
            (
                test_gic().redistributor,
                Arm64FdtError::GicMsiRegionOverlaps {
                    other: "redistributor",
                },
            ),
            (
                Arm64FdtRegion {
                    base: aarch64::DRAM_MEM_START,
                    size: 0x1_0000,
                },
                Arm64FdtError::GicMsiRegionOverlapsMemory,
            ),
        ] {
            let config = Arm64FdtConfig {
                gic: Arm64FdtGic {
                    msi: Some(Arm64FdtMsi {
                        region,
                        ..test_msi()
                    }),
                    ..test_gic()
                },
                ..test_config(
                    &layout,
                    Arm64FdtBootInfo {
                        command_line: "panic=1",
                        initrd: None,
                    },
                )
            };
            assert_eq!(
                build_test_arm64_fdt(&config).expect_err("overlapping GICv2m frame should fail"),
                expected
            );
        }

        let msi = Arm64FdtMsi {
            region: Arm64FdtRegion {
                base: 0x4000_1000,
                size: 0x1000,
            },
            ..test_msi()
        };
        let device = virtio_mmio_device(msi.region.base, msi.region.size, FIRST_SPI_INTID);
        let config = Arm64FdtConfig {
            gic: Arm64FdtGic {
                msi: Some(msi),
                ..test_gic()
            },
            virtio_mmio_devices: &[device],
            ..test_config(
                &layout,
                Arm64FdtBootInfo {
                    command_line: "panic=1",
                    initrd: None,
                },
            )
        };
        assert_eq!(
            build_test_arm64_fdt(&config).expect_err("device-overlapping GICv2m frame should fail"),
            Arm64FdtError::VirtioMmioRegionOverlapsGic {
                index: 0,
                region: device.region,
                gic: "msi",
            }
        );
    }

    #[test]
    fn rejects_invalid_gicv2m_metadata_before_guest_memory_write() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let mut memory = GuestMemory::allocate(&layout).expect("test guest memory should allocate");
        let address = aarch64::fdt_address(&layout).expect("test FDT address should resolve");
        let before = [0xa5; 16];
        memory
            .write_slice(&before, address)
            .expect("sentinel bytes should be writable");
        let config = Arm64FdtConfig {
            gic: Arm64FdtGic {
                msi: Some(Arm64FdtMsi {
                    interrupt_range: Arm64FdtInterruptRange { base: 31, count: 1 },
                    ..test_msi()
                }),
                ..test_gic()
            },
            ..test_config(
                &layout,
                Arm64FdtBootInfo {
                    command_line: "panic=1",
                    initrd: None,
                },
            )
        };

        assert_eq!(
            write_test_arm64_fdt(&config, &mut memory)
                .expect_err("invalid GICv2m metadata should fail before writing"),
            Arm64FdtError::InvalidGicMsiInterruptRange
        );
        let mut after = [0; 16];
        memory
            .read_slice(&mut after, address)
            .expect("sentinel bytes should remain readable");
        assert_eq!(after, before);
    }

    #[test]
    fn rejects_mismatched_guest_memory_layout_without_partial_mutation() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let memory_range = GuestMemoryRange::new(
            GuestAddress::new(aarch64::DRAM_MEM_START),
            aarch64::FDT_MAX_SIZE,
        )
        .expect("mapped prefix should be valid");
        let memory_layout = GuestMemoryLayout::new(vec![memory_range])
            .expect("mapped prefix layout should be valid");
        let mut memory =
            GuestMemory::allocate(&memory_layout).expect("mapped prefix should allocate");
        let mut before = vec![0; 16];
        memory
            .read_slice(&mut before, GuestAddress::new(aarch64::DRAM_MEM_START))
            .expect("initial bytes should read");
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );

        let err = write_test_arm64_fdt(&config, &mut memory)
            .expect_err("mismatched guest memory layout should fail before write");

        assert_eq!(
            err,
            Arm64FdtError::GuestMemoryLayoutMismatch {
                range_index: 0,
                expected: Some(layout.ranges()[0]),
                actual: Some(memory_range),
            }
        );
        let mut after = vec![0; before.len()];
        memory
            .read_slice(&mut after, GuestAddress::new(aarch64::DRAM_MEM_START))
            .expect("mapped prefix should remain readable");
        assert_eq!(after, before);
    }

    #[test]
    fn raw_fdt_write_rejects_unbacked_guest_memory_without_partial_mutation() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let fdt_address = aarch64::fdt_address(&layout).expect("FDT address should resolve");
        let memory_layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(
                GuestAddress::new(aarch64::DRAM_MEM_START),
                aarch64::FDT_MAX_SIZE,
            )
            .expect("mapped prefix should be valid"),
        ])
        .expect("mapped prefix layout should be valid");
        let mut memory =
            GuestMemory::allocate(&memory_layout).expect("mapped prefix should allocate");
        let mut before = vec![0; 16];
        memory
            .read_slice(&mut before, GuestAddress::new(aarch64::DRAM_MEM_START))
            .expect("initial bytes should read");
        let bytes = vec![0xa5; 256];

        let err = write_arm64_fdt_bytes(&layout, &mut memory, &bytes)
            .expect_err("unbacked raw FDT write should fail");

        assert_eq!(
            err,
            Arm64FdtError::GuestMemoryWrite {
                source: GuestMemoryAccessError::UnmappedRange {
                    range: GuestMemoryRange::new(fdt_address, 256)
                        .expect("FDT write range should be valid"),
                },
            }
        );
        let mut after = vec![0; before.len()];
        memory
            .read_slice(&mut after, GuestAddress::new(aarch64::DRAM_MEM_START))
            .expect("mapped prefix should remain readable");
        assert_eq!(after, before);
    }

    #[test]
    fn rejects_memory_without_ram_after_system_area() {
        let layout = test_layout(aarch64::SYSTEM_MEM_SIZE);
        let first_range = layout.ranges()[0];
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );

        let err = build_test_arm64_fdt(&config).expect_err("tiny memory should fail");

        assert_eq!(
            err,
            Arm64FdtError::NoGuestMemoryAfterSystemArea {
                first_range,
                system_size: aarch64::SYSTEM_MEM_SIZE,
            }
        );
    }

    #[test]
    fn rejects_layout_that_does_not_start_at_arm64_dram_start() {
        let start = GuestAddress::new(aarch64::DRAM_MEM_START + aarch64::GUEST_PAGE_SIZE);
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(start, TEST_MEMORY_SIZE).expect("test range should be valid"),
        ])
        .expect("test layout should be valid");
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
        );

        let err = build_test_arm64_fdt(&config).expect_err("unexpected DRAM start should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidDramStart {
                actual: start,
                expected: GuestAddress::new(aarch64::SYSTEM_MEM_START),
            }
        );
    }

    #[test]
    fn rejects_missing_cpus() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = Arm64FdtConfig {
            vcpu_mpidrs: &[],
            ..test_config(
                &layout,
                Arm64FdtBootInfo {
                    command_line: "panic=1",
                    initrd: None,
                },
            )
        };

        let err = build_test_arm64_fdt(&config).expect_err("missing CPU should fail");

        assert_eq!(err, Arm64FdtError::MissingCpu);
    }

    #[test]
    fn rejects_too_many_cpus() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let mpidrs: Vec<u64> = (0..=MAX_ARM64_FDT_CPUS as u64).collect();
        let config = Arm64FdtConfig {
            vcpu_mpidrs: &mpidrs,
            ..test_config(
                &layout,
                Arm64FdtBootInfo {
                    command_line: "panic=1",
                    initrd: None,
                },
            )
        };

        let err = build_test_arm64_fdt(&config).expect_err("too many CPUs should fail");

        assert_eq!(
            err,
            Arm64FdtError::TooManyCpus {
                count: MAX_ARM64_FDT_CPUS + 1,
                max: MAX_ARM64_FDT_CPUS,
            }
        );
    }

    #[test]
    fn rejects_duplicate_cpu_reg_values() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let mpidrs = [0, CPU_REG_MASK + 1];
        let config = Arm64FdtConfig {
            vcpu_mpidrs: &mpidrs,
            ..test_config(
                &layout,
                Arm64FdtBootInfo {
                    command_line: "panic=1",
                    initrd: None,
                },
            )
        };

        let err = build_test_arm64_fdt(&config).expect_err("duplicate CPU reg should fail");

        assert_eq!(
            err,
            Arm64FdtError::DuplicateCpuReg {
                first_index: 0,
                second_index: 1,
                reg: 0,
            }
        );
    }

    #[test]
    fn cpu_node_names_use_cpu_reg_values() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let mpidrs = [0, 2];
        let config = Arm64FdtConfig {
            vcpu_mpidrs: &mpidrs,
            ..test_config(
                &layout,
                Arm64FdtBootInfo {
                    command_line: "panic=1",
                    initrd: None,
                },
            )
        };

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");

        assert_eq!(
            required_node(&tree, "/cpus/cpu@0").prop_u64("reg").unwrap(),
            0
        );
        assert_eq!(
            required_node(&tree, "/cpus/cpu@2").prop_u64("reg").unwrap(),
            2
        );
        assert!(tree.find("/cpus/cpu@1").is_none());
    }

    #[test]
    fn rejects_duplicate_timer_ppis() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = Arm64FdtConfig {
            timer: Arm64FdtTimerInterrupts {
                secure_physical: 13,
                non_secure_physical: 13,
                virtual_timer: 11,
                hypervisor: 10,
            },
            ..test_config(
                &layout,
                Arm64FdtBootInfo {
                    command_line: "panic=1",
                    initrd: None,
                },
            )
        };

        let err = build_test_arm64_fdt(&config).expect_err("duplicate timer PPI should fail");

        assert_eq!(
            err,
            Arm64FdtError::DuplicatePpi {
                first: "secure_physical_timer",
                second: "non_secure_physical_timer",
                value: 13,
            }
        );
    }

    #[test]
    fn rejects_invalid_timer_intids() {
        let err = Arm64FdtTimerInterrupts::from_el1_timer_intids(15, 30)
            .expect_err("non-PPI virtual timer INTID should fail");

        assert_eq!(
            err,
            Arm64FdtError::InvalidPpiIntid {
                name: "el1_virtual_timer_intid",
                intid: 15,
            }
        );
    }

    #[test]
    fn rejects_duplicate_timer_intids() {
        let err = Arm64FdtTimerInterrupts::from_el1_timer_intids(27, 27)
            .expect_err("duplicate timer INTIDs should fail after PPI mapping");

        assert_eq!(
            err,
            Arm64FdtError::DuplicatePpi {
                first: "non_secure_physical_timer",
                second: "virtual_timer",
                value: 11,
            }
        );
    }

    #[test]
    fn rtc_node_uses_firecracker_shape() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let rtc = rtc_device(0x4000_1000, 0x1000);
        let config = test_config_with_rtc_device(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            rtc,
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let clock = required_node(&tree, "/apb-pclk");
        let rtc_node = required_node(&tree, "/rtc@40001000");

        assert_eq!(clock.prop_str("compatible").unwrap(), "fixed-clock");
        assert_eq!(clock.prop_u32("#clock-cells").unwrap(), 0);
        assert_eq!(
            clock.prop_u32("clock-frequency").unwrap(),
            APB_PCLK_CLOCK_FREQUENCY
        );
        assert_eq!(clock.prop_u32("phandle").unwrap(), CLOCK_PHANDLE);

        assert_eq!(
            rtc_node.prop_raw("compatible").unwrap(),
            b"arm,pl031\0arm,primecell\0"
        );
        assert_eq!(prop_u64_cells(rtc_node, "reg"), vec![0x4000_1000, 0x1000]);
        assert_eq!(rtc_node.prop_u32("clocks").unwrap(), CLOCK_PHANDLE);
        assert_eq!(
            rtc_node.prop_str("clock-names").unwrap(),
            APB_PCLK_CLOCK_NAME
        );
        assert!(!rtc_node.has_prop("interrupts"));
        assert!(!rtc_node.has_prop("interrupt-parent"));
        assert_eq!(tree.root.prop_u32("interrupt-parent").unwrap(), GIC_PHANDLE);
    }

    #[test]
    fn rtc_node_is_ordered_before_serial_and_sorted_virtio_mmio_nodes() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let rtc = rtc_device(0x4000_1000, 0x1000);
        let serial = serial_device(0x4000_2000, 0x1000, 33);
        let devices = [
            virtio_mmio_device(0x4000_5000, 0x1000, 35),
            virtio_mmio_device(0x4000_3000, 0x1000, 34),
        ];
        let config = test_config_with_optional_devices(
            &layout,
            Arm64FdtBootInfo {
                command_line: "panic=1",
                initrd: None,
            },
            Some(rtc),
            Some(serial),
            &devices,
        );

        let bytes = build_test_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let root_children: Vec<&str> = tree
            .root
            .children
            .iter()
            .map(|child| child.name.as_str())
            .collect();

        assert_eq!(
            root_children,
            [
                "cpus",
                "memory@ram",
                "chosen",
                "intc",
                "timer",
                "apb-pclk",
                "psci",
                "rtc@40001000",
                "uart@40002000",
                "virtio_mmio@40003000",
                "virtio_mmio@40005000",
            ]
        );
    }

    fn build_test_arm64_fdt(config: &Arm64FdtConfig<'_>) -> Result<Vec<u8>, Arm64FdtError> {
        let mut source = FixedRngSeedSource::new(TEST_RNG_SEED);
        build_arm64_fdt_with_rng_seed_source(config, &mut source)
    }

    fn write_test_arm64_fdt(
        config: &Arm64FdtConfig<'_>,
        memory: &mut GuestMemory,
    ) -> Result<Arm64FdtGuestMemoryWrite, Arm64FdtError> {
        let mut source = FixedRngSeedSource::new(TEST_RNG_SEED);
        write_arm64_fdt_with_rng_seed_source(config, memory, &mut source)
    }

    #[derive(Debug)]
    struct FixedRngSeedSource {
        seed: [u8; ARM64_FDT_RNG_SEED_SIZE],
    }

    impl FixedRngSeedSource {
        const fn new(seed: [u8; ARM64_FDT_RNG_SEED_SIZE]) -> Self {
            Self { seed }
        }
    }

    impl Arm64FdtRngSeedSource for FixedRngSeedSource {
        fn fill_rng_seed(
            &mut self,
            destination: &mut [u8; ARM64_FDT_RNG_SEED_SIZE],
        ) -> Result<(), Arm64FdtRngSeedError> {
            *destination = self.seed;
            Ok(())
        }
    }

    #[derive(Debug)]
    struct FailingRngSeedSource;

    impl Arm64FdtRngSeedSource for FailingRngSeedSource {
        fn fill_rng_seed(
            &mut self,
            _destination: &mut [u8; ARM64_FDT_RNG_SEED_SIZE],
        ) -> Result<(), Arm64FdtRngSeedError> {
            Err(Arm64FdtRngSeedError::new())
        }
    }

    fn test_layout(size: u64) -> GuestMemoryLayout {
        aarch64::dram_layout(size).expect("test layout should be valid")
    }

    fn oversized_fdt_layout() -> GuestMemoryLayout {
        let mut ranges = Vec::new();
        let mut start = aarch64::DRAM_MEM_START;
        let first_range_size = aarch64::SYSTEM_MEM_SIZE + aarch64::GUEST_PAGE_SIZE;
        ranges.push(
            GuestMemoryRange::new(GuestAddress::new(start), first_range_size)
                .expect("oversized FDT first range should be valid"),
        );
        start += first_range_size;

        for _ in 0..(aarch64::FDT_MAX_SIZE / 16 + 1) {
            ranges.push(
                GuestMemoryRange::new(GuestAddress::new(start), 1)
                    .expect("oversized FDT memory range should be valid"),
            );
            start += 1;
        }

        GuestMemoryLayout::new(ranges).expect("oversized FDT layout should be valid")
    }

    fn test_config<'a>(
        layout: &'a GuestMemoryLayout,
        boot: Arm64FdtBootInfo<'a>,
    ) -> Arm64FdtConfig<'a> {
        test_config_with_devices(layout, boot, None, &[])
    }

    fn test_config_with_virtio_mmio_devices<'a>(
        layout: &'a GuestMemoryLayout,
        boot: Arm64FdtBootInfo<'a>,
        virtio_mmio_devices: &'a [Arm64FdtVirtioMmioDevice],
    ) -> Arm64FdtConfig<'a> {
        test_config_with_devices(layout, boot, None, virtio_mmio_devices)
    }

    fn test_config_with_serial_device<'a>(
        layout: &'a GuestMemoryLayout,
        boot: Arm64FdtBootInfo<'a>,
        serial_device: Arm64FdtSerialDevice,
    ) -> Arm64FdtConfig<'a> {
        test_config_with_devices(layout, boot, Some(serial_device), &[])
    }

    fn test_config_with_rtc_device<'a>(
        layout: &'a GuestMemoryLayout,
        boot: Arm64FdtBootInfo<'a>,
        rtc_device: Arm64FdtRtcDevice,
    ) -> Arm64FdtConfig<'a> {
        test_config_with_optional_devices(layout, boot, Some(rtc_device), None, &[])
    }

    fn test_config_with_vmgenid_device<'a>(
        layout: &'a GuestMemoryLayout,
        boot: Arm64FdtBootInfo<'a>,
        vmgenid_device: Arm64FdtVmGenIdDevice,
    ) -> Arm64FdtConfig<'a> {
        test_config_with_vmgenid_and_optional_devices(
            layout,
            boot,
            Some(vmgenid_device),
            None,
            None,
            &[],
        )
    }

    fn test_config_with_devices<'a>(
        layout: &'a GuestMemoryLayout,
        boot: Arm64FdtBootInfo<'a>,
        serial_device: Option<Arm64FdtSerialDevice>,
        virtio_mmio_devices: &'a [Arm64FdtVirtioMmioDevice],
    ) -> Arm64FdtConfig<'a> {
        test_config_with_optional_devices(layout, boot, None, serial_device, virtio_mmio_devices)
    }

    fn test_config_with_optional_devices<'a>(
        layout: &'a GuestMemoryLayout,
        boot: Arm64FdtBootInfo<'a>,
        rtc_device: Option<Arm64FdtRtcDevice>,
        serial_device: Option<Arm64FdtSerialDevice>,
        virtio_mmio_devices: &'a [Arm64FdtVirtioMmioDevice],
    ) -> Arm64FdtConfig<'a> {
        Arm64FdtConfig {
            layout,
            boot,
            vcpu_mpidrs: TEST_VCPU_MPIDRS,
            cache_hierarchy: &TEST_CACHE_HIERARCHY,
            gic: test_gic(),
            timer: Arm64FdtTimerInterrupts::firecracker_default(),
            rtc_device,
            serial_device,
            vmgenid_device: None,
            vmclock_device: None,
            virtio_mmio_devices,
        }
    }

    fn test_config_with_vmgenid_and_optional_devices<'a>(
        layout: &'a GuestMemoryLayout,
        boot: Arm64FdtBootInfo<'a>,
        vmgenid_device: Option<Arm64FdtVmGenIdDevice>,
        rtc_device: Option<Arm64FdtRtcDevice>,
        serial_device: Option<Arm64FdtSerialDevice>,
        virtio_mmio_devices: &'a [Arm64FdtVirtioMmioDevice],
    ) -> Arm64FdtConfig<'a> {
        Arm64FdtConfig {
            vmgenid_device,
            ..test_config_with_optional_devices(
                layout,
                boot,
                rtc_device,
                serial_device,
                virtio_mmio_devices,
            )
        }
    }

    fn test_config_with_vmclock_and_optional_devices<'a>(
        layout: &'a GuestMemoryLayout,
        boot: Arm64FdtBootInfo<'a>,
        vmclock_device: Option<Arm64FdtVmClockDevice>,
        vmgenid_device: Option<Arm64FdtVmGenIdDevice>,
        rtc_device: Option<Arm64FdtRtcDevice>,
        serial_device: Option<Arm64FdtSerialDevice>,
        virtio_mmio_devices: &'a [Arm64FdtVirtioMmioDevice],
    ) -> Arm64FdtConfig<'a> {
        Arm64FdtConfig {
            vmgenid_device,
            vmclock_device,
            ..test_config_with_optional_devices(
                layout,
                boot,
                rtc_device,
                serial_device,
                virtio_mmio_devices,
            )
        }
    }

    const fn rtc_device(base: u64, size: u64) -> Arm64FdtRtcDevice {
        Arm64FdtRtcDevice {
            region: Arm64FdtRegion { base, size },
        }
    }

    fn serial_device(base: u64, size: u64, line: u32) -> Arm64FdtSerialDevice {
        Arm64FdtSerialDevice {
            region: Arm64FdtRegion { base, size },
            interrupt_line: GuestInterruptLine::new(line)
                .expect("test interrupt line should be nonzero"),
        }
    }

    fn vmgenid_device(base: u64, size: u64, line: u32) -> Arm64FdtVmGenIdDevice {
        Arm64FdtVmGenIdDevice {
            region: Arm64FdtRegion { base, size },
            interrupt_line: GuestInterruptLine::new(line)
                .expect("test interrupt line should be nonzero"),
        }
    }

    fn vmclock_device(base: u64, size: u64, line: u32) -> Arm64FdtVmClockDevice {
        Arm64FdtVmClockDevice {
            region: Arm64FdtRegion { base, size },
            interrupt_line: GuestInterruptLine::new(line)
                .expect("test interrupt line should be nonzero"),
        }
    }

    fn virtio_mmio_device(base: u64, size: u64, line: u32) -> Arm64FdtVirtioMmioDevice {
        Arm64FdtVirtioMmioDevice {
            region: Arm64FdtRegion { base, size },
            interrupt_line: GuestInterruptLine::new(line)
                .expect("test interrupt line should be nonzero"),
        }
    }

    const fn test_gic() -> Arm64FdtGic {
        Arm64FdtGic {
            distributor: Arm64FdtRegion {
                base: 0x3fff_0000,
                size: 0x1_0000,
            },
            redistributor: Arm64FdtRegion {
                base: 0x3ffd_0000,
                size: 0x2_0000,
            },
            compatibility: GIC_COMPATIBILITY,
            interrupt_cells: 3,
            maintenance_irq: 9,
            msi: None,
        }
    }

    const fn test_msi() -> Arm64FdtMsi {
        Arm64FdtMsi {
            region: Arm64FdtRegion {
                base: 0x3ffc_0000,
                size: 0x1_0000,
            },
            interrupt_range: Arm64FdtInterruptRange {
                base: 128,
                count: 32,
            },
        }
    }

    const fn test_initrd() -> LoadedInitrd {
        LoadedInitrd {
            address: TEST_INITRD_ADDRESS,
            size: TEST_INITRD_SIZE,
        }
    }

    fn required_node<'a>(tree: &'a DeviceTree, path: &str) -> &'a Node {
        tree.find(path).expect("node should exist")
    }

    fn prop_u32_cells(node: &Node, name: &str) -> Vec<u32> {
        node.prop_raw(name)
            .expect("property should exist")
            .chunks_exact(4)
            .map(|chunk| u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect()
    }

    fn prop_u64_cells(node: &Node, name: &str) -> Vec<u64> {
        node.prop_raw(name)
            .expect("property should exist")
            .chunks_exact(8)
            .map(|chunk| {
                u64::from_be_bytes([
                    chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
                ])
            })
            .collect()
    }
}
