//! Minimal arm64 Flattened Device Tree generation for guest boot metadata.

use std::fmt;

use vm_fdt::{Error as VmFdtError, FdtWriter};

use crate::boot::{LoadedBootSource, LoadedInitrd};
use crate::memory::{
    GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryError, GuestMemoryLayout,
    GuestMemoryRange, aarch64,
};

const ROOT_COMPATIBILITY: &str = "linux,dummy-virt";
const GIC_PHANDLE: u32 = 1;
const ADDRESS_CELLS: u32 = 2;
const SIZE_CELLS: u32 = 2;
const CPU_ADDRESS_CELLS: u32 = 2;
const CPU_SIZE_CELLS: u32 = 0;
const CPU_REG_MASK: u64 = 0x7f_ffff;
const MAX_ARM64_FDT_CPUS: usize = 32;
const GIC_FDT_IRQ_TYPE_PPI: u32 = 1;
const IRQ_TYPE_LEVEL_HIGH: u32 = 4;
const FIRST_PPI_INTID: u32 = 16;
const FIRST_SPI_INTID: u32 = 32;

pub const ARM64_FDT_SECURE_PHYSICAL_TIMER_PPI: u32 = 13;
pub const ARM64_FDT_NON_SECURE_PHYSICAL_TIMER_PPI: u32 = 14;
pub const ARM64_FDT_VIRTUAL_TIMER_PPI: u32 = 11;
pub const ARM64_FDT_HYPERVISOR_TIMER_PPI: u32 = 10;

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
    pub gic: Arm64FdtGic,
    pub timer: Arm64FdtTimerInterrupts,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64FdtMsi {
    pub region: Arm64FdtRegion,
    pub interrupt_range: Arm64FdtInterruptRange,
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
    UnsupportedMsi,
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
                "arm64 FDT initrd end address {end_exclusive} overlaps reserved FDT address {fdt_address}"
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
                "arm64 FDT timer PPIs must be distinct: {first} and {second} both use {value}"
            ),
            Self::InvalidPpiIntid { name, intid } => write!(
                f,
                "arm64 FDT {name} INTID must be in the PPI range [16, 32), got {intid}"
            ),
            Self::UnsupportedMsi => f.write_str("arm64 FDT MSI/ITS nodes are not supported yet"),
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
            Self::InvalidInitrdRange { source } => Some(source),
            Self::CreateFdt { source } => Some(source),
            Self::GuestMemoryWrite { source } => Some(source),
            Self::MissingCpu
            | Self::TooManyCpus { .. }
            | Self::DuplicateCpuReg { .. }
            | Self::NoGuestMemoryAfterSystemArea { .. }
            | Self::InvalidDramStart { .. }
            | Self::InitrdNotInGuestMemory { .. }
            | Self::InitrdOverlapsFdt { .. }
            | Self::InvalidGicRegion { .. }
            | Self::GicRegionsOverlap { .. }
            | Self::GicRegionOverlapsMemory { .. }
            | Self::InvalidGicInterruptCells { .. }
            | Self::InvalidPpi { .. }
            | Self::DuplicatePpi { .. }
            | Self::InvalidPpiIntid { .. }
            | Self::UnsupportedMsi
            | Self::FdtTooLarge { .. }
            | Self::GuestMemoryLayoutMismatch { .. } => None,
        }
    }
}

impl From<VmFdtError> for Arm64FdtError {
    fn from(source: VmFdtError) -> Self {
        Self::CreateFdt { source }
    }
}

pub fn build_arm64_fdt(config: &Arm64FdtConfig<'_>) -> Result<Vec<u8>, Arm64FdtError> {
    validate_config(config)?;

    let mut fdt = FdtWriter::new()?;
    let root = fdt.begin_node("")?;
    fdt.property_string("compatible", ROOT_COMPATIBILITY)?;
    fdt.property_u32("#address-cells", ADDRESS_CELLS)?;
    fdt.property_u32("#size-cells", SIZE_CELLS)?;
    fdt.property_u32("interrupt-parent", GIC_PHANDLE)?;

    create_cpu_nodes(&mut fdt, config.vcpu_mpidrs)?;
    create_memory_node(&mut fdt, config.layout)?;
    create_chosen_node(&mut fdt, config.boot)?;
    create_gic_node(&mut fdt, config.gic)?;
    create_timer_node(&mut fdt, config.timer)?;
    create_psci_node(&mut fdt)?;

    fdt.end_node(root)?;
    let bytes = fdt.finish()?;
    validate_fdt_size(bytes.len())?;
    Ok(bytes)
}

pub fn write_arm64_fdt(
    config: &Arm64FdtConfig<'_>,
    memory: &mut GuestMemory,
) -> Result<Arm64FdtGuestMemoryWrite, Arm64FdtError> {
    validate_guest_memory_matches_layout(config.layout, memory)?;
    let bytes = build_arm64_fdt(config)?;
    write_arm64_fdt_bytes(config.layout, memory, &bytes)
}

