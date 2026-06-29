//! Internal HVF arm64 boot-session preparation.

use std::collections::TryReserveError;
use std::fmt;

use bangbang_runtime::block::BlockMmioLayout;
use bangbang_runtime::fdt::Arm64FdtError;
use bangbang_runtime::interrupt::GuestInterruptLine;
use bangbang_runtime::memory::GuestAddress;
use bangbang_runtime::mmio::MmioRegionId;
use bangbang_runtime::serial::SharedSerialOutputBuffer;
use bangbang_runtime::startup::{
    Arm64BootResourceConfig, Arm64BootResourceError, Arm64BootResources, Arm64BootRuntimeResources,
    Arm64BootSerialDeviceConfig as RuntimeArm64BootSerialDeviceConfig,
};
use bangbang_runtime::{BackendError, VmBackend, VmmController};

use crate::backend::HvfBackend;
use crate::gic::{
    HvfGicError, HvfGicInterruptLineAllocator, HvfGicMetadata, HvfInterruptLineAllocationError,
};
use crate::memory::{HvfGuestMemoryMappingError, HvfMemoryPermissions};
use crate::runner::{HvfVcpuRunner, HvfVcpuRunnerError};
use crate::vcpu::HvfArm64BootRegisters;

const SINGLE_VCPU_COUNT: u8 = 1;

#[derive(Debug, Clone)]
pub struct HvfArm64BootSessionConfig {
    pub block_mmio_layout: BlockMmioLayout,
    pub serial_device: Option<HvfArm64BootSerialDeviceConfig>,
}

impl HvfArm64BootSessionConfig {
    pub const fn new(block_mmio_layout: BlockMmioLayout) -> Self {
        Self {
            block_mmio_layout,
            serial_device: None,
        }
    }

    pub fn with_serial_device(mut self, serial_device: HvfArm64BootSerialDeviceConfig) -> Self {
        self.serial_device = Some(serial_device);
        self
    }
}

#[derive(Debug, Clone)]
pub struct HvfArm64BootSerialDeviceConfig {
    pub region_id: MmioRegionId,
    pub address: GuestAddress,
    pub output: SharedSerialOutputBuffer,
}

impl HvfArm64BootSerialDeviceConfig {
    pub fn new(
        region_id: MmioRegionId,
        address: GuestAddress,
        output: SharedSerialOutputBuffer,
    ) -> Self {
        Self {
            region_id,
            address,
            output,
        }
    }

    fn into_runtime(
        self,
        interrupt_line: GuestInterruptLine,
    ) -> RuntimeArm64BootSerialDeviceConfig {
        RuntimeArm64BootSerialDeviceConfig::new(
            self.region_id,
            self.address,
            interrupt_line,
            self.output,
        )
    }
}

#[derive(Debug)]
pub struct HvfArm64BootSession<'vm> {
    runner: HvfVcpuRunner<'vm>,
    backend: &'vm mut HvfBackend,
    runtime_resources: Arm64BootRuntimeResources,
    gic: HvfGicMetadata,
    primary_mpidr: u64,
    block_interrupt_lines: Vec<GuestInterruptLine>,
    serial_interrupt_line: Option<GuestInterruptLine>,
    boot_registers: HvfArm64BootRegisters,
}

impl HvfArm64BootSession<'_> {
    pub fn shutdown(&mut self) -> Result<(), HvfArm64BootSessionShutdownError> {
        let runner_result = self.runner.shutdown();
        let destroy_result = <HvfBackend as VmBackend>::destroy_vm(self.backend);

        match (runner_result, destroy_result) {
            (Err(source), _) => Err(HvfArm64BootSessionShutdownError::Runner { source }),
            (Ok(()), Err(source)) => Err(HvfArm64BootSessionShutdownError::DestroyVm { source }),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    pub const fn gic_metadata(&self) -> HvfGicMetadata {
        self.gic
    }

    pub const fn primary_mpidr(&self) -> u64 {
        self.primary_mpidr
    }

    pub fn runtime_resources(&self) -> &Arm64BootRuntimeResources {
        &self.runtime_resources
    }

    pub fn block_interrupt_lines(&self) -> &[GuestInterruptLine] {
        &self.block_interrupt_lines
    }

    pub const fn serial_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.serial_interrupt_line
    }

    pub const fn boot_registers(&self) -> HvfArm64BootRegisters {
        self.boot_registers
    }
}

