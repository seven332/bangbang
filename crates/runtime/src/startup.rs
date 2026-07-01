//! Internal assembly of boot resources from validated VM configuration.

use std::collections::TryReserveError;
use std::fmt;

use crate::VmmController;
use crate::block::{
    BlockMmioDeviceRegistration, BlockMmioLayout, BlockMmioRegistrationError,
    PreparedBlockDeviceError, PreparedBlockDevices, VirtioBlockDeviceNotificationDispatch,
    VirtioBlockDeviceNotificationError, VirtioBlockMmioHandler,
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
use crate::mmio::{
    MmioBusError, MmioDispatchError, MmioDispatcher, MmioHandlerLookupError, MmioRegion,
    MmioRegionId,
};
use crate::network::{
    NetworkMmioDeviceRegistration, NetworkMmioLayout, NetworkMmioRegistrationError,
    PreparedNetworkDeviceError, PreparedNetworkDevices, VirtioNetworkDeviceNotificationDispatch,
    VirtioNetworkDeviceNotificationError, VirtioNetworkMmioHandler, VirtioNetworkRxPacketSource,
    VirtioNetworkTxPacketSink,
};
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
    pub network_mmio_layout: NetworkMmioLayout,
    pub network_interrupt_lines: &'a [GuestInterruptLine],
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
    pub network_devices: Vec<Arm64BootNetworkDevice>,
}

#[derive(Debug)]
pub struct Arm64BootResourceParts {
    pub memory: GuestMemory,
    pub mmio_dispatcher: MmioDispatcher,
    pub runtime: Arm64BootRuntimeResources,
}

#[derive(Debug)]
pub struct Arm64BootRuntimeResources {
    pub machine_config: MachineConfig,
    pub layout: GuestMemoryLayout,
    pub loaded_boot_source: LoadedBootSource,
    pub fdt: Arm64FdtGuestMemoryWrite,
    pub serial_device: Option<Arm64BootSerialDevice>,
    pub block_devices: Vec<Arm64BootBlockDevice>,
    pub network_devices: Vec<Arm64BootNetworkDevice>,
}

#[derive(Debug)]
pub struct Arm64BootBlockNotificationDispatches {
    devices: Vec<Arm64BootBlockNotificationDispatch>,
}

impl Arm64BootBlockNotificationDispatches {
    fn new(devices: Vec<Arm64BootBlockNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[Arm64BootBlockNotificationDispatch] {
        &self.devices
    }

    pub fn into_vec(self) -> Vec<Arm64BootBlockNotificationDispatch> {
        self.devices
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.devices
            .iter()
            .any(Arm64BootBlockNotificationDispatch::needs_queue_interrupt)
    }
}

#[derive(Debug)]
pub struct Arm64BootBlockNotificationDispatch {
    device: Arm64BootBlockDevice,
    outcome: Arm64BootBlockNotificationOutcome,
}

impl Arm64BootBlockNotificationDispatch {
    fn new(device: Arm64BootBlockDevice, outcome: Arm64BootBlockNotificationOutcome) -> Self {
        Self { device, outcome }
    }

    pub const fn device(&self) -> &Arm64BootBlockDevice {
        &self.device
    }

    pub const fn outcome(&self) -> &Arm64BootBlockNotificationOutcome {
        &self.outcome
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.outcome.needs_queue_interrupt()
    }
}

#[derive(Debug)]
pub enum Arm64BootBlockNotificationOutcome {
    Dispatched(VirtioBlockDeviceNotificationDispatch),
    DispatchFailed(VirtioBlockDeviceNotificationError),
    HandlerLookupFailed(MmioHandlerLookupError),
}

impl Arm64BootBlockNotificationOutcome {
    pub fn needs_queue_interrupt(&self) -> bool {
        match self {
            Self::Dispatched(dispatch) => dispatch.needs_queue_interrupt(),
            Self::DispatchFailed(source) => source
                .completed_dispatch()
                .is_some_and(crate::block::VirtioBlockQueueDispatch::needs_queue_interrupt),
            Self::HandlerLookupFailed(_) => false,
        }
    }

    pub const fn dispatched(&self) -> Option<&VirtioBlockDeviceNotificationDispatch> {
        match self {
            Self::Dispatched(dispatch) => Some(dispatch),
            Self::DispatchFailed(_) | Self::HandlerLookupFailed(_) => None,
        }
    }

    pub const fn dispatch_error(&self) -> Option<&VirtioBlockDeviceNotificationError> {
        match self {
            Self::DispatchFailed(source) => Some(source),
            Self::Dispatched(_) | Self::HandlerLookupFailed(_) => None,
        }
    }

    pub const fn handler_lookup_error(&self) -> Option<&MmioHandlerLookupError> {
        match self {
            Self::HandlerLookupFailed(source) => Some(source),
            Self::Dispatched(_) | Self::DispatchFailed(_) => None,
        }
    }
}

#[derive(Debug)]
pub enum Arm64BootBlockNotificationDispatchError {
    ResultAllocation { source: TryReserveError },
}

impl fmt::Display for Arm64BootBlockNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResultAllocation { source } => {
                write!(f, "failed to allocate block notification results: {source}")
            }
        }
    }
}

impl std::error::Error for Arm64BootBlockNotificationDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ResultAllocation { source } => Some(source),
        }
    }
}

#[derive(Debug)]
pub struct Arm64BootNetworkNotificationDispatches {
    devices: Vec<Arm64BootNetworkNotificationDispatch>,
}

impl Arm64BootNetworkNotificationDispatches {
    fn new(devices: Vec<Arm64BootNetworkNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[Arm64BootNetworkNotificationDispatch] {
        &self.devices
    }

    pub fn into_vec(self) -> Vec<Arm64BootNetworkNotificationDispatch> {
        self.devices
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.devices
            .iter()
            .any(Arm64BootNetworkNotificationDispatch::needs_queue_interrupt)
    }
}

#[derive(Debug)]
pub struct Arm64BootNetworkNotificationDispatch {
    device: Arm64BootNetworkDevice,
    outcome: Arm64BootNetworkNotificationOutcome,
}

impl Arm64BootNetworkNotificationDispatch {
    fn new(device: Arm64BootNetworkDevice, outcome: Arm64BootNetworkNotificationOutcome) -> Self {
        Self { device, outcome }
    }

    pub const fn device(&self) -> &Arm64BootNetworkDevice {
        &self.device
    }

    pub const fn outcome(&self) -> &Arm64BootNetworkNotificationOutcome {
        &self.outcome
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.outcome.needs_queue_interrupt()
    }
}

#[derive(Debug)]
pub enum Arm64BootNetworkNotificationOutcome {
    Dispatched(Box<VirtioNetworkDeviceNotificationDispatch>),
    DispatchFailed(VirtioNetworkDeviceNotificationError),
    HandlerLookupFailed(MmioHandlerLookupError),
    PacketIoProviderFailed(Arm64BootNetworkPacketIoError),
}

impl Arm64BootNetworkNotificationOutcome {
    pub fn needs_queue_interrupt(&self) -> bool {
        match self {
            Self::Dispatched(dispatch) => dispatch.needs_queue_interrupt(),
            Self::DispatchFailed(source) => {
                source.completed_tx_dispatch().is_some_and(
                    crate::network::VirtioNetworkTxQueueDispatch::needs_queue_interrupt,
                ) || source.completed_rx_dispatch().is_some_and(
                    crate::network::VirtioNetworkRxQueueDispatch::needs_queue_interrupt,
                )
            }
            Self::HandlerLookupFailed(_) | Self::PacketIoProviderFailed(_) => false,
        }
    }

    pub fn dispatched(&self) -> Option<&VirtioNetworkDeviceNotificationDispatch> {
        match self {
            Self::Dispatched(dispatch) => Some(dispatch.as_ref()),
            Self::DispatchFailed(_)
            | Self::HandlerLookupFailed(_)
            | Self::PacketIoProviderFailed(_) => None,
        }
    }

    pub const fn dispatch_error(&self) -> Option<&VirtioNetworkDeviceNotificationError> {
        match self {
            Self::DispatchFailed(source) => Some(source),
            Self::Dispatched(_)
            | Self::HandlerLookupFailed(_)
            | Self::PacketIoProviderFailed(_) => None,
        }
    }

    pub const fn handler_lookup_error(&self) -> Option<&MmioHandlerLookupError> {
        match self {
            Self::HandlerLookupFailed(source) => Some(source),
            Self::Dispatched(_) | Self::DispatchFailed(_) | Self::PacketIoProviderFailed(_) => None,
        }
    }

    pub const fn packet_io_error(&self) -> Option<&Arm64BootNetworkPacketIoError> {
        match self {
            Self::PacketIoProviderFailed(source) => Some(source),
            Self::Dispatched(_) | Self::DispatchFailed(_) | Self::HandlerLookupFailed(_) => None,
        }
    }
}

pub struct Arm64BootNetworkPacketIo<'a> {
    tx_sink: &'a mut dyn VirtioNetworkTxPacketSink,
    rx_source: &'a mut dyn VirtioNetworkRxPacketSource,
}

impl<'a> Arm64BootNetworkPacketIo<'a> {
    pub fn new(
        tx_sink: &'a mut dyn VirtioNetworkTxPacketSink,
        rx_source: &'a mut dyn VirtioNetworkRxPacketSource,
    ) -> Self {
        Self { tx_sink, rx_source }
    }
}

impl fmt::Debug for Arm64BootNetworkPacketIo<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Arm64BootNetworkPacketIo")
            .finish_non_exhaustive()
    }
}

