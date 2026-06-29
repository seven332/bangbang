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
    Arm64FdtRegion, Arm64FdtSerialDevice, Arm64FdtTimerInterrupts, Arm64FdtVirtioMmioDevice,
    write_arm64_fdt,
};
use crate::interrupt::GuestInterruptLine;
use crate::machine::MachineConfig;
use crate::memory::{
    GuestMemory, GuestMemoryAllocationError, GuestMemoryError, GuestMemoryLayout, aarch64,
};
use crate::mmio::{MmioBusError, MmioDispatchError, MmioDispatcher, MmioRegion, MmioRegionId};
use crate::serial::{SERIAL_MMIO_DEVICE_WINDOW_SIZE, SerialMmioDevice, SharedSerialOutputBuffer};

const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Arm64BootResourceConfig<'a> {
    pub vcpu_mpidrs: &'a [u64],
    pub gic: Arm64FdtGic,
    pub timer: Arm64FdtTimerInterrupts,
    pub serial_device: Option<Arm64BootSerialDeviceConfig>,
    pub block_mmio_layout: BlockMmioLayout,
    pub block_interrupt_lines: &'a [GuestInterruptLine],
}

#[derive(Debug, Clone)]
pub struct Arm64BootSerialDeviceConfig {
    pub region_id: MmioRegionId,
    pub address: crate::memory::GuestAddress,
    pub interrupt_line: GuestInterruptLine,
    pub output: SharedSerialOutputBuffer,
}

impl Arm64BootSerialDeviceConfig {
    pub fn new(
        region_id: MmioRegionId,
        address: crate::memory::GuestAddress,
        interrupt_line: GuestInterruptLine,
        output: SharedSerialOutputBuffer,
    ) -> Self {
        Self {
            region_id,
            address,
            interrupt_line,
            output,
        }
    }
}

#[derive(Debug)]
pub struct Arm64BootResources {
    pub machine_config: MachineConfig,
    pub layout: GuestMemoryLayout,
    pub memory: GuestMemory,
    pub loaded_boot_source: LoadedBootSource,
    pub fdt: Arm64FdtGuestMemoryWrite,
    pub mmio_dispatcher: MmioDispatcher,
    pub serial_device: Option<Arm64BootSerialDevice>,
    pub block_devices: Vec<Arm64BootBlockDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm64BootBlockDevice {
    pub registration: BlockMmioDeviceRegistration,
    pub fdt_device: Arm64FdtVirtioMmioDevice,
}

#[derive(Debug, Clone)]
pub struct Arm64BootSerialDevice {
    pub region: MmioRegion,
    pub output: SharedSerialOutputBuffer,
    pub fdt_device: Arm64FdtSerialDevice,
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
    RegisterSerialMmio {
        source: Box<Arm64BootSerialMmioRegistrationError>,
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
            Self::RegisterSerialMmio { source } => {
                write!(f, "failed to register serial MMIO device: {source}")
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
            Self::RegisterSerialMmio { source } => Some(source.as_ref()),
            Self::BlockDeviceMetadataAllocation { source } => Some(source),
            Self::Fdt { source } => Some(source),
            Self::MissingBootSource
            | Self::MemorySizeOverflow { .. }
            | Self::MemorySizeExceedsArchitecturalMaximum { .. }
            | Self::BlockInterruptLineCount { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum Arm64BootSerialMmioRegistrationError {
    InsertRegion {
        region_id: MmioRegionId,
        address: crate::memory::GuestAddress,
        source: MmioBusError,
    },
    RegisterHandler {
        region_id: MmioRegionId,
        source: MmioDispatchError,
    },
}

impl fmt::Display for Arm64BootSerialMmioRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InsertRegion {
                region_id,
                address,
                source,
            } => write!(
                f,
                "failed to insert serial MMIO region id={region_id} at {address}: {source}"
            ),
            Self::RegisterHandler { region_id, source } => write!(
                f,
                "failed to register serial MMIO handler for region id={region_id}: {source}"
            ),
        }
    }
}

impl std::error::Error for Arm64BootSerialMmioRegistrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InsertRegion { source, .. } => Some(source),
            Self::RegisterHandler { source, .. } => Some(source),
        }
    }
}

