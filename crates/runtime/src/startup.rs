//! Internal assembly of boot resources from validated VM configuration.

use std::collections::TryReserveError;
use std::fmt;

use crate::VmmController;
use crate::block::{
    BlockMmioDeviceRegistration, BlockMmioLayout, BlockMmioRegistrationError,
    PreparedBlockDeviceError, PreparedBlockDevices,
};
use crate::boot::{BootSource, BootSourceConfig, BootSourceLoadError, LoadedBootSource};
use crate::fdt::{
    Arm64FdtBootInfo, Arm64FdtConfig, Arm64FdtError, Arm64FdtGic, Arm64FdtGuestMemoryWrite,
    Arm64FdtRegion, Arm64FdtTimerInterrupts, Arm64FdtVirtioMmioDevice, write_arm64_fdt,
};
use crate::interrupt::GuestInterruptLine;
use crate::machine::MachineConfig;
use crate::memory::{
    GuestMemory, GuestMemoryAllocationError, GuestMemoryError, GuestMemoryLayout, aarch64,
};
use crate::mmio::MmioDispatcher;

const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64BootResourceConfig<'a> {
    pub vcpu_mpidrs: &'a [u64],
    pub gic: Arm64FdtGic,
    pub timer: Arm64FdtTimerInterrupts,
    pub block_mmio_layout: BlockMmioLayout,
    pub block_interrupt_lines: &'a [GuestInterruptLine],
}

#[derive(Debug)]
pub struct Arm64BootResources {
    pub machine_config: MachineConfig,
    pub layout: GuestMemoryLayout,
    pub memory: GuestMemory,
    pub loaded_boot_source: LoadedBootSource,
    pub fdt: Arm64FdtGuestMemoryWrite,
    pub mmio_dispatcher: MmioDispatcher,
    pub block_devices: Vec<Arm64BootBlockDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm64BootBlockDevice {
    pub registration: BlockMmioDeviceRegistration,
    pub fdt_device: Arm64FdtVirtioMmioDevice,
}

#[derive(Debug)]
pub enum Arm64BootResourceError {
    MissingBootSource,
    MemorySizeOverflow {
        mem_size_mib: u64,
    },
    MemorySizeExceedsArchitecturalMaximum {
        requested_size: u64,
        max_size: u64,
    },
    MemoryLayout {
        source: GuestMemoryError,
    },
    GuestMemoryAllocation {
        source: GuestMemoryAllocationError,
    },
    BootSourceLoad {
        source: BootSourceLoadError,
    },
    PrepareBlockDevices {
        source: PreparedBlockDeviceError,
    },
    RegisterBlockMmio {
        source: Box<BlockMmioRegistrationError>,
    },
    BlockInterruptLineCount {
        devices: usize,
        lines: usize,
    },
    BlockDeviceMetadataAllocation {
        source: TryReserveError,
    },
    Fdt {
        source: Arm64FdtError,
    },
}

impl fmt::Display for Arm64BootResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingBootSource => f.write_str("boot source must be configured before startup"),
            Self::MemorySizeOverflow { mem_size_mib } => {
                write!(f, "machine mem_size_mib {mem_size_mib} overflows bytes")
            }
            Self::MemorySizeExceedsArchitecturalMaximum {
                requested_size,
                max_size,
            } => write!(
                f,
                "machine memory size {requested_size} exceeds arm64 maximum {max_size}"
            ),
            Self::MemoryLayout { source } => {
                write!(f, "failed to build guest memory layout: {source}")
            }
            Self::GuestMemoryAllocation { source } => {
                write!(f, "failed to allocate guest memory: {source}")
            }
            Self::BootSourceLoad { source } => {
                write!(f, "failed to load boot source: {source}")
            }
            Self::PrepareBlockDevices { source } => {
                write!(f, "failed to prepare block devices: {source}")
            }
            Self::RegisterBlockMmio { source } => {
                write!(f, "failed to register block MMIO devices: {source}")
            }
            Self::BlockInterruptLineCount { devices, lines } => write!(
                f,
                "block MMIO device count {devices} does not match interrupt line count {lines}"
            ),
            Self::BlockDeviceMetadataAllocation { source } => {
                write!(f, "failed to allocate block device metadata: {source}")
            }
            Self::Fdt { source } => write!(f, "failed to write arm64 FDT: {source}"),
        }
    }
}