impl Drop for HvfArm64BootSession<'_> {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[derive(Debug)]
pub enum HvfArm64BootSessionError {
    BackendAlreadyInitialized,
    UnsupportedVcpuCount {
        vcpu_count: u8,
    },
    CreateVm {
        source: BackendError,
    },
    CreateGic {
        source: HvfGicError,
    },
    TimerMetadata {
        source: Arm64FdtError,
    },
    InterruptLineStorage {
        source: TryReserveError,
    },
    AllocateInterruptLine {
        purpose: HvfArm64BootInterruptLinePurpose,
        source: HvfInterruptLineAllocationError,
    },
    StartRunner {
        source: HvfVcpuRunnerError,
    },
    ReadMpidr {
        source: HvfVcpuRunnerError,
    },
    AssembleResources {
        source: Arm64BootResourceError,
    },
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    ConfigureBootRegisters {
        source: HvfVcpuRunnerError,
    },
}

impl fmt::Display for HvfArm64BootSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BackendAlreadyInitialized => {
                f.write_str("HVF arm64 boot session requires a backend without an existing VM")
            }
            Self::UnsupportedVcpuCount { vcpu_count } => write!(
                f,
                "HVF arm64 boot session supports exactly {SINGLE_VCPU_COUNT} vCPU, got {vcpu_count}"
            ),
            Self::CreateVm { source } => write!(f, "failed to create HVF VM: {source}"),
            Self::CreateGic { source } => write!(f, "failed to create HVF GIC: {source}"),
            Self::TimerMetadata { source } => {
                write!(
                    f,
                    "failed to convert HVF timer metadata for arm64 FDT: {source}"
                )
            }
            Self::InterruptLineStorage { source } => {
                write!(
                    f,
                    "failed to allocate HVF interrupt-line metadata: {source}"
                )
            }
            Self::AllocateInterruptLine { purpose, source } => {
                write!(
                    f,
                    "failed to allocate HVF SPI interrupt line for {purpose}: {source}"
                )
            }
            Self::StartRunner { source } => {
                write!(f, "failed to start HVF vCPU runner: {source}")
            }
            Self::ReadMpidr { source } => {
                write!(f, "failed to read primary vCPU MPIDR_EL1: {source}")
            }
            Self::AssembleResources { source } => {
                write!(f, "failed to assemble arm64 boot resources: {source}")
            }
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to map arm64 boot guest memory into HVF: {source}"
                )
            }
            Self::ConfigureBootRegisters { source } => {
                write!(
                    f,
                    "failed to configure primary HVF boot registers: {source}"
                )
            }
        }
    }
}

impl std::error::Error for HvfArm64BootSessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateVm { source } => Some(source),
            Self::CreateGic { source } => Some(source),
            Self::TimerMetadata { source } => Some(source),
            Self::InterruptLineStorage { source } => Some(source),
            Self::AllocateInterruptLine { source, .. } => Some(source),
            Self::StartRunner { source } => Some(source),
            Self::ReadMpidr { source } => Some(source),
            Self::AssembleResources { source } => Some(source),
            Self::MapGuestMemory { source } => Some(source),
            Self::ConfigureBootRegisters { source } => Some(source),
            Self::BackendAlreadyInitialized | Self::UnsupportedVcpuCount { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64BootInterruptLinePurpose {
    BlockDevice,
    SerialDevice,
}

impl fmt::Display for HvfArm64BootInterruptLinePurpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlockDevice => f.write_str("block device"),
            Self::SerialDevice => f.write_str("serial device"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfArm64BootSessionShutdownError {
    Runner { source: HvfVcpuRunnerError },
    DestroyVm { source: BackendError },
}

impl fmt::Display for HvfArm64BootSessionShutdownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runner { source } => {
                write!(f, "failed to shut down HVF boot-session runner: {source}")
            }
            Self::DestroyVm { source } => {
                write!(f, "failed to destroy HVF boot-session VM: {source}")
            }
        }
    }
}

impl std::error::Error for HvfArm64BootSessionShutdownError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Runner { source } => Some(source),
            Self::DestroyVm { source } => Some(source),
        }
    }
}

#[derive(Debug)]
struct PreparedHvfArm64BootSession<'vm> {
    runner: HvfVcpuRunner<'vm>,
    runtime_resources: Arm64BootRuntimeResources,
    gic: HvfGicMetadata,
    primary_mpidr: u64,
    block_interrupt_lines: Vec<GuestInterruptLine>,
    serial_interrupt_line: Option<GuestInterruptLine>,
    boot_registers: HvfArm64BootRegisters,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HvfArm64BootInterruptLines {
    block: Vec<GuestInterruptLine>,
    serial: Option<GuestInterruptLine>,
}