fn validate_config(config: &Arm64FdtConfig<'_>) -> Result<(), Arm64FdtError> {
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
    memory_reg_cells(config.layout)?;
    validate_gic(config.layout, config.gic)?;
    validate_timer(config.timer)?;
    if let Some(initrd) = config.boot.initrd {
        validate_initrd(config.layout, initrd)?;
    }

    Ok(())
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

fn create_cpu_nodes(fdt: &mut FdtWriter, mpidrs: &[u64]) -> Result<(), Arm64FdtError> {
    let cpus = fdt.begin_node("cpus")?;
    fdt.property_u32("#address-cells", CPU_ADDRESS_CELLS)?;
    fdt.property_u32("#size-cells", CPU_SIZE_CELLS)?;

    for (cpu_index, mpidr) in mpidrs.iter().copied().enumerate() {
        let cpu = fdt.begin_node(&format!("cpu@{cpu_index:x}"))?;
        fdt.property_string("device_type", "cpu")?;
        fdt.property_string("compatible", "arm,arm-v8")?;
        fdt.property_string("enable-method", "psci")?;
        fdt.property_u64("reg", cpu_reg(mpidr))?;
        fdt.end_node(cpu)?;
    }

    fdt.end_node(cpus)?;
    Ok(())
}

const fn cpu_reg(mpidr: u64) -> u64 {
    mpidr & CPU_REG_MASK
}

fn create_memory_node(
    fdt: &mut FdtWriter,
    layout: &GuestMemoryLayout,
) -> Result<(), Arm64FdtError> {
    let memory = fdt.begin_node("memory@ram")?;
    fdt.property_string("device_type", "memory")?;
    fdt.property_array_u64("reg", &memory_reg_cells(layout)?)?;
    fdt.end_node(memory)?;
    Ok(())
}

fn create_chosen_node(
    fdt: &mut FdtWriter,
    boot: Arm64FdtBootInfo<'_>,
) -> Result<(), Arm64FdtError> {
    let chosen = fdt.begin_node("chosen")?;
    fdt.property_string("bootargs", boot.command_line)?;

    if let Some(initrd) = boot.initrd {
        let initrd_end = initrd_range(initrd)?.end_exclusive();
        fdt.property_u64("linux,initrd-start", initrd.address.raw_value())?;
        fdt.property_u64("linux,initrd-end", initrd_end.raw_value())?;
    }

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

fn create_psci_node(fdt: &mut FdtWriter) -> Result<(), Arm64FdtError> {
    let psci = fdt.begin_node("psci")?;
    fdt.property_string("compatible", "arm,psci-0.2")?;
    fdt.property_string("method", "hvc")?;
    fdt.end_node(psci)?;
    Ok(())
}

fn memory_reg_cells(layout: &GuestMemoryLayout) -> Result<Vec<u64>, Arm64FdtError> {
    let mut cells = Vec::with_capacity(layout.ranges().len().saturating_mul(2));
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
            let start = range.start().checked_add(aarch64::SYSTEM_MEM_SIZE).ok_or(
                Arm64FdtError::InvalidLayout {
                    source: GuestMemoryError::AddressOverflow {
                        start: range.start(),
                        size: aarch64::SYSTEM_MEM_SIZE,
                    },
                },
            )?;
            cells.push(start.raw_value());
            cells.push(range.size() - aarch64::SYSTEM_MEM_SIZE);
        } else {
            cells.push(range.start().raw_value());
            cells.push(range.size());
        }
    }

    Ok(cells)
}