impl std::error::Error for Arm64BootResourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MemoryLayout { source } => Some(source),
            Self::GuestMemoryAllocation { source } => Some(source),
            Self::BootSourceLoad { source } => Some(source),
            Self::PrepareBlockDevices { source } => Some(source),
            Self::RegisterBlockMmio { source } => Some(source.as_ref()),
            Self::BlockDeviceMetadataAllocation { source } => Some(source),
            Self::Fdt { source } => Some(source),
            Self::MissingBootSource
            | Self::MemorySizeOverflow { .. }
            | Self::MemorySizeExceedsArchitecturalMaximum { .. }
            | Self::BlockInterruptLineCount { .. } => None,
        }
    }
}

impl Arm64BootResources {
    pub fn assemble_from_controller(
        controller: &VmmController,
        config: Arm64BootResourceConfig<'_>,
    ) -> Result<Self, Arm64BootResourceError> {
        let boot_source_config = controller
            .boot_source_config()
            .ok_or(Arm64BootResourceError::MissingBootSource)?;
        validate_block_interrupt_line_count(
            controller.drive_configs().len(),
            config.block_interrupt_lines.len(),
        )?;

        let machine_config = controller.machine_config();
        let memory_size = memory_size_bytes(machine_config)?;
        let layout = aarch64::dram_layout(memory_size)
            .map_err(|source| Arm64BootResourceError::MemoryLayout { source })?;
        let mut memory = GuestMemory::allocate(&layout)
            .map_err(|source| Arm64BootResourceError::GuestMemoryAllocation { source })?;
        let boot_source = boot_source_from_config(boot_source_config);
        let loaded_boot_source = boot_source
            .load(&layout, &mut memory)
            .map_err(|source| Arm64BootResourceError::BootSourceLoad { source })?;

        let prepared_blocks =
            PreparedBlockDevices::from_config_slice(controller.drive_configs())
                .map_err(|source| Arm64BootResourceError::PrepareBlockDevices { source })?;
        let block_mmio = prepared_blocks
            .register_mmio(config.block_mmio_layout)
            .map_err(|source| Arm64BootResourceError::RegisterBlockMmio {
                source: Box::new(source),
            })?;
        let (mmio_dispatcher, registrations) = block_mmio.into_parts();
        let (block_devices, fdt_devices) =
            block_device_metadata(&registrations, config.block_interrupt_lines)?;
        let fdt = write_arm64_fdt(
            &Arm64FdtConfig {
                layout: &layout,
                boot: Arm64FdtBootInfo::from(&loaded_boot_source),
                vcpu_mpidrs: config.vcpu_mpidrs,
                gic: config.gic,
                timer: config.timer,
                virtio_mmio_devices: &fdt_devices,
            },
            &mut memory,
        )
        .map_err(|source| Arm64BootResourceError::Fdt { source })?;

        Ok(Self {
            machine_config,
            layout,
            memory,
            loaded_boot_source,
            fdt,
            mmio_dispatcher,
            block_devices,
        })
    }
}

fn memory_size_bytes(config: MachineConfig) -> Result<u64, Arm64BootResourceError> {
    let memory_size = config.mem_size_mib().checked_mul(MIB).ok_or(
        Arm64BootResourceError::MemorySizeOverflow {
            mem_size_mib: config.mem_size_mib(),
        },
    )?;
    if memory_size > aarch64::DRAM_MEM_MAX_SIZE {
        return Err(
            Arm64BootResourceError::MemorySizeExceedsArchitecturalMaximum {
                requested_size: memory_size,
                max_size: aarch64::DRAM_MEM_MAX_SIZE,
            },
        );
    }
    Ok(memory_size)
}