impl Arm64BootResources {
    pub fn assemble_from_controller(
        controller: &VmmController,
        config: Arm64BootResourceConfig<'_>,
    ) -> Result<Self, Arm64BootResourceError> {
        let Arm64BootResourceConfig {
            vcpu_mpidrs,
            gic,
            timer,
            serial_device,
            block_mmio_layout,
            block_interrupt_lines,
        } = config;
        let boot_source_config = controller
            .boot_source_config()
            .ok_or(Arm64BootResourceError::MissingBootSource)?;
        validate_block_interrupt_line_count(
            controller.drive_configs().len(),
            block_interrupt_lines.len(),
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
            .register_mmio(block_mmio_layout)
            .map_err(|source| Arm64BootResourceError::RegisterBlockMmio {
                source: Box::new(source),
            })?;
        let (mut mmio_dispatcher, registrations) = block_mmio.into_parts();
        let (block_devices, fdt_devices) =
            block_device_metadata(&registrations, block_interrupt_lines)?;
        let serial_device = serial_device
            .map(|serial| register_serial_mmio(&mut mmio_dispatcher, serial))
            .transpose()?;
        let serial_fdt_device = serial_device.as_ref().map(|device| device.fdt_device);
        let fdt = write_arm64_fdt(
            &Arm64FdtConfig {
                layout: &layout,
                boot: Arm64FdtBootInfo::from(&loaded_boot_source),
                vcpu_mpidrs,
                gic,
                timer,
                serial_device: serial_fdt_device,
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
            serial_device,
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

fn register_serial_mmio(
    dispatcher: &mut MmioDispatcher,
    config: Arm64BootSerialDeviceConfig,
) -> Result<Arm64BootSerialDevice, Arm64BootResourceError> {
    let region = dispatcher
        .insert_region(
            config.region_id,
            config.address,
            SERIAL_MMIO_DEVICE_WINDOW_SIZE,
        )
        .map_err(|source| Arm64BootResourceError::RegisterSerialMmio {
            source: Box::new(Arm64BootSerialMmioRegistrationError::InsertRegion {
                region_id: config.region_id,
                address: config.address,
                source,
            }),
        })?;

    dispatcher
        .register_handler(
            config.region_id,
            SerialMmioDevice::new(config.output.clone()),
        )
        .map_err(|source| Arm64BootResourceError::RegisterSerialMmio {
            source: Box::new(Arm64BootSerialMmioRegistrationError::RegisterHandler {
                region_id: config.region_id,
                source,
            }),
        })?;

    let fdt_device = Arm64FdtSerialDevice {
        region: Arm64FdtRegion {
            base: region.range().start().raw_value(),
            size: region.range().size(),
        },
        interrupt_line: config.interrupt_line,
    };

    Ok(Arm64BootSerialDevice {
        region,
        output: config.output,
        fdt_device,
    })
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use device_tree::DeviceTree;

    use super::{
        Arm64BootResourceConfig, Arm64BootResourceError, Arm64BootResources,
        Arm64BootSerialDeviceConfig, Arm64BootSerialMmioRegistrationError, MIB,
        block_device_metadata,
    };
    use crate::VmmAction;
    use crate::block::DriveConfigInput;
    use crate::boot::{BootPayloadKind, BootSourceConfigInput, BootSourceLoadError};
    use crate::fdt::{Arm64FdtError, Arm64FdtGic, Arm64FdtRegion, Arm64FdtTimerInterrupts};
    use crate::interrupt::GuestInterruptLine;
    use crate::machine::MachineConfigInput;
    use crate::memory::{GuestAddress, aarch64};
    use crate::mmio::{
        MmioAccessBytes, MmioBusError, MmioDispatchOutcome, MmioOperation, MmioRegionId,
    };
    use crate::serial::{
        SERIAL_MMIO_DEVICE_WINDOW_SIZE, SERIAL_TRANSMIT_REGISTER_OFFSET, SharedSerialOutputBuffer,
    };

    static NEXT_TEST_FILE_ID: AtomicU64 = AtomicU64::new(0);

    const TEST_MEMORY_MIB: u64 = 8;
    const ARM64_IMAGE_HEADER_SIZE: usize = 64;
    const ARM64_IMAGE_TEXT_OFFSET_OFFSET: usize = 8;
    const ARM64_IMAGE_SIZE_OFFSET: usize = 16;
    const ARM64_IMAGE_MAGIC_OFFSET: usize = 56;
    const ARM64_IMAGE_MAGIC: u32 = 0x644d_5241;
    const TEST_BLOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_0000);
    const TEST_SERIAL_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_2000);

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
            serial_device: None,
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

    fn serial_config(
        address: GuestAddress,
        region_id: MmioRegionId,
        interrupt_line: GuestInterruptLine,
    ) -> (Arm64BootSerialDeviceConfig, SharedSerialOutputBuffer) {
        let output = SharedSerialOutputBuffer::default();
        (
            Arm64BootSerialDeviceConfig::new(region_id, address, interrupt_line, output.clone()),
            output,
        )
    }

    fn write_serial_byte(resources: &mut Arm64BootResources, address: GuestAddress, value: u8) {
        let access = resources
            .mmio_dispatcher
            .lookup(address, 1)
            .expect("serial access should resolve");
        let data = MmioAccessBytes::new(&[value]).expect("serial write byte should build");
        let operation =
            MmioOperation::write(access, data).expect("serial write operation should build");
        let outcome = resources
            .mmio_dispatcher
            .dispatch(operation)
            .expect("serial write should dispatch");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
    }

    fn read_fdt(resources: &Arm64BootResources) -> DeviceTree {
        let mut bytes = vec![0; resources.fdt.size];
        resources
            .memory
            .read_slice(&mut bytes, resources.fdt.address)
            .expect("FDT bytes should read back");

        DeviceTree::load(&bytes).expect("assembled FDT should parse")
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
        assert!(resources.serial_device.is_none());
        assert!(resources.mmio_dispatcher.regions().is_empty());
        assert!(read_fdt(&resources).find("/uart@40002000").is_none());
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
    fn assembles_boot_resources_with_serial_mmio_metadata() {
        let kernel = temp_file("kernel-with-serial", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let (serial, output) = serial_config(TEST_SERIAL_MMIO_BASE, MmioRegionId::new(9), line(32));
        let config = Arm64BootResourceConfig {
            serial_device: Some(serial),
            ..valid_config(&[])
        };

        let mut resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble with serial device");

        let serial = resources
            .serial_device
            .as_ref()
            .expect("serial metadata should be returned");
        assert_eq!(serial.region.id(), MmioRegionId::new(9));
        assert_eq!(serial.region.range().start(), TEST_SERIAL_MMIO_BASE);
        assert_eq!(serial.region.range().size(), SERIAL_MMIO_DEVICE_WINDOW_SIZE);
        assert_eq!(
            serial.fdt_device.region.base,
            TEST_SERIAL_MMIO_BASE.raw_value()
        );
        assert_eq!(
            serial.fdt_device.region.size,
            SERIAL_MMIO_DEVICE_WINDOW_SIZE
        );
        assert_eq!(serial.fdt_device.interrupt_line, line(32));
        assert_eq!(resources.mmio_dispatcher.regions().len(), 1);
        assert_eq!(
            resources.mmio_dispatcher.regions()[0].range().start(),
            TEST_SERIAL_MMIO_BASE
        );

        write_serial_byte(
            &mut resources,
            TEST_SERIAL_MMIO_BASE
                .checked_add(SERIAL_TRANSMIT_REGISTER_OFFSET)
                .expect("serial TX address should not overflow"),
            b'B',
        );
        assert_eq!(output.bytes().expect("serial output should read"), b"B");

        let tree = read_fdt(&resources);
        let serial_node = tree
            .find("/uart@40002000")
            .expect("serial node should be in assembled FDT");
        assert_eq!(serial_node.prop_str("compatible").unwrap(), "ns16550a");
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
    fn serial_region_overlapping_block_mmio_fails_during_registration() {
        let kernel = temp_file("kernel-serial-overlap-block", &arm64_image());
        let block = temp_file("block-serial-overlap", &[0x5a; 512]);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", block.path());
        let lines = [line(33)];
        let (serial, _output) = serial_config(TEST_BLOCK_MMIO_BASE, MmioRegionId::new(9), line(32));
        let config = Arm64BootResourceConfig {
            serial_device: Some(serial),
            ..valid_config(&lines)
        };

        let err = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect_err("overlapping serial MMIO should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::RegisterSerialMmio { source }
                if matches!(
                    source.as_ref(),
                    Arm64BootSerialMmioRegistrationError::InsertRegion {
                        source: MmioBusError::OverlappingRegion { .. },
                        ..
                    }
                )
        ));
    }

    #[test]
    fn serial_region_id_matching_block_fails_during_handler_registration() {
        let kernel = temp_file("kernel-serial-duplicate-region", &arm64_image());
        let block = temp_file("block-serial-duplicate-region", &[0x5a; 512]);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", block.path());
        let lines = [line(33)];
        let (serial, _output) =
            serial_config(TEST_SERIAL_MMIO_BASE, MmioRegionId::new(1), line(32));
        let config = Arm64BootResourceConfig {
            serial_device: Some(serial),
            ..valid_config(&lines)
        };

        let err = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect_err("duplicate serial handler region id should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::RegisterSerialMmio { source }
                if matches!(
                    source.as_ref(),
                    Arm64BootSerialMmioRegistrationError::RegisterHandler {
                        source: crate::mmio::MmioDispatchError::DuplicateHandler {
                            region_id
                        },
                        ..
                    } if *region_id == MmioRegionId::new(1)
                )
        ));
    }

    #[test]
    fn serial_region_overlapping_guest_memory_fails_during_fdt_write() {
        let kernel = temp_file("kernel-serial-overlap-memory", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let (serial, _output) = serial_config(
            GuestAddress::new(aarch64::DRAM_MEM_START),
            MmioRegionId::new(9),
            line(32),
        );
        let config = Arm64BootResourceConfig {
            serial_device: Some(serial),
            ..valid_config(&[])
        };

        let err = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect_err("serial overlapping guest memory should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::Fdt {
                source: Arm64FdtError::SerialRegionOverlapsMemory { .. }
            }
        ));
    }

    #[test]
    fn serial_region_overlapping_gic_fails_during_fdt_write() {
        let kernel = temp_file("kernel-serial-overlap-gic", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let (serial, _output) = serial_config(
            GuestAddress::new(0x3ffc_0000),
            MmioRegionId::new(9),
            line(32),
        );
        let config = Arm64BootResourceConfig {
            serial_device: Some(serial),
            ..valid_config(&[])
        };

        let err = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect_err("serial overlapping GIC should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::Fdt {
                source: Arm64FdtError::SerialRegionOverlapsGic { .. }
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
        let (first_serial, first_output) =
            serial_config(TEST_SERIAL_MMIO_BASE, MmioRegionId::new(9), line(40));
        let (second_serial, second_output) =
            serial_config(TEST_SERIAL_MMIO_BASE, MmioRegionId::new(9), line(41));

        let mut first = Arm64BootResources::assemble_from_controller(
            &first_controller,
            Arm64BootResourceConfig {
                serial_device: Some(first_serial),
                ..valid_config(&first_lines)
            },
        )
        .expect("first resources should assemble");
        let mut second = Arm64BootResources::assemble_from_controller(
            &second_controller,
            Arm64BootResourceConfig {
                serial_device: Some(second_serial),
                ..valid_config(&second_lines)
            },
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
        assert_eq!(
            first
                .serial_device
                .as_ref()
                .expect("first serial metadata should exist")
                .fdt_device
                .interrupt_line,
            line(40)
        );
        assert_eq!(
            second
                .serial_device
                .as_ref()
                .expect("second serial metadata should exist")
                .fdt_device
                .interrupt_line,
            line(41)
        );

        write_serial_byte(&mut first, TEST_SERIAL_MMIO_BASE, b'1');
        write_serial_byte(&mut second, TEST_SERIAL_MMIO_BASE, b'2');

        assert_eq!(
            first_output.bytes().expect("first output should read"),
            b"1"
        );
        assert_eq!(
            second_output.bytes().expect("second output should read"),
            b"2"
        );
    }
}
