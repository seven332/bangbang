//! Detached, value-redacted storage state shared by live VMM owners.

use std::fmt;

use crate::block::{DriveConfig, VirtioBlockDeviceCaptureState};
use crate::interrupt::GuestInterruptLine;
use crate::memory::GuestMemoryRange;
use crate::mmio::MmioRegion;
use crate::pci::PciSbdf;
use crate::pmem::{
    PmemBackingMappingIdentity, PmemConfig, PmemFileBackingIdentity, VirtioPmemDeviceCaptureState,
};
use crate::virtio_mmio::VirtioMmioTransportState;
use crate::virtio_pci::VirtioPciTransportState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageDeviceOrigin {
    Startup,
    Runtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageRetryState {
    None,
    Immediate,
    After { remaining_nanos: u64 },
}

#[derive(Clone, PartialEq, Eq)]
pub struct StorageMmioTransportState {
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    transport: VirtioMmioTransportState,
}

impl StorageMmioTransportState {
    pub const fn new(
        region: MmioRegion,
        interrupt_line: GuestInterruptLine,
        transport: VirtioMmioTransportState,
    ) -> Self {
        Self {
            region,
            interrupt_line,
            transport,
        }
    }

    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    pub const fn interrupt_line(&self) -> GuestInterruptLine {
        self.interrupt_line
    }

    pub const fn transport(&self) -> &VirtioMmioTransportState {
        &self.transport
    }
}

impl fmt::Debug for StorageMmioTransportState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StorageMmioTransportState")
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct StoragePciTransportState {
    origin: StorageDeviceOrigin,
    sbdf: PciSbdf,
    bar_range: GuestMemoryRange,
    transport: VirtioPciTransportState,
}

impl StoragePciTransportState {
    pub const fn new(
        origin: StorageDeviceOrigin,
        sbdf: PciSbdf,
        bar_range: GuestMemoryRange,
        transport: VirtioPciTransportState,
    ) -> Self {
        Self {
            origin,
            sbdf,
            bar_range,
            transport,
        }
    }

    pub const fn origin(&self) -> StorageDeviceOrigin {
        self.origin
    }

    pub const fn sbdf(&self) -> PciSbdf {
        self.sbdf
    }

    pub const fn bar_range(&self) -> GuestMemoryRange {
        self.bar_range
    }

    pub const fn transport(&self) -> &VirtioPciTransportState {
        &self.transport
    }
}

impl fmt::Debug for StoragePciTransportState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoragePciTransportState")
            .field("origin", &self.origin)
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum StorageTransportState {
    Mmio(StorageMmioTransportState),
    Pci(StoragePciTransportState),
}

impl fmt::Debug for StorageTransportState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mmio(_) => formatter.write_str("StorageTransportState::Mmio(<redacted>)"),
            Self::Pci(state) => formatter
                .debug_tuple("StorageTransportState::Pci")
                .field(&state.origin())
                .field(&"<redacted>")
                .finish(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CaptureReadyBlockDeviceState {
    config: DriveConfig,
    transport: StorageTransportState,
    retry: StorageRetryState,
    device: VirtioBlockDeviceCaptureState,
}

impl CaptureReadyBlockDeviceState {
    pub fn new(
        config: DriveConfig,
        transport: StorageTransportState,
        retry: StorageRetryState,
        device: VirtioBlockDeviceCaptureState,
    ) -> Self {
        Self {
            config,
            transport,
            retry,
            device,
        }
    }

    pub fn drive_id(&self) -> &str {
        self.config.drive_id()
    }

    pub const fn config(&self) -> &DriveConfig {
        &self.config
    }

    pub const fn is_root_device(&self) -> bool {
        self.config.is_root_device()
    }

    pub const fn transport(&self) -> &StorageTransportState {
        &self.transport
    }

    pub const fn retry(&self) -> StorageRetryState {
        self.retry
    }

    pub const fn device(&self) -> &VirtioBlockDeviceCaptureState {
        &self.device
    }
}

impl fmt::Debug for CaptureReadyBlockDeviceState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureReadyBlockDeviceState")
            .field("drive_id", &self.config.drive_id())
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CaptureReadyPmemDeviceState {
    config: PmemConfig,
    guest_range: GuestMemoryRange,
    backing: PmemFileBackingIdentity,
    mapping: PmemBackingMappingIdentity,
    transport: StorageTransportState,
    retry: StorageRetryState,
    device: VirtioPmemDeviceCaptureState,
}

impl CaptureReadyPmemDeviceState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: PmemConfig,
        guest_range: GuestMemoryRange,
        backing: PmemFileBackingIdentity,
        mapping: PmemBackingMappingIdentity,
        transport: StorageTransportState,
        retry: StorageRetryState,
        device: VirtioPmemDeviceCaptureState,
    ) -> Self {
        Self {
            config,
            guest_range,
            backing,
            mapping,
            transport,
            retry,
            device,
        }
    }

    pub fn pmem_id(&self) -> &str {
        self.config.id()
    }

    pub const fn config(&self) -> &PmemConfig {
        &self.config
    }

    pub const fn root_device(&self) -> bool {
        self.config.root_device()
    }

    pub const fn guest_range(&self) -> GuestMemoryRange {
        self.guest_range
    }

    pub const fn backing(&self) -> PmemFileBackingIdentity {
        self.backing
    }

    pub const fn mapping(&self) -> &PmemBackingMappingIdentity {
        &self.mapping
    }

    pub const fn transport(&self) -> &StorageTransportState {
        &self.transport
    }

    pub const fn retry(&self) -> StorageRetryState {
        self.retry
    }

    pub const fn device(&self) -> &VirtioPmemDeviceCaptureState {
        &self.device
    }
}

