//! Internal assembly of boot resources from validated VM configuration.

use std::collections::TryReserveError;
use std::fmt;
use std::os::fd::RawFd;
use std::time::Duration;

use crate::VmmController;
use crate::balloon::{
    BalloonConfig, BalloonHintingCommandError, BalloonHintingStartInput, BalloonHintingStatus,
    BalloonHintingStatusError, BalloonMmioDeviceRegistration, BalloonMmioLayout,
    BalloonMmioRegistrationError, BalloonPageCountOverflow, BalloonStats, BalloonStatsError,
    BalloonStatsUpdateInput, BalloonUpdateError, PreparedBalloonDevice,
    VirtioBalloonDeviceNotificationDispatch, VirtioBalloonDeviceNotificationError,
    VirtioBalloonMmioHandler,
};
use crate::block::{
    BlockFileBacking, BlockMmioDeviceRegistration, BlockMmioLayout, BlockMmioRegistrationError,
    DriveConfig, DriveRateLimiterConfig, DriveUpdateError, PreparedBlockDeviceError,
    PreparedBlockDevices, VirtioBlockDeviceNotificationDispatch,
    VirtioBlockDeviceNotificationError, VirtioBlockMmioHandler,
};
use crate::boot::{
    BootCommandLineError, BootSource, BootSourceConfig, BootSourceLoadError, LoadedBootSource,
};
use crate::entropy::{
    EntropyMmioDeviceRegistration, EntropyMmioLayout, EntropyMmioRegistrationError,
    PreparedEntropyDevice, VirtioRngDeviceNotificationDispatch, VirtioRngDeviceNotificationError,
    VirtioRngEntropySource, VirtioRngEntropySourceError, VirtioRngMmioHandler,
    VirtioRngOsEntropySource,
};
use crate::fdt::{
    Arm64FdtBootInfo, Arm64FdtConfig, Arm64FdtError, Arm64FdtGic, Arm64FdtGuestMemoryWrite,
    Arm64FdtRegion, Arm64FdtRtcDevice, Arm64FdtSerialDevice, Arm64FdtTimerInterrupts,
    Arm64FdtVirtioMmioDevice, write_arm64_fdt,
};
use crate::interrupt::GuestInterruptLine;
use crate::machine::MachineConfig;
use crate::memory::{
    GuestMemory, GuestMemoryAllocationError, GuestMemoryError, GuestMemoryLayout, aarch64,
};
use crate::memory_hotplug::{
    PreparedVirtioMemDevice, VirtioMemDeviceNotificationDispatch, VirtioMemDeviceNotificationError,
    VirtioMemMmioDeviceRegistration, VirtioMemMmioHandler, VirtioMemMmioLayout,
    VirtioMemMmioRegistrationError, VirtioMemPrepareError,
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
use crate::pmem::{
    PmemMmioDeviceRegistration, PmemMmioLayout, PmemMmioRegistrationError, PreparedPmemDevice,
    PreparedPmemDeviceError, PreparedPmemDevices, VirtioPmemDeviceNotificationDispatch,
    VirtioPmemDeviceNotificationError, VirtioPmemFlushStatus, VirtioPmemMmioHandler,
};
use crate::rtc::{Pl031RtcDevice, RTC_MMIO_DEVICE_WINDOW_SIZE, RtcMmioLayout};
use crate::serial::{SERIAL_MMIO_DEVICE_WINDOW_SIZE, SerialMmioDevice, SharedSerialOutput};
use crate::vsock::{
    PreparedVsockDevice, PreparedVsockDeviceError, VirtioVsockDeviceNotificationDispatch,
    VirtioVsockDeviceNotificationError, VirtioVsockMmioHandler, VsockMmioDeviceRegistration,
    VsockMmioLayout, VsockMmioRegistrationError,
};

const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Arm64BootResourceConfig<'a> {
    pub vcpu_mpidrs: &'a [u64],
    pub gic: Arm64FdtGic,
    pub timer: Arm64FdtTimerInterrupts,
    pub rtc_device: Option<Arm64BootRtcDeviceConfig>,
    pub serial_device: Option<Arm64BootSerialDeviceConfig>,
    pub block_mmio_layout: BlockMmioLayout,
    pub block_interrupt_lines: &'a [GuestInterruptLine],
    pub pmem_mmio_layout: PmemMmioLayout,
    pub pmem_interrupt_lines: &'a [GuestInterruptLine],
    pub network_mmio_layout: NetworkMmioLayout,
    pub network_interrupt_lines: &'a [GuestInterruptLine],
    pub vsock_mmio_layout: VsockMmioLayout,
    pub vsock_interrupt_line: Option<GuestInterruptLine>,
    pub balloon_mmio_layout: BalloonMmioLayout,
    pub balloon_interrupt_line: Option<GuestInterruptLine>,
    pub memory_hotplug_device: Option<Arm64BootMemoryHotplugDeviceConfig>,
    pub entropy_device: Option<Arm64BootEntropyDeviceConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64BootRtcDeviceConfig {
    pub mmio_layout: RtcMmioLayout,
}

impl Arm64BootRtcDeviceConfig {
    pub const fn new(mmio_layout: RtcMmioLayout) -> Self {
        Self { mmio_layout }
    }
}

#[derive(Debug, Clone)]
pub struct Arm64BootSerialDeviceConfig {
    pub region_id: MmioRegionId,
    pub address: crate::memory::GuestAddress,
    pub interrupt_line: GuestInterruptLine,
    pub output: SharedSerialOutput,
}

impl Arm64BootSerialDeviceConfig {
    pub fn new(
        region_id: MmioRegionId,
        address: crate::memory::GuestAddress,
        interrupt_line: GuestInterruptLine,
        output: SharedSerialOutput,
    ) -> Self {
        Self {
            region_id,
            address,
            interrupt_line,
            output,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64BootEntropyDeviceConfig {
    pub mmio_layout: EntropyMmioLayout,
    pub interrupt_line: GuestInterruptLine,
}

impl Arm64BootEntropyDeviceConfig {
    pub const fn new(mmio_layout: EntropyMmioLayout, interrupt_line: GuestInterruptLine) -> Self {
        Self {
            mmio_layout,
            interrupt_line,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64BootMemoryHotplugDeviceConfig {
    pub mmio_layout: VirtioMemMmioLayout,
    pub interrupt_line: GuestInterruptLine,
}

impl Arm64BootMemoryHotplugDeviceConfig {
    pub const fn new(mmio_layout: VirtioMemMmioLayout, interrupt_line: GuestInterruptLine) -> Self {
        Self {
            mmio_layout,
            interrupt_line,
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
    pub rtc_device: Option<Arm64BootRtcDevice>,
    pub serial_device: Option<Arm64BootSerialDevice>,
    pub block_devices: Vec<Arm64BootBlockDevice>,
    pub pmem_devices: Vec<PreparedPmemDevice>,
    pub pmem_mmio_devices: Vec<Arm64BootPmemDevice>,
    pub network_devices: Vec<Arm64BootNetworkDevice>,
    pub vsock_device: Option<Arm64BootVsockDevice>,
    pub balloon_device: Option<Arm64BootBalloonDevice>,
    pub memory_hotplug_device: Option<Arm64BootMemoryHotplugDevice>,
    pub entropy_device: Option<Arm64BootEntropyDevice>,
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
    pub rtc_device: Option<Arm64BootRtcDevice>,
    pub serial_device: Option<Arm64BootSerialDevice>,
    pub block_devices: Vec<Arm64BootBlockDevice>,
    pub pmem_devices: Vec<PreparedPmemDevice>,
    pub pmem_mmio_devices: Vec<Arm64BootPmemDevice>,
    pub network_devices: Vec<Arm64BootNetworkDevice>,
    pub vsock_device: Option<Arm64BootVsockDevice>,
    pub balloon_device: Option<Arm64BootBalloonDevice>,
    pub memory_hotplug_device: Option<Arm64BootMemoryHotplugDevice>,
    pub entropy_device: Option<Arm64BootEntropyDevice>,
}

#[derive(Debug)]
pub enum Arm64BootVsockWakeupFdsError {
    HandlerLookup { source: MmioHandlerLookupError },
    ResultAllocation { source: TryReserveError },
}

impl fmt::Display for Arm64BootVsockWakeupFdsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HandlerLookup { source } => {
                write!(f, "failed to find boot vsock MMIO handler: {source}")
            }
            Self::ResultAllocation { source } => {
                write!(f, "failed to allocate boot vsock wakeup fd list: {source}")
            }
        }
    }
}

impl std::error::Error for Arm64BootVsockWakeupFdsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HandlerLookup { source } => Some(source),
            Self::ResultAllocation { source } => Some(source),
        }
    }
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

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.devices
            .iter()
            .filter_map(Arm64BootBlockNotificationDispatch::rate_limiter_retry_after)
            .min()
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

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.outcome.rate_limiter_retry_after()
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

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        match self {
            Self::Dispatched(dispatch) => dispatch
                .queue_dispatch()
                .and_then(|dispatch| dispatch.rate_limiter_retry_after()),
            Self::DispatchFailed(source) => source
                .completed_dispatch()
                .and_then(|dispatch| dispatch.rate_limiter_retry_after()),
            Self::HandlerLookupFailed(_) => None,
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
pub struct Arm64BootPmemNotificationDispatches {
    devices: Vec<Arm64BootPmemNotificationDispatch>,
}

impl Arm64BootPmemNotificationDispatches {
    fn new(devices: Vec<Arm64BootPmemNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[Arm64BootPmemNotificationDispatch] {
        &self.devices
    }

    pub fn into_vec(self) -> Vec<Arm64BootPmemNotificationDispatch> {
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
            .any(Arm64BootPmemNotificationDispatch::needs_queue_interrupt)
    }
}

#[derive(Debug)]
pub struct Arm64BootPmemNotificationDispatch {
    device: Arm64BootPmemDevice,
    outcome: Arm64BootPmemNotificationOutcome,
}

impl Arm64BootPmemNotificationDispatch {
    fn new(device: Arm64BootPmemDevice, outcome: Arm64BootPmemNotificationOutcome) -> Self {
        Self { device, outcome }
    }

    pub const fn device(&self) -> &Arm64BootPmemDevice {
        &self.device
    }

    pub const fn outcome(&self) -> &Arm64BootPmemNotificationOutcome {
        &self.outcome
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.outcome.needs_queue_interrupt()
    }
}

#[derive(Debug)]
pub enum Arm64BootPmemNotificationOutcome {
    Dispatched(VirtioPmemDeviceNotificationDispatch),
    DispatchFailed(VirtioPmemDeviceNotificationError),
    HandlerLookupFailed(MmioHandlerLookupError),
}

impl Arm64BootPmemNotificationOutcome {
    pub fn needs_queue_interrupt(&self) -> bool {
        match self {
            Self::Dispatched(dispatch) => dispatch.needs_queue_interrupt(),
            Self::DispatchFailed(source) => source
                .completed_dispatch()
                .is_some_and(crate::pmem::VirtioPmemQueueDispatch::needs_queue_interrupt),
            Self::HandlerLookupFailed(_) => false,
        }
    }

    pub const fn dispatched(&self) -> Option<&VirtioPmemDeviceNotificationDispatch> {
        match self {
            Self::Dispatched(dispatch) => Some(dispatch),
            Self::DispatchFailed(_) | Self::HandlerLookupFailed(_) => None,
        }
    }

    pub const fn dispatch_error(&self) -> Option<&VirtioPmemDeviceNotificationError> {
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
pub enum Arm64BootPmemNotificationDispatchError {
    ResultAllocation { source: TryReserveError },
}

impl fmt::Display for Arm64BootPmemNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResultAllocation { source } => {
                write!(f, "failed to allocate pmem notification results: {source}")
            }
        }
    }
}

impl std::error::Error for Arm64BootPmemNotificationDispatchError {
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

#[derive(Debug)]
pub struct Arm64BootVsockNotificationDispatches {
    devices: Vec<Arm64BootVsockNotificationDispatch>,
}

impl Arm64BootVsockNotificationDispatches {
    fn new(devices: Vec<Arm64BootVsockNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[Arm64BootVsockNotificationDispatch] {
        &self.devices
    }

    pub fn into_vec(self) -> Vec<Arm64BootVsockNotificationDispatch> {
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
            .any(Arm64BootVsockNotificationDispatch::needs_queue_interrupt)
    }
}

#[derive(Debug)]
pub struct Arm64BootVsockNotificationDispatch {
    device: Arm64BootVsockDevice,
    outcome: Arm64BootVsockNotificationOutcome,
}

impl Arm64BootVsockNotificationDispatch {
    fn new(device: Arm64BootVsockDevice, outcome: Arm64BootVsockNotificationOutcome) -> Self {
        Self { device, outcome }
    }

    pub const fn device(&self) -> &Arm64BootVsockDevice {
        &self.device
    }

    pub const fn outcome(&self) -> &Arm64BootVsockNotificationOutcome {
        &self.outcome
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.outcome.needs_queue_interrupt()
    }
}

#[derive(Debug)]
pub enum Arm64BootVsockNotificationOutcome {
    Dispatched(Box<VirtioVsockDeviceNotificationDispatch>),
    DispatchFailed(VirtioVsockDeviceNotificationError),
    HandlerLookupFailed(MmioHandlerLookupError),
}

impl Arm64BootVsockNotificationOutcome {
    pub fn needs_queue_interrupt(&self) -> bool {
        match self {
            Self::Dispatched(dispatch) => dispatch.needs_queue_interrupt(),
            Self::DispatchFailed(source) => {
                source
                    .completed_tx_dispatch()
                    .is_some_and(crate::vsock::VirtioVsockTxQueueDispatch::needs_queue_interrupt)
                    || source.completed_rx_dispatch().is_some_and(
                        crate::vsock::VirtioVsockRxQueueDispatch::needs_queue_interrupt,
                    )
            }
            Self::HandlerLookupFailed(_) => false,
        }
    }

    pub fn dispatched(&self) -> Option<&VirtioVsockDeviceNotificationDispatch> {
        match self {
            Self::Dispatched(dispatch) => Some(dispatch.as_ref()),
            Self::DispatchFailed(_) | Self::HandlerLookupFailed(_) => None,
        }
    }

    pub const fn dispatch_error(&self) -> Option<&VirtioVsockDeviceNotificationError> {
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
pub enum Arm64BootVsockNotificationDispatchError {
    ResultAllocation { source: TryReserveError },
}

impl fmt::Display for Arm64BootVsockNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResultAllocation { source } => {
                write!(f, "failed to allocate vsock notification results: {source}")
            }
        }
    }
}

impl std::error::Error for Arm64BootVsockNotificationDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ResultAllocation { source } => Some(source),
        }
    }
}

#[derive(Debug)]
pub struct Arm64BootBalloonNotificationDispatches {
    devices: Vec<Arm64BootBalloonNotificationDispatch>,
}

impl Arm64BootBalloonNotificationDispatches {
    fn new(devices: Vec<Arm64BootBalloonNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[Arm64BootBalloonNotificationDispatch] {
        &self.devices
    }

    pub fn into_vec(self) -> Vec<Arm64BootBalloonNotificationDispatch> {
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
            .any(Arm64BootBalloonNotificationDispatch::needs_queue_interrupt)
    }
}

#[derive(Debug)]
pub struct Arm64BootBalloonNotificationDispatch {
    device: Arm64BootBalloonDevice,
    outcome: Arm64BootBalloonNotificationOutcome,
}

impl Arm64BootBalloonNotificationDispatch {
    fn new(device: Arm64BootBalloonDevice, outcome: Arm64BootBalloonNotificationOutcome) -> Self {
        Self { device, outcome }
    }

    pub const fn device(&self) -> &Arm64BootBalloonDevice {
        &self.device
    }

    pub const fn outcome(&self) -> &Arm64BootBalloonNotificationOutcome {
        &self.outcome
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.outcome.needs_queue_interrupt()
    }
}

#[derive(Debug)]
pub enum Arm64BootBalloonNotificationOutcome {
    Dispatched(Box<VirtioBalloonDeviceNotificationDispatch>),
    DispatchFailed(VirtioBalloonDeviceNotificationError),
    HandlerLookupFailed(MmioHandlerLookupError),
}

impl Arm64BootBalloonNotificationOutcome {
    pub fn needs_queue_interrupt(&self) -> bool {
        match self {
            Self::Dispatched(dispatch) => dispatch.needs_queue_interrupt(),
            Self::DispatchFailed(source) => source.completed_notification_dispatch().is_some_and(
                crate::balloon::VirtioBalloonDeviceNotificationDispatch::needs_queue_interrupt,
            ),
            Self::HandlerLookupFailed(_) => false,
        }
    }

    pub fn dispatched(&self) -> Option<&VirtioBalloonDeviceNotificationDispatch> {
        match self {
            Self::Dispatched(dispatch) => Some(dispatch.as_ref()),
            Self::DispatchFailed(_) | Self::HandlerLookupFailed(_) => None,
        }
    }

    pub const fn dispatch_error(&self) -> Option<&VirtioBalloonDeviceNotificationError> {
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
pub enum Arm64BootBalloonNotificationDispatchError {
    ResultAllocation { source: TryReserveError },
}

impl fmt::Display for Arm64BootBalloonNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate balloon notification results: {source}"
                )
            }
        }
    }
}

impl std::error::Error for Arm64BootBalloonNotificationDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ResultAllocation { source } => Some(source),
        }
    }
}

#[derive(Debug)]
pub struct Arm64BootMemoryHotplugNotificationDispatches {
    devices: Vec<Arm64BootMemoryHotplugNotificationDispatch>,
}

impl Arm64BootMemoryHotplugNotificationDispatches {
    fn new(devices: Vec<Arm64BootMemoryHotplugNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[Arm64BootMemoryHotplugNotificationDispatch] {
        &self.devices
    }

    pub fn into_vec(self) -> Vec<Arm64BootMemoryHotplugNotificationDispatch> {
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
            .any(Arm64BootMemoryHotplugNotificationDispatch::needs_queue_interrupt)
    }
}

#[derive(Debug)]
pub struct Arm64BootMemoryHotplugNotificationDispatch {
    device: Arm64BootMemoryHotplugDevice,
    outcome: Arm64BootMemoryHotplugNotificationOutcome,
}

impl Arm64BootMemoryHotplugNotificationDispatch {
    fn new(
        device: Arm64BootMemoryHotplugDevice,
        outcome: Arm64BootMemoryHotplugNotificationOutcome,
    ) -> Self {
        Self { device, outcome }
    }

    pub const fn device(&self) -> &Arm64BootMemoryHotplugDevice {
        &self.device
    }

    pub const fn outcome(&self) -> &Arm64BootMemoryHotplugNotificationOutcome {
        &self.outcome
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.outcome.needs_queue_interrupt()
    }
}

#[derive(Debug)]
pub enum Arm64BootMemoryHotplugNotificationOutcome {
    Dispatched(VirtioMemDeviceNotificationDispatch),
    DispatchFailed(VirtioMemDeviceNotificationError),
    HandlerLookupFailed(MmioHandlerLookupError),
}

impl Arm64BootMemoryHotplugNotificationOutcome {
    pub fn needs_queue_interrupt(&self) -> bool {
        match self {
            Self::Dispatched(dispatch) => dispatch.needs_queue_interrupt(),
            Self::DispatchFailed(source) => source
                .completed_dispatch()
                .is_some_and(crate::memory_hotplug::VirtioMemQueueDispatch::needs_queue_interrupt),
            Self::HandlerLookupFailed(_) => false,
        }
    }

    pub const fn dispatched(&self) -> Option<&VirtioMemDeviceNotificationDispatch> {
        match self {
            Self::Dispatched(dispatch) => Some(dispatch),
            Self::DispatchFailed(_) | Self::HandlerLookupFailed(_) => None,
        }
    }

    pub const fn dispatch_error(&self) -> Option<&VirtioMemDeviceNotificationError> {
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
pub enum Arm64BootMemoryHotplugNotificationDispatchError {
    ResultAllocation { source: TryReserveError },
}

impl fmt::Display for Arm64BootMemoryHotplugNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate memory-hotplug notification results: {source}"
                )
            }
        }
    }
}

impl std::error::Error for Arm64BootMemoryHotplugNotificationDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ResultAllocation { source } => Some(source),
        }
    }
}

#[derive(Debug)]
pub struct Arm64BootEntropyNotificationDispatches {
    devices: Vec<Arm64BootEntropyNotificationDispatch>,
}

impl Arm64BootEntropyNotificationDispatches {
    fn new(devices: Vec<Arm64BootEntropyNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[Arm64BootEntropyNotificationDispatch] {
        &self.devices
    }

    pub fn into_vec(self) -> Vec<Arm64BootEntropyNotificationDispatch> {
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
            .any(Arm64BootEntropyNotificationDispatch::needs_queue_interrupt)
    }

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.devices
            .iter()
            .filter_map(Arm64BootEntropyNotificationDispatch::rate_limiter_retry_after)
            .min()
    }
}

#[derive(Debug)]
pub struct Arm64BootEntropyNotificationDispatch {
    device: Arm64BootEntropyDevice,
    outcome: Arm64BootEntropyNotificationOutcome,
}

impl Arm64BootEntropyNotificationDispatch {
    fn new(device: Arm64BootEntropyDevice, outcome: Arm64BootEntropyNotificationOutcome) -> Self {
        Self { device, outcome }
    }

    pub const fn device(&self) -> &Arm64BootEntropyDevice {
        &self.device
    }

    pub const fn outcome(&self) -> &Arm64BootEntropyNotificationOutcome {
        &self.outcome
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.outcome.needs_queue_interrupt()
    }

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.outcome.rate_limiter_retry_after()
    }
}

#[derive(Debug)]
pub enum Arm64BootEntropyNotificationOutcome {
    Dispatched(VirtioRngDeviceNotificationDispatch),
    DispatchFailed(VirtioRngDeviceNotificationError),
    HandlerLookupFailed(MmioHandlerLookupError),
    EntropySourceProviderFailed(Arm64BootEntropySourceError),
}

impl Arm64BootEntropyNotificationOutcome {
    pub fn needs_queue_interrupt(&self) -> bool {
        match self {
            Self::Dispatched(dispatch) => dispatch.needs_queue_interrupt(),
            Self::DispatchFailed(source) => source
                .completed_dispatch()
                .is_some_and(crate::entropy::VirtioRngQueueDispatch::needs_queue_interrupt),
            Self::HandlerLookupFailed(_) | Self::EntropySourceProviderFailed(_) => false,
        }
    }

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        match self {
            Self::Dispatched(dispatch) => dispatch.rate_limiter_retry_after(),
            Self::DispatchFailed(source) => source.rate_limiter_retry_after(),
            Self::HandlerLookupFailed(_) | Self::EntropySourceProviderFailed(_) => None,
        }
    }

    pub const fn dispatched(&self) -> Option<&VirtioRngDeviceNotificationDispatch> {
        match self {
            Self::Dispatched(dispatch) => Some(dispatch),
            Self::DispatchFailed(_)
            | Self::HandlerLookupFailed(_)
            | Self::EntropySourceProviderFailed(_) => None,
        }
    }

    pub const fn dispatch_error(&self) -> Option<&VirtioRngDeviceNotificationError> {
        match self {
            Self::DispatchFailed(source) => Some(source),
            Self::Dispatched(_)
            | Self::HandlerLookupFailed(_)
            | Self::EntropySourceProviderFailed(_) => None,
        }
    }

    pub const fn handler_lookup_error(&self) -> Option<&MmioHandlerLookupError> {
        match self {
            Self::HandlerLookupFailed(source) => Some(source),
            Self::Dispatched(_)
            | Self::DispatchFailed(_)
            | Self::EntropySourceProviderFailed(_) => None,
        }
    }

    pub const fn entropy_source_error(&self) -> Option<&Arm64BootEntropySourceError> {
        match self {
            Self::EntropySourceProviderFailed(source) => Some(source),
            Self::Dispatched(_) | Self::DispatchFailed(_) | Self::HandlerLookupFailed(_) => None,
        }
    }
}

pub struct Arm64BootEntropySource<'a> {
    source: &'a mut dyn VirtioRngEntropySource,
}

impl<'a> Arm64BootEntropySource<'a> {
    pub fn new(source: &'a mut dyn VirtioRngEntropySource) -> Self {
        Self { source }
    }

    fn into_inner(self) -> &'a mut dyn VirtioRngEntropySource {
        self.source
    }
}

impl fmt::Debug for Arm64BootEntropySource<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Arm64BootEntropySource")
            .finish_non_exhaustive()
    }
}

pub trait Arm64BootEntropySourceProvider {
    fn entropy_source(
        &mut self,
        device: &Arm64BootEntropyDevice,
    ) -> Result<Arm64BootEntropySource<'_>, Arm64BootEntropySourceError>;
}