pub trait Arm64BootNetworkPacketIoProvider {
    fn packet_io(
        &mut self,
        device: &Arm64BootNetworkDevice,
    ) -> Result<Arm64BootNetworkPacketIo<'_>, Arm64BootNetworkPacketIoError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm64BootNetworkPacketIoError {
    message: String,
}

impl Arm64BootNetworkPacketIoError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Arm64BootNetworkPacketIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Arm64BootNetworkPacketIoError {}

#[derive(Debug)]
pub enum Arm64BootNetworkNotificationDispatchError {
    ResultAllocation { source: TryReserveError },
}

impl fmt::Display for Arm64BootNetworkNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate network notification results: {source}"
                )
            }
        }
    }
}

impl std::error::Error for Arm64BootNetworkNotificationDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ResultAllocation { source } => Some(source),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm64BootBlockDevice {
    pub registration: BlockMmioDeviceRegistration,
    pub fdt_device: Arm64FdtVirtioMmioDevice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm64BootNetworkDevice {
    pub registration: NetworkMmioDeviceRegistration,
    pub fdt_device: Arm64FdtVirtioMmioDevice,
}

#[derive(Debug, Clone)]
pub struct Arm64BootSerialDevice {
    pub region: MmioRegion,
    pub output: SharedSerialOutputBuffer,
    pub fdt_device: Arm64FdtSerialDevice,
}

impl Arm64BootRuntimeResources {
    pub fn dispatch_block_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Result<Arm64BootBlockNotificationDispatches, Arm64BootBlockNotificationDispatchError> {
        let mut devices = Vec::new();
        devices
            .try_reserve_exact(self.block_devices.len())
            .map_err(
                |source| Arm64BootBlockNotificationDispatchError::ResultAllocation { source },
            )?;

        for device in self.block_devices.iter().cloned() {
            let region_id = device.registration.region_id();
            let outcome = match mmio_dispatcher.handler_mut::<VirtioBlockMmioHandler>(region_id) {
                Ok(handler) => match handler.dispatch_block_queue_notifications(memory) {
                    Ok(dispatch) => Arm64BootBlockNotificationOutcome::Dispatched(dispatch),
                    Err(source) => Arm64BootBlockNotificationOutcome::DispatchFailed(source),
                },
                Err(source) => Arm64BootBlockNotificationOutcome::HandlerLookupFailed(source),
            };
            devices.push(Arm64BootBlockNotificationDispatch::new(device, outcome));
        }

        Ok(Arm64BootBlockNotificationDispatches::new(devices))
    }

    pub fn dispatch_network_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Result<Arm64BootNetworkNotificationDispatches, Arm64BootNetworkNotificationDispatchError>
    {
        let mut devices = Vec::new();
        devices
            .try_reserve_exact(self.network_devices.len())
            .map_err(
                |source| Arm64BootNetworkNotificationDispatchError::ResultAllocation { source },
            )?;

        for device in self.network_devices.iter().cloned() {
            let region_id = device.registration.region_id();
            let outcome = match mmio_dispatcher.handler_mut::<VirtioNetworkMmioHandler>(region_id) {
                Ok(handler) => match handler.dispatch_network_queue_notifications(memory) {
                    Ok(dispatch) => {
                        Arm64BootNetworkNotificationOutcome::Dispatched(Box::new(dispatch))
                    }
                    Err(source) => Arm64BootNetworkNotificationOutcome::DispatchFailed(source),
                },
                Err(source) => Arm64BootNetworkNotificationOutcome::HandlerLookupFailed(source),
            };
            devices.push(Arm64BootNetworkNotificationDispatch::new(device, outcome));
        }

        Ok(Arm64BootNetworkNotificationDispatches::new(devices))
    }

    pub fn dispatch_network_queue_notifications_with_packet_io(
        &mut self,
        memory: &mut GuestMemory,
        mmio_dispatcher: &mut MmioDispatcher,
        packet_io: &mut impl Arm64BootNetworkPacketIoProvider,
    ) -> Result<Arm64BootNetworkNotificationDispatches, Arm64BootNetworkNotificationDispatchError>
    {
        let mut devices = Vec::new();
        devices
            .try_reserve_exact(self.network_devices.len())
            .map_err(
                |source| Arm64BootNetworkNotificationDispatchError::ResultAllocation { source },
            )?;

        for device in self.network_devices.iter().cloned() {
            let region_id = device.registration.region_id();
            let outcome = match mmio_dispatcher.handler_mut::<VirtioNetworkMmioHandler>(region_id) {
                Ok(handler) => {
                    if !handler.has_pending_queue_notifications() {
                        match handler.dispatch_network_queue_notifications(memory) {
                            Ok(dispatch) => {
                                Arm64BootNetworkNotificationOutcome::Dispatched(Box::new(dispatch))
                            }
                            Err(source) => {
                                Arm64BootNetworkNotificationOutcome::DispatchFailed(source)
                            }
                        }
                    } else {
                        match packet_io.packet_io(&device) {
                            Ok(packet_io) => {
                                let Arm64BootNetworkPacketIo { tx_sink, rx_source } = packet_io;
                                match handler.dispatch_network_queue_notifications_with_packet_io(
                                    memory, tx_sink, rx_source,
                                ) {
                                    Ok(dispatch) => {
                                        Arm64BootNetworkNotificationOutcome::Dispatched(Box::new(
                                            dispatch,
                                        ))
                                    }
                                    Err(source) => {
                                        Arm64BootNetworkNotificationOutcome::DispatchFailed(source)
                                    }
                                }
                            }
                            Err(source) => {
                                Arm64BootNetworkNotificationOutcome::PacketIoProviderFailed(source)
                            }
                        }
                    }
                }
                Err(source) => Arm64BootNetworkNotificationOutcome::HandlerLookupFailed(source),
            };
            devices.push(Arm64BootNetworkNotificationDispatch::new(device, outcome));
        }

        Ok(Arm64BootNetworkNotificationDispatches::new(devices))
    }
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
    PrepareNetworkDevices {
        source: PreparedNetworkDeviceError,
    },
    RegisterBlockMmio {
        source: Box<BlockMmioRegistrationError>,
    },
    RegisterNetworkMmio {
        source: Box<NetworkMmioRegistrationError>,
    },
    RegisterSerialMmio {
        source: Box<Arm64BootSerialMmioRegistrationError>,
    },
    BlockInterruptLineCount {
        devices: usize,
        lines: usize,
    },
    NetworkInterruptLineCount {
        devices: usize,
        lines: usize,
    },
    BlockDeviceMetadataAllocation {
        source: TryReserveError,
    },
    NetworkDeviceMetadataAllocation {
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
            Self::PrepareNetworkDevices { source } => {
                write!(f, "failed to prepare network devices: {source}")
            }
            Self::RegisterBlockMmio { source } => {
                write!(f, "failed to register block MMIO devices: {source}")
            }
            Self::RegisterNetworkMmio { source } => {
                write!(f, "failed to register network MMIO devices: {source}")
            }
            Self::RegisterSerialMmio { source } => {
                write!(f, "failed to register serial MMIO device: {source}")
            }
            Self::BlockInterruptLineCount { devices, lines } => write!(
                f,
                "block MMIO device count {devices} does not match interrupt line count {lines}"
            ),
            Self::NetworkInterruptLineCount { devices, lines } => write!(
                f,
                "network MMIO device count {devices} does not match interrupt line count {lines}"
            ),
            Self::BlockDeviceMetadataAllocation { source } => {
                write!(f, "failed to allocate block device metadata: {source}")
            }
            Self::NetworkDeviceMetadataAllocation { source } => {
                write!(f, "failed to allocate network device metadata: {source}")
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
            Self::PrepareNetworkDevices { source } => Some(source),
            Self::RegisterBlockMmio { source } => Some(source.as_ref()),
            Self::RegisterNetworkMmio { source } => Some(source.as_ref()),
            Self::RegisterSerialMmio { source } => Some(source.as_ref()),
            Self::BlockDeviceMetadataAllocation { source } => Some(source),
            Self::NetworkDeviceMetadataAllocation { source } => Some(source),
            Self::Fdt { source } => Some(source),
            Self::MissingBootSource
            | Self::MemorySizeOverflow { .. }
            | Self::MemorySizeExceedsArchitecturalMaximum { .. }
            | Self::BlockInterruptLineCount { .. }
            | Self::NetworkInterruptLineCount { .. } => None,
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
            network_mmio_layout,
            network_interrupt_lines,
        } = config;
        let boot_source_config = controller
            .boot_source_config()
            .ok_or(Arm64BootResourceError::MissingBootSource)?;
        validate_block_interrupt_line_count(
            controller.drive_configs().len(),
            block_interrupt_lines.len(),
        )?;
        validate_network_interrupt_line_count(
            controller.network_interface_configs().len(),
            network_interrupt_lines.len(),
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
        let (mmio_dispatcher, registrations) = block_mmio.into_parts();
        let (block_devices, mut fdt_devices) =
            block_device_metadata(&registrations, block_interrupt_lines)?;
        let prepared_networks =
            PreparedNetworkDevices::from_config_slice(controller.network_interface_configs())
                .map_err(|source| Arm64BootResourceError::PrepareNetworkDevices { source })?;
        let network_mmio = prepared_networks
            .register_mmio_with_dispatcher(network_mmio_layout, mmio_dispatcher)
            .map_err(|source| Arm64BootResourceError::RegisterNetworkMmio {
                source: Box::new(source),
            })?;
        let (mut mmio_dispatcher, network_registrations) = network_mmio.into_parts();
        let (network_devices, network_fdt_devices) =
            arm64_boot_network_device_metadata(&network_registrations, network_interrupt_lines)?;
        fdt_devices
            .try_reserve_exact(network_fdt_devices.len())
            .map_err(|source| Arm64BootResourceError::NetworkDeviceMetadataAllocation { source })?;
        fdt_devices.extend(network_fdt_devices);
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
            network_devices,
        })
    }