impl HvfBackend {
    pub fn prepare_arm64_boot_session<'vm>(
        &'vm mut self,
        controller: &VmmController,
        config: HvfArm64BootSessionConfig,
    ) -> Result<HvfArm64BootSession<'vm>, HvfArm64BootSessionError> {
        if self.has_created_vm() {
            return Err(HvfArm64BootSessionError::BackendAlreadyInitialized);
        }

        let prepared = match prepare_arm64_boot_session_parts(self, controller, config) {
            Ok(prepared) => prepared,
            Err(err) => {
                let _ = <Self as VmBackend>::destroy_vm(self);
                return Err(err);
            }
        };

        Ok(HvfArm64BootSession {
            runner: prepared.runner,
            backend: self,
            runtime_resources: prepared.runtime_resources,
            gic: prepared.gic,
            primary_mpidr: prepared.primary_mpidr,
            block_interrupt_lines: prepared.block_interrupt_lines,
            serial_interrupt_line: prepared.serial_interrupt_line,
            boot_registers: prepared.boot_registers,
        })
    }
}

fn prepare_arm64_boot_session_parts<'vm>(
    backend: &mut HvfBackend,
    controller: &VmmController,
    config: HvfArm64BootSessionConfig,
) -> Result<PreparedHvfArm64BootSession<'vm>, HvfArm64BootSessionError> {
    validate_single_vcpu(controller)?;

    <HvfBackend as VmBackend>::create_vm(backend)
        .map_err(|source| HvfArm64BootSessionError::CreateVm { source })?;
    let gic = *backend
        .create_gic()
        .map_err(|source| HvfArm64BootSessionError::CreateGic { source })?;
    let timer = gic
        .arm64_fdt_timer_interrupts()
        .map_err(|source| HvfArm64BootSessionError::TimerMetadata { source })?;
    let interrupt_lines = allocate_interrupt_lines(
        &gic,
        controller.drive_configs().len(),
        config.serial_device.is_some(),
    )?;

    let runner = backend
        .start_session_vcpu_runner()
        .map_err(|source| HvfArm64BootSessionError::StartRunner { source })?;
    let primary_mpidr = runner
        .mpidr_el1()
        .map_err(|source| HvfArm64BootSessionError::ReadMpidr { source })?;
    let runtime_serial = config
        .serial_device
        .zip(interrupt_lines.serial)
        .map(|(serial, interrupt_line)| serial.into_runtime(interrupt_line));
    let resources = Arm64BootResources::assemble_from_controller(
        controller,
        Arm64BootResourceConfig {
            vcpu_mpidrs: &[primary_mpidr],
            gic: gic.arm64_fdt_gic(),
            timer,
            serial_device: runtime_serial,
            block_mmio_layout: config.block_mmio_layout,
            block_interrupt_lines: &interrupt_lines.block,
        },
    )
    .map_err(|source| HvfArm64BootSessionError::AssembleResources { source })?;
    let parts = resources.into_parts();

    backend
        .map_guest_memory(parts.memory, HvfMemoryPermissions::GUEST_RAM)
        .map_err(|source| HvfArm64BootSessionError::MapGuestMemory { source })?;
    let boot_registers = HvfArm64BootRegisters {
        kernel_entry: parts.runtime.loaded_boot_source.kernel.entry_address,
        fdt_address: parts.runtime.fdt.address,
    };
    runner
        .configure_arm64_boot_registers(boot_registers)
        .map_err(|source| HvfArm64BootSessionError::ConfigureBootRegisters { source })?;

    Ok(PreparedHvfArm64BootSession {
        runner,
        runtime_resources: parts.runtime,
        gic,
        primary_mpidr,
        block_interrupt_lines: interrupt_lines.block,
        serial_interrupt_line: interrupt_lines.serial,
        boot_registers,
    })
}

fn validate_single_vcpu(controller: &VmmController) -> Result<(), HvfArm64BootSessionError> {
    let vcpu_count = controller.machine_config().vcpu_count();
    if vcpu_count == SINGLE_VCPU_COUNT {
        Ok(())
    } else {
        Err(HvfArm64BootSessionError::UnsupportedVcpuCount { vcpu_count })
    }
}

fn allocate_interrupt_lines(
    gic: &HvfGicMetadata,
    block_device_count: usize,
    serial_configured: bool,
) -> Result<HvfArm64BootInterruptLines, HvfArm64BootSessionError> {
    let mut allocator = HvfGicInterruptLineAllocator::from_metadata(gic).map_err(|source| {
        HvfArm64BootSessionError::AllocateInterruptLine {
            purpose: HvfArm64BootInterruptLinePurpose::BlockDevice,
            source,
        }
    })?;
    let mut block = Vec::new();
    block
        .try_reserve_exact(block_device_count)
        .map_err(|source| HvfArm64BootSessionError::InterruptLineStorage { source })?;

    for _ in 0..block_device_count {
        block.push(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::BlockDevice,
                source,
            }
        })?);
    }

    let serial = if serial_configured {
        Some(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::SerialDevice,
                source,
            }
        })?)
    } else {
        None
    };

    Ok(HvfArm64BootInterruptLines { block, serial })
}