fn validate_initrd(layout: &GuestMemoryLayout, initrd: LoadedInitrd) -> Result<(), Arm64FdtError> {
    let range = initrd_range(initrd)?;
    if !is_range_in_guest_memory_node(layout, range)? {
        return Err(Arm64FdtError::InitrdNotInGuestMemory { range });
    }

    let fdt_address =
        aarch64::fdt_address(layout).map_err(|source| Arm64FdtError::InvalidLayout { source })?;
    if range.end_exclusive() > fdt_address {
        return Err(Arm64FdtError::InitrdOverlapsFdt {
            end_exclusive: range.end_exclusive(),
            fdt_address,
        });
    }

    Ok(())
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

    if gic.interrupt_cells != 3 {
        return Err(Arm64FdtError::InvalidGicInterruptCells {
            value: gic.interrupt_cells,
        });
    }

    validate_ppi("maintenance_irq", gic.maintenance_irq)?;

    if gic.msi.is_some() {
        return Err(Arm64FdtError::UnsupportedMsi);
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
    use device_tree::{DeviceTree, Node};

    use super::*;

    const TEST_MEMORY_SIZE: u64 = aarch64::SYSTEM_MEM_SIZE + aarch64::FDT_MAX_SIZE + 0x40_0000;
    const TEST_INITRD_ADDRESS: GuestAddress =
        GuestAddress::new(aarch64::DRAM_MEM_START + 0x30_0000);
    const TEST_INITRD_SIZE: u64 = 0x1000;
    const TEST_VCPU_MPIDRS: &[u64] = &[0];

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

        let bytes = build_arm64_fdt(&config).expect("FDT should be built");
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

        let bytes = build_arm64_fdt(&config).expect("FDT should be built");
        let tree = DeviceTree::load(&bytes).expect("FDT should parse");
        let chosen = required_node(&tree, "/chosen");

        assert_eq!(chosen.prop_str("bootargs").unwrap(), "panic=1");
        assert!(!chosen.has_prop("linux,initrd-start"));
        assert!(!chosen.has_prop("linux,initrd-end"));
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

        let err = build_arm64_fdt(&config).expect_err("empty initrd range should fail");

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

        let err =
            build_arm64_fdt(&config).expect_err("initrd outside advertised memory should fail");

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

        let err = build_arm64_fdt(&config).expect_err("initrd overlapping FDT should fail");

        assert_eq!(
            err,
            Arm64FdtError::InitrdOverlapsFdt {
                end_exclusive: fdt_address
                    .checked_add(1)
                    .expect("test initrd end should not overflow"),
                fdt_address,
            }
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

        let bytes = build_arm64_fdt(&config).expect("FDT should be built");
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

        let bytes = build_arm64_fdt(&config).expect("FDT should be built");
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

        let bytes = build_arm64_fdt(&config).expect("FDT should be built");
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

        let err = build_arm64_fdt(&config).expect_err("overlapping GIC regions should fail");

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

        let err = build_arm64_fdt(&config).expect_err("GIC region overlapping memory should fail");

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
    fn timer_node_uses_firecracker_ppi_cells() {
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
        let timer = required_node(&tree, "/timer");

        assert_eq!(timer.prop_str("compatible").unwrap(), "arm,armv8-timer");
        assert!(timer.has_prop("always-on"));
        assert_eq!(
            prop_u32_cells(timer, "interrupts"),
            vec![1, 13, 4, 1, 14, 4, 1, 11, 4, 1, 10, 4]
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

        let write = write_arm64_fdt(&config, &mut memory).expect("FDT should write");

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
    fn rejects_oversized_generated_fdt() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let command_line = "x".repeat(aarch64::FDT_MAX_SIZE as usize + 1);
        let config = test_config(
            &layout,
            Arm64FdtBootInfo {
                command_line: &command_line,
                initrd: None,
            },
        );

        let err = build_arm64_fdt(&config).expect_err("oversized generated FDT should fail");

        assert!(matches!(
            err,
            Arm64FdtError::FdtTooLarge {
                max_size: aarch64::FDT_MAX_SIZE,
                ..
            }
        ));
    }

    #[test]
    fn rejects_msi_metadata_until_its_node_is_supported() {
        let layout = test_layout(TEST_MEMORY_SIZE);
        let config = Arm64FdtConfig {
            gic: Arm64FdtGic {
                msi: Some(Arm64FdtMsi {
                    region: Arm64FdtRegion {
                        base: 0x3ffc_0000,
                        size: 0x1_0000,
                    },
                    interrupt_range: Arm64FdtInterruptRange {
                        base: 128,
                        count: 32,
                    },
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

        let err = build_arm64_fdt(&config).expect_err("MSI should be explicit unsupported work");

        assert_eq!(err, Arm64FdtError::UnsupportedMsi);
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

        let err = write_arm64_fdt(&config, &mut memory)
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

        let err = build_arm64_fdt(&config).expect_err("tiny memory should fail");

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

        let err = build_arm64_fdt(&config).expect_err("unexpected DRAM start should fail");

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

        let err = build_arm64_fdt(&config).expect_err("missing CPU should fail");

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

        let err = build_arm64_fdt(&config).expect_err("too many CPUs should fail");

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

        let err = build_arm64_fdt(&config).expect_err("duplicate CPU reg should fail");

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

        let err = build_arm64_fdt(&config).expect_err("duplicate timer PPI should fail");

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

    fn test_layout(size: u64) -> GuestMemoryLayout {
        aarch64::dram_layout(size).expect("test layout should be valid")
    }

    fn test_config<'a>(
        layout: &'a GuestMemoryLayout,
        boot: Arm64FdtBootInfo<'a>,
    ) -> Arm64FdtConfig<'a> {
        Arm64FdtConfig {
            layout,
            boot,
            vcpu_mpidrs: TEST_VCPU_MPIDRS,
            gic: test_gic(),
            timer: Arm64FdtTimerInterrupts::firecracker_default(),
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
            compatibility: "arm,gic-v3",
            interrupt_cells: 3,
            maintenance_irq: 9,
            msi: None,
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