    pub fn into_parts(self) -> Arm64BootResourceParts {
        Arm64BootResourceParts {
            memory: self.memory,
            mmio_dispatcher: self.mmio_dispatcher,
            runtime: Arm64BootRuntimeResources {
                machine_config: self.machine_config,
                layout: self.layout,
                loaded_boot_source: self.loaded_boot_source,
                fdt: self.fdt,
                serial_device: self.serial_device,
                block_devices: self.block_devices,
                network_devices: self.network_devices,
            },
        }
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

fn validate_network_interrupt_line_count(
    devices: usize,
    lines: usize,
) -> Result<(), Arm64BootResourceError> {
    if devices == lines {
        Ok(())
    } else {
        Err(Arm64BootResourceError::NetworkInterruptLineCount { devices, lines })
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

pub fn arm64_boot_network_device_metadata(
    registrations: &[NetworkMmioDeviceRegistration],
    interrupt_lines: &[GuestInterruptLine],
) -> Result<(Vec<Arm64BootNetworkDevice>, Vec<Arm64FdtVirtioMmioDevice>), Arm64BootResourceError> {
    validate_network_interrupt_line_count(registrations.len(), interrupt_lines.len())?;

    let mut network_devices = Vec::new();
    network_devices
        .try_reserve_exact(registrations.len())
        .map_err(|source| Arm64BootResourceError::NetworkDeviceMetadataAllocation { source })?;
    let mut fdt_devices = Vec::new();
    fdt_devices
        .try_reserve_exact(registrations.len())
        .map_err(|source| Arm64BootResourceError::NetworkDeviceMetadataAllocation { source })?;

    for (registration, interrupt_line) in registrations.iter().zip(interrupt_lines) {
        let range = registration.region().range();
        let fdt_device = Arm64FdtVirtioMmioDevice {
            region: Arm64FdtRegion {
                base: range.start().raw_value(),
                size: range.size(),
            },
            interrupt_line: *interrupt_line,
        };
        network_devices.push(Arm64BootNetworkDevice {
            registration: registration.clone(),
            fdt_device,
        });
        fdt_devices.push(fdt_device);
    }

    Ok((network_devices, fdt_devices))
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
    use std::collections::VecDeque;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use device_tree::DeviceTree;

    use super::{
        Arm64BootNetworkNotificationOutcome, Arm64BootNetworkPacketIo,
        Arm64BootNetworkPacketIoError, Arm64BootNetworkPacketIoProvider, Arm64BootResourceConfig,
        Arm64BootResourceError, Arm64BootResources, Arm64BootSerialDeviceConfig,
        Arm64BootSerialMmioRegistrationError, MIB, arm64_boot_network_device_metadata,
        block_device_metadata,
    };
    use crate::VmmAction;
    use crate::block::{
        DriveConfigInput, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
        VIRTIO_BLOCK_REQUEST_TYPE_IN, VIRTIO_BLOCK_SECTOR_SIZE, VIRTIO_BLOCK_STATUS_OK,
        VIRTIO_BLOCK_STATUS_SIZE,
    };
    use crate::boot::{BootPayloadKind, BootSourceConfigInput, BootSourceLoadError};
    use crate::fdt::{Arm64FdtError, Arm64FdtGic, Arm64FdtRegion, Arm64FdtTimerInterrupts};
    use crate::interrupt::{DeviceInterruptKind, GuestInterruptLine};
    use crate::machine::MachineConfigInput;
    use crate::memory::{GuestAddress, aarch64};
    use crate::mmio::{
        MmioAccessBytes, MmioBusError, MmioDispatchOutcome, MmioDispatcher, MmioOperation,
        MmioRegionId,
    };
    use crate::network::{
        NetworkInterfaceConfigInput, NetworkInterfaceConfigs, NetworkMmioDeviceRegistration,
        NetworkMmioLayout, PreparedNetworkDevices, VIRTIO_NET_RX_MIN_BUFFER_SIZE,
        VIRTIO_NET_RX_QUEUE_INDEX, VIRTIO_NET_TX_HEADER_SIZE, VIRTIO_NET_TX_QUEUE_INDEX,
        VirtioNetworkRxPacket, VirtioNetworkRxPacketSource, VirtioNetworkRxPacketSourceError,
        VirtioNetworkTxFrame, VirtioNetworkTxPacketSink, VirtioNetworkTxPacketSinkError,
    };
    use crate::serial::{
        SERIAL_MMIO_DEVICE_WINDOW_SIZE, SERIAL_TRANSMIT_REGISTER_OFFSET, SerialMmioDevice,
        SharedSerialOutputBuffer,
    };
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioRegister,
    };
    use crate::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE,
    };

    static NEXT_TEST_FILE_ID: AtomicU64 = AtomicU64::new(0);

    const TEST_MEMORY_MIB: u64 = 8;
    const ARM64_IMAGE_HEADER_SIZE: usize = 64;
    const ARM64_IMAGE_TEXT_OFFSET_OFFSET: usize = 8;
    const ARM64_IMAGE_SIZE_OFFSET: usize = 16;
    const ARM64_IMAGE_MAGIC_OFFSET: usize = 56;
    const ARM64_IMAGE_MAGIC: u32 = 0x644d_5241;
    const TEST_BLOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_0000);
    const TEST_NETWORK_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_4000);
    const TEST_SERIAL_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_2000);
    const TEST_QUEUE_SIZE: u16 = 4;
    const TEST_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8040_0000);
    const TEST_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8041_0000);
    const TEST_USED_RING: GuestAddress = GuestAddress::new(0x8042_0000);
    const HEADER_ADDR: GuestAddress = GuestAddress::new(0x8043_0000);
    const DATA_ADDR: GuestAddress = GuestAddress::new(0x8044_0000);
    const STATUS_ADDR: GuestAddress = GuestAddress::new(0x8045_0000);
    const TEST_RX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8046_0000);
    const TEST_RX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8047_0000);
    const TEST_RX_USED_RING: GuestAddress = GuestAddress::new(0x8048_0000);
    const TEST_RX_BUFFER: GuestAddress = GuestAddress::new(0x8049_0000);
    const TEST_NETWORK_QUEUE_DEVICE_STRIDE: u64 = 0x0010_0000;
    const TEST_AVAILABLE_RING_IDX_OFFSET: u64 = 2;
    const TEST_AVAILABLE_RING_RING_OFFSET: u64 = 4;
    const TEST_AVAILABLE_RING_ENTRY_SIZE: u64 = 2;
    const QUEUE_CONFIG_STATUS: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    const DRIVER_OK_STATUS: u32 = QUEUE_CONFIG_STATUS | VIRTIO_DEVICE_STATUS_DRIVER_OK;

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
        add_drive_with_root(controller, id, path, true);
    }

    fn add_drive_with_root(
        controller: &mut crate::VmmController,
        id: &str,
        path: &Path,
        is_root_device: bool,
    ) {
        controller
            .handle_action(VmmAction::PutDrive(DriveConfigInput::new(
                id,
                id,
                path.to_path_buf(),
                is_root_device,
            )))
            .expect("drive config should be stored");
    }

    fn add_network(controller: &mut crate::VmmController, iface_id: &str, host_dev_name: &str) {
        controller
            .handle_action(VmmAction::PutNetworkInterface(
                NetworkInterfaceConfigInput::new(iface_id, iface_id, host_dev_name),
            ))
            .expect("network config should be stored");
    }

    fn network_registrations(
        interfaces: &[(&str, &str)],
        layout: NetworkMmioLayout,
    ) -> Vec<NetworkMmioDeviceRegistration> {
        let mut configs = NetworkInterfaceConfigs::new();
        for (iface_id, host_dev_name) in interfaces {
            configs
                .insert(NetworkInterfaceConfigInput::new(
                    *iface_id,
                    *iface_id,
                    *host_dev_name,
                ))
                .expect("network config should be stored");
        }
        let prepared =
            PreparedNetworkDevices::from_configs(&configs).expect("network devices should prepare");
        let (_dispatcher, registrations) = prepared
            .register_mmio(layout)
            .expect("network MMIO devices should register")
            .into_parts();
        registrations
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
            network_mmio_layout: NetworkMmioLayout::new(
                TEST_NETWORK_MMIO_BASE,
                MmioRegionId::new(50),
            ),
            network_interrupt_lines: &[],
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

    fn write_boot_block_mmio_u32(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        try_write_boot_block_mmio_u32(runtime, mmio_dispatcher, device_index, register, value)
            .expect("block MMIO write should dispatch");
    }

    fn try_write_boot_block_mmio_u32(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
        value: u32,
    ) -> Result<MmioDispatchOutcome, crate::mmio::MmioDispatchError> {
        let address = runtime.block_devices[device_index]
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("block MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        mmio_dispatcher.dispatch(
            MmioOperation::write(access, data).expect("u32 write operation should be valid"),
        )
    }

    fn read_boot_block_mmio_u32(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime.block_devices[device_index]
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("block MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read operation should be valid"))
            .expect("block MMIO read should dispatch");
        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not produce write outcome"),
        }
    }

    fn write_boot_network_mmio_u32(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        try_write_boot_network_mmio_u32(runtime, mmio_dispatcher, device_index, register, value)
            .expect("network MMIO write should dispatch");
    }

    fn try_write_boot_network_mmio_u32(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
        value: u32,
    ) -> Result<MmioDispatchOutcome, crate::mmio::MmioDispatchError> {
        let address = runtime.network_devices[device_index]
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("network MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        mmio_dispatcher.dispatch(
            MmioOperation::write(access, data).expect("u32 write operation should be valid"),
        )
    }

    fn read_boot_network_mmio_u32(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime.network_devices[device_index]
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("network MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read operation should be valid"))
            .expect("network MMIO read should dispatch");
        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not produce write outcome"),
        }
    }

    fn configure_boot_block_queue(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        device_ring: GuestAddress,
    ) {
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueNum,
            u32::from(TEST_QUEUE_SIZE),
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDescLow,
            guest_address_low(TEST_DESCRIPTOR_TABLE),
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDriverLow,
            guest_address_low(TEST_AVAILABLE_RING),
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDeviceLow,
            guest_address_low(device_ring),
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueReady,
            1,
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn configure_boot_network_queue(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        queue_index: usize,
        descriptor_table: GuestAddress,
        driver_ring: GuestAddress,
        device_ring: GuestAddress,
    ) {
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueSel,
            u32::try_from(queue_index).expect("queue index should fit"),
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueNum,
            u32::from(TEST_QUEUE_SIZE),
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDescLow,
            guest_address_low(descriptor_table),
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDriverLow,
            guest_address_low(driver_ring),
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDeviceLow,
            guest_address_low(device_ring),
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueReady,
            1,
        );
    }

    fn configure_boot_network_queues(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
    ) {
        configure_boot_network_queues_with_layout(
            runtime,
            mmio_dispatcher,
            device_index,
            network_queue_layout(0),
        );
    }

    fn configure_boot_network_queues_with_layout(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        layout: TestNetworkQueueLayout,
    ) {
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        configure_boot_network_queue(
            runtime,
            mmio_dispatcher,
            device_index,
            VIRTIO_NET_RX_QUEUE_INDEX,
            layout.rx_descriptor_table,
            layout.rx_available_ring,
            layout.rx_used_ring,
        );
        configure_boot_network_queue(
            runtime,
            mmio_dispatcher,
            device_index,
            VIRTIO_NET_TX_QUEUE_INDEX,
            layout.tx_descriptor_table,
            layout.tx_available_ring,
            layout.tx_used_ring,
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn notify_boot_block_queue(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
    ) {
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueNotify,
            0,
        );
    }

    fn notify_boot_network_tx_queue(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
    ) {
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueNotify,
            u32::try_from(VIRTIO_NET_TX_QUEUE_INDEX).expect("TX queue index should fit"),
        );
    }

    fn notify_boot_network_rx_queue(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
    ) {
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueNotify,
            u32::try_from(VIRTIO_NET_RX_QUEUE_INDEX).expect("RX queue index should fit"),
        );
    }

    fn guest_address_low(address: GuestAddress) -> u32 {
        u32::try_from(address.raw_value()).expect("test address should fit in low queue register")
    }

    #[derive(Debug, Clone, Copy)]
    struct TestDescriptor {
        address: GuestAddress,
        len: u32,
        flags: u16,
        next: u16,
    }

    impl TestDescriptor {
        const fn readable(address: GuestAddress, len: u32, next: Option<u16>) -> Self {
            let (flags, next_index) = match next {
                Some(index) => (VIRTQUEUE_DESC_F_NEXT, index),
                None => (0, 0),
            };
            Self {
                address,
                len,
                flags,
                next: next_index,
            }
        }

        const fn writable(address: GuestAddress, len: u32, next: Option<u16>) -> Self {
            let (flags, next_index) = match next {
                Some(index) => (VIRTQUEUE_DESC_F_WRITE | VIRTQUEUE_DESC_F_NEXT, index),
                None => (VIRTQUEUE_DESC_F_WRITE, 0),
            };
            Self {
                address,
                len,
                flags,
                next: next_index,
            }
        }
    }

    #[derive(Debug, Default)]
    struct RecordingTxPacketSink {
        calls: usize,
        packets: Vec<Vec<u8>>,
    }

    impl VirtioNetworkTxPacketSink for RecordingTxPacketSink {
        fn transmit_frame(
            &mut self,
            memory: &crate::memory::GuestMemory,
            frame: &VirtioNetworkTxFrame,
        ) -> Result<(), VirtioNetworkTxPacketSinkError> {
            self.calls += 1;
            let payload_len = usize::try_from(frame.payload_len())
                .expect("test TX payload length should fit in usize");
            let mut packet = Vec::new();
            packet
                .try_reserve_exact(payload_len)
                .expect("test packet allocation should succeed");
            for segment in frame.payload_segments() {
                let len =
                    usize::try_from(segment.len()).expect("test TX segment should fit in usize");
                let mut bytes = vec![0; len];
                memory
                    .read_slice(&mut bytes, segment.address())
                    .expect("test TX segment should read");
                packet.extend(bytes);
            }
            self.packets.push(packet);

            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct RecordingRxPacketSource {
        packets: VecDeque<Vec<u8>>,
        peek_calls: usize,
        consume_calls: usize,
    }

    impl RecordingRxPacketSource {
        fn with_packets(packets: Vec<Vec<u8>>) -> Self {
            Self {
                packets: VecDeque::from(packets),
                peek_calls: 0,
                consume_calls: 0,
            }
        }
    }

    impl VirtioNetworkRxPacketSource for RecordingRxPacketSource {
        fn peek_packet(
            &mut self,
        ) -> Result<Option<VirtioNetworkRxPacket<'_>>, VirtioNetworkRxPacketSourceError> {
            self.peek_calls += 1;
            Ok(self
                .packets
                .front()
                .map(|packet| VirtioNetworkRxPacket::new(packet.as_slice())))
        }

        fn consume_packet(&mut self) {
            self.consume_calls += 1;
            let _ = self.packets.pop_front();
        }
    }

    #[derive(Debug)]
    struct RecordingNetworkPacketEndpoint {
        iface_id: String,
        tx_sink: RecordingTxPacketSink,
        rx_source: RecordingRxPacketSource,
    }

    impl RecordingNetworkPacketEndpoint {
        fn new(iface_id: &str, packets: Vec<Vec<u8>>) -> Self {
            Self {
                iface_id: iface_id.to_string(),
                tx_sink: RecordingTxPacketSink::default(),
                rx_source: RecordingRxPacketSource::with_packets(packets),
            }
        }
    }

    #[derive(Debug, Default)]
    struct RecordingNetworkPacketIoProvider {
        endpoints: Vec<RecordingNetworkPacketEndpoint>,
        requested_ifaces: Vec<String>,
        fail_iface: Option<String>,
    }

    impl RecordingNetworkPacketIoProvider {
        fn with_endpoint(mut self, iface_id: &str, packets: Vec<Vec<u8>>) -> Self {
            self.endpoints
                .push(RecordingNetworkPacketEndpoint::new(iface_id, packets));
            self
        }

        fn failing_for(mut self, iface_id: &str) -> Self {
            self.fail_iface = Some(iface_id.to_string());
            self
        }

        fn endpoint(&self, iface_id: &str) -> &RecordingNetworkPacketEndpoint {
            self.endpoints
                .iter()
                .find(|endpoint| endpoint.iface_id == iface_id)
                .expect("test endpoint should exist")
        }
    }

    impl Arm64BootNetworkPacketIoProvider for RecordingNetworkPacketIoProvider {
        fn packet_io(
            &mut self,
            device: &super::Arm64BootNetworkDevice,
        ) -> Result<Arm64BootNetworkPacketIo<'_>, Arm64BootNetworkPacketIoError> {
            let iface_id = device.registration.iface_id();
            self.requested_ifaces.push(iface_id.to_string());
            if self.fail_iface.as_deref() == Some(iface_id) {
                return Err(Arm64BootNetworkPacketIoError::new(format!(
                    "test packet I/O unavailable for interface {iface_id}"
                )));
            }

            let endpoint = self
                .endpoints
                .iter_mut()
                .find(|endpoint| endpoint.iface_id == iface_id)
                .ok_or_else(|| {
                    Arm64BootNetworkPacketIoError::new(format!(
                        "missing test packet I/O for interface {iface_id}"
                    ))
                })?;
            Ok(Arm64BootNetworkPacketIo::new(
                &mut endpoint.tx_sink,
                &mut endpoint.rx_source,
            ))
        }
    }

    fn write_queued_read_request(memory: &mut crate::memory::GuestMemory) {
        write_request_header(memory, HEADER_ADDR, VIRTIO_BLOCK_REQUEST_TYPE_IN, 0);
        write_descriptor(
            memory,
            0,
            TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
        );
        write_descriptor(
            memory,
            1,
            TestDescriptor::writable(DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, Some(2)),
        );
        write_descriptor(memory, 2, TestDescriptor::writable(STATUS_ADDR, 1, None));
        write_available_heads(memory, &[0]);
    }

    fn write_partially_invalid_queued_flush_request(memory: &mut crate::memory::GuestMemory) {
        write_request_header(memory, HEADER_ADDR, VIRTIO_BLOCK_REQUEST_TYPE_FLUSH, 0);
        write_descriptor(
            memory,
            0,
            TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
        );
        write_descriptor(
            memory,
            1,
            TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
        );
        write_available_heads(memory, &[0, TEST_QUEUE_SIZE]);
    }

    fn write_queued_tx_frame(memory: &mut crate::memory::GuestMemory) {
        write_queued_tx_frame_at(
            memory,
            TEST_DESCRIPTOR_TABLE,
            HEADER_ADDR,
            DATA_ADDR,
            &[0x45, 0, 0, 0],
        );
        write_available_heads(memory, &[0]);
    }

    fn write_queued_tx_frame_at(
        memory: &mut crate::memory::GuestMemory,
        descriptor_table: GuestAddress,
        header_address: GuestAddress,
        payload_address: GuestAddress,
        payload: &[u8],
    ) {
        memory
            .write_slice(&[0; VIRTIO_NET_TX_HEADER_SIZE as usize], header_address)
            .expect("TX header should write");
        memory
            .write_slice(payload, payload_address)
            .expect("TX payload should write");
        write_descriptor_at(
            memory,
            descriptor_table,
            0,
            TestDescriptor::readable(header_address, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
        );
        write_descriptor_at(
            memory,
            descriptor_table,
            1,
            TestDescriptor::readable(
                payload_address,
                u32::try_from(payload.len()).expect("test payload length should fit in u32"),
                None,
            ),
        );
    }

    fn write_rx_buffer(memory: &mut crate::memory::GuestMemory, layout: TestNetworkQueueLayout) {
        write_descriptor_at(
            memory,
            layout.rx_descriptor_table,
            0,
            TestDescriptor::writable(
                layout.rx_buffer,
                u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE)
                    .expect("test RX minimum should fit in u32"),
                None,
            ),
        );
        write_available_heads_at(memory, layout.rx_available_ring, &[0]);
    }

    fn write_request_header(
        memory: &mut crate::memory::GuestMemory,
        address: GuestAddress,
        request_type: u32,
        sector: u64,
    ) {
        let mut bytes = [0; VIRTIO_BLOCK_REQUEST_HEADER_SIZE as usize];
        let (request_type_bytes, tail) = bytes.split_at_mut(4);
        let (_reserved, sector_bytes) = tail.split_at_mut(4);
        request_type_bytes.copy_from_slice(&request_type.to_le_bytes());
        sector_bytes.copy_from_slice(&sector.to_le_bytes());
        memory
            .write_slice(&bytes, address)
            .expect("request header should write");
    }

    fn write_descriptor(
        memory: &mut crate::memory::GuestMemory,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        write_descriptor_at(memory, TEST_DESCRIPTOR_TABLE, index, descriptor);
    }

    fn write_descriptor_at(
        memory: &mut crate::memory::GuestMemory,
        descriptor_table: GuestAddress,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        let mut bytes = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
        let (address_bytes, tail) = bytes.split_at_mut(8);
        let (len_bytes, tail) = tail.split_at_mut(4);
        let (flags_bytes, next_bytes) = tail.split_at_mut(2);
        address_bytes.copy_from_slice(&descriptor.address.raw_value().to_le_bytes());
        len_bytes.copy_from_slice(&descriptor.len.to_le_bytes());
        flags_bytes.copy_from_slice(&descriptor.flags.to_le_bytes());
        next_bytes.copy_from_slice(&descriptor.next.to_le_bytes());

        let descriptor_address = descriptor_table
            .checked_add(
                u64::from(index)
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE).expect("descriptor size should fit"),
            )
            .expect("descriptor address should not overflow");
        memory
            .write_slice(&bytes, descriptor_address)
            .expect("descriptor should write");
    }

    fn write_guest_u16(memory: &mut crate::memory::GuestMemory, address: GuestAddress, value: u16) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("u16 field should write");
    }

    fn read_guest_bytes(
        memory: &crate::memory::GuestMemory,
        address: GuestAddress,
        len: usize,
    ) -> Vec<u8> {
        let mut bytes = vec![0; len];
        memory
            .read_slice(&mut bytes, address)
            .expect("guest bytes should read");
        bytes
    }

    fn available_ring_idx_address_at(available_ring: GuestAddress) -> GuestAddress {
        available_ring
            .checked_add(TEST_AVAILABLE_RING_IDX_OFFSET)
            .expect("available idx address should not overflow")
    }

    fn available_ring_entry_address_at(
        available_ring: GuestAddress,
        ring_index: u16,
    ) -> GuestAddress {
        available_ring
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("available entry address should not overflow")
    }

    fn write_available_heads(memory: &mut crate::memory::GuestMemory, heads: &[u16]) {
        write_available_heads_at(memory, TEST_AVAILABLE_RING, heads);
    }

    fn write_available_heads_at(
        memory: &mut crate::memory::GuestMemory,
        available_ring: GuestAddress,
        heads: &[u16],
    ) {
        for (ring_index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(
                memory,
                available_ring_entry_address_at(
                    available_ring,
                    u16::try_from(ring_index).expect("test ring index should fit in u16"),
                ),
                head,
            );
        }
        write_guest_u16(
            memory,
            available_ring_idx_address_at(available_ring),
            u16::try_from(heads.len()).expect("test available length should fit in u16"),
        );
    }

    fn network_queue_layout(device_index: usize) -> TestNetworkQueueLayout {
        let offset = u64::try_from(device_index)
            .expect("test device index should fit in u64")
            .checked_mul(TEST_NETWORK_QUEUE_DEVICE_STRIDE)
            .expect("test queue offset should not overflow");

        TestNetworkQueueLayout {
            rx_descriptor_table: TEST_RX_DESCRIPTOR_TABLE
                .checked_add(offset)
                .expect("test RX descriptor table should not overflow"),
            rx_available_ring: TEST_RX_AVAILABLE_RING
                .checked_add(offset)
                .expect("test RX available ring should not overflow"),
            rx_used_ring: TEST_RX_USED_RING
                .checked_add(offset)
                .expect("test RX used ring should not overflow"),
            rx_buffer: TEST_RX_BUFFER
                .checked_add(offset)
                .expect("test RX buffer should not overflow"),
            tx_descriptor_table: TEST_DESCRIPTOR_TABLE
                .checked_add(offset)
                .expect("test TX descriptor table should not overflow"),
            tx_available_ring: TEST_AVAILABLE_RING
                .checked_add(offset)
                .expect("test TX available ring should not overflow"),
            tx_used_ring: TEST_USED_RING
                .checked_add(offset)
                .expect("test TX used ring should not overflow"),
            tx_header: HEADER_ADDR
                .checked_add(offset)
                .expect("test TX header should not overflow"),
            tx_payload: DATA_ADDR
                .checked_add(offset)
                .expect("test TX payload should not overflow"),
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct TestNetworkQueueLayout {
        rx_descriptor_table: GuestAddress,
        rx_available_ring: GuestAddress,
        rx_used_ring: GuestAddress,
        rx_buffer: GuestAddress,
        tx_descriptor_table: GuestAddress,
        tx_available_ring: GuestAddress,
        tx_used_ring: GuestAddress,
        tx_header: GuestAddress,
        tx_payload: GuestAddress,
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
        assert!(resources.network_devices.is_empty());
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
    fn assembles_boot_resources_with_network_device_mmio_metadata() {
        let kernel = temp_file("kernel-with-network", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_network(&mut controller, "eth0", "/bangbang/missing-tap0");
        let network_lines = [line(33)];
        let config = Arm64BootResourceConfig {
            network_interrupt_lines: &network_lines,
            ..valid_config(&[])
        };

        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble with network device");

        assert!(resources.block_devices.is_empty());
        assert_eq!(resources.network_devices.len(), 1);
        assert_eq!(resources.network_devices[0].registration.iface_id(), "eth0");
        assert_eq!(
            resources.network_devices[0].registration.host_dev_name(),
            "/bangbang/missing-tap0"
        );
        assert_eq!(
            resources.network_devices[0].registration.address(),
            TEST_NETWORK_MMIO_BASE
        );
        assert_eq!(
            resources.network_devices[0].fdt_device.region.base,
            TEST_NETWORK_MMIO_BASE.raw_value()
        );
        assert_eq!(
            resources.network_devices[0].fdt_device.interrupt_line,
            line(33)
        );
        assert_eq!(resources.mmio_dispatcher.regions().len(), 1);

        let tree = read_fdt(&resources);
        let network_node = tree
            .find("/virtio_mmio@40004000")
            .expect("network virtio-mmio node should be in assembled FDT");
        assert_eq!(network_node.prop_str("compatible").unwrap(), "virtio,mmio");
    }

    #[test]
    fn assembles_boot_resources_preserve_multiple_network_order() {
        let kernel = temp_file("kernel-with-networks", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_network(&mut controller, "eth0", "tap0");
        add_network(&mut controller, "eth1", "tap1");
        let network_lines = [line(33), line(34)];
        let config = Arm64BootResourceConfig {
            network_interrupt_lines: &network_lines,
            ..valid_config(&[])
        };

        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble with network devices");

        assert_eq!(resources.network_devices.len(), 2);
        assert_eq!(resources.network_devices[0].registration.iface_id(), "eth0");
        assert_eq!(
            resources.network_devices[0].fdt_device.interrupt_line,
            line(33)
        );
        assert_eq!(resources.network_devices[1].registration.iface_id(), "eth1");
        assert_eq!(
            resources.network_devices[1].registration.address(),
            TEST_NETWORK_MMIO_BASE
                .checked_add(VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
                .expect("test network address should not overflow")
        );
        assert_eq!(
            resources.network_devices[1].fdt_device.interrupt_line,
            line(34)
        );

        let tree = read_fdt(&resources);
        assert!(tree.find("/virtio_mmio@40004000").is_some());
        assert!(tree.find("/virtio_mmio@40005000").is_some());
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
    fn boot_resources_split_memory_from_runtime_resources() {
        let kernel = temp_file("kernel-split", &arm64_image());
        let block = temp_file("block-split", &[0x5a; 512]);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", block.path());
        let (serial, _output) =
            serial_config(TEST_SERIAL_MMIO_BASE, MmioRegionId::new(9), line(33));
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                serial_device: Some(serial),
                ..valid_config(&[line(32)])
            },
        )
        .expect("boot resources should assemble");
        let memory_size = resources.memory.total_size();
        let layout = resources.layout.clone();
        let kernel_entry = resources.loaded_boot_source.kernel.entry_address;
        let fdt = resources.fdt;
        let block_registration = resources.block_devices[0].registration.clone();
        let serial_region = resources
            .serial_device
            .as_ref()
            .expect("serial metadata should exist")
            .region;

        let parts = resources.into_parts();

        assert_eq!(parts.memory.total_size(), memory_size);
        assert_eq!(parts.runtime.layout, layout);
        assert_eq!(
            parts.runtime.loaded_boot_source.kernel.entry_address,
            kernel_entry
        );
        assert_eq!(parts.runtime.fdt, fdt);
        assert_eq!(parts.runtime.block_devices.len(), 1);
        assert_eq!(
            parts.runtime.block_devices[0].registration,
            block_registration
        );
        assert_eq!(
            parts
                .runtime
                .serial_device
                .as_ref()
                .expect("serial metadata should split")
                .region,
            serial_region
        );
    }

    #[test]
    fn boot_runtime_block_notification_dispatch_accepts_empty_block_devices() {
        let kernel = temp_file("kernel-empty-block-dispatch", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
                .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;

        let dispatches = runtime
            .dispatch_block_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("empty block dispatch result should allocate");

        assert!(dispatches.is_empty());
        assert_eq!(dispatches.len(), 0);
        assert!(!dispatches.needs_queue_interrupt());
    }

    #[test]
    fn boot_runtime_network_notification_dispatch_accepts_empty_network_devices() {
        let kernel = temp_file("kernel-empty-network-dispatch", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
                .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;

        let dispatches = runtime
            .dispatch_network_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("empty network dispatch result should allocate");

        assert!(dispatches.is_empty());
        assert_eq!(dispatches.len(), 0);
        assert!(!dispatches.needs_queue_interrupt());
    }

    #[test]
    fn boot_runtime_network_packet_io_dispatch_accepts_empty_network_devices() {
        let kernel = temp_file("kernel-empty-network-packet-io-dispatch", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
                .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        let mut provider = RecordingNetworkPacketIoProvider::default();

        let dispatches = runtime
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut mmio_dispatcher,
                &mut provider,
            )
            .expect("empty network dispatch result should allocate");

        assert!(dispatches.is_empty());
        assert_eq!(dispatches.len(), 0);
        assert!(!dispatches.needs_queue_interrupt());
        assert!(provider.requested_ifaces.is_empty());
    }

    #[test]
    fn boot_runtime_network_notification_dispatch_without_pending_notification_is_noop() {
        let kernel = temp_file("kernel-network-noop-dispatch", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_network(&mut controller, "eth0", "tap0");
        let config = Arm64BootResourceConfig {
            network_interrupt_lines: &[line(33)],
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;

        let dispatches = runtime
            .dispatch_network_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("network dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(!dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        assert_eq!(device_dispatch.device().registration.iface_id(), "eth0");
        assert_eq!(device_dispatch.device().fdt_device.interrupt_line, line(33));
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("no pending notification should dispatch as no-op");
        assert_eq!(dispatch.drained_notifications(), []);
        assert!(dispatch.tx_queue_dispatch().is_none());
        assert!(!device_dispatch.needs_queue_interrupt());
    }

    #[test]
    fn boot_runtime_network_notification_dispatch_with_packet_io_skips_provider_without_pending() {
        let kernel = temp_file("kernel-network-packet-io-noop-dispatch", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_network(&mut controller, "eth0", "tap0");
        let config = Arm64BootResourceConfig {
            network_interrupt_lines: &[line(33)],
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        let mut provider =
            RecordingNetworkPacketIoProvider::default().with_endpoint("eth0", Vec::new());

        let dispatches = runtime
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut mmio_dispatcher,
                &mut provider,
            )
            .expect("network dispatch result should allocate");

        assert!(provider.requested_ifaces.is_empty());
        let dispatch = dispatches.as_slice()[0]
            .outcome()
            .dispatched()
            .expect("no pending notification should dispatch as no-op");
        assert_eq!(dispatch.drained_notifications(), []);
        assert!(!dispatches.needs_queue_interrupt());
    }

    #[test]
    fn boot_runtime_network_notification_dispatch_executes_tx_queue() {
        let kernel = temp_file("kernel-network-tx-dispatch", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_network(&mut controller, "eth0", "tap0");
        let config = Arm64BootResourceConfig {
            network_interrupt_lines: &[line(33)],
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        write_queued_tx_frame(&mut memory);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 0);

        let dispatches = runtime
            .dispatch_network_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("network dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        assert_eq!(device_dispatch.device().registration.iface_id(), "eth0");
        assert_eq!(device_dispatch.device().fdt_device.interrupt_line, line(33));
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("queued TX notification should dispatch");
        assert_eq!(
            dispatch.drained_notifications(),
            [VIRTIO_NET_TX_QUEUE_INDEX]
        );
        let tx_dispatch = dispatch
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(tx_dispatch.processed_frames(), 1);
        assert_eq!(tx_dispatch.successful_frames(), 1);
        assert_eq!(tx_dispatch.parse_failures(), 0);
        assert_eq!(tx_dispatch.frames()[0].payload_len(), 4);
        assert!(device_dispatch.needs_queue_interrupt());
        assert_eq!(
            read_boot_network_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn boot_runtime_network_notification_dispatch_routes_packet_io_by_interface() {
        let kernel = temp_file("kernel-network-packet-io-dispatch", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_network(&mut controller, "eth0", "tap0");
        let config = Arm64BootResourceConfig {
            network_interrupt_lines: &[line(33)],
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        let layout = network_queue_layout(0);
        configure_boot_network_queues_with_layout(&mut runtime, &mut mmio_dispatcher, 0, layout);
        write_rx_buffer(&mut memory, layout);
        write_queued_tx_frame_at(
            &mut memory,
            layout.tx_descriptor_table,
            layout.tx_header,
            layout.tx_payload,
            &[0xde, 0xad, 0xbe, 0xef],
        );
        write_available_heads_at(&mut memory, layout.tx_available_ring, &[0]);
        notify_boot_network_rx_queue(&mut runtime, &mut mmio_dispatcher, 0);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let mut provider = RecordingNetworkPacketIoProvider::default()
            .with_endpoint("eth0", vec![vec![0xaa, 0xbb]]);

        let dispatches = runtime
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut mmio_dispatcher,
                &mut provider,
            )
            .expect("network dispatch result should allocate");

        assert_eq!(provider.requested_ifaces, ["eth0".to_string()]);
        assert_eq!(dispatches.len(), 1);
        assert!(dispatches.needs_queue_interrupt());
        let dispatch = dispatches.as_slice()[0]
            .outcome()
            .dispatched()
            .expect("network dispatch should complete");
        assert_eq!(
            dispatch.drained_notifications(),
            [VIRTIO_NET_RX_QUEUE_INDEX, VIRTIO_NET_TX_QUEUE_INDEX]
        );
        let rx_dispatch = dispatch
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(rx_dispatch.delivered_packets(), 1);
        let tx_dispatch = dispatch
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(tx_dispatch.processed_frames(), 1);
        assert_eq!(tx_dispatch.sink_successful_frames(), 1);

        let endpoint = provider.endpoint("eth0");
        assert_eq!(endpoint.tx_sink.calls, 1);
        assert_eq!(endpoint.tx_sink.packets, [vec![0xde, 0xad, 0xbe, 0xef]]);
        assert_eq!(endpoint.rx_source.consume_calls, 1);
        assert!(endpoint.rx_source.packets.is_empty());
        let mut expected_rx_frame = vec![0; VIRTIO_NET_TX_HEADER_SIZE as usize];
        expected_rx_frame.extend([0xaa, 0xbb]);
        assert_eq!(
            read_guest_bytes(&memory, layout.rx_buffer, expected_rx_frame.len()),
            expected_rx_frame
        );
        assert_eq!(
            read_boot_network_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn boot_runtime_network_notification_dispatch_routes_multiple_interfaces_separately() {
        let kernel = temp_file("kernel-network-packet-io-multiple", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_network(&mut controller, "eth0", "tap0");
        add_network(&mut controller, "eth1", "tap1");
        let config = Arm64BootResourceConfig {
            network_interrupt_lines: &[line(33), line(34)],
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        let first = network_queue_layout(0);
        let second = network_queue_layout(1);
        configure_boot_network_queues_with_layout(&mut runtime, &mut mmio_dispatcher, 0, first);
        configure_boot_network_queues_with_layout(&mut runtime, &mut mmio_dispatcher, 1, second);
        write_queued_tx_frame_at(
            &mut memory,
            first.tx_descriptor_table,
            first.tx_header,
            first.tx_payload,
            &[0x10],
        );
        write_available_heads_at(&mut memory, first.tx_available_ring, &[0]);
        write_queued_tx_frame_at(
            &mut memory,
            second.tx_descriptor_table,
            second.tx_header,
            second.tx_payload,
            &[0x20, 0x21],
        );
        write_available_heads_at(&mut memory, second.tx_available_ring, &[0]);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 0);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 1);
        let mut provider = RecordingNetworkPacketIoProvider::default()
            .with_endpoint("eth0", Vec::new())
            .with_endpoint("eth1", Vec::new());

        let dispatches = runtime
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut mmio_dispatcher,
                &mut provider,
            )
            .expect("network dispatch result should allocate");

        assert_eq!(
            provider.requested_ifaces,
            ["eth0".to_string(), "eth1".to_string()]
        );
        assert_eq!(dispatches.len(), 2);
        assert!(dispatches.needs_queue_interrupt());
        assert_eq!(provider.endpoint("eth0").tx_sink.packets, [vec![0x10]]);
        assert_eq!(
            provider.endpoint("eth1").tx_sink.packets,
            [vec![0x20, 0x21]]
        );
        assert_eq!(provider.endpoint("eth0").tx_sink.calls, 1);
        assert_eq!(provider.endpoint("eth1").tx_sink.calls, 1);
    }

    #[test]
    fn boot_runtime_network_notification_dispatch_preserves_pending_on_packet_io_failure() {
        let kernel = temp_file("kernel-network-packet-io-failure", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_network(&mut controller, "eth0", "tap0");
        let config = Arm64BootResourceConfig {
            network_interrupt_lines: &[line(33)],
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        write_queued_tx_frame(&mut memory);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let mut provider = RecordingNetworkPacketIoProvider::default().failing_for("eth0");

        let failed = runtime
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut mmio_dispatcher,
                &mut provider,
            )
            .expect("network dispatch result should allocate");

        assert!(!failed.needs_queue_interrupt());
        match failed.as_slice()[0].outcome() {
            Arm64BootNetworkNotificationOutcome::PacketIoProviderFailed(source) => {
                assert_eq!(
                    source.message(),
                    "test packet I/O unavailable for interface eth0"
                );
            }
            other => panic!("expected packet I/O provider failure, got {other:?}"),
        }
        assert_eq!(
            read_boot_network_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );

        let retried = runtime
            .dispatch_network_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("retry with default packet I/O should allocate");
        let dispatch = retried.as_slice()[0]
            .outcome()
            .dispatched()
            .expect("retry should dispatch preserved notification");
        assert_eq!(
            dispatch.drained_notifications(),
            [VIRTIO_NET_TX_QUEUE_INDEX]
        );
        assert_eq!(
            dispatch
                .tx_queue_dispatch()
                .expect("TX dispatch should be present")
                .processed_frames(),
            1
        );
        assert!(retried.needs_queue_interrupt());
    }

    #[test]
    fn boot_runtime_network_notification_dispatch_skips_packet_io_when_handler_missing() {
        let kernel = temp_file("kernel-network-packet-io-missing-handler", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_network(&mut controller, "eth0", "tap0");
        let config = Arm64BootResourceConfig {
            network_interrupt_lines: &[line(33)],
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = MmioDispatcher::new();
        let mut runtime = parts.runtime;
        let mut provider =
            RecordingNetworkPacketIoProvider::default().with_endpoint("eth0", Vec::new());

        let dispatches = runtime
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut mmio_dispatcher,
                &mut provider,
            )
            .expect("network dispatch result should allocate");

        assert!(provider.requested_ifaces.is_empty());
        assert!(
            dispatches.as_slice()[0]
                .outcome()
                .handler_lookup_error()
                .is_some()
        );
        assert!(!dispatches.needs_queue_interrupt());
    }

    #[test]
    fn boot_runtime_network_notification_dispatch_reports_missing_handler() {
        let kernel = temp_file("kernel-network-missing-handler-dispatch", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_network(&mut controller, "eth0", "tap0");
        let config = Arm64BootResourceConfig {
            network_interrupt_lines: &[line(33)],
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = MmioDispatcher::new();
        let mut runtime = parts.runtime;

        let dispatches = runtime
            .dispatch_network_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("network dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(!dispatches.needs_queue_interrupt());
        let error = dispatches.as_slice()[0]
            .outcome()
            .handler_lookup_error()
            .expect("missing network handler should be reported");
        match error {
            crate::mmio::MmioHandlerLookupError::MissingHandler { region_id } => {
                assert_eq!(
                    *region_id,
                    runtime.network_devices[0].registration.region_id()
                );
            }
            other => panic!("expected missing network handler, got {other:?}"),
        }
    }

    #[test]
    fn boot_runtime_block_notification_dispatch_without_pending_notification_is_noop() {
        let kernel = temp_file("kernel-block-noop-dispatch", &arm64_image());
        let block = temp_file("block-noop-dispatch", &[0x5a; 512]);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", block.path());
        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[line(32)]))
                .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);

        let dispatches = runtime
            .dispatch_block_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("block dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(!dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        assert_eq!(device_dispatch.device().registration.drive_id(), "rootfs");
        assert_eq!(device_dispatch.device().fdt_device.interrupt_line, line(32));
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("no pending notification should still dispatch as no-op");
        assert_eq!(dispatch.drained_notifications(), []);
        assert!(dispatch.queue_dispatch().is_none());
        assert!(!device_dispatch.needs_queue_interrupt());
        assert_eq!(
            read_boot_block_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
    }

    #[test]
    fn boot_runtime_block_notification_dispatch_executes_queued_request() {
        let kernel = temp_file("kernel-block-request-dispatch", &arm64_image());
        let payload = vec![0x74; VIRTIO_BLOCK_SECTOR_SIZE as usize];
        let block = temp_file("block-request-dispatch", &payload);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", block.path());
        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[line(32)]))
                .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_queued_read_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0);

        let dispatches = runtime
            .dispatch_block_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("block dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        assert_eq!(device_dispatch.device().registration.index(), 0);
        assert_eq!(
            device_dispatch.device().registration.region_id(),
            MmioRegionId::new(1)
        );
        assert_eq!(device_dispatch.device().fdt_device.interrupt_line, line(32));
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("queued notification should dispatch");
        assert_eq!(dispatch.drained_notifications(), [0]);
        let queue = dispatch
            .queue_dispatch()
            .expect("queue dispatch summary should be present");
        assert_eq!(queue.processed_requests(), 1);
        assert_eq!(queue.successful_requests(), 1);
        assert!(queue.needs_queue_interrupt());
        assert!(device_dispatch.needs_queue_interrupt());
        assert_eq!(
            read_boot_block_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as usize),
            payload
        );
        assert_eq!(
            read_guest_bytes(&memory, STATUS_ADDR, 1),
            [VIRTIO_BLOCK_STATUS_OK]
        );

        let second_dispatches = runtime
            .dispatch_block_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("second block dispatch result should allocate");
        let second = second_dispatches.as_slice()[0]
            .outcome()
            .dispatched()
            .expect("cleared notification should dispatch as no-op");
        assert_eq!(second.drained_notifications(), []);
        assert!(second.queue_dispatch().is_none());
    }

    #[test]
    fn boot_runtime_block_notification_dispatch_preserves_device_error_metadata() {
        let kernel = temp_file("kernel-block-error-dispatch", &arm64_image());
        let block = temp_file("block-error-dispatch", &[0x5a; 512]);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", block.path());
        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[line(32)]))
                .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        write_boot_block_mmio_u32(
            &mut runtime,
            &mut mmio_dispatcher,
            0,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_block_mmio_u32(
            &mut runtime,
            &mut mmio_dispatcher,
            0,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_block_mmio_u32(
            &mut runtime,
            &mut mmio_dispatcher,
            0,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        try_write_boot_block_mmio_u32(
            &mut runtime,
            &mut mmio_dispatcher,
            0,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        )
        .expect_err("unconfigured block queue activation should fail");
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0);

        let dispatches = runtime
            .dispatch_block_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("block dispatch result should allocate");

        let device_dispatch = &dispatches.as_slice()[0];
        assert_eq!(device_dispatch.device().registration.drive_id(), "rootfs");
        assert_eq!(device_dispatch.device().fdt_device.interrupt_line, line(32));
        let error = device_dispatch
            .outcome()
            .dispatch_error()
            .expect("inactive block notification should be preserved as a device error");
        assert_eq!(error.drained_notifications(), [0]);
        assert!(error.completed_dispatch().is_none());
        assert!(!device_dispatch.needs_queue_interrupt());
    }

    #[test]
    fn boot_runtime_block_notification_dispatch_preserves_partial_error_interrupt_intent() {
        let kernel = temp_file("kernel-block-partial-error-dispatch", &arm64_image());
        let block = temp_file("block-partial-error-dispatch", &[0x5a; 512]);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", block.path());
        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[line(32)]))
                .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_partially_invalid_queued_flush_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0);

        let dispatches = runtime
            .dispatch_block_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("block dispatch result should allocate");

        assert!(dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        assert!(device_dispatch.needs_queue_interrupt());
        let error = device_dispatch
            .outcome()
            .dispatch_error()
            .expect("partial queue failure should be preserved as a device error");
        assert_eq!(error.drained_notifications(), [0]);
        let completed = error
            .completed_dispatch()
            .expect("partial queue failure should preserve completed dispatch metadata");
        assert_eq!(completed.processed_requests(), 1);
        assert_eq!(completed.successful_requests(), 1);
        assert!(completed.needs_queue_interrupt());
        assert_eq!(
            read_boot_block_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(
            read_guest_bytes(&memory, STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE as usize),
            [VIRTIO_BLOCK_STATUS_OK]
        );
    }

    #[test]
    fn boot_runtime_block_notification_dispatch_reports_missing_handler() {
        let kernel = temp_file("kernel-block-missing-handler", &arm64_image());
        let block = temp_file("block-missing-handler", &[0x5a; 512]);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", block.path());
        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[line(32)]))
                .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = MmioDispatcher::new();
        let mut runtime = parts.runtime;

        let dispatches = runtime
            .dispatch_block_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("block dispatch result should allocate");

        let error = dispatches.as_slice()[0]
            .outcome()
            .handler_lookup_error()
            .expect("missing block handler should be reported");
        assert!(matches!(
            error,
            crate::mmio::MmioHandlerLookupError::MissingHandler {
                region_id
            } if *region_id == MmioRegionId::new(1)
        ));
    }

    #[test]
    fn boot_runtime_block_notification_dispatch_reports_wrong_handler_type() {
        let kernel = temp_file("kernel-block-wrong-handler", &arm64_image());
        let block = temp_file("block-wrong-handler", &[0x5a; 512]);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", block.path());
        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[line(32)]))
                .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut runtime = parts.runtime;
        let region = runtime.block_devices[0].registration.region();
        let mut mmio_dispatcher = MmioDispatcher::new();
        mmio_dispatcher
            .insert_region(region.id(), region.range().start(), region.range().size())
            .expect("replacement region should insert");
        mmio_dispatcher
            .register_handler(
                region.id(),
                SerialMmioDevice::new(SharedSerialOutputBuffer::default()),
            )
            .expect("serial handler should register under block region id for test");

        let dispatches = runtime
            .dispatch_block_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("block dispatch result should allocate");

        let error = dispatches.as_slice()[0]
            .outcome()
            .handler_lookup_error()
            .expect("wrong block handler type should be reported");
        assert!(matches!(
            error,
            crate::mmio::MmioHandlerLookupError::UnexpectedHandlerType {
                region_id,
                ..
            } if *region_id == MmioRegionId::new(1)
        ));
    }

    #[test]
    fn boot_runtime_block_notification_dispatch_keeps_multiple_devices_independent() {
        let kernel = temp_file("kernel-block-multi-dispatch", &arm64_image());
        let first_block = temp_file("block-multi-first", &[0x11; 512]);
        let second_payload = vec![0x22; VIRTIO_BLOCK_SECTOR_SIZE as usize];
        let second_block = temp_file("block-multi-second", &second_payload);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", first_block.path());
        add_drive_with_root(&mut controller, "data", second_block.path(), false);
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_config(&[line(32), line(33)]),
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 1, TEST_USED_RING);
        write_queued_read_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 1);

        let dispatches = runtime
            .dispatch_block_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("block dispatch result should allocate");

        assert_eq!(dispatches.len(), 2);
        let first = &dispatches.as_slice()[0];
        let second = &dispatches.as_slice()[1];
        assert_eq!(first.device().registration.drive_id(), "rootfs");
        assert_eq!(first.device().fdt_device.interrupt_line, line(32));
        assert!(!first.needs_queue_interrupt());
        assert_eq!(
            first
                .outcome()
                .dispatched()
                .expect("first device should dispatch as no-op")
                .drained_notifications(),
            []
        );
        assert_eq!(second.device().registration.drive_id(), "data");
        assert_eq!(second.device().fdt_device.interrupt_line, line(33));
        assert!(second.needs_queue_interrupt());
        assert_eq!(
            second
                .outcome()
                .dispatched()
                .expect("second device should dispatch queued notification")
                .drained_notifications(),
            [0]
        );
        assert_eq!(
            read_boot_block_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
        assert_eq!(
            read_boot_block_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                1,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as usize),
            second_payload
        );
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
    fn network_interrupt_line_count_mismatch_fails_before_boot_source_load() {
        let mut controller = controller_with_kernel(&missing_path("network-line-mismatch-kernel"));
        add_network(&mut controller, "eth0", "/bangbang/missing-tap0");

        let err = Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
            .expect_err("network line mismatch should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::NetworkInterruptLineCount {
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
    fn network_metadata_accepts_empty_registrations() {
        let (devices, fdt_devices) = arm64_boot_network_device_metadata(&[], &[])
            .expect("empty network metadata should build");

        assert!(devices.is_empty());
        assert!(fdt_devices.is_empty());
    }

    #[test]
    fn network_metadata_maps_one_registration_without_host_resource_access() {
        let registrations = network_registrations(
            &[("eth0", "/bangbang/missing-tap0")],
            NetworkMmioLayout::new(TEST_NETWORK_MMIO_BASE, MmioRegionId::new(50)),
        );
        let lines = [line(34)];

        let (devices, fdt_devices) = arm64_boot_network_device_metadata(&registrations, &lines)
            .expect("network metadata should build");

        assert_eq!(devices.len(), 1);
        assert_eq!(fdt_devices.len(), 1);
        assert_eq!(devices[0].registration.index(), 0);
        assert_eq!(devices[0].registration.iface_id(), "eth0");
        assert_eq!(
            devices[0].registration.host_dev_name(),
            "/bangbang/missing-tap0"
        );
        assert_eq!(devices[0].registration.region_id(), MmioRegionId::new(50));
        assert_eq!(devices[0].registration.address(), TEST_NETWORK_MMIO_BASE);
        assert_eq!(
            devices[0].fdt_device.region.base,
            TEST_NETWORK_MMIO_BASE.raw_value()
        );
        assert_eq!(
            devices[0].fdt_device.region.size,
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        );
        assert_eq!(devices[0].fdt_device.interrupt_line, line(34));
        assert_eq!(fdt_devices[0], devices[0].fdt_device);
    }

    #[test]
    fn network_metadata_preserves_registration_order_and_interrupt_pairing() {
        let stride = VIRTIO_MMIO_DEVICE_WINDOW_SIZE * 2;
        let registrations = network_registrations(
            &[("eth0", "tap0"), ("eth1", "tap1")],
            NetworkMmioLayout::new(TEST_NETWORK_MMIO_BASE, MmioRegionId::new(60))
                .with_address_stride(stride)
                .with_region_id_stride(3),
        );
        let lines = [line(35), line(36)];

        let (devices, fdt_devices) = arm64_boot_network_device_metadata(&registrations, &lines)
            .expect("network metadata should build");

        assert_eq!(devices.len(), 2);
        assert_eq!(fdt_devices.len(), 2);
        assert_eq!(devices[0].registration.iface_id(), "eth0");
        assert_eq!(devices[0].registration.host_dev_name(), "tap0");
        assert_eq!(devices[0].registration.region_id(), MmioRegionId::new(60));
        assert_eq!(devices[0].registration.address(), TEST_NETWORK_MMIO_BASE);
        assert_eq!(devices[0].fdt_device.interrupt_line, line(35));
        assert_eq!(devices[1].registration.iface_id(), "eth1");
        assert_eq!(devices[1].registration.host_dev_name(), "tap1");
        assert_eq!(devices[1].registration.region_id(), MmioRegionId::new(63));
        assert_eq!(
            devices[1].registration.address(),
            TEST_NETWORK_MMIO_BASE
                .checked_add(stride)
                .expect("test address should not overflow"),
        );
        assert_eq!(devices[1].fdt_device.interrupt_line, line(36));
        assert_eq!(fdt_devices[0], devices[0].fdt_device);
        assert_eq!(fdt_devices[1], devices[1].fdt_device);
    }

    #[test]
    fn network_metadata_rejects_registration_line_mismatch() {
        let registrations = network_registrations(
            &[("eth0", "tap0")],
            NetworkMmioLayout::new(TEST_NETWORK_MMIO_BASE, MmioRegionId::new(70)),
        );

        let err = arm64_boot_network_device_metadata(&registrations, &[])
            .expect_err("line mismatch should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::NetworkInterruptLineCount {
                devices: 1,
                lines: 0
            }
        ));
        assert!(std::error::Error::source(&err).is_none());
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
    fn serial_region_overflow_fails_during_registration() {
        let kernel = temp_file("kernel-serial-overflow", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let (serial, _output) =
            serial_config(GuestAddress::new(u64::MAX), MmioRegionId::new(9), line(32));
        let config = Arm64BootResourceConfig {
            serial_device: Some(serial),
            ..valid_config(&[])
        };

        let err = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect_err("overflowing serial region should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::RegisterSerialMmio { source }
                if matches!(
                    source.as_ref(),
                    Arm64BootSerialMmioRegistrationError::InsertRegion {
                        source: MmioBusError::InvalidRegionRange { .. },
                        ..
                    }
                )
        ));
    }

    #[test]
    fn serial_non_spi_interrupt_fails_during_fdt_write() {
        let kernel = temp_file("kernel-serial-non-spi", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let (serial, _output) =
            serial_config(TEST_SERIAL_MMIO_BASE, MmioRegionId::new(9), line(31));
        let config = Arm64BootResourceConfig {
            serial_device: Some(serial),
            ..valid_config(&[])
        };

        let err = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect_err("non-SPI serial interrupt should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::Fdt {
                source: Arm64FdtError::InvalidSerialInterrupt { .. }
            }
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