#[cfg(test)]
mod tests {
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::serial::SharedSerialOutputBuffer;

    use super::{
        HvfArm64BootInterruptLinePurpose, HvfArm64BootSerialDeviceConfig,
        HvfArm64BootSessionConfig, HvfArm64BootSessionError, allocate_interrupt_lines,
        validate_single_vcpu,
    };
    use crate::gic::{HvfGicInterruptRange, HvfGicMetadata, HvfGicRedistributor, HvfGicRegion};

    fn gic_with_spi_range(base: u32, count: u32) -> HvfGicMetadata {
        HvfGicMetadata {
            distributor: HvfGicRegion {
                base: 0x3ffe_0000,
                size: 0x1_0000,
            },
            redistributor: HvfGicRedistributor {
                region: HvfGicRegion {
                    base: 0x3ffc_0000,
                    size: 0x2_0000,
                },
                single_redistributor_size: 0x2_0000,
            },
            spi_interrupt_range: HvfGicInterruptRange { base, count },
            timer_interrupts: crate::gic::HvfGicTimerInterrupts {
                el1_virtual_timer_intid: 27,
                el1_physical_timer_intid: 30,
            },
            msi: None,
        }
    }

    fn controller_with_vcpus(vcpu_count: u8) -> bangbang_runtime::VmmController {
        let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(
                vcpu_count, 128,
            )))
            .expect("machine config should be stored");
        controller
    }

    fn line_values(lines: &[bangbang_runtime::interrupt::GuestInterruptLine]) -> Vec<u32> {
        lines.iter().map(|line| line.raw_value()).collect()
    }

    #[test]
    fn session_config_stores_serial_device() {
        let serial = HvfArm64BootSerialDeviceConfig::new(
            MmioRegionId::new(7),
            GuestAddress::new(0x4000_0000),
            SharedSerialOutputBuffer::default(),
        );

        let config = HvfArm64BootSessionConfig::new(BlockMmioLayout::new(
            GuestAddress::new(0x5000_0000),
            MmioRegionId::new(1),
        ))
        .with_serial_device(serial);

        assert!(config.serial_device.is_some());
    }

    #[test]
    fn single_vcpu_validation_accepts_default_controller() {
        let controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");

        assert!(validate_single_vcpu(&controller).is_ok());
    }

    #[test]
    fn single_vcpu_validation_rejects_multi_vcpu_controller() {
        let controller = controller_with_vcpus(2);

        assert!(matches!(
            validate_single_vcpu(&controller),
            Err(HvfArm64BootSessionError::UnsupportedVcpuCount { vcpu_count: 2 })
        ));
    }

    #[test]
    fn interrupt_lines_allocate_blocks_before_serial() {
        let lines = allocate_interrupt_lines(&gic_with_spi_range(32, 4), 2, true)
            .expect("interrupt lines should allocate");

        assert_eq!(line_values(&lines.block), vec![32, 33]);
        assert_eq!(lines.serial.map(|line| line.raw_value()), Some(34));
    }

    #[test]
    fn interrupt_lines_allocate_none_for_absent_serial() {
        let lines = allocate_interrupt_lines(&gic_with_spi_range(40, 2), 2, false)
            .expect("interrupt lines should allocate");

        assert_eq!(line_values(&lines.block), vec![40, 41]);
        assert_eq!(lines.serial, None);
    }

    #[test]
    fn interrupt_lines_report_serial_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(&gic_with_spi_range(32, 1), 1, true)
            .expect_err("serial allocation should exhaust range");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::SerialDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_reject_invalid_gic_range() {
        let err = allocate_interrupt_lines(&gic_with_spi_range(31, 1), 0, false)
            .expect_err("invalid SPI range should fail");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::BlockDevice,
                ..
            }
        ));
    }

    #[test]
    fn instance_start_remains_unsupported() {
        let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
                "/tmp/vmlinux",
            )))
            .expect("boot source config should be stored");

        let err = controller
            .handle_action(VmmAction::InstanceStart)
            .expect_err("instance start must remain unsupported");

        assert_eq!(
            err.to_string(),
            "The requested operation is not supported: InstanceStart"
        );
    }
}