impl fmt::Debug for CaptureReadyPmemDeviceState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureReadyPmemDeviceState")
            .field("pmem_id", &self.config.id())
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CaptureReadyStorageState {
    block_devices: Vec<CaptureReadyBlockDeviceState>,
    pmem_devices: Vec<CaptureReadyPmemDeviceState>,
    shared_block_retry: StorageRetryState,
    shared_pmem_retry: StorageRetryState,
}

impl CaptureReadyStorageState {
    pub fn new(
        block_devices: Vec<CaptureReadyBlockDeviceState>,
        pmem_devices: Vec<CaptureReadyPmemDeviceState>,
        shared_block_retry: StorageRetryState,
        shared_pmem_retry: StorageRetryState,
    ) -> Self {
        Self {
            block_devices,
            pmem_devices,
            shared_block_retry,
            shared_pmem_retry,
        }
    }

    pub fn block_devices(&self) -> &[CaptureReadyBlockDeviceState] {
        &self.block_devices
    }

    pub fn pmem_devices(&self) -> &[CaptureReadyPmemDeviceState] {
        &self.pmem_devices
    }

    pub const fn shared_block_retry(&self) -> StorageRetryState {
        self.shared_block_retry
    }

    pub const fn shared_pmem_retry(&self) -> StorageRetryState {
        self.shared_pmem_retry
    }
}

impl fmt::Debug for CaptureReadyStorageState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureReadyStorageState")
            .field("block_devices", &self.block_devices.len())
            .field("pmem_devices", &self.pmem_devices.len())
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CaptureReadyStorageConfigs {
    drives: Vec<DriveConfig>,
    pmem: Vec<PmemConfig>,
}

impl CaptureReadyStorageConfigs {
    pub fn new(drives: Vec<DriveConfig>, pmem: Vec<PmemConfig>) -> Self {
        Self { drives, pmem }
    }

    pub fn drives(&self) -> &[DriveConfig] {
        &self.drives
    }

    pub fn pmem(&self) -> &[PmemConfig] {
        &self.pmem
    }
}

impl fmt::Debug for CaptureReadyStorageConfigs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureReadyStorageConfigs")
            .field("drives", &self.drives.len())
            .field("pmem", &self.pmem.len())
            .finish()
    }
}