impl Arm64BootEntropySourceProvider for VirtioRngOsEntropySource {
    fn entropy_source(
        &mut self,
        _device: &Arm64BootEntropyDevice,
    ) -> Result<Arm64BootEntropySource<'_>, Arm64BootEntropySourceError> {
        Ok(Arm64BootEntropySource::new(self))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm64BootEntropySourceError {
    message: String,
}

impl Arm64BootEntropySourceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Arm64BootEntropySourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Arm64BootEntropySourceError {}

#[derive(Debug)]
pub enum Arm64BootEntropyNotificationDispatchError {
    ResultAllocation { source: TryReserveError },
}

impl fmt::Display for Arm64BootEntropyNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate entropy notification results: {source}"
                )
            }
        }
    }
}

impl std::error::Error for Arm64BootEntropyNotificationDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ResultAllocation { source } => Some(source),
        }
    }
}

#[derive(Debug)]
struct Arm64BootNoopEntropySource;

impl VirtioRngEntropySource for Arm64BootNoopEntropySource {
    fn fill_entropy(&mut self, _destination: &mut [u8]) -> Result<(), VirtioRngEntropySourceError> {
        Err(VirtioRngEntropySourceError::new())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm64BootBlockDevice {
    pub registration: BlockMmioDeviceRegistration,
    pub fdt_device: Arm64FdtVirtioMmioDevice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm64BootPmemDevice {
    pub registration: PmemMmioDeviceRegistration,
    pub fdt_device: Arm64FdtVirtioMmioDevice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm64BootNetworkDevice {
    pub registration: NetworkMmioDeviceRegistration,
    pub fdt_device: Arm64FdtVirtioMmioDevice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm64BootVsockDevice {
    pub registration: VsockMmioDeviceRegistration,
    pub fdt_device: Arm64FdtVirtioMmioDevice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm64BootBalloonDevice {
    pub registration: BalloonMmioDeviceRegistration,
    pub fdt_device: Arm64FdtVirtioMmioDevice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm64BootMemoryHotplugDevice {
    pub registration: VirtioMemMmioDeviceRegistration,
    pub fdt_device: Arm64FdtVirtioMmioDevice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm64BootEntropyDevice {
    pub registration: EntropyMmioDeviceRegistration,
    pub fdt_device: Arm64FdtVirtioMmioDevice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm64BootRtcDevice {
    pub region: MmioRegion,
    pub fdt_device: Arm64FdtRtcDevice,
}

#[derive(Debug, Clone)]
pub struct Arm64BootSerialDevice {
    pub region: MmioRegion,
    pub output: SharedSerialOutput,
    pub fdt_device: Arm64FdtSerialDevice,
}

impl Arm64BootRuntimeResources {
    pub fn update_block_device_backing(
        &self,
        mmio_dispatcher: &mut MmioDispatcher,
        config: &DriveConfig,
    ) -> Result<(), DriveUpdateError> {
        update_block_device_backing_for_devices(&self.block_devices, mmio_dispatcher, config)
    }

    pub fn vsock_host_read_wakeup_fds(
        &self,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Result<Vec<RawFd>, Arm64BootVsockWakeupFdsError> {
        let Some(device) = self.vsock_device.as_ref() else {
            return Ok(Vec::new());
        };

        let region_id = device.registration.region_id();
        let handler = mmio_dispatcher
            .handler_mut::<VirtioVsockMmioHandler>(region_id)
            .map_err(|source| Arm64BootVsockWakeupFdsError::HandlerLookup { source })?;

        handler
            .activation_handler()
            .host_read_wakeup_fds()
            .map_err(|source| Arm64BootVsockWakeupFdsError::ResultAllocation { source })
    }

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

    pub fn has_pending_pmem_queue_notifications(
        &self,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> bool {
        self.pmem_mmio_devices.iter().any(|device| {
            let region_id = device.registration.region_id();
            mmio_dispatcher
                .handler_mut::<VirtioPmemMmioHandler>(region_id)
                .is_ok_and(|handler| handler.has_pending_queue_notifications())
        })
    }

    pub fn dispatch_pmem_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        mmio_dispatcher: &mut MmioDispatcher,
        flush_status: VirtioPmemFlushStatus,
    ) -> Result<Arm64BootPmemNotificationDispatches, Arm64BootPmemNotificationDispatchError> {
        let mut devices = Vec::new();
        devices
            .try_reserve_exact(self.pmem_mmio_devices.len())
            .map_err(
                |source| Arm64BootPmemNotificationDispatchError::ResultAllocation { source },
            )?;

        for device in self.pmem_mmio_devices.iter().cloned() {
            let region_id = device.registration.region_id();
            let outcome = match mmio_dispatcher.handler_mut::<VirtioPmemMmioHandler>(region_id) {
                Ok(handler) => {
                    match handler.dispatch_pmem_queue_notifications(memory, flush_status) {
                        Ok(dispatch) => Arm64BootPmemNotificationOutcome::Dispatched(dispatch),
                        Err(source) => Arm64BootPmemNotificationOutcome::DispatchFailed(source),
                    }
                }
                Err(source) => Arm64BootPmemNotificationOutcome::HandlerLookupFailed(source),
            };
            devices.push(Arm64BootPmemNotificationDispatch::new(device, outcome));
        }

        Ok(Arm64BootPmemNotificationDispatches::new(devices))
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

    pub fn dispatch_vsock_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Result<Arm64BootVsockNotificationDispatches, Arm64BootVsockNotificationDispatchError> {
        let mut devices = Vec::new();
        let device_count = if self.vsock_device.is_some() { 1 } else { 0 };
        devices.try_reserve_exact(device_count).map_err(|source| {
            Arm64BootVsockNotificationDispatchError::ResultAllocation { source }
        })?;

        if let Some(device) = self.vsock_device.clone() {
            let region_id = device.registration.region_id();
            let outcome = match mmio_dispatcher.handler_mut::<VirtioVsockMmioHandler>(region_id) {
                Ok(handler) => match handler.dispatch_vsock_queue_notifications(memory) {
                    Ok(dispatch) => {
                        Arm64BootVsockNotificationOutcome::Dispatched(Box::new(dispatch))
                    }
                    Err(source) => Arm64BootVsockNotificationOutcome::DispatchFailed(source),
                },
                Err(source) => Arm64BootVsockNotificationOutcome::HandlerLookupFailed(source),
            };
            devices.push(Arm64BootVsockNotificationDispatch::new(device, outcome));
        }

        Ok(Arm64BootVsockNotificationDispatches::new(devices))
    }

    pub fn dispatch_balloon_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Result<Arm64BootBalloonNotificationDispatches, Arm64BootBalloonNotificationDispatchError>
    {
        let mut devices = Vec::new();
        let device_count = if self.balloon_device.is_some() { 1 } else { 0 };
        devices.try_reserve_exact(device_count).map_err(|source| {
            Arm64BootBalloonNotificationDispatchError::ResultAllocation { source }
        })?;

        if let Some(device) = self.balloon_device.clone() {
            let region_id = device.registration.region_id();
            let outcome = match mmio_dispatcher.handler_mut::<VirtioBalloonMmioHandler>(region_id) {
                Ok(handler) => match handler.dispatch_balloon_queue_notifications(memory) {
                    Ok(dispatch) => {
                        Arm64BootBalloonNotificationOutcome::Dispatched(Box::new(dispatch))
                    }
                    Err(source) => Arm64BootBalloonNotificationOutcome::DispatchFailed(source),
                },
                Err(source) => Arm64BootBalloonNotificationOutcome::HandlerLookupFailed(source),
            };
            devices.push(Arm64BootBalloonNotificationDispatch::new(device, outcome));
        }

        Ok(Arm64BootBalloonNotificationDispatches::new(devices))
    }

    pub fn dispatch_memory_hotplug_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Result<
        Arm64BootMemoryHotplugNotificationDispatches,
        Arm64BootMemoryHotplugNotificationDispatchError,
    > {
        let mut devices = Vec::new();
        let device_count = if self.memory_hotplug_device.is_some() {
            1
        } else {
            0
        };
        devices.try_reserve_exact(device_count).map_err(|source| {
            Arm64BootMemoryHotplugNotificationDispatchError::ResultAllocation { source }
        })?;

        if let Some(device) = self.memory_hotplug_device.clone() {
            let region_id = device.registration.region_id();
            let outcome = match mmio_dispatcher.handler_mut::<VirtioMemMmioHandler>(region_id) {
                Ok(handler) => match handler.dispatch_mem_queue_notifications(memory) {
                    Ok(dispatch) => Arm64BootMemoryHotplugNotificationOutcome::Dispatched(dispatch),
                    Err(source) => {
                        Arm64BootMemoryHotplugNotificationOutcome::DispatchFailed(source)
                    }
                },
                Err(source) => {
                    Arm64BootMemoryHotplugNotificationOutcome::HandlerLookupFailed(source)
                }
            };
            devices.push(Arm64BootMemoryHotplugNotificationDispatch::new(
                device, outcome,
            ));
        }

        Ok(Arm64BootMemoryHotplugNotificationDispatches::new(devices))
    }

    pub fn trigger_balloon_statistics_update(
        &mut self,
        memory: &mut GuestMemory,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Result<Arm64BootBalloonNotificationDispatches, Arm64BootBalloonNotificationDispatchError>
    {
        let mut devices = Vec::new();
        let device_count = if self.balloon_device.is_some() { 1 } else { 0 };
        devices.try_reserve_exact(device_count).map_err(|source| {
            Arm64BootBalloonNotificationDispatchError::ResultAllocation { source }
        })?;

        if let Some(device) = self.balloon_device.clone() {
            let region_id = device.registration.region_id();
            let outcome = match mmio_dispatcher.handler_mut::<VirtioBalloonMmioHandler>(region_id) {
                Ok(handler) => match handler.trigger_balloon_statistics_update(memory) {
                    Ok(dispatch) => {
                        Arm64BootBalloonNotificationOutcome::Dispatched(Box::new(dispatch))
                    }
                    Err(source) => Arm64BootBalloonNotificationOutcome::DispatchFailed(source),
                },
                Err(source) => Arm64BootBalloonNotificationOutcome::HandlerLookupFailed(source),
            };
            devices.push(Arm64BootBalloonNotificationDispatch::new(device, outcome));
        }

        Ok(Arm64BootBalloonNotificationDispatches::new(devices))
    }

    pub fn dispatch_entropy_queue_notifications_with_source(
        &mut self,
        memory: &mut GuestMemory,
        mmio_dispatcher: &mut MmioDispatcher,
        entropy_source: &mut impl Arm64BootEntropySourceProvider,
    ) -> Result<Arm64BootEntropyNotificationDispatches, Arm64BootEntropyNotificationDispatchError>
    {
        let mut devices = Vec::new();
        let device_count = if self.entropy_device.is_some() { 1 } else { 0 };
        devices.try_reserve_exact(device_count).map_err(|source| {
            Arm64BootEntropyNotificationDispatchError::ResultAllocation { source }
        })?;

        if let Some(device) = self.entropy_device.clone() {
            let region_id = device.registration.region_id();
            let outcome = match mmio_dispatcher.handler_mut::<VirtioRngMmioHandler>(region_id) {
                Ok(handler) => {
                    if handler.has_pending_rng_queue_work() {
                        match entropy_source.entropy_source(&device) {
                            Ok(source) => match handler
                                .dispatch_rng_queue_notifications(memory, source.into_inner())
                            {
                                Ok(dispatch) => {
                                    Arm64BootEntropyNotificationOutcome::Dispatched(dispatch)
                                }
                                Err(source) => {
                                    Arm64BootEntropyNotificationOutcome::DispatchFailed(source)
                                }
                            },
                            Err(source) => {
                                Arm64BootEntropyNotificationOutcome::EntropySourceProviderFailed(
                                    source,
                                )
                            }
                        }
                    } else {
                        let mut source = Arm64BootNoopEntropySource;
                        match handler.dispatch_rng_queue_notifications(memory, &mut source) {
                            Ok(dispatch) => {
                                Arm64BootEntropyNotificationOutcome::Dispatched(dispatch)
                            }
                            Err(source) => {
                                Arm64BootEntropyNotificationOutcome::DispatchFailed(source)
                            }
                        }
                    }
                }
                Err(source) => Arm64BootEntropyNotificationOutcome::HandlerLookupFailed(source),
            };
            devices.push(Arm64BootEntropyNotificationDispatch::new(device, outcome));
        }

        Ok(Arm64BootEntropyNotificationDispatches::new(devices))
    }
}

pub fn update_block_device_backing_for_devices(
    block_devices: &[Arm64BootBlockDevice],
    mmio_dispatcher: &mut MmioDispatcher,
    config: &DriveConfig,
) -> Result<(), DriveUpdateError> {
    let region_id = block_device_region_id(block_devices, config)?;
    let backing =
        BlockFileBacking::open(config).map_err(|source| DriveUpdateError::OpenBacking {
            drive_id: config.drive_id().to_string(),
            message: source.to_string(),
        })?;

    update_block_device_backing_for_region_with_opened(mmio_dispatcher, region_id, config, backing)
}

pub fn update_block_device_backing_for_devices_with_opened(
    block_devices: &[Arm64BootBlockDevice],
    mmio_dispatcher: &mut MmioDispatcher,
    config: &DriveConfig,
    backing: BlockFileBacking,
) -> Result<(), DriveUpdateError> {
    update_block_device_for_devices_with_opened(
        block_devices,
        mmio_dispatcher,
        config,
        Some(backing),
        None,
    )
}

pub fn update_block_device_for_devices_with_opened(
    block_devices: &[Arm64BootBlockDevice],
    mmio_dispatcher: &mut MmioDispatcher,
    config: &DriveConfig,
    backing: Option<BlockFileBacking>,
    rate_limiter_update: Option<DriveRateLimiterConfig>,
) -> Result<(), DriveUpdateError> {
    let region_id = block_device_region_id(block_devices, config)?;

    update_block_device_for_region_with_opened(
        mmio_dispatcher,
        region_id,
        config,
        backing,
        rate_limiter_update,
    )
}

fn block_device_region_id(
    block_devices: &[Arm64BootBlockDevice],
    config: &DriveConfig,
) -> Result<MmioRegionId, DriveUpdateError> {
    let Some(device) = block_devices
        .iter()
        .find(|device| device.registration.drive_id() == config.drive_id())
    else {
        return Err(DriveUpdateError::UnknownDrive {
            drive_id: config.drive_id().to_string(),
        });
    };

    Ok(device.registration.region_id())
}

fn update_block_device_backing_for_region_with_opened(
    mmio_dispatcher: &mut MmioDispatcher,
    region_id: MmioRegionId,
    config: &DriveConfig,
    backing: BlockFileBacking,
) -> Result<(), DriveUpdateError> {
    update_block_device_for_region_with_opened(
        mmio_dispatcher,
        region_id,
        config,
        Some(backing),
        None,
    )
}

fn update_block_device_for_region_with_opened(
    mmio_dispatcher: &mut MmioDispatcher,
    region_id: MmioRegionId,
    config: &DriveConfig,
    backing: Option<BlockFileBacking>,
    rate_limiter_update: Option<DriveRateLimiterConfig>,
) -> Result<(), DriveUpdateError> {
    let handler = mmio_dispatcher
        .handler_mut::<VirtioBlockMmioHandler>(region_id)
        .map_err(|source| DriveUpdateError::HandlerLookup {
            drive_id: config.drive_id().to_string(),
            region_id,
            message: source.to_string(),
        })?;

    if let Some(backing) = backing {
        handler.refresh_block_backing_with_opened(config, backing);
    }
    if let Some(rate_limiter) = rate_limiter_update {
        handler.update_block_rate_limiter(rate_limiter);
    }

    Ok(())
}

pub fn update_balloon_config_for_device(
    device: &Arm64BootBalloonDevice,
    mmio_dispatcher: &mut MmioDispatcher,
    config: BalloonConfig,
) -> Result<(), BalloonUpdateError> {
    mmio_dispatcher
        .handler_mut::<VirtioBalloonMmioHandler>(device.registration.region_id())
        .map_err(BalloonUpdateError::HandlerLookup)?
        .update_balloon_config(config)
}

pub fn update_balloon_statistics_for_device(
    device: &Arm64BootBalloonDevice,
    mmio_dispatcher: &mut MmioDispatcher,
    input: BalloonStatsUpdateInput,
) -> Result<(), BalloonUpdateError> {
    mmio_dispatcher
        .handler_mut::<VirtioBalloonMmioHandler>(device.registration.region_id())
        .map_err(BalloonUpdateError::HandlerLookup)?
        .update_balloon_statistics(input)
}

pub fn start_balloon_hinting_for_device(
    device: &Arm64BootBalloonDevice,
    mmio_dispatcher: &mut MmioDispatcher,
    input: BalloonHintingStartInput,
) -> Result<(), BalloonHintingCommandError> {
    mmio_dispatcher
        .handler_mut::<VirtioBalloonMmioHandler>(device.registration.region_id())
        .map_err(BalloonHintingCommandError::HandlerLookup)?
        .start_balloon_hinting(input)
}

pub fn stop_balloon_hinting_for_device(
    device: &Arm64BootBalloonDevice,
    mmio_dispatcher: &mut MmioDispatcher,
) -> Result<(), BalloonHintingCommandError> {
    mmio_dispatcher
        .handler_mut::<VirtioBalloonMmioHandler>(device.registration.region_id())
        .map_err(BalloonHintingCommandError::HandlerLookup)?
        .stop_balloon_hinting()
}

pub fn balloon_stats_for_device(
    device: &Arm64BootBalloonDevice,
    mmio_dispatcher: &mut MmioDispatcher,
    config: BalloonConfig,
) -> Result<BalloonStats, BalloonStatsError> {
    let handler = mmio_dispatcher
        .handler_mut::<VirtioBalloonMmioHandler>(device.registration.region_id())
        .map_err(BalloonStatsError::HandlerLookup)?;
    let actual_pages = handler
        .activation_handler()
        .memory_accounting()
        .inflated_page_count();
    let optional_stats = handler.activation_handler().statistics();

    BalloonStats::from_config_actual_pages_and_optional_stats(config, actual_pages, optional_stats)
}

pub fn balloon_hinting_status_for_device(
    device: &Arm64BootBalloonDevice,
    mmio_dispatcher: &mut MmioDispatcher,
) -> Result<BalloonHintingStatus, BalloonHintingStatusError> {
    mmio_dispatcher
        .handler_mut::<VirtioBalloonMmioHandler>(device.registration.region_id())
        .map_err(BalloonHintingStatusError::HandlerLookup)?
        .balloon_hinting_status()
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
    RootDriveCommandLine {
        source: BootCommandLineError,
    },
    PrepareBlockDevices {
        source: PreparedBlockDeviceError,
    },
    PreparePmemDevices {
        source: PreparedPmemDeviceError,
    },
    PrepareNetworkDevices {
        source: PreparedNetworkDeviceError,
    },
    PrepareVsockDevice {
        source: PreparedVsockDeviceError,
    },
    PrepareBalloonDevice {
        source: BalloonPageCountOverflow,
    },
    PrepareMemoryHotplugDevice {
        source: VirtioMemPrepareError,
    },
    RegisterBlockMmio {
        source: Box<BlockMmioRegistrationError>,
    },
    RegisterPmemMmio {
        source: Box<PmemMmioRegistrationError>,
    },
    RegisterNetworkMmio {
        source: Box<NetworkMmioRegistrationError>,
    },
    RegisterVsockMmio {
        source: Box<VsockMmioRegistrationError>,
    },
    RegisterBalloonMmio {
        source: Box<BalloonMmioRegistrationError>,
    },
    RegisterMemoryHotplugMmio {
        source: Box<VirtioMemMmioRegistrationError>,
    },
    RegisterEntropyMmio {
        source: Box<EntropyMmioRegistrationError>,
    },
    RegisterRtcMmio {
        source: Box<Arm64BootRtcMmioRegistrationError>,
    },
    RegisterSerialMmio {
        source: Box<Arm64BootSerialMmioRegistrationError>,
    },
    BlockInterruptLineCount {
        devices: usize,
        lines: usize,
    },
    PmemInterruptLineCount {
        devices: usize,
        lines: usize,
    },
    NetworkInterruptLineCount {
        devices: usize,
        lines: usize,
    },
    VsockInterruptLineCount {
        devices: usize,
        lines: usize,
    },
    BalloonInterruptLineCount {
        devices: usize,
        lines: usize,
    },
    MemoryHotplugInterruptLineCount {
        devices: usize,
        lines: usize,
    },
    BlockDeviceMetadataAllocation {
        source: TryReserveError,
    },
    PmemDeviceMetadataAllocation {
        source: TryReserveError,
    },
    NetworkDeviceMetadataAllocation {
        source: TryReserveError,
    },
    VsockDeviceMetadataAllocation {
        source: TryReserveError,
    },
    BalloonDeviceMetadataAllocation {
        source: TryReserveError,
    },
    MemoryHotplugDeviceMetadataAllocation {
        source: TryReserveError,
    },
    EntropyDeviceMetadataAllocation {
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
            Self::RootDriveCommandLine { source } => {
                write!(
                    f,
                    "failed to append root-drive kernel command-line arguments: {source}"
                )
            }
            Self::PrepareBlockDevices { source } => {
                write!(f, "failed to prepare block devices: {source}")
            }
            Self::PreparePmemDevices { source } => {
                write!(f, "failed to prepare pmem devices: {source}")
            }
            Self::PrepareNetworkDevices { source } => {
                write!(f, "failed to prepare network devices: {source}")
            }
            Self::PrepareVsockDevice { source } => {
                write!(f, "failed to prepare vsock device: {source}")
            }
            Self::PrepareBalloonDevice { source } => {
                write!(f, "failed to prepare balloon device: {source}")
            }
            Self::PrepareMemoryHotplugDevice { source } => {
                write!(f, "failed to prepare memory hotplug device: {source}")
            }
            Self::RegisterBlockMmio { source } => {
                write!(f, "failed to register block MMIO devices: {source}")
            }
            Self::RegisterPmemMmio { source } => {
                write!(f, "failed to register pmem MMIO devices: {source}")
            }
            Self::RegisterNetworkMmio { source } => {
                write!(f, "failed to register network MMIO devices: {source}")
            }
            Self::RegisterVsockMmio { source } => {
                write!(f, "failed to register vsock MMIO device: {source}")
            }
            Self::RegisterBalloonMmio { source } => {
                write!(f, "failed to register balloon MMIO device: {source}")
            }
            Self::RegisterMemoryHotplugMmio { source } => {
                write!(f, "failed to register memory hotplug MMIO device: {source}")
            }
            Self::RegisterEntropyMmio { source } => {
                write!(f, "failed to register entropy MMIO device: {source}")
            }
            Self::RegisterRtcMmio { source } => {
                write!(f, "failed to register RTC MMIO device: {source}")
            }
            Self::RegisterSerialMmio { source } => {
                write!(f, "failed to register serial MMIO device: {source}")
            }
            Self::BlockInterruptLineCount { devices, lines } => write!(
                f,
                "block MMIO device count {devices} does not match interrupt line count {lines}"
            ),
            Self::PmemInterruptLineCount { devices, lines } => write!(
                f,
                "pmem MMIO device count {devices} does not match interrupt line count {lines}"
            ),
            Self::NetworkInterruptLineCount { devices, lines } => write!(
                f,
                "network MMIO device count {devices} does not match interrupt line count {lines}"
            ),
            Self::VsockInterruptLineCount { devices, lines } => write!(
                f,
                "vsock MMIO device count {devices} does not match interrupt line count {lines}"
            ),
            Self::BalloonInterruptLineCount { devices, lines } => write!(
                f,
                "balloon MMIO device count {devices} does not match interrupt line count {lines}"
            ),
            Self::MemoryHotplugInterruptLineCount { devices, lines } => write!(
                f,
                "memory hotplug MMIO device count {devices} does not match interrupt line count {lines}"
            ),
            Self::BlockDeviceMetadataAllocation { source } => {
                write!(f, "failed to allocate block device metadata: {source}")
            }
            Self::PmemDeviceMetadataAllocation { source } => {
                write!(f, "failed to allocate pmem device metadata: {source}")
            }
            Self::NetworkDeviceMetadataAllocation { source } => {
                write!(f, "failed to allocate network device metadata: {source}")
            }
            Self::VsockDeviceMetadataAllocation { source } => {
                write!(f, "failed to allocate vsock device metadata: {source}")
            }
            Self::BalloonDeviceMetadataAllocation { source } => {
                write!(f, "failed to allocate balloon device metadata: {source}")
            }
            Self::MemoryHotplugDeviceMetadataAllocation { source } => {
                write!(
                    f,
                    "failed to allocate memory hotplug device metadata: {source}"
                )
            }
            Self::EntropyDeviceMetadataAllocation { source } => {
                write!(f, "failed to allocate entropy device metadata: {source}")
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
            Self::RootDriveCommandLine { source } => Some(source),
            Self::PrepareBlockDevices { source } => Some(source),
            Self::PreparePmemDevices { source } => Some(source),
            Self::PrepareNetworkDevices { source } => Some(source),
            Self::PrepareVsockDevice { source } => Some(source),
            Self::PrepareBalloonDevice { source } => Some(source),
            Self::PrepareMemoryHotplugDevice { source } => Some(source),
            Self::RegisterBlockMmio { source } => Some(source.as_ref()),
            Self::RegisterPmemMmio { source } => Some(source.as_ref()),
            Self::RegisterNetworkMmio { source } => Some(source.as_ref()),
            Self::RegisterVsockMmio { source } => Some(source.as_ref()),
            Self::RegisterBalloonMmio { source } => Some(source.as_ref()),
            Self::RegisterMemoryHotplugMmio { source } => Some(source.as_ref()),
            Self::RegisterEntropyMmio { source } => Some(source.as_ref()),
            Self::RegisterRtcMmio { source } => Some(source.as_ref()),
            Self::RegisterSerialMmio { source } => Some(source.as_ref()),
            Self::BlockDeviceMetadataAllocation { source } => Some(source),
            Self::PmemDeviceMetadataAllocation { source } => Some(source),
            Self::NetworkDeviceMetadataAllocation { source } => Some(source),
            Self::VsockDeviceMetadataAllocation { source } => Some(source),
            Self::BalloonDeviceMetadataAllocation { source } => Some(source),
            Self::MemoryHotplugDeviceMetadataAllocation { source } => Some(source),
            Self::EntropyDeviceMetadataAllocation { source } => Some(source),
            Self::Fdt { source } => Some(source),
            Self::MissingBootSource
            | Self::MemorySizeOverflow { .. }
            | Self::MemorySizeExceedsArchitecturalMaximum { .. }
            | Self::BlockInterruptLineCount { .. }
            | Self::PmemInterruptLineCount { .. }
            | Self::NetworkInterruptLineCount { .. }
            | Self::VsockInterruptLineCount { .. }
            | Self::BalloonInterruptLineCount { .. }
            | Self::MemoryHotplugInterruptLineCount { .. } => None,
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

#[derive(Debug)]
pub enum Arm64BootRtcMmioRegistrationError {
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

impl fmt::Display for Arm64BootRtcMmioRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InsertRegion {
                region_id,
                address,
                source,
            } => write!(
                f,
                "failed to insert RTC MMIO region id={region_id} at {address}: {source}"
            ),
            Self::RegisterHandler { region_id, source } => write!(
                f,
                "failed to register RTC MMIO handler for region id={region_id}: {source}"
            ),
        }
    }
}

impl std::error::Error for Arm64BootRtcMmioRegistrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InsertRegion { source, .. } => Some(source),
            Self::RegisterHandler { source, .. } => Some(source),
        }
    }
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
            rtc_device,
            serial_device,
            block_mmio_layout,
            block_interrupt_lines,
            pmem_mmio_layout,
            pmem_interrupt_lines,
            network_mmio_layout,
            network_interrupt_lines,
            vsock_mmio_layout,
            vsock_interrupt_line,
            balloon_mmio_layout,
            balloon_interrupt_line,
            memory_hotplug_device,
            entropy_device,
        } = config;
        let boot_source_config = controller
            .boot_source_config()
            .ok_or(Arm64BootResourceError::MissingBootSource)?;
        validate_block_interrupt_line_count(
            controller.drive_configs().len(),
            block_interrupt_lines.len(),
        )?;
        validate_pmem_interrupt_line_count(
            controller.pmem_configs().len(),
            pmem_interrupt_lines.len(),
        )?;
        validate_network_interrupt_line_count(
            controller.network_interface_configs().len(),
            network_interrupt_lines.len(),
        )?;
        validate_vsock_interrupt_line_count(
            controller.vsock_config().is_some(),
            vsock_interrupt_line.is_some(),
        )?;
        validate_balloon_interrupt_line_count(
            controller.balloon_config().is_some(),
            balloon_interrupt_line.is_some(),
        )?;
        validate_memory_hotplug_interrupt_line_count(
            controller.memory_hotplug_config().is_some(),
            memory_hotplug_device.is_some(),
        )?;

        let machine_config = controller.machine_config();
        let memory_size = memory_size_bytes(machine_config)?;
        let layout = aarch64::dram_layout(memory_size)
            .map_err(|source| Arm64BootResourceError::MemoryLayout { source })?;
        let mut memory = GuestMemory::allocate(&layout)
            .map_err(|source| Arm64BootResourceError::GuestMemoryAllocation { source })?;
        let boot_source = boot_source_from_config(boot_source_config);
        let mut loaded_boot_source = boot_source
            .load(&layout, &mut memory)
            .map_err(|source| Arm64BootResourceError::BootSourceLoad { source })?;
        append_root_drive_command_line(&mut loaded_boot_source, controller.drive_configs())?;
        let prepared_pmems =
            PreparedPmemDevices::from_config_slice_with_layout(controller.pmem_configs(), &layout)
                .map_err(|source| Arm64BootResourceError::PreparePmemDevices { source })?;

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
        let pmem_mmio = prepared_pmems
            .register_mmio_with_dispatcher(pmem_mmio_layout, mmio_dispatcher)
            .map_err(|source| Arm64BootResourceError::RegisterPmemMmio {
                source: Box::new(source),
            })?;
        let (mmio_dispatcher, pmem_registrations, pmem_devices) = pmem_mmio.into_parts();
        let (pmem_mmio_devices, pmem_fdt_devices) =
            pmem_device_metadata(&pmem_registrations, pmem_interrupt_lines)?;
        fdt_devices
            .try_reserve_exact(pmem_fdt_devices.len())
            .map_err(|source| Arm64BootResourceError::PmemDeviceMetadataAllocation { source })?;
        fdt_devices.extend(pmem_fdt_devices);
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
        let vsock_device = match (controller.vsock_config(), vsock_interrupt_line) {
            (Some(config), Some(interrupt_line)) => {
                let prepared_vsock = PreparedVsockDevice::from_config_with_host_socket(config)
                    .map_err(|source| Arm64BootResourceError::PrepareVsockDevice { source })?;
                let vsock_mmio = prepared_vsock
                    .register_mmio_with_dispatcher(vsock_mmio_layout, mmio_dispatcher)
                    .map_err(|source| Arm64BootResourceError::RegisterVsockMmio {
                        source: Box::new(source),
                    })?;
                let (dispatcher, registration) = vsock_mmio.into_parts();
                mmio_dispatcher = dispatcher;
                let (device, fdt_device) =
                    arm64_boot_vsock_device_metadata(registration, interrupt_line);
                fdt_devices.try_reserve_exact(1).map_err(|source| {
                    Arm64BootResourceError::VsockDeviceMetadataAllocation { source }
                })?;
                fdt_devices.push(fdt_device);
                Some(device)
            }
            (None, None) => None,
            (Some(_), None) | (None, Some(_)) => {
                return Err(vsock_interrupt_line_count_error(
                    controller.vsock_config().is_some(),
                    vsock_interrupt_line.is_some(),
                ));
            }
        };
        let balloon_device = match (controller.balloon_config(), balloon_interrupt_line) {
            (Some(config), Some(interrupt_line)) => {
                let prepared_balloon = PreparedBalloonDevice::from_config(config)
                    .map_err(|source| Arm64BootResourceError::PrepareBalloonDevice { source })?;
                let balloon_mmio = prepared_balloon
                    .register_mmio_with_dispatcher(balloon_mmio_layout, mmio_dispatcher)
                    .map_err(|source| Arm64BootResourceError::RegisterBalloonMmio {
                        source: Box::new(source),
                    })?;
                let (dispatcher, registration) = balloon_mmio.into_parts();
                mmio_dispatcher = dispatcher;
                let (device, fdt_device) =
                    arm64_boot_balloon_device_metadata(registration, interrupt_line);
                fdt_devices.try_reserve_exact(1).map_err(|source| {
                    Arm64BootResourceError::BalloonDeviceMetadataAllocation { source }
                })?;
                fdt_devices.push(fdt_device);
                Some(device)
            }
            (None, None) => None,
            (Some(_), None) | (None, Some(_)) => {
                return Err(balloon_interrupt_line_count_error(
                    controller.balloon_config().is_some(),
                    balloon_interrupt_line.is_some(),
                ));
            }
        };
        let memory_hotplug_device =
            match (controller.memory_hotplug_config(), memory_hotplug_device) {
                (Some(config), Some(device_config)) => {
                    let prepared_mem =
                        PreparedVirtioMemDevice::from_config(config).map_err(|source| {
                            Arm64BootResourceError::PrepareMemoryHotplugDevice { source }
                        })?;
                    let mem_mmio = prepared_mem
                        .register_mmio_with_dispatcher(device_config.mmio_layout, mmio_dispatcher)
                        .map_err(|source| Arm64BootResourceError::RegisterMemoryHotplugMmio {
                            source: Box::new(source),
                        })?;
                    let (dispatcher, registration) = mem_mmio.into_parts();
                    mmio_dispatcher = dispatcher;
                    let (device, fdt_device) = arm64_boot_memory_hotplug_device_metadata(
                        registration,
                        device_config.interrupt_line,
                    );
                    fdt_devices.try_reserve_exact(1).map_err(|source| {
                        Arm64BootResourceError::MemoryHotplugDeviceMetadataAllocation { source }
                    })?;
                    fdt_devices.push(fdt_device);
                    Some(device)
                }
                (None, None) => None,
                (Some(_), None) | (None, Some(_)) => {
                    return Err(memory_hotplug_interrupt_line_count_error(
                        controller.memory_hotplug_config().is_some(),
                        memory_hotplug_device.is_some(),
                    ));
                }
            };
        let entropy_device = match entropy_device {
            Some(config) => {
                let entropy_mmio = PreparedEntropyDevice::from_config(
                    controller.entropy_config().unwrap_or_default(),
                )
                .register_mmio_with_dispatcher(config.mmio_layout, mmio_dispatcher)
                .map_err(|source| Arm64BootResourceError::RegisterEntropyMmio {
                    source: Box::new(source),
                })?;
                let (dispatcher, registration) = entropy_mmio.into_parts();
                mmio_dispatcher = dispatcher;
                let (device, fdt_device) =
                    arm64_boot_entropy_device_metadata(registration, config.interrupt_line);
                fdt_devices.try_reserve_exact(1).map_err(|source| {
                    Arm64BootResourceError::EntropyDeviceMetadataAllocation { source }
                })?;
                fdt_devices.push(fdt_device);
                Some(device)
            }
            None => None,
        };
        let rtc_device = rtc_device
            .map(|rtc| register_rtc_mmio(&mut mmio_dispatcher, rtc))
            .transpose()?;
        let serial_device = serial_device
            .map(|serial| register_serial_mmio(&mut mmio_dispatcher, serial))
            .transpose()?;
        let rtc_fdt_device = rtc_device.as_ref().map(|device| device.fdt_device);
        let serial_fdt_device = serial_device.as_ref().map(|device| device.fdt_device);
        let fdt = write_arm64_fdt(
            &Arm64FdtConfig {
                layout: &layout,
                boot: Arm64FdtBootInfo::from(&loaded_boot_source),
                vcpu_mpidrs,
                gic,
                timer,
                rtc_device: rtc_fdt_device,
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
            rtc_device,
            serial_device,
            block_devices,
            pmem_devices,
            pmem_mmio_devices,
            network_devices,
            vsock_device,
            balloon_device,
            memory_hotplug_device,
            entropy_device,
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
                rtc_device: self.rtc_device,
                serial_device: self.serial_device,
                block_devices: self.block_devices,
                pmem_devices: self.pmem_devices,
                pmem_mmio_devices: self.pmem_mmio_devices,
                network_devices: self.network_devices,
                vsock_device: self.vsock_device,
                balloon_device: self.balloon_device,
                memory_hotplug_device: self.memory_hotplug_device,
                entropy_device: self.entropy_device,
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

fn append_root_drive_command_line(
    loaded_boot_source: &mut LoadedBootSource,
    drive_configs: &[DriveConfig],
) -> Result<(), Arm64BootResourceError> {
    if let Some(root_drive) = drive_configs.iter().find(|config| config.is_root_device()) {
        let root_arg = root_drive
            .partuuid()
            .map(|partuuid| format!("root=PARTUUID={partuuid}"))
            .unwrap_or_else(|| "root=/dev/vda".to_string());
        let mode_arg = if root_drive.is_read_only() {
            "ro"
        } else {
            "rw"
        };
        loaded_boot_source.command_line = loaded_boot_source
            .command_line
            .with_appended_kernel_args([root_arg.as_str(), mode_arg])
            .map_err(|source| Arm64BootResourceError::RootDriveCommandLine { source })?;
    }

    Ok(())
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

fn validate_pmem_interrupt_line_count(
    devices: usize,
    lines: usize,
) -> Result<(), Arm64BootResourceError> {
    if devices == lines {
        Ok(())
    } else {
        Err(Arm64BootResourceError::PmemInterruptLineCount { devices, lines })
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

fn validate_vsock_interrupt_line_count(
    configured: bool,
    line_present: bool,
) -> Result<(), Arm64BootResourceError> {
    if configured == line_present {
        Ok(())
    } else {
        Err(vsock_interrupt_line_count_error(configured, line_present))
    }
}

fn validate_balloon_interrupt_line_count(
    configured: bool,
    line_present: bool,
) -> Result<(), Arm64BootResourceError> {
    if configured == line_present {
        Ok(())
    } else {
        Err(balloon_interrupt_line_count_error(configured, line_present))
    }
}

fn validate_memory_hotplug_interrupt_line_count(
    configured: bool,
    line_present: bool,
) -> Result<(), Arm64BootResourceError> {
    if configured == line_present {
        Ok(())
    } else {
        Err(memory_hotplug_interrupt_line_count_error(
            configured,
            line_present,
        ))
    }
}

fn vsock_interrupt_line_count_error(
    configured: bool,
    line_present: bool,
) -> Arm64BootResourceError {
    Arm64BootResourceError::VsockInterruptLineCount {
        devices: usize::from(configured),
        lines: usize::from(line_present),
    }
}

fn balloon_interrupt_line_count_error(
    configured: bool,
    line_present: bool,
) -> Arm64BootResourceError {
    Arm64BootResourceError::BalloonInterruptLineCount {
        devices: usize::from(configured),
        lines: usize::from(line_present),
    }
}

fn memory_hotplug_interrupt_line_count_error(
    configured: bool,
    line_present: bool,
) -> Arm64BootResourceError {
    Arm64BootResourceError::MemoryHotplugInterruptLineCount {
        devices: usize::from(configured),
        lines: usize::from(line_present),
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

fn pmem_device_metadata(
    registrations: &[PmemMmioDeviceRegistration],
    interrupt_lines: &[GuestInterruptLine],
) -> Result<(Vec<Arm64BootPmemDevice>, Vec<Arm64FdtVirtioMmioDevice>), Arm64BootResourceError> {
    validate_pmem_interrupt_line_count(registrations.len(), interrupt_lines.len())?;

    let mut pmem_devices = Vec::new();
    pmem_devices
        .try_reserve_exact(registrations.len())
        .map_err(|source| Arm64BootResourceError::PmemDeviceMetadataAllocation { source })?;
    let mut fdt_devices = Vec::new();
    fdt_devices
        .try_reserve_exact(registrations.len())
        .map_err(|source| Arm64BootResourceError::PmemDeviceMetadataAllocation { source })?;

    for (registration, interrupt_line) in registrations.iter().zip(interrupt_lines) {
        let range = registration.region().range();
        let fdt_device = Arm64FdtVirtioMmioDevice {
            region: Arm64FdtRegion {
                base: range.start().raw_value(),
                size: range.size(),
            },
            interrupt_line: *interrupt_line,
        };
        pmem_devices.push(Arm64BootPmemDevice {
            registration: registration.clone(),
            fdt_device,
        });
        fdt_devices.push(fdt_device);
    }

    Ok((pmem_devices, fdt_devices))
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

fn arm64_boot_vsock_device_metadata(
    registration: VsockMmioDeviceRegistration,
    interrupt_line: GuestInterruptLine,
) -> (Arm64BootVsockDevice, Arm64FdtVirtioMmioDevice) {
    let range = registration.region().range();
    let fdt_device = Arm64FdtVirtioMmioDevice {
        region: Arm64FdtRegion {
            base: range.start().raw_value(),
            size: range.size(),
        },
        interrupt_line,
    };

    (
        Arm64BootVsockDevice {
            registration,
            fdt_device,
        },
        fdt_device,
    )
}

fn arm64_boot_entropy_device_metadata(
    registration: EntropyMmioDeviceRegistration,
    interrupt_line: GuestInterruptLine,
) -> (Arm64BootEntropyDevice, Arm64FdtVirtioMmioDevice) {
    let range = registration.region().range();
    let fdt_device = Arm64FdtVirtioMmioDevice {
        region: Arm64FdtRegion {
            base: range.start().raw_value(),
            size: range.size(),
        },
        interrupt_line,
    };

    (
        Arm64BootEntropyDevice {
            registration,
            fdt_device,
        },
        fdt_device,
    )
}

fn arm64_boot_balloon_device_metadata(
    registration: BalloonMmioDeviceRegistration,
    interrupt_line: GuestInterruptLine,
) -> (Arm64BootBalloonDevice, Arm64FdtVirtioMmioDevice) {
    let range = registration.region().range();
    let fdt_device = Arm64FdtVirtioMmioDevice {
        region: Arm64FdtRegion {
            base: range.start().raw_value(),
            size: range.size(),
        },
        interrupt_line,
    };

    (
        Arm64BootBalloonDevice {
            registration,
            fdt_device,
        },
        fdt_device,
    )
}

fn arm64_boot_memory_hotplug_device_metadata(
    registration: VirtioMemMmioDeviceRegistration,
    interrupt_line: GuestInterruptLine,
) -> (Arm64BootMemoryHotplugDevice, Arm64FdtVirtioMmioDevice) {
    let range = registration.region().range();
    let fdt_device = Arm64FdtVirtioMmioDevice {
        region: Arm64FdtRegion {
            base: range.start().raw_value(),
            size: range.size(),
        },
        interrupt_line,
    };

    (
        Arm64BootMemoryHotplugDevice {
            registration,
            fdt_device,
        },
        fdt_device,
    )
}

fn register_rtc_mmio(
    dispatcher: &mut MmioDispatcher,
    config: Arm64BootRtcDeviceConfig,
) -> Result<Arm64BootRtcDevice, Arm64BootResourceError> {
    let region_id = config.mmio_layout.region_id();
    let address = config.mmio_layout.base();
    let region = dispatcher
        .insert_region(region_id, address, RTC_MMIO_DEVICE_WINDOW_SIZE)
        .map_err(|source| Arm64BootResourceError::RegisterRtcMmio {
            source: Box::new(Arm64BootRtcMmioRegistrationError::InsertRegion {
                region_id,
                address,
                source,
            }),
        })?;

    dispatcher
        .register_handler(region_id, Pl031RtcDevice::system())
        .map_err(|source| Arm64BootResourceError::RegisterRtcMmio {
            source: Box::new(Arm64BootRtcMmioRegistrationError::RegisterHandler {
                region_id,
                source,
            }),
        })?;

    let fdt_device = Arm64FdtRtcDevice {
        region: Arm64FdtRegion {
            base: region.range().start().raw_value(),
            size: region.range().size(),
        },
    };

    Ok(Arm64BootRtcDevice { region, fdt_device })
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
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use device_tree::DeviceTree;

    use super::{
        Arm64BootEntropySource, Arm64BootEntropySourceError, Arm64BootEntropySourceProvider,
        Arm64BootNetworkNotificationOutcome, Arm64BootNetworkPacketIo,
        Arm64BootNetworkPacketIoError, Arm64BootNetworkPacketIoProvider, Arm64BootResourceConfig,
        Arm64BootResourceError, Arm64BootResources, Arm64BootRtcDeviceConfig,
        Arm64BootRtcMmioRegistrationError, Arm64BootSerialDeviceConfig,
        Arm64BootSerialMmioRegistrationError, MIB, arm64_boot_network_device_metadata,
        balloon_hinting_status_for_device, balloon_stats_for_device, block_device_metadata,
        start_balloon_hinting_for_device, stop_balloon_hinting_for_device,
        update_balloon_config_for_device, update_balloon_statistics_for_device,
        update_block_device_for_devices_with_opened,
    };
    use crate::VmmAction;
    use crate::balloon::{
        BalloonConfigInput, BalloonHintingCommandError, BalloonHintingStartInput,
        BalloonHintingStatusError, BalloonMmioLayout, BalloonStatsUpdateInput,
        VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX, VIRTIO_BALLOON_DEVICE_ID,
        VIRTIO_BALLOON_FREE_PAGE_HINT_DONE, VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
        VIRTIO_BALLOON_INFLATE_QUEUE_INDEX, VIRTIO_BALLOON_MIB_TO_4K_PAGES,
        VIRTIO_BALLOON_S_MEMFREE, VIRTIO_BALLOON_S_SWAP_OUT, VIRTIO_BALLOON_STAT_SIZE,
        VIRTIO_BALLOON_STATS_QUEUE_INDEX, VirtioBalloonMmioHandler,
    };
    use crate::block::{
        DriveConfigInput, DriveRateLimiterConfig, DriveTokenBucketConfig, DriveUpdateError,
        VIRTIO_BLOCK_REQUEST_HEADER_SIZE, VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
        VIRTIO_BLOCK_REQUEST_TYPE_IN, VIRTIO_BLOCK_SECTOR_SIZE, VIRTIO_BLOCK_STATUS_OK,
        VIRTIO_BLOCK_STATUS_SIZE, VirtioBlockMmioHandler,
    };
    use crate::boot::{
        BootCommandLineError, BootPayloadKind, BootSourceConfigInput, BootSourceLoadError,
        DEFAULT_KERNEL_COMMAND_LINE,
    };
    use crate::entropy::{
        EntropyConfigInput, EntropyMmioLayout, EntropyRateLimiterConfig, EntropyTokenBucketConfig,
        VirtioRngEntropySource, VirtioRngEntropySourceError, VirtioRngMmioHandler,
        VirtioRngOsEntropySource,
    };
    use crate::fdt::{Arm64FdtError, Arm64FdtGic, Arm64FdtRegion, Arm64FdtTimerInterrupts};
    use crate::interrupt::{DeviceInterruptKind, GuestInterruptLine};
    use crate::machine::{MachineConfig, MachineConfigInput};
    use crate::memory::{GuestAddress, aarch64};
    use crate::memory_hotplug::{
        MemoryHotplugConfigInput, VIRTIO_MEM_DEFAULT_REGION_ADDRESS, VIRTIO_MEM_REQUEST_SIZE,
        VIRTIO_MEM_RESPONSE_SIZE, VirtioMemMmioHandler, VirtioMemMmioLayout,
    };
    use crate::mmio::{
        MmioAccess, MmioAccessBytes, MmioBusError, MmioDispatchOutcome, MmioDispatcher,
        MmioHandler, MmioHandlerError, MmioOperation, MmioRegionId,
    };
    use crate::network::{
        NetworkInterfaceConfigInput, NetworkInterfaceConfigs, NetworkMmioDeviceRegistration,
        NetworkMmioLayout, PreparedNetworkDevices, VIRTIO_NET_RX_MIN_BUFFER_SIZE,
        VIRTIO_NET_RX_QUEUE_INDEX, VIRTIO_NET_TX_HEADER_SIZE, VIRTIO_NET_TX_QUEUE_INDEX,
        VirtioNetworkRxPacket, VirtioNetworkRxPacketSource, VirtioNetworkRxPacketSourceError,
        VirtioNetworkTxFrame, VirtioNetworkTxPacketSink, VirtioNetworkTxPacketSinkError,
    };
    use crate::pmem::{
        PmemConfigInput, PmemMmioLayout, PreparedPmemDeviceError, VIRTIO_PMEM_ALIGNMENT,
    };
    use crate::rtc::{Pl031RtcDevice, RTC_MMIO_DEVICE_WINDOW_SIZE, RtcMmioLayout};
    use crate::serial::{
        SERIAL_MMIO_DEVICE_WINDOW_SIZE, SERIAL_TRANSMIT_REGISTER_OFFSET, SerialMmioDevice,
        SerialOutputFile, SharedSerialOutput, SharedSerialOutputBuffer,
    };
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioRegister,
    };
    use crate::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE,
    };
    use crate::vsock::{
        VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE, VIRTIO_VSOCK_EVENT_QUEUE_INDEX, VIRTIO_VSOCK_HOST_CID,
        VIRTIO_VSOCK_OP_REQUEST, VIRTIO_VSOCK_OP_RESPONSE, VIRTIO_VSOCK_PACKET_HEADER_SIZE,
        VIRTIO_VSOCK_PACKET_TYPE_STREAM, VIRTIO_VSOCK_RX_QUEUE_INDEX, VIRTIO_VSOCK_TX_QUEUE_INDEX,
        VSOCK_HOST_LOCAL_PORT_BASE, VirtioVsockMmioHandler, VirtioVsockPacketHeader,
        VsockConfigInput, VsockHostSocketOwnerError, VsockMmioLayout,
    };

    static NEXT_TEST_FILE_ID: AtomicU64 = AtomicU64::new(0);

    const TEST_MEMORY_MIB: u64 = 8;
    const ARM64_IMAGE_HEADER_SIZE: usize = 64;
    const ARM64_IMAGE_TEXT_OFFSET_OFFSET: usize = 8;
    const ARM64_IMAGE_SIZE_OFFSET: usize = 16;
    const ARM64_IMAGE_MAGIC_OFFSET: usize = 56;
    const ARM64_IMAGE_MAGIC: u32 = 0x644d_5241;
    const TEST_BLOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_0000);
    const TEST_PMEM_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_9000);
    const TEST_NETWORK_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_4000);
    const TEST_VSOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_6000);
    const TEST_ENTROPY_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_7000);
    const TEST_BALLOON_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_8000);
    const TEST_MEMORY_HOTPLUG_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_a000);
    const TEST_RTC_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_1000);
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
    const SECOND_DATA_ADDR: GuestAddress = GuestAddress::new(0x804a_0000);
    const TEST_VSOCK_RX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8050_0000);
    const TEST_VSOCK_RX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8051_0000);
    const TEST_VSOCK_RX_USED_RING: GuestAddress = GuestAddress::new(0x8052_0000);
    const TEST_VSOCK_TX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8053_0000);
    const TEST_VSOCK_TX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8054_0000);
    const TEST_VSOCK_TX_USED_RING: GuestAddress = GuestAddress::new(0x8055_0000);
    const TEST_VSOCK_EVENT_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8056_0000);
    const TEST_VSOCK_EVENT_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8057_0000);
    const TEST_VSOCK_EVENT_USED_RING: GuestAddress = GuestAddress::new(0x8058_0000);
    const TEST_VSOCK_HEADER: GuestAddress = GuestAddress::new(0x8059_0000);
    const TEST_BALLOON_INFLATE_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8060_0000);
    const TEST_BALLOON_INFLATE_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8061_0000);
    const TEST_BALLOON_INFLATE_USED_RING: GuestAddress = GuestAddress::new(0x8062_0000);
    const TEST_BALLOON_DEFLATE_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8063_0000);
    const TEST_BALLOON_DEFLATE_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8064_0000);
    const TEST_BALLOON_DEFLATE_USED_RING: GuestAddress = GuestAddress::new(0x8065_0000);
    const TEST_BALLOON_PFN_PAYLOAD: GuestAddress = GuestAddress::new(0x8066_0000);
    const TEST_BALLOON_STATS_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8067_0000);
    const TEST_BALLOON_STATS_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8068_0000);
    const TEST_BALLOON_STATS_USED_RING: GuestAddress = GuestAddress::new(0x8069_0000);
    const TEST_BALLOON_STATS_PAYLOAD: GuestAddress = GuestAddress::new(0x806a_0000);
    const TEST_BALLOON_MAPPED_PFN: u32 = 0x80000;
    const TEST_QUEUE_DEVICE_STRIDE: u64 = 0x0010_0000;
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

    fn balloon_stat_payload_bytes(stats: &[(u16, u64)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (tag, value) in stats {
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    fn controller_with_kernel(kernel: &Path) -> crate::VmmController {
        controller_with_kernel_and_memory(kernel, TEST_MEMORY_MIB)
    }

    fn controller_with_kernel_and_memory(kernel: &Path, mem_size_mib: u64) -> crate::VmmController {
        controller_with_kernel_memory_and_boot_args(kernel, mem_size_mib, None)
    }

    fn controller_with_kernel_and_boot_args(
        kernel: &Path,
        boot_args: &str,
    ) -> crate::VmmController {
        controller_with_kernel_memory_and_boot_args(kernel, TEST_MEMORY_MIB, Some(boot_args))
    }

    fn controller_with_kernel_memory_and_boot_args(
        kernel: &Path,
        mem_size_mib: u64,
        boot_args: Option<&str>,
    ) -> crate::VmmController {
        let mut controller = crate::VmmController::new("test", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(
                1,
                mem_size_mib,
            )))
            .expect("machine config should be stored");
        let mut boot_source = BootSourceConfigInput::new(kernel.to_path_buf());
        if let Some(args) = boot_args {
            boot_source = boot_source.with_boot_args(args);
        }
        controller
            .handle_action(VmmAction::PutBootSource(boot_source))
            .expect("boot source should be stored");
        controller
    }

    fn add_drive(controller: &mut crate::VmmController, id: &str, path: &Path) {
        add_drive_with_root(controller, id, path, true);
    }

    fn add_drive_with_rate_limiter(
        controller: &mut crate::VmmController,
        id: &str,
        path: &Path,
        is_root_device: bool,
        rate_limiter: DriveRateLimiterConfig,
    ) {
        controller
            .handle_action(VmmAction::PutDrive(
                DriveConfigInput::new(id, id, path.to_path_buf(), is_root_device)
                    .with_rate_limiter(rate_limiter),
            ))
            .expect("drive config with rate limiter should be stored");
    }

    fn add_drive_with_root(
        controller: &mut crate::VmmController,
        id: &str,
        path: &Path,
        is_root_device: bool,
    ) {
        add_drive_with_options(controller, id, path, is_root_device, None, None);
    }

    fn add_drive_with_options(
        controller: &mut crate::VmmController,
        id: &str,
        path: &Path,
        is_root_device: bool,
        is_read_only: Option<bool>,
        partuuid: Option<&str>,
    ) {
        let mut input = DriveConfigInput::new(id, id, path.to_path_buf(), is_root_device);
        if let Some(read_only) = is_read_only {
            input = input.with_is_read_only(read_only);
        }
        if let Some(partuuid) = partuuid {
            input = input.with_partuuid(partuuid);
        }
        controller
            .handle_action(VmmAction::PutDrive(input))
            .expect("drive config should be stored");
    }

    fn add_pmem(controller: &mut crate::VmmController, id: &str, path: &Path, read_only: bool) {
        controller
            .handle_action(VmmAction::PutPmem(
                PmemConfigInput::new(id, path.to_string_lossy().into_owned())
                    .with_read_only(read_only),
            ))
            .expect("pmem config should be stored");
    }

    fn add_network(controller: &mut crate::VmmController, iface_id: &str, host_dev_name: &str) {
        controller
            .handle_action(VmmAction::PutNetworkInterface(
                NetworkInterfaceConfigInput::new(iface_id, iface_id, host_dev_name),
            ))
            .expect("network config should be stored");
    }

    fn add_vsock(controller: &mut crate::VmmController, guest_cid: u32, uds_path: &Path) {
        controller
            .handle_action(VmmAction::PutVsock(VsockConfigInput::new(
                guest_cid,
                uds_path.to_string_lossy().into_owned(),
            )))
            .expect("vsock config should be stored");
    }

    fn add_balloon(controller: &mut crate::VmmController, amount_mib: u32) {
        add_balloon_config(controller, BalloonConfigInput::new(amount_mib, false));
    }

    fn add_balloon_config(controller: &mut crate::VmmController, config: BalloonConfigInput) {
        controller
            .handle_action(VmmAction::PutBalloon(config))
            .expect("balloon config should be stored");
    }

    fn add_memory_hotplug(controller: &mut crate::VmmController) {
        controller
            .handle_action(VmmAction::PutMemoryHotplug(MemoryHotplugConfigInput::new(
                1024, 2, 128,
            )))
            .expect("memory hotplug config should be stored");
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
        valid_config_with_pmem_lines(lines, &[])
    }

    fn valid_config_with_pmem_lines<'a>(
        block_lines: &'a [GuestInterruptLine],
        pmem_lines: &'a [GuestInterruptLine],
    ) -> Arm64BootResourceConfig<'a> {
        Arm64BootResourceConfig {
            vcpu_mpidrs: &[0],
            gic: valid_gic(),
            timer: Arm64FdtTimerInterrupts::firecracker_default(),
            rtc_device: None,
            serial_device: None,
            block_mmio_layout: crate::block::BlockMmioLayout::new(
                TEST_BLOCK_MMIO_BASE,
                MmioRegionId::new(1),
            ),
            block_interrupt_lines: block_lines,
            pmem_mmio_layout: PmemMmioLayout::new(TEST_PMEM_MMIO_BASE, MmioRegionId::new(25)),
            pmem_interrupt_lines: pmem_lines,
            network_mmio_layout: NetworkMmioLayout::new(
                TEST_NETWORK_MMIO_BASE,
                MmioRegionId::new(50),
            ),
            network_interrupt_lines: &[],
            vsock_mmio_layout: VsockMmioLayout::new(TEST_VSOCK_MMIO_BASE, MmioRegionId::new(90)),
            vsock_interrupt_line: None,
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            memory_hotplug_device: None,
            entropy_device: None,
        }
    }

    fn boot_runtime_with_entropy(
        kernel_name: &str,
    ) -> (
        crate::memory::GuestMemory,
        super::Arm64BootRuntimeResources,
        MmioDispatcher,
    ) {
        boot_runtime_with_optional_entropy_rate_limiter(kernel_name, None)
    }

    fn boot_runtime_with_entropy_rate_limiter(
        kernel_name: &str,
        rate_limiter: EntropyRateLimiterConfig,
    ) -> (
        crate::memory::GuestMemory,
        super::Arm64BootRuntimeResources,
        MmioDispatcher,
    ) {
        boot_runtime_with_optional_entropy_rate_limiter(kernel_name, Some(rate_limiter))
    }

    fn boot_runtime_with_optional_entropy_rate_limiter(
        kernel_name: &str,
        rate_limiter: Option<EntropyRateLimiterConfig>,
    ) -> (
        crate::memory::GuestMemory,
        super::Arm64BootRuntimeResources,
        MmioDispatcher,
    ) {
        let kernel = temp_file(kernel_name, &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        if let Some(rate_limiter) = rate_limiter {
            controller
                .handle_action(VmmAction::PutEntropy(
                    EntropyConfigInput::new().with_rate_limiter(rate_limiter),
                ))
                .expect("entropy config with rate limiter should be stored");
        }
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                entropy_device: Some(super::Arm64BootEntropyDeviceConfig::new(
                    EntropyMmioLayout::new(TEST_ENTROPY_MMIO_BASE, MmioRegionId::new(100)),
                    line(36),
                )),
                ..valid_config(&[])
            },
        )
        .expect("boot resources should assemble with entropy device");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_balloon(
        kernel_name: &str,
    ) -> (
        crate::memory::GuestMemory,
        super::Arm64BootRuntimeResources,
        MmioDispatcher,
    ) {
        boot_runtime_with_balloon_target(kernel_name, TEST_MEMORY_MIB as u32)
    }

    fn boot_runtime_with_balloon_target(
        kernel_name: &str,
        amount_mib: u32,
    ) -> (
        crate::memory::GuestMemory,
        super::Arm64BootRuntimeResources,
        MmioDispatcher,
    ) {
        let kernel = temp_file(kernel_name, &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_balloon(&mut controller, amount_mib);
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                balloon_interrupt_line: Some(line(36)),
                ..valid_config(&[])
            },
        )
        .expect("boot resources should assemble with balloon device");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_balloon_config(
        kernel_name: &str,
        config: BalloonConfigInput,
    ) -> (
        crate::memory::GuestMemory,
        super::Arm64BootRuntimeResources,
        MmioDispatcher,
    ) {
        let kernel = temp_file(kernel_name, &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_balloon_config(&mut controller, config);
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                balloon_interrupt_line: Some(line(36)),
                ..valid_config(&[])
            },
        )
        .expect("boot resources should assemble with balloon device");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_memory_hotplug(
        kernel_name: &str,
    ) -> (
        crate::memory::GuestMemory,
        super::Arm64BootRuntimeResources,
        MmioDispatcher,
    ) {
        let kernel = temp_file(kernel_name, &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_memory_hotplug(&mut controller);
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                memory_hotplug_device: Some(super::Arm64BootMemoryHotplugDeviceConfig::new(
                    VirtioMemMmioLayout::new(TEST_MEMORY_HOTPLUG_MMIO_BASE, MmioRegionId::new(120)),
                    line(36),
                )),
                ..valid_config(&[])
            },
        )
        .expect("boot resources should assemble with memory-hotplug device");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
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
            Arm64BootSerialDeviceConfig::new(
                region_id,
                address,
                interrupt_line,
                SharedSerialOutput::from(output.clone()),
            ),
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

    fn write_boot_vsock_mmio_u32(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime
            .vsock_device
            .as_ref()
            .expect("vsock device should be configured")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("vsock MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        mmio_dispatcher
            .dispatch(
                MmioOperation::write(access, data).expect("u32 write operation should be valid"),
            )
            .expect("vsock MMIO write should dispatch");
    }

    fn read_boot_vsock_mmio_u32(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime
            .vsock_device
            .as_ref()
            .expect("vsock device should be configured")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("vsock MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read operation should be valid"))
            .expect("vsock MMIO read should dispatch");
        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not produce write outcome"),
        }
    }

    fn write_boot_balloon_mmio_u32(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should be configured")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("balloon MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        mmio_dispatcher
            .dispatch(
                MmioOperation::write(access, data).expect("u32 write operation should be valid"),
            )
            .expect("balloon MMIO write should dispatch");
    }

    fn read_boot_balloon_mmio_u32(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should be configured")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("balloon MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read operation should be valid"))
            .expect("balloon MMIO read should dispatch");
        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not produce write outcome"),
        }
    }

    fn write_boot_memory_hotplug_mmio_u32(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime
            .memory_hotplug_device
            .as_ref()
            .expect("memory-hotplug device should be configured")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("memory-hotplug MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        mmio_dispatcher
            .dispatch(
                MmioOperation::write(access, data).expect("u32 write operation should be valid"),
            )
            .expect("memory-hotplug MMIO write should dispatch");
    }

    fn read_boot_memory_hotplug_mmio_u32(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime
            .memory_hotplug_device
            .as_ref()
            .expect("memory-hotplug device should be configured")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("memory-hotplug MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read operation should be valid"))
            .expect("memory-hotplug MMIO read should dispatch");
        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not produce write outcome"),
        }
    }

    fn configure_boot_memory_hotplug_queue(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_ring: GuestAddress,
    ) {
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNum,
            u32::from(TEST_QUEUE_SIZE),
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDescLow,
            guest_address_low(TEST_DESCRIPTOR_TABLE),
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDriverLow,
            guest_address_low(TEST_AVAILABLE_RING),
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDeviceLow,
            guest_address_low(device_ring),
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueReady,
            1,
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn notify_boot_memory_hotplug_queue(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) {
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNotify,
            0,
        );
    }

    fn configure_boot_block_queue(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        device_ring: GuestAddress,
    ) {
        configure_boot_block_queue_at(
            runtime,
            mmio_dispatcher,
            device_index,
            TEST_DESCRIPTOR_TABLE,
            TEST_AVAILABLE_RING,
            device_ring,
            TEST_QUEUE_SIZE,
        );
    }

    fn configure_boot_block_queue_at(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        descriptor_table: GuestAddress,
        available_ring: GuestAddress,
        used_ring: GuestAddress,
        queue_size: u16,
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
            u32::from(queue_size),
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDescLow,
            guest_address_low(descriptor_table),
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDriverLow,
            guest_address_low(available_ring),
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDeviceLow,
            guest_address_low(used_ring),
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

    fn configure_boot_vsock_queue(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        queue_index: usize,
        descriptor_table: GuestAddress,
        driver_ring: GuestAddress,
        device_ring: GuestAddress,
    ) {
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueSel,
            u32::try_from(queue_index).expect("queue index should fit"),
        );
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNum,
            u32::from(TEST_QUEUE_SIZE),
        );
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDescLow,
            guest_address_low(descriptor_table),
        );
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDriverLow,
            guest_address_low(driver_ring),
        );
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDeviceLow,
            guest_address_low(device_ring),
        );
        write_boot_vsock_mmio_u32(runtime, mmio_dispatcher, VirtioMmioRegister::QueueReady, 1);
    }

    fn configure_boot_vsock_queues(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) {
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        configure_boot_vsock_queue(
            runtime,
            mmio_dispatcher,
            VIRTIO_VSOCK_RX_QUEUE_INDEX,
            TEST_VSOCK_RX_DESCRIPTOR_TABLE,
            TEST_VSOCK_RX_AVAILABLE_RING,
            TEST_VSOCK_RX_USED_RING,
        );
        configure_boot_vsock_queue(
            runtime,
            mmio_dispatcher,
            VIRTIO_VSOCK_TX_QUEUE_INDEX,
            TEST_VSOCK_TX_DESCRIPTOR_TABLE,
            TEST_VSOCK_TX_AVAILABLE_RING,
            TEST_VSOCK_TX_USED_RING,
        );
        configure_boot_vsock_queue(
            runtime,
            mmio_dispatcher,
            VIRTIO_VSOCK_EVENT_QUEUE_INDEX,
            TEST_VSOCK_EVENT_DESCRIPTOR_TABLE,
            TEST_VSOCK_EVENT_AVAILABLE_RING,
            TEST_VSOCK_EVENT_USED_RING,
        );
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn notify_boot_vsock_queue(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        queue_index: usize,
    ) {
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNotify,
            u32::try_from(queue_index).expect("queue index should fit"),
        );
    }

    fn configure_boot_balloon_queue(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        queue_index: usize,
        descriptor_table: GuestAddress,
        driver_ring: GuestAddress,
        device_ring: GuestAddress,
    ) {
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueSel,
            u32::try_from(queue_index).expect("queue index should fit"),
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNum,
            u32::from(TEST_QUEUE_SIZE),
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDescLow,
            guest_address_low(descriptor_table),
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDriverLow,
            guest_address_low(driver_ring),
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDeviceLow,
            guest_address_low(device_ring),
        );
        write_boot_balloon_mmio_u32(runtime, mmio_dispatcher, VirtioMmioRegister::QueueReady, 1);
    }

    fn configure_boot_balloon_queues(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) {
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        configure_boot_balloon_queue(
            runtime,
            mmio_dispatcher,
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
            TEST_BALLOON_INFLATE_DESCRIPTOR_TABLE,
            TEST_BALLOON_INFLATE_AVAILABLE_RING,
            TEST_BALLOON_INFLATE_USED_RING,
        );
        configure_boot_balloon_queue(
            runtime,
            mmio_dispatcher,
            VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
            TEST_BALLOON_DEFLATE_DESCRIPTOR_TABLE,
            TEST_BALLOON_DEFLATE_AVAILABLE_RING,
            TEST_BALLOON_DEFLATE_USED_RING,
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn configure_boot_balloon_statistics_queues(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) {
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        configure_boot_balloon_queue(
            runtime,
            mmio_dispatcher,
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
            TEST_BALLOON_INFLATE_DESCRIPTOR_TABLE,
            TEST_BALLOON_INFLATE_AVAILABLE_RING,
            TEST_BALLOON_INFLATE_USED_RING,
        );
        configure_boot_balloon_queue(
            runtime,
            mmio_dispatcher,
            VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
            TEST_BALLOON_DEFLATE_DESCRIPTOR_TABLE,
            TEST_BALLOON_DEFLATE_AVAILABLE_RING,
            TEST_BALLOON_DEFLATE_USED_RING,
        );
        configure_boot_balloon_queue(
            runtime,
            mmio_dispatcher,
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
            TEST_BALLOON_STATS_DESCRIPTOR_TABLE,
            TEST_BALLOON_STATS_AVAILABLE_RING,
            TEST_BALLOON_STATS_USED_RING,
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn notify_boot_balloon_queue(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        queue_index: usize,
    ) {
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNotify,
            u32::try_from(queue_index).expect("queue index should fit"),
        );
    }

    fn write_boot_entropy_mmio_u32(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime
            .entropy_device
            .as_ref()
            .expect("entropy device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test entropy MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("entropy MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        mmio_dispatcher
            .dispatch(
                MmioOperation::write(access, data).expect("u32 write operation should be valid"),
            )
            .expect("entropy MMIO write should dispatch");
    }

    fn read_boot_entropy_mmio_u32(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime
            .entropy_device
            .as_ref()
            .expect("entropy device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test entropy MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("entropy MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read operation should be valid"))
            .expect("entropy MMIO read should dispatch");
        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not produce write outcome"),
        }
    }

    fn configure_boot_entropy_queue(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_ring: GuestAddress,
    ) {
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNum,
            u32::from(TEST_QUEUE_SIZE),
        );
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDescLow,
            guest_address_low(TEST_DESCRIPTOR_TABLE),
        );
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDriverLow,
            guest_address_low(TEST_AVAILABLE_RING),
        );
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDeviceLow,
            guest_address_low(device_ring),
        );
        write_boot_entropy_mmio_u32(runtime, mmio_dispatcher, VirtioMmioRegister::QueueReady, 1);
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn notify_boot_entropy_queue(
        runtime: &mut super::Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) {
        write_boot_entropy_mmio_u32(runtime, mmio_dispatcher, VirtioMmioRegister::QueueNotify, 0);
    }

    fn write_boot_entropy_request(memory: &mut crate::memory::GuestMemory, len: u32) {
        write_descriptor(memory, 0, TestDescriptor::writable(DATA_ADDR, len, None));
        write_available_heads(memory, &[0]);
    }

    fn write_two_boot_entropy_requests(memory: &mut crate::memory::GuestMemory, len: u32) {
        write_descriptor(memory, 0, TestDescriptor::writable(DATA_ADDR, len, None));
        write_descriptor(
            memory,
            1,
            TestDescriptor::writable(SECOND_DATA_ADDR, len, None),
        );
        write_available_heads(memory, &[0, 1]);
    }

    fn write_partially_invalid_entropy_request(memory: &mut crate::memory::GuestMemory) {
        write_descriptor(memory, 0, TestDescriptor::writable(DATA_ADDR, 16, None));
        write_available_heads(memory, &[0, TEST_QUEUE_SIZE]);
    }

    fn write_queued_balloon_inflate_request(memory: &mut crate::memory::GuestMemory) {
        memory
            .write_slice(
                &TEST_BALLOON_MAPPED_PFN.to_le_bytes(),
                TEST_BALLOON_PFN_PAYLOAD,
            )
            .expect("balloon PFN payload should write");
        write_descriptor_at(
            memory,
            TEST_BALLOON_INFLATE_DESCRIPTOR_TABLE,
            0,
            TestDescriptor::readable(TEST_BALLOON_PFN_PAYLOAD, 4, None),
        );
        write_available_heads_at(memory, TEST_BALLOON_INFLATE_AVAILABLE_RING, &[0]);
    }

    fn write_partially_invalid_balloon_inflate_request(memory: &mut crate::memory::GuestMemory) {
        memory
            .write_slice(
                &TEST_BALLOON_MAPPED_PFN.to_le_bytes(),
                TEST_BALLOON_PFN_PAYLOAD,
            )
            .expect("balloon PFN payload should write");
        write_descriptor_at(
            memory,
            TEST_BALLOON_INFLATE_DESCRIPTOR_TABLE,
            0,
            TestDescriptor::readable(TEST_BALLOON_PFN_PAYLOAD, 4, None),
        );
        write_available_heads_at(
            memory,
            TEST_BALLOON_INFLATE_AVAILABLE_RING,
            &[0, TEST_QUEUE_SIZE],
        );
    }

    fn write_queued_balloon_deflate_request(memory: &mut crate::memory::GuestMemory) {
        memory
            .write_slice(
                &TEST_BALLOON_MAPPED_PFN.to_le_bytes(),
                TEST_BALLOON_PFN_PAYLOAD,
            )
            .expect("balloon PFN payload should write");
        write_descriptor_at(
            memory,
            TEST_BALLOON_DEFLATE_DESCRIPTOR_TABLE,
            0,
            TestDescriptor::readable(TEST_BALLOON_PFN_PAYLOAD, 4, None),
        );
        write_available_heads_at(memory, TEST_BALLOON_DEFLATE_AVAILABLE_RING, &[0]);
    }

    fn write_queued_balloon_statistics_request(memory: &mut crate::memory::GuestMemory) {
        let stats = balloon_stat_payload_bytes(&[
            (VIRTIO_BALLOON_S_SWAP_OUT, 9),
            (VIRTIO_BALLOON_S_MEMFREE, 0x5678),
        ]);
        assert_eq!(stats.len(), 2 * VIRTIO_BALLOON_STAT_SIZE);
        memory
            .write_slice(&stats, TEST_BALLOON_STATS_PAYLOAD)
            .expect("balloon statistics payload should write");
        write_descriptor_at(
            memory,
            TEST_BALLOON_STATS_DESCRIPTOR_TABLE,
            0,
            TestDescriptor::readable(
                TEST_BALLOON_STATS_PAYLOAD,
                u32::try_from(stats.len()).expect("stats payload length should fit"),
                None,
            ),
        );
        write_available_heads_at(memory, TEST_BALLOON_STATS_AVAILABLE_RING, &[0]);
    }

    fn write_queued_memory_hotplug_state_request(memory: &mut crate::memory::GuestMemory) {
        let mut request = Vec::new();
        request.extend_from_slice(&3u16.to_le_bytes());
        request.extend_from_slice(&[0; 6]);
        request.extend_from_slice(&VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value().to_le_bytes());
        request.extend_from_slice(&1u16.to_le_bytes());
        request.extend_from_slice(&[0; 6]);
        assert_eq!(request.len(), VIRTIO_MEM_REQUEST_SIZE);
        memory
            .write_slice(&request, HEADER_ADDR)
            .expect("virtio-mem request should write");
        write_descriptor(
            memory,
            0,
            TestDescriptor::readable(
                HEADER_ADDR,
                u32::try_from(VIRTIO_MEM_REQUEST_SIZE).expect("request size should fit"),
                Some(1),
            ),
        );
        write_descriptor(
            memory,
            1,
            TestDescriptor::writable(
                DATA_ADDR,
                u32::try_from(VIRTIO_MEM_RESPONSE_SIZE).expect("response size should fit"),
                None,
            ),
        );
        write_available_heads(memory, &[0]);
    }

    fn read_boot_memory_hotplug_response(memory: &crate::memory::GuestMemory) -> (u16, u16) {
        let bytes = read_guest_bytes(memory, DATA_ADDR, VIRTIO_MEM_RESPONSE_SIZE);
        (
            u16::from_le_bytes([bytes[0], bytes[1]]),
            u16::from_le_bytes([bytes[8], bytes[9]]),
        )
    }

    fn read_boot_memory_hotplug_used_index(memory: &crate::memory::GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_USED_RING
                .checked_add(2)
                .expect("memory-hotplug used idx address should not overflow"),
        )
    }

    fn read_boot_memory_hotplug_used_element(
        memory: &crate::memory::GuestMemory,
        ring_index: u16,
    ) -> (u32, u32) {
        let element = TEST_USED_RING
            .checked_add(4 + u64::from(ring_index) * 8)
            .expect("memory-hotplug used element address should not overflow");
        (
            read_guest_u32(memory, element),
            read_guest_u32(
                memory,
                element
                    .checked_add(4)
                    .expect("memory-hotplug used len address should not overflow"),
            ),
        )
    }

    fn read_boot_entropy_used_index(memory: &crate::memory::GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_USED_RING
                .checked_add(2)
                .expect("entropy used idx address should not overflow"),
        )
    }

    fn read_boot_balloon_inflate_used_index(memory: &crate::memory::GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_BALLOON_INFLATE_USED_RING
                .checked_add(2)
                .expect("balloon inflate used idx address should not overflow"),
        )
    }

    fn read_boot_balloon_deflate_used_index(memory: &crate::memory::GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_BALLOON_DEFLATE_USED_RING
                .checked_add(2)
                .expect("balloon deflate used idx address should not overflow"),
        )
    }

    fn read_boot_balloon_statistics_used_index(memory: &crate::memory::GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_BALLOON_STATS_USED_RING
                .checked_add(2)
                .expect("balloon statistics used idx address should not overflow"),
        )
    }

    fn read_boot_entropy_used_element(
        memory: &crate::memory::GuestMemory,
        ring_index: u16,
    ) -> (u32, u32) {
        let element = TEST_USED_RING
            .checked_add(4 + u64::from(ring_index) * 8)
            .expect("entropy used element address should not overflow");
        (
            read_guest_u32(memory, element),
            read_guest_u32(
                memory,
                element
                    .checked_add(4)
                    .expect("entropy used len address should not overflow"),
            ),
        )
    }

    fn write_boot_vsock_tx_packet_header(
        memory: &mut crate::memory::GuestMemory,
        header: VirtioVsockPacketHeader,
    ) {
        memory
            .write_slice(&header.to_bytes(), TEST_VSOCK_HEADER)
            .expect("vsock packet header should write");
    }

    fn write_boot_vsock_tx_descriptor(
        memory: &mut crate::memory::GuestMemory,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        write_descriptor_at(memory, TEST_VSOCK_TX_DESCRIPTOR_TABLE, index, descriptor);
    }

    fn write_boot_vsock_tx_available_heads(memory: &mut crate::memory::GuestMemory, heads: &[u16]) {
        write_available_heads_at(memory, TEST_VSOCK_TX_AVAILABLE_RING, heads);
    }

    fn write_boot_vsock_rx_descriptor(
        memory: &mut crate::memory::GuestMemory,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        write_descriptor_at(memory, TEST_VSOCK_RX_DESCRIPTOR_TABLE, index, descriptor);
    }

    fn write_boot_vsock_rx_available_heads(memory: &mut crate::memory::GuestMemory, heads: &[u16]) {
        write_available_heads_at(memory, TEST_VSOCK_RX_AVAILABLE_RING, heads);
    }

    fn read_boot_vsock_tx_used_index(memory: &crate::memory::GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_VSOCK_TX_USED_RING
                .checked_add(2)
                .expect("vsock TX used idx address should not overflow"),
        )
    }

    fn read_boot_vsock_tx_used_element(
        memory: &crate::memory::GuestMemory,
        ring_index: u16,
    ) -> (u32, u32) {
        let element = TEST_VSOCK_TX_USED_RING
            .checked_add(4 + u64::from(ring_index) * 8)
            .expect("vsock TX used element address should not overflow");
        (
            read_guest_u32(memory, element),
            read_guest_u32(
                memory,
                element
                    .checked_add(4)
                    .expect("vsock TX used len address should not overflow"),
            ),
        )
    }

    fn read_boot_vsock_rx_used_index(memory: &crate::memory::GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_VSOCK_RX_USED_RING
                .checked_add(2)
                .expect("vsock RX used idx address should not overflow"),
        )
    }

    fn read_boot_vsock_rx_used_element(
        memory: &crate::memory::GuestMemory,
        ring_index: u16,
    ) -> (u32, u32) {
        let element = TEST_VSOCK_RX_USED_RING
            .checked_add(4 + u64::from(ring_index) * 8)
            .expect("vsock RX used element address should not overflow");
        (
            read_guest_u32(memory, element),
            read_guest_u32(
                memory,
                element
                    .checked_add(4)
                    .expect("vsock RX used len address should not overflow"),
            ),
        )
    }

    fn read_boot_vsock_packet_header(
        memory: &crate::memory::GuestMemory,
        address: GuestAddress,
    ) -> VirtioVsockPacketHeader {
        let bytes = read_guest_bytes(memory, address, VIRTIO_VSOCK_PACKET_HEADER_SIZE)
            .try_into()
            .expect("vsock packet header length should match");
        VirtioVsockPacketHeader::try_from_bytes(bytes).expect("vsock packet header should parse")
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

    #[derive(Debug, Default)]
    struct RecordingEntropySource {
        calls: Vec<usize>,
        next_byte: u8,
    }

    impl VirtioRngEntropySource for RecordingEntropySource {
        fn fill_entropy(
            &mut self,
            destination: &mut [u8],
        ) -> Result<(), VirtioRngEntropySourceError> {
            self.calls.push(destination.len());
            for byte in destination {
                *byte = self.next_byte;
                self.next_byte = self.next_byte.wrapping_add(1);
            }

            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct RecordingEntropySourceProvider {
        source: RecordingEntropySource,
        requested_regions: Vec<MmioRegionId>,
        fail: bool,
    }

    impl RecordingEntropySourceProvider {
        fn failing() -> Self {
            Self {
                source: RecordingEntropySource::default(),
                requested_regions: Vec::new(),
                fail: true,
            }
        }
    }

    impl Arm64BootEntropySourceProvider for RecordingEntropySourceProvider {
        fn entropy_source(
            &mut self,
            device: &super::Arm64BootEntropyDevice,
        ) -> Result<Arm64BootEntropySource<'_>, Arm64BootEntropySourceError> {
            self.requested_regions.push(device.registration.region_id());
            if self.fail {
                return Err(Arm64BootEntropySourceError::new(
                    "test entropy source unavailable",
                ));
            }

            Ok(Arm64BootEntropySource::new(&mut self.source))
        }
    }

    #[derive(Debug)]
    struct WrongBalloonHandler;

    impl MmioHandler for WrongBalloonHandler {
        fn read(&mut self, _access: MmioAccess) -> Result<MmioAccessBytes, MmioHandlerError> {
            Err(MmioHandlerError::new("wrong balloon handler read"))
        }

        fn write(
            &mut self,
            _access: MmioAccess,
            _data: MmioAccessBytes,
        ) -> Result<(), MmioHandlerError> {
            Err(MmioHandlerError::new("wrong balloon handler write"))
        }
    }

    fn write_queued_read_request(memory: &mut crate::memory::GuestMemory) {
        write_read_request_at(
            memory,
            0,
            TEST_DESCRIPTOR_TABLE,
            HEADER_ADDR,
            DATA_ADDR,
            STATUS_ADDR,
            0,
        );
        write_available_heads(memory, &[0]);
    }

    fn write_read_request_at(
        memory: &mut crate::memory::GuestMemory,
        head: u16,
        descriptor_table: GuestAddress,
        header: GuestAddress,
        data: GuestAddress,
        status: GuestAddress,
        sector: u64,
    ) {
        write_request_header(memory, header, VIRTIO_BLOCK_REQUEST_TYPE_IN, sector);
        let data_descriptor = head
            .checked_add(1)
            .expect("test data descriptor index should not overflow");
        let status_descriptor = head
            .checked_add(2)
            .expect("test status descriptor index should not overflow");
        write_descriptor_at(
            memory,
            descriptor_table,
            head,
            TestDescriptor::readable(
                header,
                VIRTIO_BLOCK_REQUEST_HEADER_SIZE,
                Some(data_descriptor),
            ),
        );
        write_descriptor_at(
            memory,
            descriptor_table,
            data_descriptor,
            TestDescriptor::writable(
                data,
                VIRTIO_BLOCK_SECTOR_SIZE as u32,
                Some(status_descriptor),
            ),
        );
        write_descriptor_at(
            memory,
            descriptor_table,
            status_descriptor,
            TestDescriptor::writable(status, 1, None),
        );
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

    fn read_guest_u16(memory: &crate::memory::GuestMemory, address: GuestAddress) -> u16 {
        let mut bytes = [0; 2];
        memory
            .read_slice(&mut bytes, address)
            .expect("guest u16 should read");
        u16::from_le_bytes(bytes)
    }

    fn read_guest_u32(memory: &crate::memory::GuestMemory, address: GuestAddress) -> u32 {
        let mut bytes = [0; 4];
        memory
            .read_slice(&mut bytes, address)
            .expect("guest u32 should read");
        u32::from_le_bytes(bytes)
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

    fn block_queue_layout(device_index: usize) -> TestBlockQueueLayout {
        let offset = u64::try_from(device_index)
            .expect("test device index should fit in u64")
            .checked_mul(TEST_QUEUE_DEVICE_STRIDE)
            .expect("test queue offset should not overflow");
        let second_request_offset = 0x1000;

        TestBlockQueueLayout {
            descriptor_table: TEST_DESCRIPTOR_TABLE
                .checked_add(offset)
                .expect("test block descriptor table should not overflow"),
            available_ring: TEST_AVAILABLE_RING
                .checked_add(offset)
                .expect("test block available ring should not overflow"),
            used_ring: TEST_USED_RING
                .checked_add(offset)
                .expect("test block used ring should not overflow"),
            first_header: HEADER_ADDR
                .checked_add(offset)
                .expect("test first header should not overflow"),
            first_data: DATA_ADDR
                .checked_add(offset)
                .expect("test first data should not overflow"),
            first_status: STATUS_ADDR
                .checked_add(offset)
                .expect("test first status should not overflow"),
            second_header: HEADER_ADDR
                .checked_add(offset + second_request_offset)
                .expect("test second header should not overflow"),
            second_data: DATA_ADDR
                .checked_add(offset + second_request_offset)
                .expect("test second data should not overflow"),
            second_status: STATUS_ADDR
                .checked_add(offset + second_request_offset)
                .expect("test second status should not overflow"),
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct TestBlockQueueLayout {
        descriptor_table: GuestAddress,
        available_ring: GuestAddress,
        used_ring: GuestAddress,
        first_header: GuestAddress,
        first_data: GuestAddress,
        first_status: GuestAddress,
        second_header: GuestAddress,
        second_data: GuestAddress,
        second_status: GuestAddress,
    }

    fn write_two_queued_block_read_requests(
        memory: &mut crate::memory::GuestMemory,
        layout: TestBlockQueueLayout,
    ) {
        write_read_request_at(
            memory,
            0,
            layout.descriptor_table,
            layout.first_header,
            layout.first_data,
            layout.first_status,
            0,
        );
        write_read_request_at(
            memory,
            3,
            layout.descriptor_table,
            layout.second_header,
            layout.second_data,
            layout.second_status,
            1,
        );
        write_available_heads_at(memory, layout.available_ring, &[0, 3]);
    }

    fn network_queue_layout(device_index: usize) -> TestNetworkQueueLayout {
        let offset = u64::try_from(device_index)
            .expect("test device index should fit in u64")
            .checked_mul(TEST_QUEUE_DEVICE_STRIDE)
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

    fn fdt_bootargs(resources: &Arm64BootResources) -> String {
        read_fdt(resources)
            .find("/chosen")
            .expect("assembled FDT should contain /chosen")
            .prop_str("bootargs")
            .expect("assembled FDT should contain bootargs")
            .to_string()
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
        assert!(resources.pmem_devices.is_empty());
        assert!(resources.pmem_mmio_devices.is_empty());
        assert!(resources.network_devices.is_empty());
        assert!(resources.vsock_device.is_none());
        assert!(resources.balloon_device.is_none());
        assert!(resources.serial_device.is_none());
        assert!(resources.mmio_dispatcher.regions().is_empty());
        assert_eq!(
            resources.loaded_boot_source.command_line.as_str(),
            DEFAULT_KERNEL_COMMAND_LINE
        );
        assert_eq!(fdt_bootargs(&resources), DEFAULT_KERNEL_COMMAND_LINE);
        assert!(read_fdt(&resources).find("/uart@40002000").is_none());
        assert!(read_fdt(&resources).find("/virtio_mmio@40006000").is_none());
        assert!(read_fdt(&resources).find("/virtio_mmio@40007000").is_none());
        assert!(read_fdt(&resources).find("/virtio_mmio@40008000").is_none());
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
    fn assembles_boot_resources_with_prepared_pmem_backings() {
        let kernel = temp_file("kernel-with-pmem", &arm64_image());
        let first_pmem = temp_file("pmem-first", b"first");
        let second_pmem = temp_file("pmem-second", b"second");
        let mut controller = controller_with_kernel(kernel.path());
        add_pmem(&mut controller, "pmem0", first_pmem.path(), false);
        add_pmem(&mut controller, "pmem1", second_pmem.path(), true);

        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_config_with_pmem_lines(&[], &[line(32), line(33)]),
        )
        .expect("boot resources should assemble with pmem backing files");

        assert_eq!(resources.pmem_devices.len(), 2);
        assert_eq!(resources.pmem_devices[0].id(), "pmem0");
        assert_eq!(resources.pmem_devices[0].backing().len(), 5);
        assert!(!resources.pmem_devices[0].backing().is_read_only());
        assert_eq!(resources.pmem_devices[0].mapping().file_len(), 5);
        assert_eq!(
            resources.pmem_devices[0].mapping().mapped_len(),
            VIRTIO_PMEM_ALIGNMENT
        );
        assert!(!resources.pmem_devices[0].mapping().is_read_only());
        assert_eq!(
            resources.pmem_devices[0].guest_range().start(),
            GuestAddress::new(aarch64::FIRST_ADDR_PAST_64BITS_MMIO)
        );
        assert_eq!(
            resources.pmem_devices[0].guest_range().size(),
            VIRTIO_PMEM_ALIGNMENT
        );
        assert_eq!(
            resources.pmem_devices[0].config_space().start(),
            aarch64::FIRST_ADDR_PAST_64BITS_MMIO
        );
        assert_eq!(
            resources.pmem_devices[0].config_space().size(),
            VIRTIO_PMEM_ALIGNMENT
        );
        assert_eq!(resources.pmem_devices[1].id(), "pmem1");
        assert_eq!(resources.pmem_devices[1].backing().len(), 6);
        assert!(resources.pmem_devices[1].backing().is_read_only());
        assert_eq!(resources.pmem_devices[1].mapping().file_len(), 6);
        assert_eq!(
            resources.pmem_devices[1].mapping().mapped_len(),
            VIRTIO_PMEM_ALIGNMENT
        );
        assert!(resources.pmem_devices[1].mapping().is_read_only());
        assert_eq!(
            resources.pmem_devices[1].guest_range().start(),
            GuestAddress::new(aarch64::FIRST_ADDR_PAST_64BITS_MMIO + VIRTIO_PMEM_ALIGNMENT)
        );
        assert_eq!(
            resources.pmem_devices[1].guest_range().size(),
            VIRTIO_PMEM_ALIGNMENT
        );
        assert!(
            !resources.pmem_devices[0]
                .guest_range()
                .overlaps(resources.pmem_devices[1].guest_range())
        );
        assert_eq!(resources.pmem_mmio_devices.len(), 2);
        assert_eq!(
            resources.pmem_mmio_devices[0].registration.pmem_id(),
            "pmem0"
        );
        assert_eq!(
            resources.pmem_mmio_devices[0].registration.address(),
            TEST_PMEM_MMIO_BASE
        );
        assert_eq!(
            resources.pmem_mmio_devices[0].registration.config_space(),
            resources.pmem_devices[0].config_space()
        );
        assert_eq!(
            resources.pmem_mmio_devices[0].fdt_device.interrupt_line,
            line(32)
        );
        assert_eq!(
            resources.pmem_mmio_devices[1].registration.pmem_id(),
            "pmem1"
        );
        assert_eq!(
            resources.pmem_mmio_devices[1].registration.address(),
            TEST_PMEM_MMIO_BASE
                .checked_add(VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
                .expect("second pmem MMIO address should not overflow")
        );
        assert_eq!(
            resources.pmem_mmio_devices[1].registration.config_space(),
            resources.pmem_devices[1].config_space()
        );
        assert_eq!(
            resources.pmem_mmio_devices[1].fdt_device.interrupt_line,
            line(33)
        );
        assert_eq!(resources.mmio_dispatcher.regions().len(), 2);
        assert_eq!(
            resources.loaded_boot_source.command_line.as_str(),
            DEFAULT_KERNEL_COMMAND_LINE
        );
        assert_eq!(fdt_bootargs(&resources), DEFAULT_KERNEL_COMMAND_LINE);
        assert!(read_fdt(&resources).find("/virtio_mmio@40009000").is_some());
        assert!(read_fdt(&resources).find("/virtio_mmio@4000a000").is_some());
    }

    #[test]
    fn runtime_parts_retain_prepared_pmem_backings() {
        let kernel = temp_file("kernel-runtime-pmem", &arm64_image());
        let pmem = temp_file("pmem-runtime", b"pmem");
        let mut controller = controller_with_kernel(kernel.path());
        add_pmem(&mut controller, "pmem0", pmem.path(), false);

        let parts = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_config_with_pmem_lines(&[], &[line(32)]),
        )
        .expect("boot resources should assemble with pmem")
        .into_parts();

        assert_eq!(parts.runtime.pmem_devices.len(), 1);
        assert_eq!(parts.runtime.pmem_devices[0].id(), "pmem0");
        assert_eq!(parts.runtime.pmem_devices[0].backing().len(), 4);
        assert_eq!(parts.runtime.pmem_devices[0].mapping().file_len(), 4);
        assert_eq!(
            parts.runtime.pmem_devices[0].mapping().mapped_len(),
            VIRTIO_PMEM_ALIGNMENT
        );
        assert_eq!(
            parts.runtime.pmem_devices[0].config_space().start(),
            aarch64::FIRST_ADDR_PAST_64BITS_MMIO
        );
        assert_eq!(
            parts.runtime.pmem_devices[0].config_space().size(),
            VIRTIO_PMEM_ALIGNMENT
        );
        assert_eq!(parts.runtime.pmem_mmio_devices.len(), 1);
        assert_eq!(
            parts.runtime.pmem_mmio_devices[0].registration.pmem_id(),
            "pmem0"
        );
        assert_eq!(
            parts.runtime.pmem_mmio_devices[0].fdt_device.interrupt_line,
            line(32)
        );
    }

    #[test]
    fn pmem_interrupt_line_count_mismatch_fails_startup() {
        let kernel = temp_file("kernel-pmem-lines", &arm64_image());
        let pmem = temp_file("pmem-lines", b"pmem");
        let mut controller = controller_with_kernel(kernel.path());
        add_pmem(&mut controller, "pmem0", pmem.path(), false);

        let err = Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
            .expect_err("missing pmem interrupt line should fail startup resource assembly");

        assert!(matches!(
            err,
            Arm64BootResourceError::PmemInterruptLineCount {
                devices: 1,
                lines: 0
            }
        ));
    }

    #[test]
    fn missing_pmem_backing_fails_startup_without_echoing_path() {
        let kernel = temp_file("kernel-missing-pmem", &arm64_image());
        let missing = missing_path("secret-missing-pmem.img");
        let mut controller = controller_with_kernel(kernel.path());
        add_pmem(&mut controller, "pmem0", &missing, false);

        let err = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_config_with_pmem_lines(&[], &[line(32)]),
        )
        .expect_err("missing pmem backing should fail startup resource assembly");

        assert!(matches!(
            err,
            Arm64BootResourceError::PreparePmemDevices { ref source }
                if matches!(
                    source,
                    PreparedPmemDeviceError::OpenBacking {
                        pmem_id,
                        source: crate::pmem::PmemFileBackingError::OpenFile { .. },
                    } if pmem_id == "pmem0"
                )
        ));
        assert!(!err.to_string().contains("secret-missing-pmem"));
    }

    #[test]
    fn zero_sized_pmem_backing_fails_startup() {
        let kernel = temp_file("kernel-empty-pmem", &arm64_image());
        let pmem = temp_file("empty-pmem", b"");
        let mut controller = controller_with_kernel(kernel.path());
        add_pmem(&mut controller, "pmem0", pmem.path(), true);

        let err = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_config_with_pmem_lines(&[], &[line(32)]),
        )
        .expect_err("zero-sized pmem backing should fail startup resource assembly");

        assert!(matches!(
            err,
            Arm64BootResourceError::PreparePmemDevices { ref source }
                if matches!(
                    source,
                    PreparedPmemDeviceError::OpenBacking {
                        pmem_id,
                        source: crate::pmem::PmemFileBackingError::ZeroSizedFile,
                    } if pmem_id == "pmem0"
                )
        ));
        assert_eq!(
            err.to_string(),
            "failed to prepare pmem devices: failed to prepare pmem device pmem0: pmem backing file is zero-sized"
        );
    }

    #[test]
    fn assembles_boot_resources_with_read_only_root_drive_boot_args() {
        let kernel = temp_file("kernel-root-ro", &arm64_image());
        let block = temp_file("block-root-ro", &[0x5a; 512]);
        let mut controller = controller_with_kernel_and_boot_args(kernel.path(), "console=ttyS0");
        add_drive_with_options(
            &mut controller,
            "rootfs",
            block.path(),
            true,
            Some(true),
            None,
        );

        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[line(32)]))
                .expect("boot resources should assemble with read-only root drive");

        assert_eq!(
            resources.loaded_boot_source.command_line.as_str(),
            "console=ttyS0 root=/dev/vda ro"
        );
        assert_eq!(fdt_bootargs(&resources), "console=ttyS0 root=/dev/vda ro");
    }

    #[test]
    fn assembles_boot_resources_with_writable_root_drive_boot_args() {
        let kernel = temp_file("kernel-root-rw", &arm64_image());
        let block = temp_file("block-root-rw", &[0x5a; 512]);
        let mut controller = controller_with_kernel_and_boot_args(kernel.path(), "console=ttyS0");
        add_drive_with_options(
            &mut controller,
            "rootfs",
            block.path(),
            true,
            Some(false),
            None,
        );

        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[line(32)]))
                .expect("boot resources should assemble with writable root drive");

        assert_eq!(
            resources.loaded_boot_source.command_line.as_str(),
            "console=ttyS0 root=/dev/vda rw"
        );
        assert_eq!(fdt_bootargs(&resources), "console=ttyS0 root=/dev/vda rw");
    }

    #[test]
    fn assembles_boot_resources_with_partuuid_root_drive_boot_args() {
        let kernel = temp_file("kernel-root-partuuid", &arm64_image());
        let block = temp_file("block-root-partuuid", &[0x5a; 512]);
        let mut controller = controller_with_kernel_and_boot_args(kernel.path(), "console=ttyS0");
        add_drive_with_options(
            &mut controller,
            "rootfs",
            block.path(),
            true,
            Some(true),
            Some("0eaa91a0-01"),
        );

        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[line(32)]))
                .expect("boot resources should assemble with PARTUUID root drive");

        assert_eq!(
            resources.loaded_boot_source.command_line.as_str(),
            "console=ttyS0 root=PARTUUID=0eaa91a0-01 ro"
        );
        assert_eq!(
            fdt_bootargs(&resources),
            "console=ttyS0 root=PARTUUID=0eaa91a0-01 ro"
        );
    }

    #[test]
    fn assembles_boot_resources_does_not_append_root_args_for_non_root_drive() {
        let kernel = temp_file("kernel-non-root", &arm64_image());
        let block = temp_file("block-non-root", &[0x5a; 512]);
        let mut controller = controller_with_kernel_and_boot_args(kernel.path(), "console=ttyS0");
        add_drive_with_options(
            &mut controller,
            "data",
            block.path(),
            false,
            Some(true),
            Some("0eaa91a0-01"),
        );

        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[line(32)]))
                .expect("boot resources should assemble with non-root drive");

        assert_eq!(
            resources.loaded_boot_source.command_line.as_str(),
            "console=ttyS0"
        );
        assert_eq!(fdt_bootargs(&resources), "console=ttyS0");
    }

    #[test]
    fn assembles_boot_resources_appends_root_args_before_init_args() {
        let kernel = temp_file("kernel-root-init-args", &arm64_image());
        let block = temp_file("block-root-init-args", &[0x5a; 512]);
        let mut controller =
            controller_with_kernel_and_boot_args(kernel.path(), "console=ttyS0 -- /init");
        add_drive_with_options(
            &mut controller,
            "rootfs",
            block.path(),
            true,
            Some(true),
            None,
        );

        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[line(32)]))
                .expect("boot resources should assemble with root drive and init args");

        assert_eq!(
            resources.loaded_boot_source.command_line.as_str(),
            "console=ttyS0 root=/dev/vda ro -- /init"
        );
        assert_eq!(
            fdt_bootargs(&resources),
            "console=ttyS0 root=/dev/vda ro -- /init"
        );
    }

    #[test]
    fn root_drive_boot_args_overflow_fails_before_block_preparation() {
        let kernel = temp_file("kernel-root-overflow", &arm64_image());
        let boot_args = "a".repeat(aarch64::CMDLINE_MAX_SIZE - 1);
        let mut controller = controller_with_kernel_and_boot_args(kernel.path(), &boot_args);
        add_drive(
            &mut controller,
            "rootfs",
            &missing_path("root-overflow-block"),
        );

        let err =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[line(32)]))
                .expect_err("root-drive boot args should exceed command-line limit");

        assert!(matches!(
            err,
            Arm64BootResourceError::RootDriveCommandLine {
                source: BootCommandLineError::TooLarge { .. }
            }
        ));
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
    fn assembles_boot_resources_with_vsock_mmio_metadata() {
        let kernel = temp_file("kernel-with-vsock", &arm64_image());
        let socket_path = missing_path("vsock-startup.sock");
        let mut controller = controller_with_kernel(kernel.path());
        add_vsock(&mut controller, 42, &socket_path);
        let config = Arm64BootResourceConfig {
            vsock_interrupt_line: Some(line(35)),
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            ..valid_config(&[])
        };

        assert!(!socket_path.exists());

        let mut resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble with vsock device");

        assert!(resources.block_devices.is_empty());
        assert!(resources.network_devices.is_empty());
        let vsock = resources
            .vsock_device
            .as_ref()
            .expect("vsock metadata should be returned");
        assert_eq!(vsock.registration.guest_cid(), 42);
        assert_eq!(vsock.registration.uds_path(), socket_path.as_path());
        assert_eq!(vsock.registration.region_id(), MmioRegionId::new(90));
        assert_eq!(vsock.registration.address(), TEST_VSOCK_MMIO_BASE);
        assert_eq!(
            vsock.registration.region().range().size(),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        );
        assert_eq!(
            vsock.fdt_device.region.base,
            TEST_VSOCK_MMIO_BASE.raw_value()
        );
        assert_eq!(vsock.fdt_device.region.size, VIRTIO_MMIO_DEVICE_WINDOW_SIZE);
        assert_eq!(vsock.fdt_device.interrupt_line, line(35));
        assert_eq!(resources.mmio_dispatcher.regions().len(), 1);
        resources
            .mmio_dispatcher
            .handler_mut::<VirtioVsockMmioHandler>(vsock.registration.region_id())
            .expect("vsock MMIO handler should be registered");
        assert!(socket_path.exists());

        let tree = read_fdt(&resources);
        let vsock_node = tree
            .find("/virtio_mmio@40006000")
            .expect("vsock virtio-mmio node should be in assembled FDT");
        assert_eq!(vsock_node.prop_str("compatible").unwrap(), "virtio,mmio");

        drop(resources);

        assert!(!socket_path.exists());
    }

    #[test]
    fn assembles_boot_resources_with_balloon_mmio_metadata() {
        let kernel = temp_file("kernel-with-balloon", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_balloon(&mut controller, TEST_MEMORY_MIB as u32);
        let config = Arm64BootResourceConfig {
            balloon_interrupt_line: Some(line(36)),
            ..valid_config(&[])
        };

        let mut resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble with balloon device");

        assert!(resources.block_devices.is_empty());
        assert!(resources.network_devices.is_empty());
        assert!(resources.vsock_device.is_none());
        assert!(resources.entropy_device.is_none());
        let balloon = resources
            .balloon_device
            .as_ref()
            .expect("balloon metadata should be returned");
        assert_eq!(balloon.registration.region_id(), MmioRegionId::new(110));
        assert_eq!(balloon.registration.address(), TEST_BALLOON_MMIO_BASE);
        assert_eq!(
            balloon.registration.region().range().size(),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        );
        assert_eq!(
            balloon.fdt_device.region.base,
            TEST_BALLOON_MMIO_BASE.raw_value()
        );
        assert_eq!(
            balloon.fdt_device.region.size,
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        );
        assert_eq!(balloon.fdt_device.interrupt_line, line(36));
        assert_eq!(resources.mmio_dispatcher.regions().len(), 1);

        let region_id = balloon.registration.region_id();
        let handler = resources
            .mmio_dispatcher
            .handler_mut::<VirtioBalloonMmioHandler>(region_id)
            .expect("balloon MMIO handler should be registered");
        assert_eq!(
            handler.device_registers().device_id(),
            VIRTIO_BALLOON_DEVICE_ID
        );
        assert_eq!(
            handler.device_config_handler().num_pages(),
            TEST_MEMORY_MIB as u32 * VIRTIO_BALLOON_MIB_TO_4K_PAGES
        );

        let tree = read_fdt(&resources);
        let balloon_node = tree
            .find("/virtio_mmio@40008000")
            .expect("balloon virtio-mmio node should be in assembled FDT");
        assert_eq!(balloon_node.prop_str("compatible").unwrap(), "virtio,mmio");

        let parts = resources.into_parts();
        assert_eq!(
            parts
                .runtime
                .balloon_device
                .as_ref()
                .expect("runtime balloon metadata should be preserved")
                .registration
                .region_id(),
            MmioRegionId::new(110)
        );
    }

    #[test]
    fn assembles_boot_resources_with_entropy_mmio_metadata() {
        let kernel = temp_file("kernel-with-entropy", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let config = Arm64BootResourceConfig {
            entropy_device: Some(super::Arm64BootEntropyDeviceConfig::new(
                EntropyMmioLayout::new(TEST_ENTROPY_MMIO_BASE, MmioRegionId::new(100)),
                line(36),
            )),
            ..valid_config(&[])
        };

        let mut resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble with entropy device");

        assert!(resources.block_devices.is_empty());
        assert!(resources.network_devices.is_empty());
        assert!(resources.vsock_device.is_none());
        let entropy = resources
            .entropy_device
            .as_ref()
            .expect("entropy metadata should be returned");
        assert_eq!(entropy.registration.region_id(), MmioRegionId::new(100));
        assert_eq!(entropy.registration.address(), TEST_ENTROPY_MMIO_BASE);
        assert_eq!(
            entropy.registration.region().range().size(),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        );
        assert_eq!(
            entropy.fdt_device.region.base,
            TEST_ENTROPY_MMIO_BASE.raw_value()
        );
        assert_eq!(
            entropy.fdt_device.region.size,
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        );
        assert_eq!(entropy.fdt_device.interrupt_line, line(36));
        assert_eq!(resources.mmio_dispatcher.regions().len(), 1);
        resources
            .mmio_dispatcher
            .handler_mut::<VirtioRngMmioHandler>(entropy.registration.region_id())
            .expect("entropy MMIO handler should be registered");

        let tree = read_fdt(&resources);
        let entropy_node = tree
            .find("/virtio_mmio@40007000")
            .expect("entropy virtio-mmio node should be in assembled FDT");
        assert_eq!(entropy_node.prop_str("compatible").unwrap(), "virtio,mmio");

        let parts = resources.into_parts();
        assert_eq!(
            parts
                .runtime
                .entropy_device
                .as_ref()
                .expect("runtime entropy metadata should be preserved")
                .registration
                .region_id(),
            MmioRegionId::new(100)
        );
    }

    #[test]
    fn assembles_boot_resources_with_rtc_mmio_metadata() {
        let kernel = temp_file("kernel-with-rtc", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let config = Arm64BootResourceConfig {
            rtc_device: Some(Arm64BootRtcDeviceConfig::new(RtcMmioLayout::new(
                TEST_RTC_MMIO_BASE,
                MmioRegionId::new(8),
            ))),
            ..valid_config(&[])
        };

        let mut resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble with RTC device");

        let rtc = resources
            .rtc_device
            .as_ref()
            .expect("RTC metadata should be returned");
        assert_eq!(rtc.region.id(), MmioRegionId::new(8));
        assert_eq!(rtc.region.range().start(), TEST_RTC_MMIO_BASE);
        assert_eq!(rtc.region.range().size(), RTC_MMIO_DEVICE_WINDOW_SIZE);
        assert_eq!(rtc.fdt_device.region.base, TEST_RTC_MMIO_BASE.raw_value());
        assert_eq!(rtc.fdt_device.region.size, RTC_MMIO_DEVICE_WINDOW_SIZE);
        assert_eq!(resources.mmio_dispatcher.regions().len(), 1);
        resources
            .mmio_dispatcher
            .handler_mut::<Pl031RtcDevice>(rtc.region.id())
            .expect("RTC MMIO handler should be registered");

        let tree = read_fdt(&resources);
        let rtc_node = tree
            .find("/rtc@40001000")
            .expect("RTC node should be in assembled FDT");
        assert_eq!(
            rtc_node.prop_raw("compatible").unwrap(),
            b"arm,pl031\0arm,primecell\0"
        );

        let parts = resources.into_parts();
        assert_eq!(
            parts
                .runtime
                .rtc_device
                .as_ref()
                .expect("runtime RTC metadata should be preserved")
                .region
                .id(),
            MmioRegionId::new(8)
        );
    }

    #[test]
    fn configured_vsock_without_interrupt_line_fails_before_socket_access() {
        let kernel = temp_file("kernel-vsock-missing-line", &arm64_image());
        let socket_path = missing_path("vsock-missing-line.sock");
        let mut controller = controller_with_kernel(kernel.path());
        add_vsock(&mut controller, 43, &socket_path);

        let err = Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
            .expect_err("configured vsock without interrupt line should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::VsockInterruptLineCount {
                devices: 1,
                lines: 0
            }
        ));
        assert!(std::error::Error::source(&err).is_none());
        assert!(!socket_path.exists());
    }

    #[test]
    fn configured_balloon_without_interrupt_line_fails() {
        let kernel = temp_file("kernel-balloon-missing-line", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_balloon(&mut controller, TEST_MEMORY_MIB as u32);

        let err = Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
            .expect_err("configured balloon without interrupt line should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::BalloonInterruptLineCount {
                devices: 1,
                lines: 0
            }
        ));
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn configured_vsock_existing_socket_path_fails_without_unlinking() {
        let kernel = temp_file("kernel-vsock-existing-socket-path", &arm64_image());
        let socket_path = missing_path("vsock-existing-secret.sock");
        fs::write(&socket_path, "existing file").expect("fixture file should be written");
        let mut controller = controller_with_kernel(kernel.path());
        add_vsock(&mut controller, 44, &socket_path);
        let config = Arm64BootResourceConfig {
            vsock_interrupt_line: Some(line(35)),
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            ..valid_config(&[])
        };

        let err = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect_err("existing vsock socket path should fail startup");

        assert!(matches!(
            err,
            Arm64BootResourceError::PrepareVsockDevice {
                source: crate::vsock::PreparedVsockDeviceError::HostSocket {
                    guest_cid: 44,
                    source: VsockHostSocketOwnerError::SocketPathExists
                }
            }
        ));
        assert!(!err.to_string().contains("vsock-existing-secret"));
        assert_eq!(
            fs::read_to_string(&socket_path).expect("existing file should remain"),
            "existing file"
        );

        fs::remove_file(socket_path).expect("fixture file should clean up");
    }

    #[test]
    fn extra_vsock_interrupt_line_without_config_fails() {
        let kernel = temp_file("kernel-vsock-extra-line", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let config = Arm64BootResourceConfig {
            vsock_interrupt_line: Some(line(35)),
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            ..valid_config(&[])
        };

        let err = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect_err("extra vsock interrupt line should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::VsockInterruptLineCount {
                devices: 0,
                lines: 1
            }
        ));
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn vsock_region_overlapping_block_mmio_fails_during_registration_without_path_leak() {
        let kernel = temp_file("kernel-vsock-overlap-block", &arm64_image());
        let block = temp_file("block-vsock-overlap", &[0x5a; 512]);
        let socket_path = missing_path("vsock-overlap-secret.sock");
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", block.path());
        add_vsock(&mut controller, 44, &socket_path);
        let block_interrupt_lines = [line(32)];
        let config = Arm64BootResourceConfig {
            vsock_mmio_layout: VsockMmioLayout::new(TEST_BLOCK_MMIO_BASE, MmioRegionId::new(90)),
            vsock_interrupt_line: Some(line(35)),
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            ..valid_config(&block_interrupt_lines)
        };

        let err = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect_err("overlapping vsock MMIO should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::RegisterVsockMmio { .. }
        ));
        assert!(!err.to_string().contains("vsock-overlap-secret"));
        assert!(!socket_path.exists());
    }

    #[test]
    fn entropy_region_overlapping_block_mmio_fails_during_registration() {
        let kernel = temp_file("kernel-entropy-overlap-block", &arm64_image());
        let block = temp_file("block-entropy-overlap", &[0x5a; 512]);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", block.path());
        let block_interrupt_lines = [line(32)];
        let config = Arm64BootResourceConfig {
            entropy_device: Some(super::Arm64BootEntropyDeviceConfig::new(
                EntropyMmioLayout::new(TEST_BLOCK_MMIO_BASE, MmioRegionId::new(100)),
                line(36),
            )),
            ..valid_config(&block_interrupt_lines)
        };

        let err = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect_err("overlapping entropy MMIO should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::RegisterEntropyMmio { .. }
        ));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn rtc_region_overlapping_block_mmio_fails_during_registration() {
        let kernel = temp_file("kernel-rtc-overlap-block", &arm64_image());
        let block = temp_file("block-rtc-overlap", &[0x5a; 512]);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", block.path());
        let block_interrupt_lines = [line(32)];
        let config = Arm64BootResourceConfig {
            rtc_device: Some(Arm64BootRtcDeviceConfig::new(RtcMmioLayout::new(
                TEST_BLOCK_MMIO_BASE,
                MmioRegionId::new(8),
            ))),
            ..valid_config(&block_interrupt_lines)
        };

        let err = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect_err("overlapping RTC MMIO should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::RegisterRtcMmio {
                ref source
            } if matches!(
                source.as_ref(),
                Arm64BootRtcMmioRegistrationError::InsertRegion { .. }
            )
        ));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn serial_region_overlapping_rtc_mmio_fails_during_registration() {
        let kernel = temp_file("kernel-serial-overlap-rtc", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let (serial, _output) = serial_config(TEST_RTC_MMIO_BASE, MmioRegionId::new(9), line(32));
        let config = Arm64BootResourceConfig {
            rtc_device: Some(Arm64BootRtcDeviceConfig::new(RtcMmioLayout::new(
                TEST_RTC_MMIO_BASE,
                MmioRegionId::new(8),
            ))),
            serial_device: Some(serial),
            ..valid_config(&[])
        };

        let err = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect_err("overlapping serial and RTC MMIO should fail");

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
    fn assembles_boot_resources_with_entropy_and_existing_mmio_devices() {
        let kernel = temp_file("kernel-entropy-with-existing-mmio", &arm64_image());
        let block = temp_file("block-entropy-with-existing-mmio", &[0x5a; 512]);
        let socket_path = missing_path("vsock-entropy.sock");
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", block.path());
        add_network(&mut controller, "eth0", "tap0");
        add_vsock(&mut controller, 42, &socket_path);
        let block_lines = [line(32)];
        let network_lines = [line(33)];
        let (serial, _output) =
            serial_config(TEST_SERIAL_MMIO_BASE, MmioRegionId::new(9), line(37));
        let config = Arm64BootResourceConfig {
            serial_device: Some(serial),
            network_interrupt_lines: &network_lines,
            vsock_interrupt_line: Some(line(35)),
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            entropy_device: Some(super::Arm64BootEntropyDeviceConfig::new(
                EntropyMmioLayout::new(TEST_ENTROPY_MMIO_BASE, MmioRegionId::new(100)),
                line(36),
            )),
            ..valid_config(&block_lines)
        };

        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble with entropy and existing MMIO devices");

        assert_eq!(resources.block_devices.len(), 1);
        assert_eq!(resources.network_devices.len(), 1);
        assert!(resources.vsock_device.is_some());
        assert!(resources.entropy_device.is_some());
        assert!(resources.serial_device.is_some());
        assert_eq!(resources.mmio_dispatcher.regions().len(), 5);
        assert_eq!(
            resources
                .entropy_device
                .as_ref()
                .expect("entropy metadata should exist")
                .fdt_device
                .interrupt_line,
            line(36)
        );

        let tree = read_fdt(&resources);
        assert!(tree.find("/virtio_mmio@40000000").is_some());
        assert!(tree.find("/uart@40002000").is_some());
        assert!(tree.find("/virtio_mmio@40004000").is_some());
        assert!(tree.find("/virtio_mmio@40006000").is_some());
        assert!(tree.find("/virtio_mmio@40007000").is_some());
    }

    #[test]
    fn assembles_boot_resources_with_memory_hotplug_device() {
        let kernel = temp_file("kernel-memory-hotplug", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_memory_hotplug(&mut controller);
        let config = Arm64BootResourceConfig {
            memory_hotplug_device: Some(super::Arm64BootMemoryHotplugDeviceConfig::new(
                VirtioMemMmioLayout::new(TEST_MEMORY_HOTPLUG_MMIO_BASE, MmioRegionId::new(120)),
                line(32),
            )),
            ..valid_config(&[])
        };

        let mut resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble with memory hotplug device");

        let device = resources
            .memory_hotplug_device
            .as_ref()
            .expect("memory hotplug metadata should be returned");
        assert_eq!(device.registration.region_id(), MmioRegionId::new(120));
        assert_eq!(device.registration.address(), TEST_MEMORY_HOTPLUG_MMIO_BASE);
        assert_eq!(device.fdt_device.interrupt_line, line(32));
        assert_eq!(device.fdt_device.region.base, 0x4000_a000);
        assert_eq!(
            device.fdt_device.region.size,
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        );
        assert_eq!(resources.mmio_dispatcher.regions().len(), 1);
        let handler = resources
            .mmio_dispatcher
            .handler_mut::<VirtioMemMmioHandler>(device.registration.region_id())
            .expect("virtio-mem handler should be registered");
        let config_space = *handler.device_config_handler();
        assert_eq!(config_space.block_size(), 2 * MIB);
        assert_eq!(
            config_space.addr(),
            VIRTIO_MEM_DEFAULT_REGION_ADDRESS.raw_value()
        );
        assert_eq!(config_space.region_size(), 1024 * MIB);
        assert_eq!(config_space.usable_region_size(), 0);
        assert_eq!(config_space.plugged_size(), 0);
        assert_eq!(config_space.requested_size(), 0);

        let tree = read_fdt(&resources);
        assert!(tree.find("/virtio_mmio@4000a000").is_some());
    }

    #[test]
    fn memory_hotplug_config_without_startup_device_fails() {
        let kernel = temp_file("kernel-memory-hotplug-missing-device", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_memory_hotplug(&mut controller);

        let err = Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
            .expect_err("memory hotplug config without device config should fail");

        assert_eq!(
            err.to_string(),
            "memory hotplug MMIO device count 1 does not match interrupt line count 0"
        );
        assert!(matches!(
            err,
            Arm64BootResourceError::MemoryHotplugInterruptLineCount {
                devices: 1,
                lines: 0
            }
        ));
    }

    #[test]
    fn memory_hotplug_region_overlapping_balloon_mmio_fails_during_registration() {
        let kernel = temp_file("kernel-memory-hotplug-overlap-balloon", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_memory_hotplug(&mut controller);
        add_balloon(&mut controller, 4);
        let config = Arm64BootResourceConfig {
            balloon_interrupt_line: Some(line(32)),
            memory_hotplug_device: Some(super::Arm64BootMemoryHotplugDeviceConfig::new(
                VirtioMemMmioLayout::new(TEST_BALLOON_MMIO_BASE, MmioRegionId::new(120)),
                line(33),
            )),
            ..valid_config(&[])
        };

        let err = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect_err("overlapping memory hotplug and balloon MMIO should fail");

        assert!(matches!(
            err,
            Arm64BootResourceError::RegisterMemoryHotplugMmio { source }
                if matches!(
                    source.as_ref(),
                    crate::memory_hotplug::VirtioMemMmioRegistrationError::InsertRegion {
                        source: MmioBusError::OverlappingRegion { .. },
                        ..
                    }
                )
        ));
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
    fn serial_mmio_can_write_to_file_output_sink() {
        let kernel = temp_file("kernel-with-serial-file", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let serial_output = temp_file("serial-file-output", b"");
        let file_output =
            SerialOutputFile::open(serial_output.path()).expect("serial output file should open");
        let serial = Arm64BootSerialDeviceConfig::new(
            MmioRegionId::new(9),
            TEST_SERIAL_MMIO_BASE,
            line(32),
            SharedSerialOutput::new(file_output),
        );
        let config = Arm64BootResourceConfig {
            serial_device: Some(serial),
            ..valid_config(&[])
        };
        let mut resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble with serial file output");

        write_serial_byte(
            &mut resources,
            TEST_SERIAL_MMIO_BASE
                .checked_add(SERIAL_TRANSMIT_REGISTER_OFFSET)
                .expect("serial TX address should not overflow"),
            b'F',
        );

        assert_eq!(
            fs::read(serial_output.path()).expect("serial output should read"),
            b"F"
        );
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
        assert!(parts.runtime.vsock_device.is_none());
        assert!(parts.runtime.entropy_device.is_none());
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
    fn boot_runtime_vsock_notification_dispatch_accepts_empty_vsock_device() {
        let kernel = temp_file("kernel-empty-vsock-dispatch", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
                .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;

        let dispatches = runtime
            .dispatch_vsock_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("empty vsock dispatch result should allocate");

        assert!(dispatches.is_empty());
        assert_eq!(dispatches.len(), 0);
        assert!(!dispatches.needs_queue_interrupt());
    }

    #[test]
    fn boot_runtime_balloon_notification_dispatch_accepts_empty_balloon_device() {
        let kernel = temp_file("kernel-empty-balloon-dispatch", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
                .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;

        let dispatches = runtime
            .dispatch_balloon_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("empty balloon dispatch result should allocate");

        assert!(dispatches.is_empty());
        assert_eq!(dispatches.len(), 0);
        assert!(!dispatches.needs_queue_interrupt());
    }

    #[test]
    fn boot_runtime_memory_hotplug_notification_dispatch_accepts_empty_device() {
        let kernel = temp_file("kernel-empty-memory-hotplug-dispatch", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
                .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;

        let dispatches = runtime
            .dispatch_memory_hotplug_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("empty memory-hotplug dispatch result should allocate");

        assert!(dispatches.is_empty());
        assert_eq!(dispatches.len(), 0);
        assert!(!dispatches.needs_queue_interrupt());
    }

    #[test]
    fn boot_runtime_balloon_notification_dispatch_without_pending_notification_is_noop() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_balloon("kernel-balloon-noop-dispatch");

        let dispatches = runtime
            .dispatch_balloon_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("balloon dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(!dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        assert_eq!(
            device_dispatch.device().registration.region_id(),
            MmioRegionId::new(110)
        );
        assert_eq!(device_dispatch.device().fdt_device.interrupt_line, line(36));
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("no pending balloon notification should dispatch as no-op");
        assert!(dispatch.drained_notifications().is_empty());
        assert_eq!(dispatch.inflate_notifications(), 0);
        assert_eq!(dispatch.deflate_notifications(), 0);
        assert!(dispatch.inflate_queue_dispatch().is_none());
        assert!(dispatch.deflate_queue_dispatch().is_none());
        assert!(!device_dispatch.needs_queue_interrupt());
        assert_eq!(
            read_boot_balloon_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus,
            ),
            0
        );
    }

    #[test]
    fn boot_runtime_memory_hotplug_notification_dispatch_without_pending_notification_is_noop() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_memory_hotplug("kernel-memory-hotplug-noop-dispatch");
        configure_boot_memory_hotplug_queue(&mut runtime, &mut mmio_dispatcher, TEST_USED_RING);

        let dispatches = runtime
            .dispatch_memory_hotplug_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("memory-hotplug dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(!dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        assert_eq!(
            device_dispatch.device().registration.region_id(),
            MmioRegionId::new(120)
        );
        assert_eq!(device_dispatch.device().fdt_device.interrupt_line, line(36));
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("no pending memory-hotplug notification should dispatch as no-op");
        assert!(dispatch.drained_notifications().is_empty());
        assert!(dispatch.queue_dispatch().is_none());
        assert!(!device_dispatch.needs_queue_interrupt());
        assert_eq!(
            read_boot_memory_hotplug_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus,
            ),
            0
        );
    }

    #[test]
    fn boot_runtime_memory_hotplug_notification_dispatch_executes_queued_request() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_memory_hotplug("kernel-memory-hotplug-request-dispatch");
        configure_boot_memory_hotplug_queue(&mut runtime, &mut mmio_dispatcher, TEST_USED_RING);
        write_queued_memory_hotplug_state_request(&mut memory);
        notify_boot_memory_hotplug_queue(&mut runtime, &mut mmio_dispatcher);

        let dispatches = runtime
            .dispatch_memory_hotplug_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("memory-hotplug dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("queued memory-hotplug notification should dispatch");
        assert_eq!(dispatch.drained_notifications(), [0]);
        let queue = dispatch
            .queue_dispatch()
            .expect("queue dispatch summary should be present");
        assert_eq!(queue.processed_requests(), 1);
        assert_eq!(queue.policy_errors(), 1);
        assert!(queue.needs_queue_interrupt());
        assert!(device_dispatch.needs_queue_interrupt());
        assert_eq!(read_boot_memory_hotplug_response(&memory), (3, 0));
        assert_eq!(read_boot_memory_hotplug_used_index(&memory), 1);
        assert_eq!(
            read_boot_memory_hotplug_used_element(&memory, 0),
            (
                0,
                u32::try_from(VIRTIO_MEM_RESPONSE_SIZE).expect("response size should fit")
            ),
        );
        assert_eq!(
            read_boot_memory_hotplug_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus,
            ),
            1
        );
    }

    #[test]
    fn boot_runtime_balloon_config_update_updates_active_handler() {
        let initial_amount_mib = TEST_MEMORY_MIB as u32 / 2;
        let (_, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_balloon_target("kernel-balloon-update-config", initial_amount_mib);
        let device = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should exist")
            .clone();

        let handler = mmio_dispatcher
            .handler_mut::<VirtioBalloonMmioHandler>(device.registration.region_id())
            .expect("balloon handler should be registered");
        assert_eq!(
            handler.device_config_handler().num_pages(),
            initial_amount_mib * VIRTIO_BALLOON_MIB_TO_4K_PAGES
        );

        update_balloon_config_for_device(
            &device,
            &mut mmio_dispatcher,
            BalloonConfigInput::new(TEST_MEMORY_MIB as u32, false).into(),
        )
        .expect("balloon config update should succeed");

        let handler = mmio_dispatcher
            .handler_mut::<VirtioBalloonMmioHandler>(device.registration.region_id())
            .expect("balloon handler should be registered");
        assert_eq!(
            handler.device_config_handler().num_pages(),
            TEST_MEMORY_MIB as u32 * VIRTIO_BALLOON_MIB_TO_4K_PAGES
        );
        assert_eq!(
            read_boot_balloon_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::ConfigGeneration,
            ),
            1
        );
        assert_eq!(
            read_boot_balloon_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus,
            ),
            DeviceInterruptKind::Config.status().bits()
        );
    }

    #[test]
    fn boot_runtime_balloon_statistics_update_updates_active_handler() {
        let initial_amount_mib = TEST_MEMORY_MIB as u32 / 2;
        let (_, runtime, mut mmio_dispatcher) = boot_runtime_with_balloon_config(
            "kernel-balloon-update-stats",
            BalloonConfigInput::new(initial_amount_mib, false).with_stats_polling_interval_s(60),
        );
        let device = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should exist")
            .clone();

        let handler = mmio_dispatcher
            .handler_mut::<VirtioBalloonMmioHandler>(device.registration.region_id())
            .expect("balloon handler should be registered");
        assert_eq!(handler.activation_handler().stats_polling_interval_s(), 60);

        update_balloon_statistics_for_device(
            &device,
            &mut mmio_dispatcher,
            BalloonStatsUpdateInput::new(30),
        )
        .expect("balloon statistics update should succeed");

        let handler = mmio_dispatcher
            .handler_mut::<VirtioBalloonMmioHandler>(device.registration.region_id())
            .expect("balloon handler should be registered");
        assert_eq!(handler.activation_handler().stats_polling_interval_s(), 30);
    }

    #[test]
    fn boot_runtime_balloon_notification_dispatch_executes_queued_inflate_request() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_balloon("kernel-balloon-inflate-dispatch");
        configure_boot_balloon_queues(&mut runtime, &mut mmio_dispatcher);
        write_queued_balloon_inflate_request(&mut memory);
        notify_boot_balloon_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
        );

        let dispatches = runtime
            .dispatch_balloon_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("balloon dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        assert!(device_dispatch.needs_queue_interrupt());
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("queued inflate notification should dispatch");
        assert_eq!(
            dispatch.drained_notifications(),
            [VIRTIO_BALLOON_INFLATE_QUEUE_INDEX]
        );
        assert_eq!(dispatch.inflate_notifications(), 1);
        assert_eq!(dispatch.deflate_notifications(), 0);
        let inflate = dispatch
            .inflate_queue_dispatch()
            .expect("inflate queue dispatch should be present");
        assert_eq!(inflate.completed_descriptors(), 1);
        let ranges = inflate.inflated_page_ranges();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start_pfn(), TEST_BALLOON_MAPPED_PFN);
        assert_eq!(ranges[0].page_count(), 1);
        assert!(inflate.needs_queue_interrupt());
        assert!(dispatch.deflate_queue_dispatch().is_none());
        assert_eq!(read_boot_balloon_inflate_used_index(&memory), 1);
        assert_eq!(
            read_boot_balloon_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus,
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn boot_runtime_balloon_stats_reads_active_handler_accounting() {
        let amount_mib = TEST_MEMORY_MIB as u32 / 2;
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_balloon_target("kernel-balloon-stats", amount_mib);
        configure_boot_balloon_queues(&mut runtime, &mut mmio_dispatcher);
        write_queued_balloon_inflate_request(&mut memory);
        notify_boot_balloon_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
        );

        runtime
            .dispatch_balloon_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("balloon dispatch should update accounting");
        let device = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should exist")
            .clone();

        let stats = balloon_stats_for_device(
            &device,
            &mut mmio_dispatcher,
            BalloonConfigInput::new(amount_mib, false).into(),
        )
        .expect("balloon stats should read from active handler");

        assert_eq!(
            stats.target_pages(),
            amount_mib * VIRTIO_BALLOON_MIB_TO_4K_PAGES
        );
        assert_eq!(stats.actual_pages(), 1);
        assert_eq!(stats.target_mib(), amount_mib);
        assert_eq!(stats.actual_mib(), 0);
    }

    #[test]
    fn boot_runtime_balloon_stats_reads_recorded_guest_optional_stats() {
        let amount_mib = TEST_MEMORY_MIB as u32 / 2;
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_balloon_config(
            "kernel-balloon-optional-stats",
            BalloonConfigInput::new(amount_mib, false).with_stats_polling_interval_s(1),
        );
        configure_boot_balloon_statistics_queues(&mut runtime, &mut mmio_dispatcher);
        write_queued_balloon_statistics_request(&mut memory);
        notify_boot_balloon_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_BALLOON_STATS_QUEUE_INDEX,
        );

        let dispatches = runtime
            .dispatch_balloon_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("balloon stats dispatch should succeed");
        let dispatch = dispatches.as_slice()[0]
            .outcome()
            .dispatched()
            .expect("statistics notification should dispatch");
        assert_eq!(dispatch.statistics_notifications(), 1);
        assert_eq!(read_boot_balloon_statistics_used_index(&memory), 0);
        let device = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should exist")
            .clone();

        let stats = balloon_stats_for_device(
            &device,
            &mut mmio_dispatcher,
            BalloonConfigInput::new(amount_mib, false)
                .with_stats_polling_interval_s(1)
                .into(),
        )
        .expect("balloon stats should read optional stats from active handler");

        assert_eq!(stats.optional().swap_out(), Some(9));
        assert_eq!(stats.optional().free_memory(), Some(0x5678));
        assert_eq!(stats.optional().swap_in(), None);
    }

    #[test]
    fn boot_runtime_balloon_hinting_status_reads_active_handler_state() {
        let (_memory, runtime, mut mmio_dispatcher) = boot_runtime_with_balloon_config(
            "kernel-balloon-hinting-status",
            BalloonConfigInput::new(TEST_MEMORY_MIB as u32 / 2, false).with_free_page_hinting(true),
        );
        let device = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should exist")
            .clone();

        let status = balloon_hinting_status_for_device(&device, &mut mmio_dispatcher)
            .expect("balloon hinting status should read from active handler");

        assert_eq!(status.host_cmd(), VIRTIO_BALLOON_FREE_PAGE_HINT_STOP);
        assert_eq!(status.guest_cmd(), None);
    }

    #[test]
    fn boot_runtime_balloon_hinting_start_stop_updates_active_handler_state() {
        let (_memory, runtime, mut mmio_dispatcher) = boot_runtime_with_balloon_config(
            "kernel-balloon-hinting-start-stop",
            BalloonConfigInput::new(TEST_MEMORY_MIB as u32 / 2, false).with_free_page_hinting(true),
        );
        let device = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should exist")
            .clone();

        start_balloon_hinting_for_device(
            &device,
            &mut mmio_dispatcher,
            BalloonHintingStartInput::new(false),
        )
        .expect("balloon hinting start should update active handler");

        let started = balloon_hinting_status_for_device(&device, &mut mmio_dispatcher)
            .expect("balloon hinting status should read after start");
        assert_eq!(started.host_cmd(), VIRTIO_BALLOON_FREE_PAGE_HINT_DONE + 1);
        assert_eq!(started.guest_cmd(), None);

        stop_balloon_hinting_for_device(&device, &mut mmio_dispatcher)
            .expect("balloon hinting stop should update active handler");

        let stopped = balloon_hinting_status_for_device(&device, &mut mmio_dispatcher)
            .expect("balloon hinting status should read after stop");
        assert_eq!(stopped.host_cmd(), VIRTIO_BALLOON_FREE_PAGE_HINT_DONE);
        assert_eq!(stopped.guest_cmd(), None);
    }

    #[test]
    fn boot_runtime_balloon_hinting_status_rejects_without_hinting_queue() {
        let (_memory, runtime, mut mmio_dispatcher) =
            boot_runtime_with_balloon("kernel-balloon-hinting-status-disabled");
        let device = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should exist")
            .clone();

        let err = balloon_hinting_status_for_device(&device, &mut mmio_dispatcher)
            .expect_err("balloon hinting status should require hinting support");

        assert_eq!(err, BalloonHintingStatusError::HintingNotEnabled);
    }

    #[test]
    fn boot_runtime_balloon_hinting_start_rejects_without_hinting_queue() {
        let (_memory, runtime, mut mmio_dispatcher) =
            boot_runtime_with_balloon("kernel-balloon-hinting-start-disabled");
        let device = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should exist")
            .clone();

        let err = start_balloon_hinting_for_device(
            &device,
            &mut mmio_dispatcher,
            BalloonHintingStartInput::new(true),
        )
        .expect_err("balloon hinting start should require hinting support");

        assert_eq!(err, BalloonHintingCommandError::HintingNotEnabled);
    }

    #[test]
    fn boot_runtime_balloon_notification_dispatch_executes_queued_deflate_request() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_balloon("kernel-balloon-deflate-dispatch");
        configure_boot_balloon_queues(&mut runtime, &mut mmio_dispatcher);
        write_queued_balloon_deflate_request(&mut memory);
        notify_boot_balloon_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
        );

        let dispatches = runtime
            .dispatch_balloon_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("balloon dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("queued deflate notification should dispatch");
        assert_eq!(
            dispatch.drained_notifications(),
            [VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX]
        );
        assert_eq!(dispatch.inflate_notifications(), 0);
        assert_eq!(dispatch.deflate_notifications(), 1);
        assert!(dispatch.inflate_queue_dispatch().is_none());
        let deflate = dispatch
            .deflate_queue_dispatch()
            .expect("deflate queue dispatch should be present");
        assert_eq!(deflate.completed_descriptors(), 1);
        assert!(deflate.needs_queue_interrupt());
        assert_eq!(read_boot_balloon_deflate_used_index(&memory), 1);
    }

    #[test]
    fn boot_runtime_balloon_notification_dispatch_preserves_partial_error_interrupt_intent() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_balloon("kernel-balloon-partial-error-dispatch");
        configure_boot_balloon_queues(&mut runtime, &mut mmio_dispatcher);
        write_partially_invalid_balloon_inflate_request(&mut memory);
        notify_boot_balloon_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
        );

        let dispatches = runtime
            .dispatch_balloon_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("balloon dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        assert!(device_dispatch.needs_queue_interrupt());
        let err = device_dispatch
            .outcome()
            .dispatch_error()
            .expect("partial inflate dispatch error should be preserved");
        let completed = err
            .completed_notification_dispatch()
            .expect("completed notification metadata should be preserved");
        let inflate = completed
            .inflate_queue_dispatch()
            .expect("completed inflate metadata should be present");
        assert_eq!(inflate.completed_descriptors(), 1);
        assert!(inflate.needs_queue_interrupt());
        assert_eq!(read_boot_balloon_inflate_used_index(&memory), 1);
        assert_eq!(
            read_boot_balloon_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus,
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn boot_runtime_balloon_notification_dispatch_preserves_missing_handler() {
        let (mut memory, mut runtime, _) =
            boot_runtime_with_balloon("kernel-balloon-missing-handler-dispatch");
        let mut mmio_dispatcher = MmioDispatcher::new();

        let dispatches = runtime
            .dispatch_balloon_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("balloon dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        let device_dispatch = &dispatches.as_slice()[0];
        assert!(device_dispatch.outcome().handler_lookup_error().is_some());
        assert!(!device_dispatch.needs_queue_interrupt());
    }

    #[test]
    fn boot_runtime_balloon_notification_dispatch_preserves_wrong_handler() {
        let (mut memory, mut runtime, _) =
            boot_runtime_with_balloon("kernel-balloon-wrong-handler-dispatch");
        let region = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should exist")
            .registration
            .region();
        let mut mmio_dispatcher = MmioDispatcher::new();
        mmio_dispatcher
            .insert_region(region.id(), region.range().start(), region.range().size())
            .expect("replacement balloon region should insert");
        mmio_dispatcher
            .register_handler(region.id(), WrongBalloonHandler)
            .expect("wrong balloon handler should register");

        let dispatches = runtime
            .dispatch_balloon_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("balloon dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        let device_dispatch = &dispatches.as_slice()[0];
        assert!(device_dispatch.outcome().handler_lookup_error().is_some());
        assert!(!device_dispatch.needs_queue_interrupt());
    }

    #[test]
    fn boot_runtime_entropy_notification_dispatch_accepts_empty_entropy_device() {
        let kernel = temp_file("kernel-empty-entropy-dispatch", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[]))
                .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        let mut provider = RecordingEntropySourceProvider::default();

        let dispatches = runtime
            .dispatch_entropy_queue_notifications_with_source(
                &mut memory,
                &mut mmio_dispatcher,
                &mut provider,
            )
            .expect("empty entropy dispatch result should allocate");

        assert!(dispatches.is_empty());
        assert_eq!(dispatches.len(), 0);
        assert!(!dispatches.needs_queue_interrupt());
        assert!(provider.requested_regions.is_empty());
    }

    #[test]
    fn boot_runtime_entropy_notification_dispatch_without_pending_notification_is_noop() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_entropy("kernel-entropy-noop-dispatch");
        let mut provider = RecordingEntropySourceProvider::default();

        let dispatches = runtime
            .dispatch_entropy_queue_notifications_with_source(
                &mut memory,
                &mut mmio_dispatcher,
                &mut provider,
            )
            .expect("entropy dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(!dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        assert_eq!(
            device_dispatch.device().registration.region_id(),
            MmioRegionId::new(100)
        );
        assert_eq!(device_dispatch.device().fdt_device.interrupt_line, line(36));
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("no pending entropy notification should dispatch as no-op");
        assert!(dispatch.drained_notifications().is_empty());
        assert!(dispatch.queue_dispatch().is_none());
        assert!(!device_dispatch.needs_queue_interrupt());
        assert_eq!(device_dispatch.rate_limiter_retry_after(), None);
        assert!(provider.requested_regions.is_empty());
        assert_eq!(
            read_boot_entropy_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus,
            ),
            0
        );
    }

    #[test]
    fn boot_runtime_entropy_notification_dispatch_executes_queued_request() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_entropy("kernel-entropy-request-dispatch");
        configure_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher, TEST_USED_RING);
        write_boot_entropy_request(&mut memory, 16);
        notify_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher);
        let mut provider = RecordingEntropySourceProvider::default();

        let dispatches = runtime
            .dispatch_entropy_queue_notifications_with_source(
                &mut memory,
                &mut mmio_dispatcher,
                &mut provider,
            )
            .expect("entropy dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(dispatches.needs_queue_interrupt());
        assert_eq!(dispatches.rate_limiter_retry_after(), None);
        let device_dispatch = &dispatches.as_slice()[0];
        assert_eq!(device_dispatch.device().fdt_device.interrupt_line, line(36));
        assert!(device_dispatch.needs_queue_interrupt());
        assert_eq!(device_dispatch.rate_limiter_retry_after(), None);
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("queued entropy notification should dispatch");
        assert_eq!(dispatch.drained_notifications(), [0]);
        let queue = dispatch
            .queue_dispatch()
            .expect("entropy queue dispatch summary should be present");
        assert_eq!(queue.processed_requests(), 1);
        assert_eq!(queue.successful_requests(), 1);
        assert_eq!(queue.bytes_written_to_guest(), 16);
        assert!(queue.needs_queue_interrupt());
        assert_eq!(provider.requested_regions, [MmioRegionId::new(100)]);
        assert_eq!(provider.source.calls, [16]);
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, 16),
            (0_u8..16).collect::<Vec<_>>()
        );
        assert_eq!(read_boot_entropy_used_index(&memory), 1);
        assert_eq!(read_boot_entropy_used_element(&memory, 0), (0, 16));
        assert_eq!(
            read_boot_entropy_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus,
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn boot_runtime_entropy_notification_dispatch_reports_rate_limiter_retry() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_entropy_rate_limiter(
            "kernel-entropy-rate-limit-retry-dispatch",
            EntropyRateLimiterConfig::new(
                Some(EntropyTokenBucketConfig::new(16, None, 86_400_000)),
                None,
            ),
        );
        configure_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher, TEST_USED_RING);
        write_two_boot_entropy_requests(&mut memory, 16);
        notify_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher);
        let mut provider = RecordingEntropySourceProvider::default();

        let dispatches = runtime
            .dispatch_entropy_queue_notifications_with_source(
                &mut memory,
                &mut mmio_dispatcher,
                &mut provider,
            )
            .expect("entropy dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(dispatches.needs_queue_interrupt());
        let retry_after = dispatches
            .rate_limiter_retry_after()
            .expect("rate-limited entropy dispatch should report retry delay");
        assert!(retry_after > Duration::ZERO);
        assert!(retry_after <= Duration::from_secs(86_400));
        let device_dispatch = &dispatches.as_slice()[0];
        assert_eq!(
            device_dispatch.rate_limiter_retry_after(),
            Some(retry_after)
        );
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("queued entropy notification should dispatch");
        assert_eq!(dispatch.rate_limiter_retry_after(), Some(retry_after));
        let queue = dispatch
            .queue_dispatch()
            .expect("entropy queue dispatch summary should be present");
        assert_eq!(queue.processed_requests(), 1);
        assert_eq!(queue.successful_requests(), 1);
        assert_eq!(queue.rate_limiter_throttled_requests(), 1);
        assert_eq!(queue.rate_limiter_retry_after(), Some(retry_after));
        assert_eq!(provider.source.calls, [16]);
        assert_eq!(read_boot_entropy_used_index(&memory), 1);
        assert_eq!(read_boot_entropy_used_element(&memory, 0), (0, 16));
        assert_eq!(read_guest_bytes(&memory, SECOND_DATA_ADDR, 16), vec![0; 16]);
    }

    #[test]
    fn boot_runtime_entropy_notification_dispatch_accepts_os_entropy_source() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_entropy("kernel-entropy-os-source-dispatch");
        configure_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher, TEST_USED_RING);
        write_boot_entropy_request(&mut memory, 16);
        notify_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher);
        let mut provider = VirtioRngOsEntropySource::new();

        let dispatches = runtime
            .dispatch_entropy_queue_notifications_with_source(
                &mut memory,
                &mut mmio_dispatcher,
                &mut provider,
            )
            .expect("entropy dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("queued entropy notification should dispatch");
        let queue = dispatch
            .queue_dispatch()
            .expect("entropy queue dispatch summary should be present");
        assert_eq!(queue.processed_requests(), 1);
        assert_eq!(queue.successful_requests(), 1);
        assert_eq!(queue.bytes_written_to_guest(), 16);
        assert_eq!(read_boot_entropy_used_index(&memory), 1);
        assert_eq!(read_boot_entropy_used_element(&memory, 0), (0, 16));
        assert_eq!(
            read_boot_entropy_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus,
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn boot_runtime_entropy_notification_dispatch_reports_source_provider_error() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_entropy("kernel-entropy-provider-error-dispatch");
        configure_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher, TEST_USED_RING);
        write_boot_entropy_request(&mut memory, 16);
        notify_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher);
        let mut provider = RecordingEntropySourceProvider::failing();

        let dispatches = runtime
            .dispatch_entropy_queue_notifications_with_source(
                &mut memory,
                &mut mmio_dispatcher,
                &mut provider,
            )
            .expect("entropy dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(!dispatches.needs_queue_interrupt());
        let error = dispatches.as_slice()[0]
            .outcome()
            .entropy_source_error()
            .expect("source-provider error should be preserved");
        assert_eq!(error.message(), "test entropy source unavailable");
        assert_eq!(provider.requested_regions, [MmioRegionId::new(100)]);
        assert!(provider.source.calls.is_empty());
        assert_eq!(read_boot_entropy_used_index(&memory), 0);
        assert_eq!(
            read_boot_entropy_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus,
            ),
            0
        );
    }

    #[test]
    fn boot_runtime_entropy_notification_dispatch_reports_missing_handler() {
        let (mut memory, mut runtime, original_mmio_dispatcher) =
            boot_runtime_with_entropy("kernel-entropy-missing-handler-dispatch");
        drop(original_mmio_dispatcher);
        let mut mmio_dispatcher = MmioDispatcher::new();
        let mut provider = RecordingEntropySourceProvider::default();

        let dispatches = runtime
            .dispatch_entropy_queue_notifications_with_source(
                &mut memory,
                &mut mmio_dispatcher,
                &mut provider,
            )
            .expect("entropy dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(!dispatches.needs_queue_interrupt());
        let error = dispatches.as_slice()[0]
            .outcome()
            .handler_lookup_error()
            .expect("missing entropy handler should be reported");
        assert!(matches!(
            error,
            crate::mmio::MmioHandlerLookupError::MissingHandler { region_id }
                if *region_id == MmioRegionId::new(100)
        ));
        assert!(provider.requested_regions.is_empty());
    }

    #[test]
    fn boot_runtime_entropy_notification_dispatch_preserves_partial_error_interrupt_intent() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_entropy("kernel-entropy-partial-error-dispatch");
        configure_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher, TEST_USED_RING);
        write_partially_invalid_entropy_request(&mut memory);
        notify_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher);
        let mut provider = RecordingEntropySourceProvider::default();

        let dispatches = runtime
            .dispatch_entropy_queue_notifications_with_source(
                &mut memory,
                &mut mmio_dispatcher,
                &mut provider,
            )
            .expect("entropy dispatch result should allocate");

        assert!(dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        assert!(device_dispatch.needs_queue_interrupt());
        let error = device_dispatch
            .outcome()
            .dispatch_error()
            .expect("partial entropy failure should be preserved as a device error");
        assert_eq!(error.drained_notifications(), [0]);
        let completed = error
            .completed_dispatch()
            .expect("partial entropy failure should preserve completed dispatch metadata");
        assert_eq!(completed.processed_requests(), 1);
        assert_eq!(completed.successful_requests(), 1);
        assert_eq!(completed.bytes_written_to_guest(), 16);
        assert!(completed.needs_queue_interrupt());
        assert_eq!(provider.source.calls, [16]);
        assert_eq!(read_boot_entropy_used_index(&memory), 1);
        assert_eq!(read_boot_entropy_used_element(&memory, 0), (0, 16));
        assert_eq!(
            read_boot_entropy_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus,
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn boot_runtime_vsock_notification_dispatch_without_pending_notification_is_noop() {
        let kernel = temp_file("kernel-vsock-noop-dispatch", &arm64_image());
        let socket_path = missing_path("vsock-noop.sock");
        let mut controller = controller_with_kernel(kernel.path());
        add_vsock(&mut controller, 42, &socket_path);
        let config = Arm64BootResourceConfig {
            vsock_interrupt_line: Some(line(35)),
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;

        let dispatches = runtime
            .dispatch_vsock_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("vsock dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(!dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        assert_eq!(device_dispatch.device().registration.guest_cid(), 42);
        assert_eq!(device_dispatch.device().fdt_device.interrupt_line, line(35));
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("no pending notification should dispatch as no-op");
        assert!(dispatch.drained_notifications().is_empty());
        assert!(dispatch.tx_queue_dispatch().is_none());
        assert!(!device_dispatch.needs_queue_interrupt());
        assert_eq!(
            read_boot_vsock_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
        assert!(socket_path.exists());
        drop(mmio_dispatcher);
        assert!(!socket_path.exists());
    }

    #[test]
    fn boot_runtime_vsock_notification_dispatch_executes_tx_queue() {
        let kernel = temp_file("kernel-vsock-tx-dispatch", &arm64_image());
        let socket_path = missing_path("vsock-tx.sock");
        let mut controller = controller_with_kernel(kernel.path());
        add_vsock(&mut controller, 43, &socket_path);
        let config = Arm64BootResourceConfig {
            vsock_interrupt_line: Some(line(35)),
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        configure_boot_vsock_queues(&mut runtime, &mut mmio_dispatcher);
        write_boot_vsock_tx_packet_header(&mut memory, VirtioVsockPacketHeader::new());
        write_boot_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_boot_vsock_tx_available_heads(&mut memory, &[0]);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_TX_QUEUE_INDEX,
        );

        let dispatches = runtime
            .dispatch_vsock_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("vsock dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        assert_eq!(device_dispatch.device().registration.guest_cid(), 43);
        assert_eq!(device_dispatch.device().fdt_device.interrupt_line, line(35));
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("queued TX notification should dispatch");
        assert_eq!(
            dispatch.drained_notifications(),
            [VIRTIO_VSOCK_TX_QUEUE_INDEX]
        );
        let tx_dispatch = dispatch
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(tx_dispatch.processed_packets(), 1);
        assert_eq!(tx_dispatch.successful_packets(), 1);
        assert_eq!(tx_dispatch.parse_failures(), 0);
        assert!(device_dispatch.needs_queue_interrupt());
        assert_eq!(read_boot_vsock_tx_used_index(&memory), 1);
        assert_eq!(
            read_boot_vsock_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert!(socket_path.exists());
        drop(mmio_dispatcher);
        assert!(!socket_path.exists());
    }

    #[test]
    fn boot_runtime_vsock_notification_dispatch_reports_rx_noop() {
        let kernel = temp_file("kernel-vsock-rx-noop-dispatch", &arm64_image());
        let socket_path = missing_path("vsock-rx-noop.sock");
        let mut controller = controller_with_kernel(kernel.path());
        add_vsock(&mut controller, 44, &socket_path);
        let config = Arm64BootResourceConfig {
            vsock_interrupt_line: Some(line(35)),
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        configure_boot_vsock_queues(&mut runtime, &mut mmio_dispatcher);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_RX_QUEUE_INDEX,
        );

        let dispatches = runtime
            .dispatch_vsock_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("vsock dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(!dispatches.needs_queue_interrupt());
        let dispatch = dispatches.as_slice()[0]
            .outcome()
            .dispatched()
            .expect("RX queue notification should dispatch as no-op");
        assert_eq!(
            dispatch.drained_notifications(),
            [VIRTIO_VSOCK_RX_QUEUE_INDEX]
        );
        let rx = dispatch
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(rx.processed_buffers(), 0);
        assert_eq!(rx.delivered_requests(), 0);
        assert!(!rx.needs_queue_interrupt());
        assert!(dispatch.tx_queue_dispatch().is_none());
        assert_eq!(
            read_boot_vsock_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
        assert!(socket_path.exists());
        drop(mmio_dispatcher);
        assert!(!socket_path.exists());
    }

    #[test]
    fn boot_runtime_vsock_acknowledges_guest_response_after_late_host_connect() {
        let kernel = temp_file("kernel-vsock-late-connect", &arm64_image());
        let socket_path = missing_path("vsock-late-connect.sock");
        let mut controller = controller_with_kernel(kernel.path());
        add_vsock(&mut controller, 44, &socket_path);
        let config = Arm64BootResourceConfig {
            vsock_interrupt_line: Some(line(35)),
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        configure_boot_vsock_queues(&mut runtime, &mut mmio_dispatcher);
        write_boot_vsock_rx_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_boot_vsock_rx_available_heads(&mut memory, &[0]);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_RX_QUEUE_INDEX,
        );

        let first = runtime
            .dispatch_vsock_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("vsock dispatch result should allocate");

        let first_dispatch = first.as_slice()[0]
            .outcome()
            .dispatched()
            .expect("first RX dispatch should be a no-op");
        assert_eq!(
            first_dispatch.drained_notifications(),
            [VIRTIO_VSOCK_RX_QUEUE_INDEX]
        );
        assert_eq!(
            first_dispatch
                .host_request_dispatch()
                .accepted_connections(),
            0
        );
        assert_eq!(read_boot_vsock_rx_used_index(&memory), 0);

        let mut client = UnixStream::connect(&socket_path).expect("host client should connect");
        client
            .write_all(b"CONNECT 4000\n")
            .expect("host CONNECT should write");

        let second = runtime
            .dispatch_vsock_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("late host CONNECT dispatch should allocate");

        assert_eq!(second.len(), 1);
        assert!(second.needs_queue_interrupt());
        let device_dispatch = &second.as_slice()[0];
        assert!(device_dispatch.needs_queue_interrupt());
        let dispatch = device_dispatch
            .outcome()
            .dispatched()
            .expect("late host CONNECT should dispatch");
        assert!(dispatch.drained_notifications().is_empty());
        assert_eq!(dispatch.host_request_dispatch().accepted_connections(), 1);
        assert_eq!(dispatch.host_request_dispatch().completed_requests(), 1);
        assert_eq!(dispatch.host_request_dispatch().dropped_connections(), 0);
        assert_eq!(dispatch.host_request_dispatch().pending_connections(), 0);
        let rx = dispatch
            .rx_queue_dispatch()
            .expect("late host CONNECT should produce RX dispatch");
        assert_eq!(rx.processed_buffers(), 1);
        assert_eq!(rx.delivered_requests(), 1);
        assert!(rx.needs_queue_interrupt());
        assert_eq!(read_boot_vsock_rx_used_index(&memory), 1);
        assert_eq!(
            read_boot_vsock_rx_used_element(&memory, 0),
            (0, VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32)
        );
        let header = read_boot_vsock_packet_header(&memory, TEST_VSOCK_HEADER);
        assert_eq!(header.src_cid(), VIRTIO_VSOCK_HOST_CID);
        assert_eq!(header.dst_cid(), 44);
        assert_eq!(header.src_port(), VSOCK_HOST_LOCAL_PORT_BASE);
        assert_eq!(header.dst_port(), 4000);
        assert_eq!(header.operation(), VIRTIO_VSOCK_OP_REQUEST);
        assert_eq!(header.packet_type(), VIRTIO_VSOCK_PACKET_TYPE_STREAM);
        assert_eq!(
            header.buffer_allocation(),
            VIRTIO_VSOCK_CONNECTION_BUFFER_SIZE
        );

        let response_header = VirtioVsockPacketHeader::new()
            .with_src_cid(44)
            .with_dst_cid(VIRTIO_VSOCK_HOST_CID)
            .with_src_port(4000)
            .with_dst_port(VSOCK_HOST_LOCAL_PORT_BASE)
            .with_packet_type(VIRTIO_VSOCK_PACKET_TYPE_STREAM)
            .with_operation(VIRTIO_VSOCK_OP_RESPONSE);
        write_boot_vsock_tx_packet_header(&mut memory, response_header);
        write_boot_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_boot_vsock_tx_available_heads(&mut memory, &[0]);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_TX_QUEUE_INDEX,
        );

        let third = runtime
            .dispatch_vsock_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("guest RESPONSE dispatch should allocate");

        assert_eq!(third.len(), 1);
        assert!(third.needs_queue_interrupt());
        let dispatch = third.as_slice()[0]
            .outcome()
            .dispatched()
            .expect("guest RESPONSE should dispatch");
        assert_eq!(
            dispatch.drained_notifications(),
            [VIRTIO_VSOCK_TX_QUEUE_INDEX]
        );
        assert_eq!(dispatch.guest_response_dispatch().response_packets(), 1);
        assert_eq!(
            dispatch.guest_response_dispatch().acknowledged_responses(),
            1
        );
        let tx = dispatch
            .tx_queue_dispatch()
            .expect("guest RESPONSE should produce TX dispatch");
        assert_eq!(tx.processed_packets(), 1);
        assert_eq!(tx.successful_packets(), 1);
        assert_eq!(read_boot_vsock_tx_used_index(&memory), 1);
        assert_eq!(read_boot_vsock_tx_used_element(&memory, 0), (0, 0));
        let mut ok = [0; "OK 1073741824\n".len()];
        client
            .read_exact(&mut ok)
            .expect("host stream should receive OK");
        assert_eq!(&ok, b"OK 1073741824\n");
        assert_eq!(
            read_boot_vsock_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert!(socket_path.exists());
        drop(mmio_dispatcher);
        assert!(!socket_path.exists());
    }

    #[test]
    fn boot_runtime_vsock_notification_dispatch_accepts_event_queue_as_noop() {
        let kernel = temp_file("kernel-vsock-event-noop", &arm64_image());
        let socket_path = missing_path("vsock-event-noop.sock");
        let mut controller = controller_with_kernel(kernel.path());
        add_vsock(&mut controller, 44, &socket_path);
        let config = Arm64BootResourceConfig {
            vsock_interrupt_line: Some(line(35)),
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        configure_boot_vsock_queues(&mut runtime, &mut mmio_dispatcher);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_EVENT_QUEUE_INDEX,
        );

        let dispatches = runtime
            .dispatch_vsock_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("vsock dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(!dispatches.needs_queue_interrupt());
        let dispatch = dispatches.as_slice()[0]
            .outcome()
            .dispatched()
            .expect("event queue should be accepted as no-op dispatch");
        assert_eq!(
            dispatch.drained_notifications(),
            [VIRTIO_VSOCK_EVENT_QUEUE_INDEX]
        );
        assert_eq!(dispatch.event_notifications(), 1);
        assert!(dispatch.rx_queue_dispatch().is_none());
        assert!(dispatch.tx_queue_dispatch().is_none());
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(
            read_boot_vsock_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
        assert!(socket_path.exists());
        drop(mmio_dispatcher);
        assert!(!socket_path.exists());
    }

    #[test]
    fn boot_runtime_vsock_notification_dispatch_preserves_partial_error_interrupt_intent() {
        let kernel = temp_file("kernel-vsock-partial-error-dispatch", &arm64_image());
        let socket_path = missing_path("vsock-partial-error.sock");
        let mut controller = controller_with_kernel(kernel.path());
        add_vsock(&mut controller, 45, &socket_path);
        let config = Arm64BootResourceConfig {
            vsock_interrupt_line: Some(line(35)),
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        configure_boot_vsock_queues(&mut runtime, &mut mmio_dispatcher);
        write_boot_vsock_tx_packet_header(&mut memory, VirtioVsockPacketHeader::new());
        write_boot_vsock_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            ),
        );
        write_boot_vsock_tx_available_heads(&mut memory, &[0, TEST_QUEUE_SIZE]);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_TX_QUEUE_INDEX,
        );

        let dispatches = runtime
            .dispatch_vsock_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("vsock dispatch result should allocate");

        assert!(dispatches.needs_queue_interrupt());
        let device_dispatch = &dispatches.as_slice()[0];
        assert!(device_dispatch.needs_queue_interrupt());
        let error = device_dispatch
            .outcome()
            .dispatch_error()
            .expect("partial TX failure should be preserved as a device error");
        assert_eq!(error.drained_notifications(), [VIRTIO_VSOCK_TX_QUEUE_INDEX]);
        let completed = error
            .completed_tx_dispatch()
            .expect("partial TX failure should preserve completed dispatch metadata");
        assert_eq!(completed.processed_packets(), 1);
        assert_eq!(completed.successful_packets(), 1);
        assert!(completed.needs_queue_interrupt());
        assert_eq!(read_boot_vsock_tx_used_index(&memory), 1);
        assert_eq!(
            read_boot_vsock_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert!(socket_path.exists());
        drop(mmio_dispatcher);
        assert!(!socket_path.exists());
    }

    #[test]
    fn boot_runtime_vsock_notification_dispatch_reports_missing_handler() {
        let kernel = temp_file("kernel-vsock-missing-handler", &arm64_image());
        let socket_path = missing_path("vsock-missing-handler.sock");
        let mut controller = controller_with_kernel(kernel.path());
        add_vsock(&mut controller, 46, &socket_path);
        let config = Arm64BootResourceConfig {
            vsock_interrupt_line: Some(line(35)),
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let original_mmio_dispatcher = parts.mmio_dispatcher;
        drop(original_mmio_dispatcher);
        let mut mmio_dispatcher = MmioDispatcher::new();
        let mut runtime = parts.runtime;

        let dispatches = runtime
            .dispatch_vsock_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("vsock dispatch result should allocate");

        assert_eq!(dispatches.len(), 1);
        assert!(!dispatches.needs_queue_interrupt());
        let error = dispatches.as_slice()[0]
            .outcome()
            .handler_lookup_error()
            .expect("missing vsock handler should be reported");
        assert!(matches!(
            error,
            crate::mmio::MmioHandlerLookupError::MissingHandler {
                region_id
            } if *region_id == MmioRegionId::new(90)
        ));
        assert!(!socket_path.exists());
    }

    #[test]
    fn boot_runtime_vsock_notification_dispatch_reports_wrong_handler_type() {
        let kernel = temp_file("kernel-vsock-wrong-handler", &arm64_image());
        let socket_path = missing_path("vsock-wrong-handler.sock");
        let mut controller = controller_with_kernel(kernel.path());
        add_vsock(&mut controller, 47, &socket_path);
        let config = Arm64BootResourceConfig {
            vsock_interrupt_line: Some(line(35)),
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            ..valid_config(&[])
        };
        let resources = Arm64BootResources::assemble_from_controller(&controller, config)
            .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let original_mmio_dispatcher = parts.mmio_dispatcher;
        drop(original_mmio_dispatcher);
        let mut runtime = parts.runtime;
        let region = runtime
            .vsock_device
            .as_ref()
            .expect("vsock device should exist")
            .registration
            .region();
        let mut mmio_dispatcher = MmioDispatcher::new();
        mmio_dispatcher
            .insert_region(region.id(), region.range().start(), region.range().size())
            .expect("replacement region should insert");
        mmio_dispatcher
            .register_handler(
                region.id(),
                SerialMmioDevice::new(SharedSerialOutputBuffer::default()),
            )
            .expect("serial handler should register under vsock region id for test");

        let dispatches = runtime
            .dispatch_vsock_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("vsock dispatch result should allocate");

        let error = dispatches.as_slice()[0]
            .outcome()
            .handler_lookup_error()
            .expect("wrong vsock handler type should be reported");
        assert!(matches!(
            error,
            crate::mmio::MmioHandlerLookupError::UnexpectedHandlerType {
                region_id,
                ..
            } if *region_id == MmioRegionId::new(90)
        ));
        assert!(!socket_path.exists());
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
        assert!(dispatch.drained_notifications().is_empty());
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
        assert!(dispatch.drained_notifications().is_empty());
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
        assert!(dispatch.drained_notifications().is_empty());
        assert!(dispatch.queue_dispatch().is_none());
        assert!(!device_dispatch.needs_queue_interrupt());
        assert_eq!(device_dispatch.rate_limiter_retry_after(), None);
        assert_eq!(dispatches.rate_limiter_retry_after(), None);
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
        assert!(second.drained_notifications().is_empty());
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
        assert!(
            first
                .outcome()
                .dispatched()
                .expect("first device should dispatch as no-op")
                .drained_notifications()
                .is_empty()
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
    fn boot_runtime_block_notification_dispatch_reports_earliest_rate_limiter_retry() {
        let kernel = temp_file("kernel-block-rate-limit-retry", &arm64_image());
        let first_block = temp_file(
            "block-rate-limit-retry-first",
            &vec![0x11; (VIRTIO_BLOCK_SECTOR_SIZE * 2) as usize],
        );
        let second_block = temp_file(
            "block-rate-limit-retry-second",
            &vec![0x22; (VIRTIO_BLOCK_SECTOR_SIZE * 2) as usize],
        );
        let mut controller = controller_with_kernel(kernel.path());
        add_drive_with_rate_limiter(
            &mut controller,
            "rootfs",
            first_block.path(),
            true,
            DriveRateLimiterConfig::new(None, Some(DriveTokenBucketConfig::new(1, None, 60_000))),
        );
        add_drive_with_rate_limiter(
            &mut controller,
            "data",
            second_block.path(),
            false,
            DriveRateLimiterConfig::new(None, Some(DriveTokenBucketConfig::new(1, None, 30_000))),
        );
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_config(&[line(32), line(33)]),
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut memory = parts.memory;
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let mut runtime = parts.runtime;
        let first_layout = block_queue_layout(0);
        let second_layout = block_queue_layout(1);
        configure_boot_block_queue_at(
            &mut runtime,
            &mut mmio_dispatcher,
            0,
            first_layout.descriptor_table,
            first_layout.available_ring,
            first_layout.used_ring,
            8,
        );
        configure_boot_block_queue_at(
            &mut runtime,
            &mut mmio_dispatcher,
            1,
            second_layout.descriptor_table,
            second_layout.available_ring,
            second_layout.used_ring,
            8,
        );
        write_two_queued_block_read_requests(&mut memory, first_layout);
        write_two_queued_block_read_requests(&mut memory, second_layout);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 1);

        let dispatches = runtime
            .dispatch_block_queue_notifications(&mut memory, &mut mmio_dispatcher)
            .expect("block dispatch result should allocate");

        assert_eq!(dispatches.len(), 2);
        assert!(dispatches.needs_queue_interrupt());
        let first = &dispatches.as_slice()[0];
        let second = &dispatches.as_slice()[1];
        let first_retry = first
            .rate_limiter_retry_after()
            .expect("first dispatch should report retry delay");
        let second_retry = second
            .rate_limiter_retry_after()
            .expect("second dispatch should report retry delay");
        assert!(first_retry > second_retry);
        assert_eq!(dispatches.rate_limiter_retry_after(), Some(second_retry));
        assert!(first_retry <= Duration::from_secs(60));
        assert!(second_retry <= Duration::from_secs(30));
        assert_eq!(
            first
                .outcome()
                .dispatched()
                .expect("first block queue should dispatch")
                .queue_dispatch()
                .expect("first queue dispatch should be present")
                .rate_limiter_throttled_requests(),
            1
        );
        assert_eq!(
            second
                .outcome()
                .dispatched()
                .expect("second block queue should dispatch")
                .queue_dispatch()
                .expect("second queue dispatch should be present")
                .rate_limiter_throttled_requests(),
            1
        );
        assert_eq!(
            read_guest_bytes(&memory, first_layout.second_status, 1),
            [0]
        );
        assert_eq!(
            read_guest_bytes(&memory, second_layout.second_status, 1),
            [0]
        );
    }

    #[test]
    fn boot_runtime_update_block_device_backing_updates_matching_device_only() {
        let kernel = temp_file("kernel-block-refresh", &arm64_image());
        let first_block = temp_file("block-refresh-first", &[0x11; 512]);
        let second_block = temp_file("block-refresh-second", &[0x22; 512]);
        let replacement = temp_file("block-refresh-replacement", &[0x33; 1024]);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", first_block.path());
        add_drive_with_root(&mut controller, "data", second_block.path(), false);
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_config(&[line(32), line(33)]),
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let runtime = parts.runtime;
        let updated = DriveConfigInput::new("data", "data", replacement.path(), false)
            .validate()
            .expect("updated drive config should validate");

        runtime
            .update_block_device_backing(&mut mmio_dispatcher, &updated)
            .expect("matching block device should refresh");

        let first_region = runtime.block_devices[0].registration.region_id();
        let first = mmio_dispatcher
            .handler_mut::<VirtioBlockMmioHandler>(first_region)
            .expect("first block handler should exist");
        assert_eq!(first.device_config_handler().capacity_sectors(), 1);
        assert_eq!(first.activation_handler().backing().len(), 512);
        assert_eq!(
            first.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(0)
        );
        assert_eq!(
            first.read_register(VirtioMmioRegister::InterruptStatus),
            Ok(0)
        );

        let second_region = runtime.block_devices[1].registration.region_id();
        let second = mmio_dispatcher
            .handler_mut::<VirtioBlockMmioHandler>(second_region)
            .expect("second block handler should exist");
        assert_eq!(second.device_config_handler().capacity_sectors(), 2);
        assert_eq!(second.activation_handler().backing().len(), 1024);
        assert_eq!(
            second.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(1)
        );
        assert_eq!(
            second.read_register(VirtioMmioRegister::InterruptStatus),
            Ok(DeviceInterruptKind::Config.status().bits())
        );
    }

    #[test]
    fn boot_runtime_update_block_device_rate_limiter_updates_matching_device_only() {
        let kernel = temp_file("kernel-block-rate-limiter", &arm64_image());
        let first_block = temp_file("block-rate-limiter-first", &[0x11; 512]);
        let second_block = temp_file("block-rate-limiter-second", &[0x22; 512]);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", first_block.path());
        add_drive_with_root(&mut controller, "data", second_block.path(), false);
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_config(&[line(32), line(33)]),
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let runtime = parts.runtime;
        let updated = DriveConfigInput::new("data", "data", second_block.path(), false)
            .with_rate_limiter(DriveRateLimiterConfig::new(
                Some(DriveTokenBucketConfig::new(1024, Some(2048), 100)),
                None,
            ))
            .validate()
            .expect("updated drive config should validate");

        update_block_device_for_devices_with_opened(
            &runtime.block_devices,
            &mut mmio_dispatcher,
            &updated,
            None,
            updated.rate_limiter(),
        )
        .expect("matching block device rate limiter should refresh");

        let first_region = runtime.block_devices[0].registration.region_id();
        let first = mmio_dispatcher
            .handler_mut::<VirtioBlockMmioHandler>(first_region)
            .expect("first block handler should exist");
        assert_eq!(first.device_config_handler().capacity_sectors(), 1);
        assert_eq!(first.activation_handler().backing().len(), 512);
        assert!(first.activation_handler().rate_limiter().is_none());
        assert_eq!(
            first.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(0)
        );
        assert_eq!(
            first.read_register(VirtioMmioRegister::InterruptStatus),
            Ok(0)
        );

        let second_region = runtime.block_devices[1].registration.region_id();
        let second = mmio_dispatcher
            .handler_mut::<VirtioBlockMmioHandler>(second_region)
            .expect("second block handler should exist");
        assert_eq!(second.device_config_handler().capacity_sectors(), 1);
        assert_eq!(second.activation_handler().backing().len(), 512);
        assert!(second.activation_handler().rate_limiter().is_some());
        assert_eq!(
            second.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(0)
        );
        assert_eq!(
            second.read_register(VirtioMmioRegister::InterruptStatus),
            Ok(0)
        );
    }

    #[test]
    fn boot_runtime_update_block_device_backing_rejects_unknown_drive() {
        let kernel = temp_file("kernel-block-refresh-unknown", &arm64_image());
        let block = temp_file("block-refresh-unknown", &[0x11; 512]);
        let replacement = temp_file("block-refresh-unknown-replacement", &[0x33; 1024]);
        let mut controller = controller_with_kernel(kernel.path());
        add_drive(&mut controller, "rootfs", block.path());
        let resources =
            Arm64BootResources::assemble_from_controller(&controller, valid_config(&[line(32)]))
                .expect("boot resources should assemble");
        let parts = resources.into_parts();
        let mut mmio_dispatcher = parts.mmio_dispatcher;
        let runtime = parts.runtime;
        let updated = DriveConfigInput::new("missing", "missing", replacement.path(), false)
            .validate()
            .expect("updated drive config should validate");

        let err = runtime
            .update_block_device_backing(&mut mmio_dispatcher, &updated)
            .expect_err("unknown block device should fail");

        assert_eq!(
            err,
            DriveUpdateError::UnknownDrive {
                drive_id: "missing".to_string()
            }
        );
        let region = runtime.block_devices[0].registration.region_id();
        let handler = mmio_dispatcher
            .handler_mut::<VirtioBlockMmioHandler>(region)
            .expect("block handler should exist");
        assert_eq!(handler.device_config_handler().capacity_sectors(), 1);
        assert_eq!(handler.activation_handler().backing().len(), 512);
        assert_eq!(
            handler.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(0)
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
    fn memory_size_bytes_rejects_oversized_unchecked_config() {
        let mem_size_mib = aarch64::DRAM_MEM_MAX_SIZE / MIB + 1;
        let config = MachineConfig::new_unchecked_for_tests(1, mem_size_mib);

        let err = super::memory_size_bytes(config).expect_err("oversized memory should fail");

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