fn boot_source_from_config(config: &BootSourceConfig) -> BootSource {
    let mut source = BootSource::new(config.kernel_image_path().to_path_buf());
    if let Some(initrd_path) = config.initrd_path() {
        source = source.with_initrd_path(initrd_path.to_path_buf());
    }
    if let Some(boot_args) = config.boot_args() {
        source = source.with_boot_args(boot_args.to_string());
    }
    source
}

fn validate_block_interrupt_line_count(
    devices: usize,
    lines: usize,
) -> Result<(), Arm64BootResourceError> {
    if devices == lines {
        Ok(())
    } else {
        Err(Arm64BootResourceError::BlockInterruptLineCount { devices, lines })
    }
}

fn block_device_metadata(
    registrations: &[BlockMmioDeviceRegistration],
    interrupt_lines: &[GuestInterruptLine],
) -> Result<(Vec<Arm64BootBlockDevice>, Vec<Arm64FdtVirtioMmioDevice>), Arm64BootResourceError> {
    validate_block_interrupt_line_count(registrations.len(), interrupt_lines.len())?;

    let mut block_devices = Vec::new();
    block_devices
        .try_reserve_exact(registrations.len())
        .map_err(|source| Arm64BootResourceError::BlockDeviceMetadataAllocation { source })?;
    let mut fdt_devices = Vec::new();
    fdt_devices
        .try_reserve_exact(registrations.len())
        .map_err(|source| Arm64BootResourceError::BlockDeviceMetadataAllocation { source })?;

    for (registration, interrupt_line) in registrations.iter().zip(interrupt_lines) {
        let range = registration.region().range();
        let fdt_device = Arm64FdtVirtioMmioDevice {
            region: Arm64FdtRegion {
                base: range.start().raw_value(),
                size: range.size(),
            },
            interrupt_line: *interrupt_line,
        };
        block_devices.push(Arm64BootBlockDevice {
            registration: registration.clone(),
            fdt_device,
        });
        fdt_devices.push(fdt_device);
    }

    Ok((block_devices, fdt_devices))
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        Arm64BootResourceConfig, Arm64BootResourceError, Arm64BootResources, MIB,
        block_device_metadata,
    };
    use crate::VmmAction;
    use crate::block::DriveConfigInput;
    use crate::boot::{BootPayloadKind, BootSourceConfigInput, BootSourceLoadError};
    use crate::fdt::{Arm64FdtError, Arm64FdtGic, Arm64FdtRegion, Arm64FdtTimerInterrupts};
    use crate::interrupt::GuestInterruptLine;
    use crate::machine::MachineConfigInput;
    use crate::memory::{GuestAddress, aarch64};
    use crate::mmio::MmioRegionId;

    static NEXT_TEST_FILE_ID: AtomicU64 = AtomicU64::new(0);

    const TEST_MEMORY_MIB: u64 = 8;
    const ARM64_IMAGE_HEADER_SIZE: usize = 64;
    const ARM64_IMAGE_TEXT_OFFSET_OFFSET: usize = 8;
    const ARM64_IMAGE_SIZE_OFFSET: usize = 16;
    const ARM64_IMAGE_MAGIC_OFFSET: usize = 56;
    const ARM64_IMAGE_MAGIC: u32 = 0x644d_5241;
    const TEST_BLOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_0000);

    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let id = NEXT_TEST_FILE_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "bangbang-startup-{name}-{}-{id}",
            std::process::id()
        ))
    }

    fn temp_file(name: &str, bytes: &[u8]) -> TempFile {
        let path = temp_path(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("test file should be created");
        file.write_all(bytes)
            .expect("test file bytes should be written");
        TempFile { path }
    }

    fn missing_path(name: &str) -> PathBuf {
        temp_path(name)
    }

    fn arm64_image() -> Vec<u8> {
        let mut bytes = vec![0xaa; ARM64_IMAGE_HEADER_SIZE];
        write_u64_le(&mut bytes, ARM64_IMAGE_TEXT_OFFSET_OFFSET, 0);
        write_u64_le(
            &mut bytes,
            ARM64_IMAGE_SIZE_OFFSET,
            ARM64_IMAGE_HEADER_SIZE as u64,
        );
        write_u32_le(&mut bytes, ARM64_IMAGE_MAGIC_OFFSET, ARM64_IMAGE_MAGIC);
        bytes
    }

    fn write_u64_le(bytes: &mut [u8], offset: usize, value: u64) {
        let end = offset + std::mem::size_of::<u64>();
        bytes[offset..end].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32_le(bytes: &mut [u8], offset: usize, value: u32) {
        let end = offset + std::mem::size_of::<u32>();
        bytes[offset..end].copy_from_slice(&value.to_le_bytes());
    }

    fn controller_with_kernel(kernel: &Path) -> crate::VmmController {
        controller_with_kernel_and_memory(kernel, TEST_MEMORY_MIB)
    }

    fn controller_with_kernel_and_memory(kernel: &Path, mem_size_mib: u64) -> crate::VmmController {
        let mut controller = crate::VmmController::new("test", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(
                1,
                mem_size_mib,
            )))
            .expect("machine config should be stored");
        controller
            .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
                kernel.to_path_buf(),
            )))
            .expect("boot source should be stored");
        controller
    }

    fn add_drive(controller: &mut crate::VmmController, id: &str, path: &Path) {
        controller
            .handle_action(VmmAction::PutDrive(DriveConfigInput::new(
                id,
                id,
                path.to_path_buf(),
                true,
            )))
            .expect("drive config should be stored");
    }

    fn valid_config(lines: &[GuestInterruptLine]) -> Arm64BootResourceConfig<'_> {
        Arm64BootResourceConfig {
            vcpu_mpidrs: &[0],
            gic: valid_gic(),
            timer: Arm64FdtTimerInterrupts::firecracker_default(),
            block_mmio_layout: crate::block::BlockMmioLayout::new(
                TEST_BLOCK_MMIO_BASE,
                MmioRegionId::new(1),
            ),
            block_interrupt_lines: lines,
        }
    }

    fn valid_gic() -> Arm64FdtGic {
        Arm64FdtGic {
            distributor: Arm64FdtRegion {
                base: 0x3ffc_0000,
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

    fn line(value: u32) -> GuestInterruptLine {
        GuestInterruptLine::new(value).expect("test interrupt line should be valid")
    }

    #[test]
    fn assembles_boot_resources_without_drives() {
        let kernel = temp_file("kernel", &arm64_image());
        let controller = controller_with_kernel(kernel.path());

        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
                .expect("boot resources should assemble");

        assert_eq!(resources.machine_config.mem_size_mib(), TEST_MEMORY_MIB);
        assert_eq!(resources.layout.total_size(), TEST_MEMORY_MIB * MIB);
        assert_eq!(
            resources.loaded_boot_source.kernel.entry_address,
            aarch64::kernel_load_address()
        );
        assert_eq!(
            resources.fdt.address,
            aarch64::fdt_address(&resources.layout).expect("FDT address should be valid")
        );
        assert!(resources.block_devices.is_empty());
        assert!(resources.mmio_dispatcher.regions().is_empty());
    }

    #[test]
    fn assembles_boot_resources_with_block_device_mmio_metadata() {
        let kernel = temp_file("kernel-with-block", &arm64_image());
        let block = temp_file("block", &[0x5a; 512]);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", block.path());
        let lines = [line(32)];

        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&lines))
                .expect("boot resources should assemble with block device");

        assert_eq!(resources.block_devices.len(), 1);
        assert_eq!(resources.block_devices[0].registration.drive_id(), "rootfs");
        assert_eq!(
            resources.block_devices[0].registration.address(),
            TEST_BLOCK_MMIO_BASE
        );
        assert_eq!(
            resources.block_devices[0].fdt_device.region.base,
            TEST_BLOCK_MMIO_BASE.raw_value()
        );
        assert_eq!(
            resources.block_devices[0].fdt_device.interrupt_line,
            line(32)
        );
        assert_eq!(resources.mmio_dispatcher.regions().len(), 1);
    }

    #[test]
    fn missing_boot_source_fails_before_block_preparation() {
        let mut controller = crate::VmmController::new("test", "0.1.0", "bangbang");
        add_drive(&mut controller, "rootfs", &missing_path("missing-block"));
        let lines = [line(32)];

        let err = Arm64BootResources::assemble_from_controller(&controller, valid_config(&lines))
            .expect_err("missing boot source should fail");

        assert!(matches!(err, Arm64BootResourceError::MissingBootSource));
    }

    #[test]
    fn missing_kernel_file_surfaces_boot_source_load_error() {
        let controller = controller_with_kernel(&missing_path("missing-kernel"));

        let err = Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
            .expect_err("missing kernel should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::BootSourceLoad {
                source: BootSourceLoadError::OpenFile {
                    payload: BootPayloadKind::Kernel,
                    ..
                }
            }
        ));
    }

    #[test]
    fn oversized_memory_fails_before_boot_source_load() {
        let mem_size_mib = aarch64::DRAM_MEM_MAX_SIZE / MIB + 1;
        let controller = controller_with_kernel_and_memory(
            &missing_path("oversized-memory-kernel"),
            mem_size_mib,
        );

        let err = Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
            .expect_err("oversized memory should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::MemorySizeExceedsArchitecturalMaximum {
                requested_size,
                max_size: aarch64::DRAM_MEM_MAX_SIZE
            } if requested_size == mem_size_mib * MIB
        ));
    }

    #[test]
    fn missing_block_file_surfaces_block_preparation_error() {
        let kernel = temp_file("kernel-bad-block", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", &missing_path("missing-drive"));
        let lines = [line(32)];

        let err = Arm64BootResources::assemble_from_controller(&controller, valid_config(&lines))
            .expect_err("missing block backing should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::PrepareBlockDevices { .. }
        ));
    }

    #[test]
    fn interrupt_line_count_mismatch_fails_before_block_preparation() {
        let kernel = temp_file("kernel-line-mismatch", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(
            &mut controller,
            "rootfs",
            &missing_path("line-mismatch-drive"),
        );

        let err = Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
            .expect_err("line mismatch should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::BlockInterruptLineCount {
                devices: 1,
                lines: 0
            }
        ));
    }

    #[test]
    fn block_metadata_rejects_registration_line_mismatch() {
        let lines = [line(32)];

        let err = block_device_metadata(&[], &lines).expect_err("line mismatch should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::BlockInterruptLineCount {
                devices: 0,
                lines: 1
            }
        ));
    }

    #[test]
    fn invalid_fdt_input_surfaces_fdt_error() {
        let kernel = temp_file("kernel-bad-fdt", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let config = Arm64BootResourceConfig {
            vcpu_mpidrs: &[],
            ..valid_config(&[])
        };

        let err = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect_err("invalid FDT input should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::Fdt {
                source: Arm64FdtError::MissingCpu
            }
        ));
    }

    #[test]
    fn assembled_resources_are_independent() {
        let kernel = temp_file("kernel-independent", &arm64_image());
        let first_block = temp_file("block-independent-1", &[0x11; 512]);
        let second_block = temp_file("block-independent-2", &[0x22; 512]);
        let mut first_controller = controller_with_kernel(kernel.path());
        let mut second_controller = controller_with_kernel(kernel.path());
        add_drive(&mut first_controller, "first", first_block.path());
        add_drive(&mut second_controller, "second", second_block.path());
        let first_lines = [line(32)];
        let second_lines = [line(33)];

        let first = Arm64BootResources::assemble_from_controller(
            &first_controller,
            valid_config(&first_lines),
        )
        .expect("first resources should assemble");
        let second = Arm64BootResources::assemble_from_controller(
            &second_controller,
            valid_config(&second_lines),
        )
        .expect("second resources should assemble");

        assert_ne!(
            first.memory.regions()[0].host_address(),
            second.memory.regions()[0].host_address()
        );
        assert_eq!(first.block_devices[0].registration.drive_id(), "first");
        assert_eq!(second.block_devices[0].registration.drive_id(), "second");
        assert_eq!(first.block_devices[0].fdt_device.interrupt_line, line(32));
        assert_eq!(second.block_devices[0].fdt_device.interrupt_line, line(33));
    }
}
